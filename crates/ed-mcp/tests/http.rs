//! Streamable HTTP against an in-process rmcp server, and the Ed executor on top of it.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use ed_agent::{ToolExecutor, ToolRisk};
use ed_mcp::{
    ClientInfo, Connection, McpExecutor, McpToolset, ServerSpec, ToolPolicy, ToolsetConfig,
};
use rmcp::RoleServer;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "s3cret";

#[derive(Clone)]
struct Mailbox;

impl ServerHandler for Mailbox {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let schema = Arc::new(json!({"type": "object"}).as_object().unwrap().clone());
        Ok(ListToolsResult {
            tools: vec![
                // Declares itself read-only.
                Tool::new("whoami", "Who is signed in.", schema.clone())
                    .annotate(ToolAnnotations::new().read_only(true)),
                // Says nothing, like a server that predates annotations.
                Tool::new("list_messages", "Recent messages.", schema.clone()),
                Tool::new("send_message", "Send mail.", schema),
            ],
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        Ok(
            CallToolResult::success(vec![ContentBlock::text(format!("ran {}", request.name))])
                .into(),
        )
    }
}

async fn require_bearer(request: Request, next: Next) -> Result<Response, StatusCode> {
    let ok = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        == Some(&format!("Bearer {TOKEN}"));
    if ok {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Serve [`Mailbox`] behind bearer auth; returns its URL.
async fn serve() -> String {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(true)
        .with_sse_keep_alive(None)
        .with_cancellation_token(CancellationToken::new());
    let service: StreamableHttpService<Mailbox, LocalSessionManager> =
        StreamableHttpService::new(|| Ok(Mailbox), Default::default(), config);
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(require_bearer));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}/mcp")
}

fn client() -> ClientInfo {
    ClientInfo::new("ed-mcp-test", "0")
}

#[tokio::test]
async fn connects_lists_and_calls_with_a_bearer_token() {
    let url = serve().await;
    let spec = ServerSpec::http("mail", url).with_bearer(TOKEN);

    let set = McpToolset::connect_specs(ToolsetConfig::new(client()), &[spec], &[]).await;

    let names: Vec<_> = set.tools().iter().map(|t| t.name.clone()).collect();
    assert_eq!(
        names,
        [
            "mcp__mail__list_messages",
            "mcp__mail__send_message",
            "mcp__mail__whoami"
        ]
    );
    let out = set
        .call(
            "mcp__mail__whoami",
            json!({}),
            &CancellationToken::new(),
            &|_| {},
        )
        .await
        .unwrap();
    assert_eq!(out.text, "ran whoami");
    assert!(!out.failed);
    set.shutdown().await;
}

#[tokio::test]
async fn a_connection_lists_and_calls_tools_by_their_own_names() {
    let url = serve().await;
    let spec = ServerSpec::http("mail", url).with_bearer(TOKEN);
    let conn = Connection::connect(&spec, &client(), Duration::from_secs(5))
        .await
        .unwrap();

    let mut names: Vec<_> = conn.tools().iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["list_messages", "send_message", "whoami"]);

    let result = conn
        .call_tool("whoami", json!({}), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "ran whoami");
    let err = conn
        .call_tool("whoami", json!([1]), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("JSON object"), "{err:#}");
}

#[tokio::test]
async fn a_wrong_bearer_token_is_rejected() {
    let url = serve().await;
    let spec = ServerSpec::http("mail", url).with_bearer("nope");

    let err = Connection::connect(&spec, &client(), Duration::from_secs(5))
        .await
        .err()
        .expect("the server answers 401");
    assert!(err.to_string().contains("handshake failed"), "{err:#}");

    // A rejected server is left out, not fatal.
    let set = McpToolset::connect_specs(ToolsetConfig::new(client()), &[spec], &[]).await;
    assert!(set.tools().is_empty());
}

#[tokio::test]
async fn a_missing_bearer_token_is_rejected() {
    let url = serve().await;
    let spec = ServerSpec::http("mail", url);
    assert!(
        Connection::connect(&spec, &client(), Duration::from_secs(5))
            .await
            .is_err()
    );
}

async fn executor(policy: ToolPolicy) -> McpExecutor {
    let url = serve().await;
    let spec = ServerSpec::http("mail", url).with_bearer(TOKEN);
    let set = McpToolset::connect_specs(ToolsetConfig::new(client()), &[spec], &[]).await;
    let exec = McpExecutor::new(policy);
    exec.set_toolset(Some(set)).await;
    exec
}

fn risk(defs: &[ed_agent::AgentToolDefinition], name: &str) -> ToolRisk {
    defs.iter().find(|d| d.name == name).unwrap().risk
}

#[tokio::test]
async fn risk_follows_the_read_only_hint() {
    let defs = executor(ToolPolicy::new()).await.definitions().await;

    assert_eq!(risk(&defs, "mcp__mail__whoami"), ToolRisk::ReadOnly);
    // No hint means it may change things.
    assert_eq!(risk(&defs, "mcp__mail__list_messages"), ToolRisk::Mutating);
    assert_eq!(risk(&defs, "mcp__mail__send_message"), ToolRisk::Mutating);
}

#[tokio::test]
async fn the_host_can_override_risk_and_drop_the_prefix() {
    let policy = ToolPolicy::new()
        .read_only(&["list_messages", "whoami"])
        .unprefixed();
    let exec = executor(policy).await;
    let defs = exec.definitions().await;

    assert_eq!(risk(&defs, "list_messages"), ToolRisk::ReadOnly);
    assert_eq!(risk(&defs, "whoami"), ToolRisk::ReadOnly);
    assert_eq!(risk(&defs, "send_message"), ToolRisk::Mutating);
    assert_eq!(
        defs.iter()
            .find(|d| d.name == "send_message")
            .unwrap()
            .description,
        "Send mail."
    );

    // The executor answers to the names the model sees.
    assert_eq!(
        exec.execute("send_message", "{}").await.unwrap(),
        "ran send_message"
    );
    assert!(exec.execute("mcp__mail__send_message", "{}").await.is_err());
}

#[tokio::test]
async fn disconnecting_removes_the_tools() {
    let exec = executor(ToolPolicy::new()).await;
    assert!(!exec.definitions().await.is_empty());

    exec.set_toolset(None).await;

    assert!(exec.definitions().await.is_empty());
    assert!(exec.execute("mcp__mail__whoami", "{}").await.is_err());
}
