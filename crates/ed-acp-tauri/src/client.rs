//! The ACP client side, and the agent it serves in-process.
//!
//! [`AgentState::connection`] starts both halves on first use:
//!
//! - `ed_acp::serve(profile)` on one end of a [`Channel::duplex`], exactly what the product's
//!   agent runs over stdio;
//! - an ACP client on the other end, which forwards `session/update` notifications and
//!   `session/request_permission` requests to the webview, serves `fs/read_text_file` and
//!   `fs/write_text_file` inside the session's roots, and opens the page of a URL-mode
//!   elicitation in the system browser.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    AuthMethodId, AuthenticateRequest, CancelNotification, ClientCapabilities,
    CompleteElicitationNotification, ContentBlock, CreateElicitationRequest,
    CreateElicitationResponse, ElicitationAcceptAction, ElicitationAction, ElicitationCapabilities,
    ElicitationMode, ElicitationUrlCapabilities, ErrorCode, FileSystemCapabilities, Implementation,
    InitializeRequest, ListSessionsRequest, LoadSessionRequest, LogoutRequest, NewSessionRequest,
    PromptRequest, ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigOption, SessionConfigOptionValue, SessionId, SessionNotification,
    SetSessionConfigOptionRequest, TextContent, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
use ed_acp::llm::LlmClient;
use ed_acp::{cli, LlmConfig, ServeOptions};
use log::{error, info, warn};
use serde_json::json;
use tauri::{AppHandle, Emitter};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;

use crate::{
    AgentSessionOpened, AgentSessionSummary, AgentStatus, AgentTurnEndedPayload, HostConfig, Roots,
    AUTH_REQUIRED,
};

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<RequestPermissionOutcome>>>>;
/// The roots each open session may use, by session id.
type SessionRoots = Arc<Mutex<HashMap<String, Roots>>>;

/// Managed Tauri state: the live connection and the permission requests waiting on the user.
pub struct AgentState {
    config: HostConfig,
    connection: tokio::sync::Mutex<Option<Live>>,
    permissions: Pending,
    roots: SessionRoots,
}

struct Live {
    conn: ConnectionTo<Agent>,
    /// The ids of the auth methods the agent advertised in `initialize`.
    auth_methods: Vec<String>,
    /// Dropped (or fired) to hang up, which stops the agent too.
    _hang_up: oneshot::Sender<()>,
    /// Set when the client side has ended, so the next call reconnects.
    ended: Arc<AtomicBool>,
}

/// The `cwd` to ask `session/list` for, or `None` for every stored session.
///
/// On a desktop the agent's data directory can be shared with the command-line agent, so the
/// app asks only for the conversations in the folder it opened. On iOS that directory is
/// inside the app's container, which nothing else writes to and whose path changes with every
/// app update. Sessions stored under the old path would never match, and the user's earlier
/// conversations would drop out of the list. Loading one rebinds it to the current path.
fn list_filter(cwd: &Path, private_data_dir: bool) -> Option<PathBuf> {
    (!private_data_dir).then(|| cwd.to_path_buf())
}

fn list_request(roots: &Roots) -> ListSessionsRequest {
    ListSessionsRequest::new().cwd(list_filter(&roots.cwd, cfg!(target_os = "ios")))
}

/// What the agent will use, without starting it: the profile's own session when it has an
/// auth provider, else the environment and the stored `env` file.
pub fn status(host: &HostConfig) -> AgentStatus {
    let profile = (host.profile)();
    let config = match profile.auth() {
        Some(auth) => auth.llm_config(),
        None => Some(LlmConfig::from_env(&profile.info().env)),
    };
    match config {
        Some(config) => AgentStatus {
            available: true,
            has_key: config.api_key.is_some(),
            model: config.model,
        },
        None => AgentStatus {
            available: true,
            has_key: false,
            model: profile.info().env.default_model.to_string(),
        },
    }
}

