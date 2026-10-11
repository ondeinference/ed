//! MCP for Onde agents, over [`rmcp`], the official Rust SDK.
//!
//! This crate is not an MCP client: `rmcp` is. It owns what every Onde agent was writing for
//! itself around one:
//!
//! - **Server configuration.** Stdio servers supplied by an ACP client in
//!   `session/new|load|resume`, plus process-wide built-in servers ([`SharedServer`]) that start
//!   at most once, on first need. A client-supplied server with the same name as a built-in wins.
//! - **Tool namespacing.** Tools are exposed to the model as `mcp__<server>__<tool>`, sanitised
//!   and capped at [`MAX_TOOL_NAME`] bytes.
//! - **Read-only gating.** A tool is read-only only when its server sets `readOnlyHint: true`.
//! - **Calls.** Progress notifications are handed to a callback, session cancellation is
//!   forwarded as `notifications/cancelled`, and every call has a deadline.
//!
//! A server that fails to start or to list its tools is never fatal: it is logged and its tools
//! are absent.
//!
//! Servers are reached over stdio (a child process) or streamable HTTP with an optional bearer
//! token, described by a [`ServerSpec`].
//!
//! Features: `acp` maps ACP `McpServer` lists, `agent` adds [`McpExecutor`] to expose a toolset
//! as [Ed](ed_agent) tools, and `test-server` adds a scriptable server for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use rmcp::handler::client::progress::ProgressDispatcher;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientRequest, ContentBlock, Implementation,
    ProgressNotificationParam, Request, ServerResult, Tool,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use rmcp;

#[cfg(feature = "agent")]
mod executor;
#[cfg(feature = "agent")]
pub use executor::{McpExecutor, ToolPolicy};

#[cfg(feature = "test-server")]
pub mod test_server;

/// Prefix of every MCP tool name the model sees.
pub const PREFIX: &str = "mcp__";
/// Longest tool name sent to the model; OpenAI-compatible endpoints reject longer ones.
pub const MAX_TOOL_NAME: usize = 64;
/// Time a server has to finish the `initialize` handshake and answer `tools/list`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound for one tool call. Long, since some tools (stem separation) take many minutes.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Split `mcp__<server>__<tool>` into its parts.
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

/// The name the model sees for `tool` on `server`.
pub fn qualified(server: &str, tool: &str) -> String {
    let mut name = format!("{PREFIX}{}__{}", sanitize(server), sanitize(tool));
    name.truncate(MAX_TOOL_NAME);
    name
}

fn sanitize(part: &str) -> String {
    let mut out: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

/// Who is connecting: sent to every server in `initialize` as `clientInfo`.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

impl ClientInfo {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

/// How to reach a server.
#[derive(Clone)]
pub enum Transport {
    /// Start `command args…` with extra environment variables and talk over its stdio.
    Stdio {
        command: PathBuf,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    /// Streamable HTTP. `bearer` is the token without the `Bearer ` prefix.
    Http {
        url: String,
        bearer: Option<String>,
        headers: Vec<(String, String)>,
    },
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdio { command, args, .. } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .finish_non_exhaustive(),
            // The token and header values are credentials; never print them.
            Self::Http { url, .. } => f
                .debug_struct("Http")
                .field("url", url)
                .finish_non_exhaustive(),
        }
    }
}

/// A server to connect to.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub name: String,
    pub transport: Transport,
}

impl ServerSpec {
    /// A stdio server.
    pub fn stdio(
        name: impl Into<String>,
        command: impl Into<PathBuf>,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Self {
        Self {
            name: name.into(),
            transport: Transport::Stdio {
                command: command.into(),
                args,
                env,
            },
        }
    }

    /// A streamable HTTP server.
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: Transport::Http {
                url: url.into(),
                bearer: None,
                headers: Vec::new(),
            },
        }
    }

    /// Send `Authorization: Bearer <token>`. No effect on a stdio server.
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        if let Transport::Http { bearer, .. } = &mut self.transport {
            *bearer = Some(token.into());
        }
        self
    }

    /// Send an extra header on every request. No effect on a stdio server.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        if let Transport::Http { headers, .. } = &mut self.transport {
            headers.push((name.into(), value.into()));
        }
        self
    }

    /// The spec for an ACP-supplied server.
    #[cfg(feature = "acp")]
    pub fn from_acp(server: &agent_client_protocol::schema::v1::McpServer) -> Option<Self> {
        use agent_client_protocol::schema::v1::McpServer;
        match server {
            McpServer::Stdio(s) => Some(Self::stdio(
                s.name.clone(),
                s.command.clone(),
                s.args.clone(),
                s.env
                    .iter()
                    .map(|e| (e.name.clone(), e.value.clone()))
                    .collect(),
            )),
            McpServer::Http(h) => Some(Self {
                name: h.name.clone(),
                transport: Transport::Http {
                    url: h.url.clone(),
                    bearer: None,
                    headers: h
                        .headers
                        .iter()
                        .map(|h| (h.name.clone(), h.value.clone()))
                        .collect(),
                },
            }),
            _ => None,
        }
    }
}

