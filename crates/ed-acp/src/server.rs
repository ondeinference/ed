//! The stdio wiring: one handler per ACP method, each spawned off the dispatch loop when it
//! awaits anything (a model list, MCP servers, disk, or a turn that makes client requests).

use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    AgentAuthCapabilities, AgentCapabilities, AuthenticateRequest, CancelNotification,
    CloseSessionRequest, DeleteSessionRequest, Implementation, InitializeRequest,
    InitializeResponse, ListSessionsRequest, LoadSessionRequest, LogoutCapabilities, LogoutRequest,
    McpCapabilities, NewSessionRequest, PromptRequest, ResumeSessionRequest,
    SessionAdditionalDirectoriesCapabilities, SessionCapabilities, SessionCloseCapabilities,
    SessionDeleteCapabilities, SessionListCapabilities, SessionResumeCapabilities,
    SetSessionConfigOptionRequest,
};
use agent_client_protocol::{Agent, Stdio};

use crate::agent::{self, AcpServer};
use crate::{AgentInfo, Profile, cli};

/// How the server was started.
#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    /// Default for each session's auto-approve: `--yolo` or `<PREFIX>_YOLO=1`.
    pub yolo: bool,
    /// `tui` when launched by the terminal UI (it sets `<PREFIX>_SURFACE=tui`), else `acp`.
    pub surface: &'static str,
}

impl ServeOptions {
    /// `<PREFIX>_YOLO` and `<PREFIX>_SURFACE` from the environment.
    pub fn from_env(info: &AgentInfo) -> Self {
        Self {
            yolo: cli::env_flag(&info.env.var("YOLO")),
            surface: cli::surface(&info.env),
        }
    }
}

/// Serve ACP over stdin/stdout until the client disconnects, then stop every MCP server.
/// Logs must go to stderr: stdout carries JSON-RPC only.
pub async fn serve_stdio(
    profile: Arc<dyn Profile>,
    opts: ServeOptions,
) -> agent_client_protocol::Result<()> {
    let info = profile.info();
    let prompt_caps = profile.prompt_capabilities();
    let agent = AcpServer::new(profile, opts.yolo, opts.surface);
    tracing::info!("{} starting with model {}", info.name, agent.model());

    let result = Agent
        .builder()
        .name(info.name)
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: InitializeRequest, responder, _cx| {
                    agent.set_client_caps(req.client_capabilities.clone());
                    // Terminal auth is only usable by clients that can run it.
                    let methods = if agent::supports_terminal_auth(&req.client_capabilities) {
                        agent::auth_methods()
                    } else {
                        Vec::new()
                    };
                    responder.respond(
                        InitializeResponse::new(agent::negotiate_version(req.protocol_version))
                            .auth_methods(methods)
                            .agent_capabilities(
                                AgentCapabilities::new()
                                    .prompt_capabilities(prompt_caps.clone())
                                    .session_capabilities(
                                        SessionCapabilities::new()
                                            .list(SessionListCapabilities::new())
                                            .additional_directories(
                                                SessionAdditionalDirectoriesCapabilities::new(),
                                            )
                                            .resume(SessionResumeCapabilities::new())
                                            .close(SessionCloseCapabilities::new())
                                            .delete(SessionDeleteCapabilities::new()),
                                    )
                                    .auth(
                                        AgentAuthCapabilities::new()
                                            .logout(LogoutCapabilities::new()),
                                    )
                                    .mcp_capabilities(McpCapabilities::new().http(true))
                                    .load_session(true),
                            )
                            .agent_info(Implementation::new(info.name, info.version)),
                    )
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: AuthenticateRequest, responder, cx| {
                    let agent = agent.clone();
                    cx.spawn(async move {
                        match agent.authenticate(req).await {
                            Ok(resp) => responder.respond(resp),
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |_req: LogoutRequest, responder, cx| {
                    let agent = agent.clone();
                    cx.spawn(async move {
                        match agent.logout().await {
                            Ok(resp) => responder.respond(resp),
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: NewSessionRequest, responder, cx| {
                    // Listing models and starting MCP servers are slow; keep them off the
                    // dispatch loop.
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        // Picks up a key stored by `--setup` since this process started.
                        if !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent.auth_required());
                        }
                        match agent.new_session(req).await {
                            Ok(resp) => {
                                let id = resp.session_id.clone();
                                responder.respond(resp)?;
                                agent.send_commands(&connection, &id)
                            }
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: ListSessionsRequest, responder, cx| {
                    // Reads the session directory from disk.
                    let agent = agent.clone();
                    cx.spawn(async move {
                        match tokio::task::spawn_blocking(move || agent.list_sessions(req)).await {
                            Ok(Ok(resp)) => responder.respond(resp),
                            Ok(Err(e)) => responder.respond_with_error(e),
                            Err(e) => responder.respond_with_internal_error(e.to_string()),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: LoadSessionRequest, responder, cx| {
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        // Editors reopen a thread with session/load after terminal auth, so
                        // AUTH_REQUIRED here shows sign-in instead of a dead thread.
                        if !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent.auth_required());
                        }
                        let id = req.session_id.clone();
                        match agent.load_session(req, &connection).await {
                            Ok(resp) => {
                                responder.respond(resp)?;
                                agent.send_commands(&connection, &id)
                            }
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: ResumeSessionRequest, responder, cx| {
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        if !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent.auth_required());
                        }
                        let id = req.session_id.clone();
                        match agent.resume_session(req, &connection).await {
                            Ok(resp) => {
                                responder.respond(resp)?;
                                agent.send_commands(&connection, &id)
                            }
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: CloseSessionRequest, responder, _cx| match agent.close_session(req)
                {
                    Ok(resp) => responder.respond(resp),
                    Err(e) => responder.respond_with_error(e),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: DeleteSessionRequest, responder, _cx| match agent
                    .delete_session(req)
                {
                    Ok(resp) => responder.respond(resp),
                    Err(e) => responder.respond_with_error(e),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: SetSessionConfigOptionRequest, responder, cx| {
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        match agent.set_config_option(req, &connection).await {
                            Ok(resp) => responder.respond(resp),
                            Err(e) => responder.respond_with_error(e),
                        }
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = agent.clone();
                async move |req: PromptRequest, responder, cx| {
                    // Run the turn off the dispatch loop so it can make requests to the client.
                    let agent = agent.clone();
                    let connection = cx.clone();
                    cx.spawn(async move {
                        if !agent.has_api_key() && !agent.refresh_credentials().await {
                            return responder.respond_with_error(agent.auth_required());
                        }
                        agent.prompt(req, responder, connection).await
                    })
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let agent = agent.clone();
                async move |n: CancelNotification, _cx| {
                    agent.cancel(&n.session_id);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Stdio::new())
        .await;
    // The editor went away: stop MCP servers instead of leaving them to be orphaned.
    agent.shutdown().await;
    result
}