/// What `<agent> --setup` does for Onde Inference, minus the terminal: check the key with Onde
/// Cloud, then write it to the agent's `env` file (mode 0600). With a `model` it is written as
/// `<PREFIX>_MODEL` beside the key and used for the check, so the agent doesn't fall back to a
/// default tier the key isn't allowed to call; without one, or when the key can't call it,
/// the model the check passed with is written instead (see `LlmClient::verify_key`). In the App
/// Store sandbox that file lives in the app's container, so a key stored by the command-line
/// agent isn't shared.
pub async fn set_api_key(host: &HostConfig, key: &str, model: Option<&str>) -> Result<(), String> {
    let info = host.info();
    let key = key.trim();
    if !cli::valid_onde_key(key) {
        return Err("An Onde key looks like \"app-id:app-secret\".".to_string());
    }
    let provider_var = info.env.var("PROVIDER");
    let mut vars = vec![
        (provider_var, "onde".to_string()),
        ("ONDE_API_KEY".to_string(), key.to_string()),
    ];
    if let Some(model) = model {
        vars.push((info.env.var("MODEL"), model.to_string()));
    }
    let config = LlmConfig::from_lookup(&info.env, |name| {
        vars.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(name).ok())
    });
    let configured = config.model.clone();
    let verified = LlmClient::new(config)
        .verify_key()
        .await
        .map_err(|e| format!("Onde Cloud rejected the key: {e:#}"))?;
    if verified != configured {
        let model_var = info.env.var("MODEL");
        vars.retain(|(k, _)| *k != model_var);
        vars.push((model_var, verified));
    }

    let dir = info
        .env
        .config_dir()
        .ok_or("No config directory available.")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("env");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, cli::with_vars(&existing, &vars))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    info!("Stored the agent's Onde key in {}", path.display());
    Ok(())
}

/// Remove the agent's Onde key, and the model that came with it, from its env file. Nothing to
/// do when there is no file or no key in it.
pub fn clear_api_key(host: &HostConfig) -> Result<(), String> {
    let info = host.info();
    let Some(dir) = info.env.config_dir() else {
        return Ok(());
    };
    let path = dir.join("env");
    let existing = match std::fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let (kept, removed) = without_api_key(&existing, &info.env.var("MODEL"));
    if removed {
        std::fs::write(&path, kept).map_err(|e| format!("{}: {e}", path.display()))?;
        info!("Removed the agent's Onde key from {}", path.display());
    }
    Ok(())
}

/// `env` file content without any `ONDE_API_KEY` or `model_var` line, and whether a key was
/// there.
fn without_api_key(existing: &str, model_var: &str) -> (String, bool) {
    let model_prefix = format!("{model_var}=");
    let mut removed = false;
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let is_key = line.starts_with("ONDE_API_KEY=");
            removed |= is_key;
            !is_key && !line.starts_with(&model_prefix)
        })
        .collect();
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    (out, removed)
}

impl AgentState {
    pub fn new(config: HostConfig) -> Self {
        Self {
            config,
            connection: tokio::sync::Mutex::new(None),
            permissions: Pending::default(),
            roots: SessionRoots::default(),
        }
    }

    /// The live connection, starting the agent and the client on first use or after the
    /// previous connection ended.
    async fn connection(
        &self,
        app: &AppHandle,
    ) -> Result<(ConnectionTo<Agent>, Vec<String>), String> {
        let mut slot = self.connection.lock().await;
        if let Some(live) = slot.as_ref() {
            if !live.ended.load(Ordering::SeqCst) {
                return Ok((live.conn.clone(), live.auth_methods.clone()));
            }
        }
        let live = self.connect(app).await?;
        let out = (live.conn.clone(), live.auth_methods.clone());
        *slot = Some(live);
        Ok(out)
    }