struct Handler {
    progress: ProgressDispatcher,
    info: ClientInfo,
}

impl ClientHandler for Handler {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress.handle_notification(params).await;
    }

    fn get_info(&self) -> rmcp::model::ClientConfig {
        rmcp::model::ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(self.info.name.clone(), self.info.version.clone()),
        )
    }
}

/// One live connection to an MCP server.
pub struct Connection {
    name: String,
    service: RunningService<RoleClient, Handler>,
    tools: Vec<Tool>,
}

impl Connection {
    /// Connect to `spec`, run the handshake and list its tools, each within `timeout`.
    pub async fn connect(spec: &ServerSpec, info: &ClientInfo, timeout: Duration) -> Result<Self> {
        let handler = Handler {
            progress: ProgressDispatcher::new(),
            info: info.clone(),
        };
        let handshake = match &spec.transport {
            Transport::Stdio { command, args, env } => {
                let mut cmd = tokio::process::Command::new(command);
                cmd.args(args).envs(env.iter().map(|(k, v)| (k, v)));
                let transport = TokioChildProcess::new(cmd)
                    .with_context(|| format!("starting {}", command.display()))?;
                tokio::time::timeout(timeout, handler.serve(transport)).await
            }
            Transport::Http {
                url,
                bearer,
                headers,
            } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.as_str());
                if let Some(token) = bearer {
                    config = config.auth_header(token.as_str());
                }
                if !headers.is_empty() {
                    let mut map = HashMap::new();
                    for (name, value) in headers {
                        map.insert(
                            http::HeaderName::from_bytes(name.as_bytes())
                                .with_context(|| format!("invalid header name `{name}`"))?,
                            http::HeaderValue::from_str(value)
                                .with_context(|| format!("invalid value for header `{name}`"))?,
                        );
                    }
                    config = config.custom_headers(map);
                }
                let transport = StreamableHttpClientTransport::from_config(config);
                tokio::time::timeout(timeout, handler.serve(transport)).await
            }
        };
        let service = handshake
            .map_err(|_| anyhow!("no MCP handshake within {}s", timeout.as_secs()))?
            .map_err(|e| anyhow!("MCP handshake failed: {e}"))?;
        let tools = tokio::time::timeout(timeout, service.peer().list_all_tools())
            .await
            .map_err(|_| anyhow!("tools/list timed out"))?
            .map_err(|e| anyhow!("tools/list failed: {e}"))?;
        Ok(Self {
            name: spec.name.clone(),
            service,
            tools,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tools the server listed when it connected, under their own names.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Call a tool on this server directly, outside any turn (for example to build a
    /// permission prompt). Answers `None` on any failure or after `timeout`.
    pub async fn call_raw(&self, tool: &str, args: Value, timeout: Duration) -> Option<Value> {
        self.call_tool(tool, args, timeout).await.ok()
    }

    /// Call `tool` by its name on this server and return the `CallToolResult` as JSON. A
    /// result with `isError: true` is `Ok`; a transport or protocol failure, or no answer
    /// within `timeout`, is an `Err` that says which.
    pub async fn call_tool(&self, tool: &str, args: Value, timeout: Duration) -> Result<Value> {
        let mut params = CallToolRequestParams::new(tool.to_string());
        match args {
            Value::Object(map) => params = params.with_arguments(map),
            Value::Null => {}
            _ => bail!("tool arguments must be a JSON object"),
        }
        let result = tokio::time::timeout(timeout, self.service.peer().call_tool(params))
            .await
            .map_err(|_| anyhow!("timed out after {}s", timeout.as_secs()))?
            .map_err(|e| anyhow!("{e}"))?;
        Ok(serde_json::to_value(result)?)
    }

    async fn close(mut self) {
        let _ = self
            .service
            .close_with_timeout(Duration::from_secs(2))
            .await;
    }
}

enum SharedState {
    NotStarted,
    Unavailable,
    Running(Arc<Connection>),
}

/// A built-in server shared by every session in the process, started at most once, on first
/// need. `resolve` decides at that moment whether it is available: `None` (disabled, binary
/// missing) is logged once and the server stays absent for the life of the process.
///
/// ```ignore
/// static DEMUCS: SharedServer = SharedServer::new("demucs", demucs_spec);
/// ```
pub struct SharedServer {
    name: &'static str,
    resolve: fn() -> Option<ServerSpec>,
    state: Mutex<SharedState>,
}

impl SharedServer {
    pub const fn new(name: &'static str, resolve: fn() -> Option<ServerSpec>) -> Self {
        Self {
            name,
            resolve,
            state: Mutex::const_new(SharedState::NotStarted),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    async fn get(&self, info: &ClientInfo, timeout: Duration) -> Option<Arc<Connection>> {
        let mut state = self.state.lock().await;
        match &*state {
            SharedState::Running(conn) => return Some(conn.clone()),
            SharedState::Unavailable => return None,
            SharedState::NotStarted => {}
        }
        *state = SharedState::Unavailable;
        let Some(spec) = (self.resolve)() else {
            tracing::info!("built-in MCP server `{}` is not available", self.name);
            return None;
        };
        match Connection::connect(&spec, info, timeout).await {
            Ok(conn) => {
                tracing::info!("built-in MCP server `{}` available", self.name);
                let conn = Arc::new(conn);
                *state = SharedState::Running(conn.clone());
                Some(conn)
            }
            Err(e) => {
                tracing::info!("built-in MCP server `{}` unavailable: {e:#}", self.name);
                None
            }
        }
    }

    /// Stop the server if it was started. Call once every session's [`McpToolset`] has been
    /// shut down, so this is the last owner.
    pub async fn shutdown(&self) {
        let state = std::mem::replace(&mut *self.state.lock().await, SharedState::Unavailable);
        if let SharedState::Running(conn) = state
            && let Some(conn) = Arc::into_inner(conn)
        {
            conn.close().await;
        }
    }
}

/// How to connect a session's servers.
#[derive(Clone)]
pub struct ToolsetConfig {
    pub client: ClientInfo,
    pub connect_timeout: Duration,
    pub call_timeout: Duration,
}

impl ToolsetConfig {
    pub fn new(client: ClientInfo) -> Self {
        Self {
            client,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }
}

/// One MCP tool as the model sees it.
#[derive(Debug, Clone)]
pub struct McpTool {
    /// `mcp__<server>__<tool>`.
    pub name: String,
    pub server: String,
    /// The tool's name on its server.
    pub tool: String,
    /// The server's description, prefixed with `[<server> MCP server]`.
    pub description: String,
    /// JSON Schema for the arguments; always has `"type": "object"`.
    pub parameters: Value,
    pub read_only: bool,
}

struct Entry {
    conn: Arc<Connection>,
    tool: McpTool,
}

/// What a call produced, flattened for the model.
#[derive(Debug, Clone)]
pub struct CallOutcome {
    /// Text content joined by newlines; non-text blocks are described in brackets, and
    /// `structuredContent` is used when there is no content at all.
    pub text: String,
    /// The server reported `isError: true`, or the call failed, was cancelled or timed out.
    pub failed: bool,
    /// `structuredContent`, when the server sent one.
    pub structured: Option<Value>,
}

impl CallOutcome {
    fn failed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            failed: true,
            structured: None,
        }
    }
}

/// The MCP tools available to one session.
pub struct McpToolset {
    config: ToolsetConfig,
    tools: HashMap<String, Entry>,
    conns: Vec<Arc<Connection>>,
}

impl McpToolset {
    /// A toolset with no servers.
    pub fn empty(config: ToolsetConfig) -> Self {
        Self {
            config,
            tools: HashMap::new(),
            conns: Vec::new(),
        }
    }

    /// Connect the client-supplied ACP servers (stdio and HTTP) and every built-in the client
    /// did not supply a server of the same name for.
    #[cfg(feature = "acp")]
    pub async fn connect(
        config: ToolsetConfig,
        client_servers: &[agent_client_protocol::schema::v1::McpServer],
        builtins: &[&'static SharedServer],
    ) -> Self {
        let specs: Vec<ServerSpec> = client_servers
            .iter()
            .filter_map(|server| {
                let spec = ServerSpec::from_acp(server);
                if spec.is_none() {
                    tracing::info!("ignoring MCP server of an unsupported transport");
                }
                spec
            })
            .collect();
        Self::connect_specs(config, &specs, builtins).await
    }

    /// Connect `servers` and every built-in no server of the same name was supplied for. A server
    /// that fails is logged and left out.
    pub async fn connect_specs(
        config: ToolsetConfig,
        servers: &[ServerSpec],
        builtins: &[&'static SharedServer],
    ) -> Self {
        let mut set = Self::empty(config);
        for spec in servers {
            match Connection::connect(spec, &set.config.client, set.config.connect_timeout).await {
                Ok(conn) => set.add(Arc::new(conn)),
                Err(e) => tracing::warn!("MCP server `{}` unavailable: {e:#}", spec.name),
            }
        }
        for builtin in builtins {
            if servers.iter().any(|s| s.name == builtin.name()) {
                continue;
            }
            if let Some(conn) = builtin
                .get(&set.config.client, set.config.connect_timeout)
                .await
            {
                set.add(conn);
            }
        }
        set
    }

    fn add(&mut self, conn: Arc<Connection>) {
        for tool in &conn.tools {
            let name = qualified(&conn.name, &tool.name);
            if self.tools.contains_key(&name) {
                tracing::warn!("duplicate MCP tool name {name}; keeping the first");
                continue;
            }
            let mut parameters = Value::Object((*tool.input_schema).clone());
            if parameters.get("type").is_none() {
                parameters["type"] = json!("object");
            }
            let read_only = tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false);
            let entry = McpTool {
                name: name.clone(),
                server: conn.name.clone(),
                tool: tool.name.to_string(),
                description: format!(
                    "[{} MCP server] {}",
                    conn.name,
                    tool.description.as_deref().unwrap_or("")
                ),
                parameters,
                read_only,
            };
            self.tools.insert(
                name,
                Entry {
                    conn: conn.clone(),
                    tool: entry,
                },
            );
        }
        self.conns.push(conn);
    }

    /// Every tool, sorted by name.
    pub fn tools(&self) -> Vec<&McpTool> {
        let mut tools: Vec<&McpTool> = self.tools.values().map(|e| &e.tool).collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    /// The tool the model calls `name`, if any.
    pub fn tool(&self, name: &str) -> Option<&McpTool> {
        self.tools.get(name).map(|e| &e.tool)
    }

    /// Whether `tool` on `server` is connected.
    pub fn has_tool(&self, server: &str, tool: &str) -> bool {
        self.tools.contains_key(&qualified(server, tool))
    }

    /// The connection serving the model-facing tool `name`.
    pub fn connection(&self, name: &str) -> Option<&Connection> {
        self.tools.get(name).map(|e| e.conn.as_ref())
    }

    /// Call `name` with `args`. `on_progress` gets one line per MCP progress notification
    /// (`"Separating (25%)"`). Cancelling `cancel` sends `notifications/cancelled` and returns
    /// a failed outcome. Only an unknown tool or malformed arguments are an `Err`.
    pub async fn call(
        &self,
        name: &str,
        args: Value,
        cancel: &CancellationToken,
        on_progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<CallOutcome> {
        let entry = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow!("unknown tool `{name}`"))?;
        let arguments: Map<String, Value> = match args {
            Value::Object(o) => o,
            Value::Null => Map::new(),
            _ => bail!("tool arguments must be a JSON object"),
        };
        let params = CallToolRequestParams::new(entry.tool.tool.clone()).with_arguments(arguments);
        let mut handle = match entry
            .conn
            .service
            .peer()
            .send_cancellable_request(
                ClientRequest::CallToolRequest(Request::new(params)),
                PeerRequestOptions::no_options(),
            )
            .await
        {
            Ok(handle) => handle,
            Err(e) => {
                return Ok(CallOutcome::failed(format!(
                    "MCP server `{}` is not responding: {e}",
                    entry.conn.name
                )));
            }
        };
        let mut progress = entry
            .conn
            .service
            .service()
            .progress
            .subscribe(handle.progress_token.clone())
            .await;
        let deadline = tokio::time::sleep(self.config.call_timeout);
        tokio::pin!(deadline);

        enum End {
            Done(Box<Result<ServerResult, rmcp::ServiceError>>),
            Cancelled,
            TimedOut,
        }
        let end = loop {
            tokio::select! {
                r = &mut handle.rx => {
                    break End::Done(Box::new(r.unwrap_or(Err(rmcp::ServiceError::TransportClosed))));
                }
                Some(p) = progress.next() => on_progress(progress_line(&p)),
                () = cancel.cancelled() => break End::Cancelled,
                () = &mut deadline => break End::TimedOut,
            }
        };
        Ok(match end {
            End::Done(done) => match *done {
                Ok(ServerResult::CallToolResult(r)) => outcome_from(r),
                Ok(_) => CallOutcome::failed("The MCP server sent an unexpected response."),
                Err(e) => CallOutcome::failed(format!("MCP call failed: {e}")),
            },
            End::Cancelled => {
                let _ = handle.cancel(Some("cancelled by user".into())).await;
                CallOutcome::failed("Cancelled by user.")
            }
            End::TimedOut => {
                let _ = handle.cancel(Some("timed out".into())).await;
                CallOutcome::failed(format!(
                    "Timed out after {} minutes.",
                    self.config.call_timeout.as_secs() / 60
                ))
            }
        })
    }

    /// Close this session's own servers. Built-ins stay up for other sessions.
    pub async fn shutdown(self) {
        drop(self.tools);
        for conn in self.conns {
            if let Some(conn) = Arc::into_inner(conn) {
                conn.close().await;
            }
        }
    }
}

/// One line for an MCP progress notification: its message, and a percentage when it has a
/// total.
pub fn progress_line(p: &ProgressNotificationParam) -> String {
    let pct = p
        .total
        .filter(|t| *t > 0.0)
        .map(|t| format!(" ({:.0}%)", 100.0 * p.progress / t));
    format!(
        "{}{}",
        p.message.as_deref().unwrap_or("Working…"),
        pct.unwrap_or_default()
    )
}

fn outcome_from(r: rmcp::model::CallToolResult) -> CallOutcome {
    let mut parts = Vec::new();
    for block in &r.content {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::Image(i) => parts.push(format!("[image: {}]", i.mime_type)),
            ContentBlock::Audio(a) => parts.push(format!("[audio: {}]", a.mime_type)),
            ContentBlock::ResourceLink(l) => parts.push(format!("[resource: {}]", l.uri)),
            ContentBlock::Resource(_) => parts.push("[embedded resource]".into()),
            _ => parts.push("[unsupported content]".into()),
        }
    }
    let mut text = parts.join("\n");
    if text.is_empty() {
        text = r
            .structured_content
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(no output)".into());
    }
    CallOutcome {
        text,
        failed: r.is_error == Some(true),
        structured: r.structured_content,
    }
}

/// `true` when an `*_OFF`-style switch is set to `off`, `0`, `false` or `no`.
pub fn switched_off(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

/// `override_var` when set and non-empty, else `binary` found on `PATH`.
pub fn find_binary(override_var: &str, binary: &str) -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os(override_var).filter(|v| !v.is_empty()) {
        return Some(bin.into());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| is_file(candidate))
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_names_round_trip() {
        let q = qualified("My Server!", "do.thing");
        assert_eq!(q, "mcp__My_Server__do_thing");
        assert_eq!(split_qualified(&q), Some(("My_Server", "do_thing")));
        assert_eq!(split_qualified("read_file"), None);
        assert_eq!(split_qualified("mcp__x"), None);
        assert_eq!(split_qualified("mcp____t"), None);
        assert!(qualified("s", &"t".repeat(100)).len() <= MAX_TOOL_NAME);
    }

    #[test]
    fn progress_text() {
        let p = ProgressNotificationParam::new(
            rmcp::model::ProgressToken(rmcp::model::NumberOrString::Number(1)),
            25.0,
        )
        .with_total(100.0)
        .with_message("Separating");
        assert_eq!(progress_line(&p), "Separating (25%)");
        let bare = ProgressNotificationParam::new(
            rmcp::model::ProgressToken(rmcp::model::NumberOrString::Number(2)),
            3.0,
        );
        assert_eq!(progress_line(&bare), "Working…");
    }

    #[test]
    fn structured_content_stands_in_for_empty_content() {
        let r: rmcp::model::CallToolResult = serde_json::from_value(json!({
            "content": [],
            "structuredContent": {"stems": []},
            "isError": false
        }))
        .unwrap();
        let out = outcome_from(r);
        assert_eq!(out.text, r#"{"stems":[]}"#);
        assert!(!out.failed);
        assert_eq!(out.structured, Some(json!({"stems": []})));
    }

    #[test]
    fn errors_are_reported_as_failed() {
        let r: rmcp::model::CallToolResult = serde_json::from_value(json!({
            "content": [{"type": "text", "text": "busy"}],
            "isError": true
        }))
        .unwrap();
        let out = outcome_from(r);
        assert_eq!(out.text, "busy");
        assert!(out.failed);
    }

    #[test]
    fn off_switches() {
        // SAFETY: only this test touches this variable.
        unsafe { std::env::set_var("ED_MCP_TEST_SWITCH", "OFF") };
        assert!(switched_off("ED_MCP_TEST_SWITCH"));
        unsafe { std::env::set_var("ED_MCP_TEST_SWITCH", "on") };
        assert!(!switched_off("ED_MCP_TEST_SWITCH"));
        unsafe { std::env::remove_var("ED_MCP_TEST_SWITCH") };
        assert!(!switched_off("ED_MCP_TEST_SWITCH"));
    }
}