    async fn connect(&self, app: &AppHandle) -> Result<Live, String> {
        let (agent_end, client_end) = Channel::duplex();

        let opts = ServeOptions {
            yolo: false,
            surface: self.config.surface,
        };
        let profile = (self.config.profile)();
        let agent_name = profile.info().display_name;
        tauri::async_runtime::spawn(async move {
            if let Err(e) = ed_acp::serve(profile, opts, agent_end).await {
                error!("{agent_name} agent stopped: {e}");
            }
        });

        let (ready_tx, ready_rx) =
            oneshot::channel::<Result<(ConnectionTo<Agent>, Vec<String>), String>>();
        let (hang_up, hung_up) = oneshot::channel::<()>();
        let ended = Arc::new(AtomicBool::new(false));

        let client = Client
            .builder()
            .name(self.config.client_name)
            .on_receive_notification(
                {
                    let app = app.clone();
                    let event = self.config.events.session_update;
                    async move |n: SessionNotification, _cx| {
                        if let Err(e) = app.emit(event, &n) {
                            error!("Failed to emit agent session update: {e:?}");
                        }
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_notification(
                async move |n: CompleteElicitationNotification, _cx| {
                    info!("Sign-in page completed ({})", n.elicitation_id);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                {
                    let app = app.clone();
                    async move |req: CreateElicitationRequest, responder, _cx| {
                        responder.respond(CreateElicitationResponse::new(open_url(&app, &req)))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let app = app.clone();
                    let pending = self.permissions.clone();
                    let event = self.config.events.permission_requested;
                    async move |req: RequestPermissionRequest, responder, cx| {
                        // Wait for the user off the dispatch loop so updates keep flowing.
                        let request_id = uuid::Uuid::new_v4().to_string();
                        let (answer_tx, answer_rx) = oneshot::channel();
                        pending
                            .lock()
                            .unwrap()
                            .insert(request_id.clone(), answer_tx);
                        let payload = json!({ "requestId": request_id, "request": req });
                        if let Err(e) = app.emit(event, &payload) {
                            error!("Failed to emit agent permission request: {e:?}");
                            pending.lock().unwrap().remove(&request_id);
                        }
                        cx.spawn(async move {
                            let outcome = answer_rx
                                .await
                                .unwrap_or(RequestPermissionOutcome::Cancelled);
                            responder.respond(RequestPermissionResponse::new(outcome))
                        })
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let roots = self.roots.clone();
                    async move |req: ReadTextFileRequest, responder, _cx| {
                        let roots = roots.clone();
                        let result =
                            tokio::task::spawn_blocking(move || read_text_file(&roots, &req)).await;
                        match result {
                            Ok(Ok(r)) => responder.respond(r),
                            Ok(Err(e)) => responder.respond_with_error(e),
                            Err(e) => responder.respond_with_internal_error(e.to_string()),
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let roots = self.roots.clone();
                    async move |req: WriteTextFileRequest, responder, _cx| {
                        let roots = roots.clone();
                        let result =
                            tokio::task::spawn_blocking(move || write_text_file(&roots, &req))
                                .await;
                        match result {
                            Ok(Ok(r)) => responder.respond(r),
                            Ok(Err(e)) => responder.respond_with_error(e),
                            Err(e) => responder.respond_with_internal_error(e.to_string()),
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            );

        let ended_flag = ended.clone();
        let pending = self.permissions.clone();
        let client_name = self.config.client_name;
        let client_version = self.config.client_version;
        tauri::async_runtime::spawn(async move {
            let result = client
                .connect_with(client_end, |conn: ConnectionTo<Agent>| async move {
                    let init = conn
                        .send_request(
                            InitializeRequest::new(ProtocolVersion::V1)
                                .client_capabilities(
                                    ClientCapabilities::new()
                                        .fs(FileSystemCapabilities::new()
                                            .read_text_file(true)
                                            .write_text_file(true))
                                        .elicitation(
                                            ElicitationCapabilities::new()
                                                .url(ElicitationUrlCapabilities::new()),
                                        ),
                                )
                                .client_info(Implementation::new(client_name, client_version)),
                        )
                        .block_task()
                        .await;
                    match init {
                        Ok(init) => {
                            let methods = init
                                .auth_methods
                                .iter()
                                .map(|m| m.id().to_string())
                                .collect();
                            let _ = ready_tx.send(Ok((conn, methods)));
                            // Stay connected until the state is dropped or replaced.
                            let _ = hung_up.await;
                            Ok(())
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(format!("Agent failed to start: {e}")));
                            Err(e)
                        }
                    }
                })
                .await;
            if let Err(e) = result {
                warn!("Agent connection ended: {e}");
            }
            ended_flag.store(true, Ordering::SeqCst);
            // Nobody can answer these any more.
            pending.lock().unwrap().clear();
        });

        let (conn, auth_methods) = ready_rx
            .await
            .map_err(|_| "Agent failed to start.".to_string())??;
        info!("Agent connected in-process");
        Ok(Live {
            conn,
            auth_methods,
            _hang_up: hang_up,
            ended,
        })
    }

    fn roots_for(&self, app: &AppHandle, cwd: Option<&Path>) -> Result<Roots, String> {
        (self.config.roots)(app, cwd)
    }

    /// The agent's conversations in `cwd` (the host's default roots when `None`), newest
    /// first.
    pub async fn list_sessions(
        &self,
        app: &AppHandle,
        cwd: Option<PathBuf>,
    ) -> Result<Vec<AgentSessionSummary>, String> {
        let roots = self.roots_for(app, cwd.as_deref())?;
        let (conn, _) = self.connection(app).await?;
        let listed = conn
            .send_request(list_request(&roots))
            .block_task()
            .await
            .map_err(describe)?;
        Ok(listed
            .sessions
            .into_iter()
            .map(|session| AgentSessionSummary {
                session_id: session.session_id.to_string(),
                title: session.title,
                updated_at: session.updated_at,
            })
            .collect())
    }

    /// Open a conversation in `cwd`: the one named by `session_id`, else (unless `fresh`) the
    /// latest in that folder, else a new one.
    pub async fn open_session(
        &self,
        app: &AppHandle,
        cwd: Option<PathBuf>,
        fresh: bool,
        access_token: Option<String>,
        session_id: Option<String>,
    ) -> Result<AgentSessionOpened, String> {
        let roots = self.roots_for(app, cwd.as_deref())?;
        let (conn, _) = self.connection(app).await?;
        let mcp_servers = (self.config.mcp_servers)(app, access_token);
        // The one the user picked, else the latest.
        let picked = session_id.is_some();
        let target = match session_id {
            Some(id) => Some(SessionId::new(id)),
            None if !fresh => conn
                .send_request(list_request(&roots))
                .block_task()
                .await
                .map_err(describe)?
                .sessions
                .into_iter()
                // Newest first.
                .next()
                .map(|latest| latest.session_id),
            None => None,
        };
        if let Some(id) = target {
            // The agent may read files while it replays, so the roots come first.
            self.roots
                .lock()
                .unwrap()
                .insert(id.to_string(), roots.clone());
            let load = LoadSessionRequest::new(id.clone(), roots.cwd.clone())
                .additional_directories(roots.extra.clone())
                .mcp_servers(mcp_servers.clone());
            match conn.send_request(load).block_task().await {
                Ok(loaded) => {
                    return Ok(AgentSessionOpened {
                        session_id: id.to_string(),
                        restored: true,
                        config_options: option_values(loaded.config_options),
                    });
                }
                Err(e) if e.code == ErrorCode::AuthRequired => {
                    self.roots.lock().unwrap().remove(&id.to_string());
                    return Err(AUTH_REQUIRED.to_string());
                }
                Err(e) => {
                    self.roots.lock().unwrap().remove(&id.to_string());
                    // One the user picked and can't have is an error to show.
                    if picked {
                        return Err(describe(e));
                    }
                    // The latest no longer loading shouldn't block a new one.
                    warn!("Could not load agent session {id}: {e}");
                }
            }
        }
        let new = NewSessionRequest::new(roots.cwd.clone())
            .additional_directories(roots.extra.clone())
            .mcp_servers(mcp_servers);
        let session = conn
            .send_request(new)
            .block_task()
            .await
            .map_err(describe)?;
        self.roots
            .lock()
            .unwrap()
            .insert(session.session_id.to_string(), roots);
        Ok(AgentSessionOpened {
            session_id: session.session_id.to_string(),
            restored: false,
            config_options: option_values(session.config_options),
        })
    }

    pub async fn prompt(
        &self,
        app: &AppHandle,
        session_id: String,
        text: String,
    ) -> Result<(), String> {
        let (conn, _) = self.connection(app).await?;
        let request = PromptRequest::new(
            SessionId::new(session_id.clone()),
            vec![ContentBlock::Text(TextContent::new(text))],
        );
        let app = app.clone();
        let event = self.config.events.turn_ended;
        // Returned before the turn ends: a long turn would otherwise hold the invoke
        // callback open for minutes.
        tauri::async_runtime::spawn(async move {
            let result = conn.send_request(request).block_task().await;
            let payload = match result {
                Ok(r) => AgentTurnEndedPayload {
                    session_id,
                    stop_reason: serde_json::to_value(r.stop_reason)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from)),
                    error: None,
                },
                Err(e) => AgentTurnEndedPayload {
                    session_id,
                    stop_reason: None,
                    error: Some(describe(e)),
                },
            };
            if let Err(e) = app.emit(event, &payload) {
                error!("Failed to emit agent turn end: {e:?}");
            }
        });
        Ok(())
    }

    pub async fn cancel(&self, session_id: String) -> Result<(), String> {
        // ACP: a client cancelling a turn answers its open permission requests with
        // `cancelled`.
        for (_, answer) in self.permissions.lock().unwrap().drain() {
            let _ = answer.send(RequestPermissionOutcome::Cancelled);
        }
        let slot = self.connection.lock().await;
        let Some(live) = slot.as_ref() else {
            return Ok(());
        };
        live.conn
            .send_notification(CancelNotification::new(SessionId::new(session_id)))
            .map_err(|e| e.to_string())
    }

    pub fn respond_permission(
        &self,
        request_id: &str,
        option_id: Option<String>,
    ) -> Result<(), String> {
        let answer = self
            .permissions
            .lock()
            .unwrap()
            .remove(request_id)
            .ok_or("That request is no longer waiting for an answer.")?;
        let outcome = match option_id {
            Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
            None => RequestPermissionOutcome::Cancelled,
        };
        let _ = answer.send(outcome);
        Ok(())
    }

    /// Change a config option (the model, auto-approve). `value` is a select value id as a
    /// string, or a boolean. Returns the session's options as they are afterwards.
    pub async fn set_config_option(
        &self,
        app: &AppHandle,
        session_id: String,
        config_id: String,
        value: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, String> {
        let value = match value {
            serde_json::Value::Bool(value) => SessionConfigOptionValue::from(value),
            serde_json::Value::String(value) => SessionConfigOptionValue::from(value.as_str()),
            other => return Err(format!("Not a config option value: {other}")),
        };
        let (conn, _) = self.connection(app).await?;
        let response = conn
            .send_request(SetSessionConfigOptionRequest::new(
                SessionId::new(session_id),
                config_id,
                value,
            ))
            .block_task()
            .await
            .map_err(describe)?;
        Ok(option_values(Some(response.config_options)))
    }

    /// Sign in with one of the agent's auth methods: `method_id`, else the first it
    /// advertised. For a product with its own sign-in this opens the browser through the
    /// client's URL elicitation and returns once the page is finished.
    pub async fn authenticate(
        &self,
        app: &AppHandle,
        method_id: Option<String>,
    ) -> Result<(), String> {
        let (conn, methods) = self.connection(app).await?;
        let method = method_id
            .or_else(|| methods.first().cloned())
            .ok_or("The agent offers no way to sign in.")?;
        conn.send_request(AuthenticateRequest::new(AuthMethodId::new(method)))
            .block_task()
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Sign out: the agent forgets its stored session.
    pub async fn logout(&self, app: &AppHandle) -> Result<(), String> {
        let (conn, _) = self.connection(app).await?;
        conn.send_request(LogoutRequest::new())
            .block_task()
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Answer a URL-mode elicitation by opening its page in the system browser. Only `http` and
/// `https` are opened; the user "declines" anything else, or a browser that won't launch.
fn open_url(app: &AppHandle, req: &CreateElicitationRequest) -> ElicitationAction {
    let ElicitationMode::Url(mode) = &req.mode else {
        // Form mode was never advertised.
        return ElicitationAction::Decline;
    };
    if !(mode.url.starts_with("https://") || mode.url.starts_with("http://")) {
        warn!("Refusing to open a non-web sign-in URL");
        return ElicitationAction::Decline;
    }
    match app.opener().open_url(&mode.url, None::<&str>) {
        Ok(()) => ElicitationAction::Accept(ElicitationAcceptAction::new()),
        Err(e) => {
            warn!("Could not open the sign-in page: {e}");
            ElicitationAction::Decline
        }
    }
}

fn option_values(options: Option<Vec<SessionConfigOption>>) -> Vec<serde_json::Value> {
    options
        .unwrap_or_default()
        .into_iter()
        .filter_map(|option| serde_json::to_value(option).ok())
        .collect()
}

/// [`AUTH_REQUIRED`] for a missing key or session, so the page can ask for sign-in; the
/// agent's message otherwise.
fn describe(e: agent_client_protocol::Error) -> String {
    if e.code == ErrorCode::AuthRequired {
        AUTH_REQUIRED.to_string()
    } else {
        e.to_string()
    }
}

fn outside(path: &Path) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(format!(
        "{} is outside the session's workspace",
        path.display()
    ))
}

fn roots_of(
    roots: &SessionRoots,
    session: &SessionId,
    path: &Path,
) -> agent_client_protocol::Result<Roots> {
    roots
        .lock()
        .unwrap()
        .get(&session.to_string())
        .cloned()
        .filter(|roots| roots.contains(path))
        .ok_or_else(|| outside(path))
}

/// `fs/read_text_file`, honouring the optional 1-based `line` and `limit`.
fn read_text_file(
    roots: &SessionRoots,
    req: &ReadTextFileRequest,
) -> agent_client_protocol::Result<ReadTextFileResponse> {
    roots_of(roots, &req.session_id, &req.path)?;
    let content = std::fs::read_to_string(&req.path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            agent_client_protocol::Error::resource_not_found(Some(req.path.display().to_string()))
        } else {
            agent_client_protocol::Error::internal_error().data(e.to_string())
        }
    })?;
    let skip = req.line.map_or(0, |l| l.saturating_sub(1) as usize);
    let take = req.limit.map_or(usize::MAX, |l| l as usize);
    let content = content
        .lines()
        .skip(skip)
        .take(take)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ReadTextFileResponse::new(content))
}

/// `fs/write_text_file`: the client creates the file and its parent directories when they
/// don't exist.
fn write_text_file(
    roots: &SessionRoots,
    req: &WriteTextFileRequest,
) -> agent_client_protocol::Result<WriteTextFileResponse> {
    roots_of(roots, &req.session_id, &req.path)?;
    let write = || {
        if let Some(parent) = req.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&req.path, &req.content)
    };
    write().map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;
    Ok(WriteTextFileResponse::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_listed_by_directory_unless_the_data_dir_is_private() {
        let cwd = Path::new("/data/agent");
        assert_eq!(list_filter(cwd, false), Some(cwd.to_path_buf()));
        // iOS: the container path changes with every update, so no filter.
        assert_eq!(list_filter(cwd, true), None);
    }

    #[test]
    fn removing_the_key_keeps_every_other_line() {
        let (kept, removed) = without_api_key(
            "AGENT_PROVIDER=onde\nONDE_API_KEY=app:secret\nAGENT_MODEL=x\nAGENT_SURFACE=app\n",
            "AGENT_MODEL",
        );
        assert!(removed);
        assert_eq!(kept, "AGENT_PROVIDER=onde\nAGENT_SURFACE=app\n");
    }

    #[test]
    fn an_exported_key_goes_too_and_a_file_without_one_is_untouched() {
        assert_eq!(
            without_api_key("export ONDE_API_KEY=a:b\n", "AGENT_MODEL"),
            (String::new(), true)
        );
        assert_eq!(
            without_api_key("AGENT_PROVIDER=onde\n", "AGENT_MODEL"),
            ("AGENT_PROVIDER=onde\n".to_string(), false)
        );
        assert_eq!(without_api_key("", "AGENT_MODEL"), (String::new(), false));
    }

    #[test]
    fn a_similar_variable_is_not_the_key() {
        let (kept, removed) =
            without_api_key("ONDE_API_KEY_BACKUP=x\nAGENT_MODEL_X=y\n", "AGENT_MODEL");
        assert!(!removed);
        assert_eq!(kept, "ONDE_API_KEY_BACKUP=x\nAGENT_MODEL_X=y\n");
    }

    #[test]
    fn a_session_reaches_only_its_own_roots() {
        let roots = SessionRoots::default();
        roots.lock().unwrap().insert(
            "a".into(),
            Roots {
                cwd: PathBuf::from("/work/a"),
                extra: Vec::new(),
            },
        );
        let a = SessionId::new("a");
        let b = SessionId::new("b");
        assert!(roots_of(&roots, &a, Path::new("/work/a/src/main.rs")).is_ok());
        assert!(roots_of(&roots, &a, Path::new("/work/b/src/main.rs")).is_err());
        // An id nobody opened has no roots at all.
        assert!(roots_of(&roots, &b, Path::new("/work/a/src/main.rs")).is_err());
    }
}
