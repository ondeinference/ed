//! [`AcpServer`]: the ACP v1 agent behind every Onde product. Owns sessions (in memory, backed
//! by [`SessionStore`]), the turn loop, auth, and the model and auto-approve config options.
//! Everything product-specific comes from the [`Profile`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodTerminal, AuthenticateRequest, AuthenticateResponse,
    AvailableCommandsUpdate, ClientCapabilities, CloseSessionRequest, CloseSessionResponse,
    ConfigOptionUpdate, ContentBlock, ContentChunk, DeleteSessionRequest, DeleteSessionResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, LoadSessionResponse,
    LogoutResponse, McpServer, MessageId, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, ResumeSessionRequest, ResumeSessionResponse, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionConfigSelectOptions, SessionId,
    SessionInfo, SessionInfoUpdate, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StopReason, ToolCall,
    ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdateFields, UsageUpdate,
};
use agent_client_protocol::{Client, ConnectionTo, Responder};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use ed_mcp::{ClientInfo, McpToolset, ToolsetConfig};

use crate::content as prompt;
use crate::llm::{Delta, LlmClient, LlmConfig, LlmEnv};
use crate::store::{SessionStore, StoredSession};
use crate::tools::{self, ToolCtx, ToolOutcome, Toolset, absolutize, normalize};
use crate::{Profile, PromptCtx, SessionCtx};

const MAX_TURNS: usize = 50;
/// Id of the session config option that selects the model.
pub const MODEL_CONFIG_ID: &str = "model";
/// Id of the boolean session config option that skips permission prompts.
pub const AUTO_APPROVE_CONFIG_ID: &str = "auto_approve";
/// Context window reported in `usage_update` when `<PREFIX>_CONTEXT_WINDOW` is unset.
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;
/// Id of the terminal auth method advertised on initialize.
pub const AUTH_METHOD_ID: &str = "terminal-setup";
/// Cap on the sessions returned by `session/list`.
const MAX_LISTED: usize = 200;

/// The terminal auth method: clients run `<agent> --setup`.
pub fn auth_methods() -> Vec<AuthMethod> {
    vec![AuthMethod::Terminal(
        AuthMethodTerminal::new(AUTH_METHOD_ID, "Run in terminal")
            .description("Interactive setup: store your Onde Inference API key")
            .args(vec!["--setup".into()]),
    )]
}

/// Whether the client can run terminal auth. ACP v1 has the `auth.terminal` capability; clients
/// that predate it (including the ACP registry's validator, which probes every listed agent)
/// say so with `_meta["terminal-auth"]: true` instead. Offering no method to such a client
/// fails registry validation, so either signal counts.
pub fn supports_terminal_auth(caps: &ClientCapabilities) -> bool {
    caps.auth.terminal
        || caps
            .meta
            .as_ref()
            .and_then(|m| m.get("terminal-auth"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

pub fn auth_required_error(agent: &str) -> agent_client_protocol::Error {
    agent_client_protocol::Error::auth_required().data(format!(
        "No Onde API key configured. Authenticate with the terminal method (`{agent} --setup`)."
    ))
}

/// The only protocol version this agent speaks is v1. A client proposing a newer version gets
/// v1 back (it can then decide whether to continue); an older (v0) client is told v1 too, as
/// the spec requires the latest supported version when the requested one isn't supported.
pub fn negotiate_version(client: ProtocolVersion) -> ProtocolVersion {
    let _ = client;
    ProtocolVersion::V1
}

struct Session {
    cwd: PathBuf,
    /// Additional workspace roots (from the request's `additional_directories`).
    roots: Vec<PathBuf>,
    model: String,
    /// Conversation history without the system prompt (rebuilt every turn).
    messages: Vec<Value>,
    cancel: CancellationToken,
    always_allowed: Arc<Mutex<HashSet<String>>>,
    always_rejected: Arc<Mutex<HashSet<String>>>,
    /// Approve tool calls without asking; the `auto_approve` config option. Not persisted:
    /// a reopened session starts from the process default (`--yolo`).
    auto_approve: Arc<AtomicBool>,
    title: Option<String>,
    /// Last activity, seconds since the Unix epoch.
    updated_at: u64,
    mcp: Arc<McpToolset>,
    /// A turn is running; its history is checked out of `messages`.
    busy: bool,
}

impl Session {
    fn fresh(
        cwd: PathBuf,
        roots: Vec<PathBuf>,
        model: String,
        yolo: bool,
        mcp: Arc<McpToolset>,
    ) -> Self {
        Self::restored(
            StoredSession {
                cwd,
                roots,
                model,
                title: None,
                updated_at: now_secs(),
                messages: Vec::new(),
            },
            yolo,
            mcp,
        )
    }

    /// A session brought back from the store. Permission grants start fresh; MCP servers are
    /// connected by the request that reopens it.
    fn restored(stored: StoredSession, yolo: bool, mcp: Arc<McpToolset>) -> Self {
        Self {
            cwd: stored.cwd,
            roots: stored.roots,
            model: stored.model,
            messages: stored.messages,
            cancel: CancellationToken::new(),
            always_allowed: Arc::default(),
            always_rejected: Arc::default(),
            auto_approve: Arc::new(AtomicBool::new(yolo)),
            title: stored.title,
            updated_at: stored.updated_at,
            mcp,
            busy: false,
        }
    }

    fn stored(&self) -> StoredSession {
        StoredSession {
            cwd: self.cwd.clone(),
            roots: self.roots.clone(),
            model: self.model.clone(),
            title: self.title.clone(),
            updated_at: self.updated_at,
            messages: self.messages.clone(),
        }
    }
}

fn new_message_id() -> MessageId {
    MessageId::new(uuid::Uuid::new_v4().to_string())
}

/// ACP v1 requires absolute `cwd` and `additionalDirectories`.
fn check_absolute(cwd: &Path, roots: &[PathBuf]) -> agent_client_protocol::Result<()> {
    if !cwd.is_absolute() {
        return Err(invalid("cwd must be an absolute path"));
    }
    if roots.iter().any(|r| !r.is_absolute()) {
        return Err(invalid(
            "additionalDirectories entries must be absolute paths",
        ));
    }
    Ok(())
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// ISO 8601 UTC timestamp without sub-second precision (no chrono dependency).
pub fn iso8601(secs: u64) -> String {
    let days = secs / 86_400;
    let secs_of_day = secs % 86_400;
    let (h, m, s) = (
        secs_of_day / 3600,
        secs_of_day % 3600 / 60,
        secs_of_day % 60,
    );
    // Civil-from-days algorithm (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Derive a short session title from the first user prompt.
pub fn session_title(prompt: &str) -> String {
    const MAX: usize = 80;
    let first = prompt.lines().next().unwrap_or("").trim();
    if first.chars().count() <= MAX {
        first.to_string()
    } else {
        let cut: String = first.chars().take(MAX - 1).collect();
        format!("{}…", cut.trim_end())
    }
}

fn invalid(msg: impl Into<String>) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(msg.into())
}

fn unknown_session(id: &SessionId) -> agent_client_protocol::Error {
    invalid(format!("unknown session {id}"))
}

/// A tool message; `failed` is kept in a private field that never reaches the model.
fn tool_message(id: &str, content: &str, failed: bool) -> Value {
    let mut m = json!({ "role": "tool", "tool_call_id": id, "content": content });
    if failed {
        m["x_failed"] = json!(true);
    }
    m
}

fn wire_message(m: &Value) -> Value {
    match m.as_object() {
        Some(o) if o.contains_key("x_failed") => {
            let mut o = o.clone();
            o.remove("x_failed");
            Value::Object(o)
        }
        _ => m.clone(),
    }
}

fn locations_of(paths: &[PathBuf]) -> Vec<ToolCallLocation> {
    paths
        .iter()
        .map(|p| ToolCallLocation::new(absolutize(p)))
        .collect()
}

/// Text of a stored user message (`content` is a string, or an array of parts).
fn user_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p.get("text").and_then(Value::as_str).map(String::from),
                Some("image_url") => Some("[image]".into()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

#[derive(Clone)]
pub struct AcpServer {
    profile: Arc<dyn Profile>,
    /// The profile's toolsets, built once.
    toolsets: Arc<Vec<Arc<dyn Toolset>>>,
    env: LlmEnv,
    /// Replaced when credentials change: `authenticate`, `logout`, or a key stored by
    /// `--setup` in another process.
    llm: Arc<Mutex<LlmClient>>,
    /// Process default for each session's `auto_approve` (`--yolo` / `<PREFIX>_YOLO`).
    yolo: bool,
    /// How the agent was launched: `tui` (interactive terminal UI) or `acp` (editor).
    surface: &'static str,
    client_caps: Arc<Mutex<ClientCapabilities>>,
    sessions: Arc<Mutex<HashMap<SessionId, Session>>>,
    /// Models offered for selection, fetched from `GET /models` on first use and dropped
    /// when credentials change.
    models: Arc<tokio::sync::Mutex<Option<Vec<String>>>>,
    store: SessionStore,
    data_dir: PathBuf,
}

impl AcpServer {
    pub fn new(profile: Arc<dyn Profile>, yolo: bool, surface: &'static str) -> Self {
        let env = profile.info().env;
        let data_dir = env.data_dir();
        Self {
            toolsets: Arc::new(profile.toolsets()),
            profile,
            env,
            llm: Arc::new(Mutex::new(LlmClient::new(LlmConfig::from_env(&env)))),
            yolo,
            surface,
            client_caps: Arc::default(),
            sessions: Arc::default(),
            models: Arc::default(),
            store: SessionStore::new(&data_dir),
            data_dir,
        }
    }

    pub fn set_client_caps(&self, caps: ClientCapabilities) {
        *self.client_caps.lock().unwrap() = caps;
    }

    fn llm(&self) -> LlmClient {
        self.llm.lock().unwrap().clone()
    }

    pub fn has_api_key(&self) -> bool {
        self.llm().has_api_key()
    }

    pub fn model(&self) -> String {
        self.llm().model().to_string()
    }

    /// Re-read credentials (environment + stored `env` file) and report whether a key is
    /// configured. Editors run terminal auth (`<agent> --setup`) in a separate process
    /// and then retry `session/new` or `session/load` on this one without calling
    /// `authenticate`, so the stored key has to be picked up here. The model list is only
    /// dropped when the configuration actually changed.
    pub async fn refresh_credentials(&self) -> bool {
        let fresh = LlmConfig::from_env(&self.env);
        let changed = fresh != *self.llm().config();
        if changed {
            tracing::info!("credentials changed; reloading the Onde client");
            *self.llm.lock().unwrap() = LlmClient::new(fresh);
            *self.models.lock().await = None;
        }
        self.has_api_key()
    }

    /// `authenticate`: the client has run `<agent> --setup`; pick up the stored key.
    pub async fn authenticate(
        &self,
        req: AuthenticateRequest,
    ) -> agent_client_protocol::Result<AuthenticateResponse> {
        if &*req.method_id.0 != AUTH_METHOD_ID {
            return Err(invalid(format!("unknown auth method {}", req.method_id.0)));
        }
        if self.refresh_credentials().await {
            Ok(AuthenticateResponse::new())
        } else {
            Err(self.auth_required())
        }
    }

    /// `logout`: remove the stored key and forget it. A key exported in the agent's own
    /// environment can't be removed from here and keeps the agent signed in.
    pub async fn logout(&self) -> agent_client_protocol::Result<LogoutResponse> {
        let removed = match self.env.config_file() {
            Some(path) => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("removing {}: {e}", path.display()))
                }
                _ => Ok(()),
            },
            None => Ok(()),
        };
        self.refresh_credentials().await;
        removed.map_err(|e| agent_client_protocol::Error::internal_error().data(e))?;
        Ok(LogoutResponse::new())
    }

    fn scratch_dir(&self, id: &SessionId) -> Option<PathBuf> {
        let id = crate::store::sanitize_id(&id.0).ok()?;
        Some(self.data_dir.join("scratch").join(id))
    }

    fn remove_scratch(&self, id: &SessionId) {
        if let Some(dir) = self.scratch_dir(id) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// `AUTH_REQUIRED`, naming this agent's setup command.
    pub fn auth_required(&self) -> agent_client_protocol::Error {
        auth_required_error(self.profile.info().name)
    }

    /// How a session's MCP servers are connected.
    fn mcp_config(&self) -> ToolsetConfig {
        let info = self.profile.info();
        ToolsetConfig::new(ClientInfo::new(info.name, info.version))
    }

    async fn connect_mcp(
        &self,
        servers: &[agent_client_protocol::schema::v1::McpServer],
    ) -> Arc<McpToolset> {
        Arc::new(
            McpToolset::connect(
                self.mcp_config(),
                servers,
                &self.profile.builtin_mcp_servers(),
            )
            .await,
        )
    }

    fn system_prompt(&self, cwd: &Path, roots: &[PathBuf], mcp: &McpToolset) -> String {
        self.profile.system_prompt(&PromptCtx {
            surface: self.surface,
            cwd,
            roots,
            mcp,
        })
    }

    /// The selectable models. Falls back to just the configured model if the endpoint
    /// can't list them; the configured model is always offered.
    async fn models(&self) -> Vec<String> {
        let mut cache = self.models.lock().await;
        if let Some(ids) = &*cache {
            return ids.clone();
        }
        let llm = self.llm();
        let default = llm.model().to_string();
        // An explicit list wins; editors show this list as the model picker.
        let configured: Vec<String> = std::env::var(self.env.var("MODELS"))
            .unwrap_or_default()
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        let mut ids = if !configured.is_empty() {
            configured
        } else {
            match llm.models().await {
                Ok(models) => models.into_iter().map(|m| m.id).collect(),
                Err(e) => {
                    tracing::warn!("listing models failed, offering only {default}: {e:#}");
                    Vec::new()
                }
            }
        };
        if !ids.contains(&default) {
            ids.insert(0, default);
        }
        *cache = Some(ids.clone());
        ids
    }

    /// Whether the client advertised `session.configOptions.boolean`, which ACP requires
    /// before an agent may offer `type: "boolean"` options.
    fn boolean_options_supported(&self) -> bool {
        self.client_caps
            .lock()
            .unwrap()
            .session
            .as_ref()
            .and_then(|s| s.config_options.as_ref())
            .is_some_and(|c| c.boolean.is_some())
    }

    async fn config_options(&self, model: &str, auto_approve: bool) -> Vec<SessionConfigOption> {
        let options = self
            .models()
            .await
            .into_iter()
            .map(|id| SessionConfigSelectOption::new(id.clone(), id))
            .collect();
        let mut config = vec![
            SessionConfigOption::select(
                MODEL_CONFIG_ID,
                "Model",
                model.to_string(),
                SessionConfigSelectOptions::Ungrouped(options),
            )
            .category(SessionConfigOptionCategory::Model),
        ];
        if self.boolean_options_supported() {
            config.push(
                SessionConfigOption::boolean(
                    AUTO_APPROVE_CONFIG_ID,
                    "Auto-approve actions",
                    auto_approve,
                )
                .description("Run tools that change things without asking for permission"),
            );
        }
        config
    }

    /// The model and auto-approve setting of a live session.
    fn session_settings(&self, id: &SessionId) -> Option<(String, bool)> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| (s.model.clone(), s.auto_approve.load(Ordering::Relaxed)))
    }

    async fn session_config_options(
        &self,
        id: &SessionId,
    ) -> agent_client_protocol::Result<Vec<SessionConfigOption>> {
        let (model, auto_approve) = self
            .session_settings(id)
            .ok_or_else(|| unknown_session(id))?;
        Ok(self.config_options(&model, auto_approve).await)
    }

    pub async fn set_config_option(
        &self,
        req: SetSessionConfigOptionRequest,
        connection: &ConnectionTo<Client>,
    ) -> agent_client_protocol::Result<SetSessionConfigOptionResponse> {
        let id = req.session_id.clone();
        self.restore(&id).await?;
        match &*req.config_id.0 {
            MODEL_CONFIG_ID => {
                let Some(model) = req.value.as_value_id().map(|v| v.0.to_string()) else {
                    return Err(invalid("model must be a value id"));
                };
                if !self.models().await.contains(&model) {
                    return Err(invalid(format!("unknown model {model}")));
                }
                match self.sessions.lock().unwrap().get_mut(&id) {
                    Some(s) => s.model = model.clone(),
                    None => return Err(unknown_session(&id)),
                }
                self.persist(&id).await;
                tracing::info!("session {id} now uses model {model}");
            }
            AUTO_APPROVE_CONFIG_ID if self.boolean_options_supported() => {
                let Some(on) = req.value.as_bool() else {
                    return Err(invalid("auto_approve must be a boolean"));
                };
                match self.sessions.lock().unwrap().get(&id) {
                    Some(s) => s.auto_approve.store(on, Ordering::Relaxed),
                    None => return Err(unknown_session(&id)),
                }
                tracing::info!("session {id} auto-approve {on}");
            }
            other => return Err(invalid(format!("unknown config option {other}"))),
        }
        let options = self.session_config_options(&id).await?;
        // Tell every view of the session about the change, not just the requester.
        connection.send_notification(SessionNotification::new(
            id,
            SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(options.clone())),
        ))?;
        Ok(SetSessionConfigOptionResponse::new(options))
    }

    /// Save a session that is not running a turn (a turn checks the history out; the
    /// end-of-turn save covers it).
    async fn persist(&self, id: &SessionId) {
        let stored = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .filter(|s| !s.busy)
            .map(Session::stored);
        let Some(stored) = stored else { return };
        let store = self.store.clone();
        let sid = id.0.to_string();
        match tokio::task::spawn_blocking(move || store.save(&sid, &stored)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("saving session {id} failed: {e:#}"),
            Err(e) => tracing::warn!("saving session {id} failed: {e}"),
        }
    }

    /// Bring a stored session into memory without rebinding it (a prompt or config change
    /// for a thread the client reopened without `session/load`). Errors if it is unknown.
    async fn restore(&self, id: &SessionId) -> agent_client_protocol::Result<()> {
        if self.sessions.lock().unwrap().contains_key(id) {
            return Ok(());
        }
        let stored = self
            .load_stored(id)
            .await?
            .ok_or_else(|| unknown_session(id))?;
        tracing::info!("restored session {id} from disk");
        self.sessions
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_insert_with(|| Session::restored(stored, self.yolo, self.no_mcp()));
        Ok(())
    }

    async fn load_stored(
        &self,
        id: &SessionId,
    ) -> agent_client_protocol::Result<Option<StoredSession>> {
        let store = self.store.clone();
        let sid = id.0.to_string();
        tokio::task::spawn_blocking(move || store.load(&sid))
            .await
            .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?
            .map_err(|e| agent_client_protocol::Error::internal_error().data(format!("{e:#}")))
    }

    /// A session placeholder until its MCP servers are connected.
    fn no_mcp(&self) -> Arc<McpToolset> {
        Arc::new(McpToolset::empty(self.mcp_config()))
    }

    fn session_mcp(&self, id: &SessionId) -> Arc<McpToolset> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.mcp.clone())
            .unwrap_or_else(|| self.no_mcp())
    }

    /// `AvailableCommandsUpdate` for a session, sent after the response that created it.
    pub fn send_commands(
        &self,
        connection: &ConnectionTo<Client>,
        id: &SessionId,
    ) -> agent_client_protocol::Result<()> {
        let mcp = self.session_mcp(id);
        let commands = self.profile.slash_commands(&SessionCtx { mcp: &mcp });
        connection.send_notification(SessionNotification::new(
            id.clone(),
            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(commands)),
        ))
    }

    pub async fn new_session(
        &self,
        req: NewSessionRequest,
    ) -> agent_client_protocol::Result<NewSessionResponse> {
        check_absolute(&req.cwd, &req.additional_directories)?;
        // Lexical normalization only: the client sees `cwd` again in `session/list`, so
        // symlinks in it are not rewritten.
        let cwd = normalize(&req.cwd);
        let roots: Vec<PathBuf> = req
            .additional_directories
            .iter()
            .map(|r| normalize(r))
            .collect();
        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let model = self.model();
        let mcp = self.connect_mcp(&req.mcp_servers).await;
        let session = Session::fresh(cwd, roots, model.clone(), self.yolo, mcp);
        self.sessions.lock().unwrap().insert(id.clone(), session);
        // Saved right away so the thread survives a restart before its first prompt.
        self.persist(&id).await;
        let options = self.config_options(&model, self.yolo).await;
        Ok(NewSessionResponse::new(id).config_options(options))
    }

    /// Live and stored sessions, optionally filtered by working directory, newest first.
    pub fn list_sessions(
        &self,
        req: ListSessionsRequest,
    ) -> agent_client_protocol::Result<ListSessionsResponse> {
        // Every session fits on one page, so no cursor is ever handed out; any cursor is stale.
        if let Some(cursor) = &req.cursor {
            return Err(invalid(format!("invalid cursor {cursor}")));
        }
        struct Row {
            id: SessionId,
            cwd: PathBuf,
            roots: Vec<PathBuf>,
            title: Option<String>,
            updated_at: u64,
        }
        let mut rows: HashMap<String, Row> = self
            .store
            .list()
            .into_iter()
            .map(|(id, s)| {
                (
                    id.clone(),
                    Row {
                        id: SessionId::new(id),
                        cwd: s.cwd,
                        roots: s.roots,
                        title: s.title,
                        updated_at: s.updated_at,
                    },
                )
            })
            .collect();
        // Live state wins over the (possibly older) stored copy.
        for (id, s) in self.sessions.lock().unwrap().iter() {
            rows.insert(
                id.0.to_string(),
                Row {
                    id: id.clone(),
                    cwd: s.cwd.clone(),
                    roots: s.roots.clone(),
                    title: s.title.clone(),
                    updated_at: s.updated_at,
                },
            );
        }
        // Stored cwds are normalized, so normalize the filter the same way.
        let wanted = req.cwd.as_deref().map(normalize);
        let mut rows: Vec<Row> = rows
            .into_values()
            .filter(|r| wanted.as_ref().is_none_or(|cwd| *cwd == normalize(&r.cwd)))
            .collect();
        rows.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.0.cmp(&b.id.0))
        });
        rows.truncate(MAX_LISTED);
        Ok(ListSessionsResponse::new(
            rows.into_iter()
                .map(|r| {
                    SessionInfo::new(r.id, r.cwd)
                        .additional_directories(r.roots)
                        .title(r.title)
                        .updated_at(iso8601(r.updated_at))
                })
                .collect(),
        ))
    }

    pub fn cancel(&self, session_id: &SessionId) {
        if let Some(s) = self.sessions.lock().unwrap().get(session_id) {
            s.cancel.cancel();
        }
    }

    /// Make `id` live for `session/load` and `session/resume`: from memory, else from the
    /// store; rebind cwd and roots, and reconnect MCP servers from the request. A well-formed
    /// id with no saved history (a thread from a process that never saved it) reopens empty
    /// under the same id, so the editor's thread opens instead of failing to launch. Returns
    /// whether the history was lost.
    async fn reopen(
        &self,
        id: &SessionId,
        cwd: &Path,
        roots: &[PathBuf],
        mcp_servers: &[McpServer],
    ) -> agent_client_protocol::Result<bool> {
        check_absolute(cwd, roots)?;
        if crate::store::sanitize_id(&id.0).is_err() {
            return Err(unknown_session(id));
        }
        let live = self.sessions.lock().unwrap().contains_key(id);
        let stored = if live {
            None
        } else {
            self.load_stored(id).await?
        };
        let history_lost = !live && stored.is_none();
        let cwd = normalize(cwd);
        let roots: Vec<PathBuf> = roots.iter().map(|r| normalize(r)).collect();
        let mcp = self.connect_mcp(mcp_servers).await;

        let old_mcp = {
            let mut sessions = self.sessions.lock().unwrap();
            if !sessions.contains_key(id) {
                let session = match stored {
                    Some(stored) => Session::restored(stored, self.yolo, self.no_mcp()),
                    None => {
                        tracing::warn!("session {id} has no saved history; reopening it empty");
                        Session::fresh(
                            cwd.clone(),
                            roots.clone(),
                            self.model(),
                            self.yolo,
                            self.no_mcp(),
                        )
                    }
                };
                sessions.insert(id.clone(), session);
            }
            let s = sessions.get_mut(id).ok_or_else(|| unknown_session(id))?;
            s.cwd = cwd;
            if !roots.is_empty() {
                s.roots = roots;
            }
            s.updated_at = now_secs();
            std::mem::replace(&mut s.mcp, mcp)
        };
        if let Some(old) = Arc::into_inner(old_mcp) {
            tokio::spawn(old.shutdown());
        }
        self.persist(id).await;
        Ok(history_lost)
    }

    /// Tell the user a reopened thread starts without its earlier history. Shown in the
    /// thread only; the model doesn't see it.
    fn notify_history_lost(
        &self,
        connection: &ConnectionTo<Client>,
        id: &SessionId,
    ) -> agent_client_protocol::Result<()> {
        connection.send_notification(SessionNotification::new(
            id.clone(),
            SessionUpdate::AgentMessageChunk(
                ContentChunk::new(
                    format!(
                        "{} no longer has this conversation's history, so it starts fresh \
                         from here.",
                        self.profile.info().display_name
                    )
                    .into(),
                )
                .message_id(new_message_id()),
            ),
        ))
    }

    /// Replay a session's conversation as `session/update` notifications (text, tool calls
    /// with their status and output), then respond with config options. `session/load`.
    pub async fn load_session(
        &self,
        req: LoadSessionRequest,
        connection: &ConnectionTo<Client>,
    ) -> agent_client_protocol::Result<LoadSessionResponse> {
        let id = req.session_id.clone();
        let history_lost = self
            .reopen(&id, &req.cwd, &req.additional_directories, &req.mcp_servers)
            .await?;
        let (messages, cwd, roots) = {
            let sessions = self.sessions.lock().unwrap();
            let s = sessions.get(&id).ok_or_else(|| unknown_session(&id))?;
            (s.messages.clone(), s.cwd.clone(), s.roots.clone())
        };
        let notify = |update: SessionUpdate| {
            connection.send_notification(SessionNotification::new(id.clone(), update))
        };
        // Tool output by call id, from the stored `tool` messages.
        let results: HashMap<&str, (&str, bool)> = messages
            .iter()
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
            .filter_map(|m| {
                Some((
                    m.get("tool_call_id")?.as_str()?,
                    (
                        m.get("content").and_then(Value::as_str).unwrap_or(""),
                        m.get("x_failed").and_then(Value::as_bool).unwrap_or(false),
                    ),
                ))
            })
            .collect();
        for msg in &messages {
            match msg.get("role").and_then(Value::as_str).unwrap_or("") {
                "user" => {
                    let text = user_text(msg);
                    if !text.is_empty() {
                        notify(SessionUpdate::UserMessageChunk(
                            ContentChunk::new(text.into()).message_id(new_message_id()),
                        ))?;
                    }
                }
                "assistant" => {
                    let text = msg.get("content").and_then(Value::as_str).unwrap_or("");
                    if !text.is_empty() {
                        notify(SessionUpdate::AgentMessageChunk(
                            ContentChunk::new(text.to_string().into()).message_id(new_message_id()),
                        ))?;
                    }
                    let calls = msg.get("tool_calls").and_then(Value::as_array);
                    for tc in calls.into_iter().flatten() {
                        let call_id = tc.get("id").and_then(Value::as_str).unwrap_or("unknown");
                        let name = tc
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        let args: Value = tc
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .and_then(|a| serde_json::from_str(a).ok())
                            .unwrap_or_else(|| json!({}));
                        let (title, kind, locations) =
                            tools::describe(&self.toolsets, &cwd, &roots, name, &args);
                        let mut call = ToolCall::new(call_id.to_string(), title)
                            .name(name.to_string())
                            .kind(kind)
                            .locations(locations)
                            .raw_input(args);
                        match results.get(call_id) {
                            Some((output, failed)) => {
                                call = call
                                    .status(if *failed {
                                        ToolCallStatus::Failed
                                    } else {
                                        ToolCallStatus::Completed
                                    })
                                    .content(vec![ToolCallContent::from(output.to_string())])
                                    .raw_output(json!({ "output": output, "failed": failed }));
                            }
                            // The turn was interrupted before this call produced a result.
                            None => call = call.status(ToolCallStatus::Failed),
                        }
                        notify(SessionUpdate::ToolCall(call))?;
                    }
                }
                // system and tool messages are not replayed on their own.
                _ => {}
            }
        }
        if history_lost {
            self.notify_history_lost(connection, &id)?;
        }
        Ok(LoadSessionResponse::new().config_options(self.session_config_options(&id).await?))
    }

    /// Rebind a session to a new cwd / additional directories without replaying it.
    pub async fn resume_session(
        &self,
        req: ResumeSessionRequest,
        connection: &ConnectionTo<Client>,
    ) -> agent_client_protocol::Result<ResumeSessionResponse> {
        let id = req.session_id.clone();
        let history_lost = self
            .reopen(&id, &req.cwd, &req.additional_directories, &req.mcp_servers)
            .await?;
        if history_lost {
            self.notify_history_lost(connection, &id)?;
        }
        Ok(ResumeSessionResponse::new().config_options(self.session_config_options(&id).await?))
    }

    /// Cancel any in-progress work, release MCP servers and scratch audio. The stored
    /// conversation stays; `session/delete` removes it.
    pub fn close_session(
        &self,
        req: CloseSessionRequest,
    ) -> agent_client_protocol::Result<CloseSessionResponse> {
        let id = req.session_id.clone();
        let removed = self.sessions.lock().unwrap().remove(&id);
        match removed {
            Some(s) => self.release(&id, s),
            // A session from an earlier process that was never reopened has nothing to close.
            None if self.store.load(&id.0).ok().flatten().is_some() => {}
            None => return Err(unknown_session(&id)),
        }
        Ok(CloseSessionResponse::new())
    }

    /// Forget a session, in memory and on disk, with its scratch audio. Idempotent: deleting
    /// an unknown session succeeds.
    pub fn delete_session(
        &self,
        req: DeleteSessionRequest,
    ) -> agent_client_protocol::Result<DeleteSessionResponse> {
        let id = req.session_id.clone();
        if let Some(s) = self.sessions.lock().unwrap().remove(&id) {
            self.release(&id, s);
        }
        // A malformed id can't name a stored session; nothing to remove.
        if crate::store::sanitize_id(&id.0).is_ok() {
            self.store.delete(&id.0).map_err(|e| {
                agent_client_protocol::Error::internal_error().data(format!("{e:#}"))
            })?;
        }
        self.remove_scratch(&id);
        Ok(DeleteSessionResponse::new())
    }

    fn release(&self, id: &SessionId, s: Session) {
        s.cancel.cancel();
        self.remove_scratch(id);
        if let Some(mcp) = Arc::into_inner(s.mcp) {
            tokio::spawn(mcp.shutdown());
        }
    }

    /// Stop everything: used when the connection ends.
    pub async fn shutdown(&self) {
        let sessions: Vec<Session> = self
            .sessions
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for s in sessions {
            s.cancel.cancel();
            if let Some(mcp) = Arc::into_inner(s.mcp) {
                mcp.shutdown().await;
            }
        }
        for server in self.profile.builtin_mcp_servers() {
            server.shutdown().await;
        }
    }

    pub async fn prompt(
        &self,
        request: PromptRequest,
        responder: Responder<PromptResponse>,
        connection: ConnectionTo<Client>,
    ) -> agent_client_protocol::Result<()> {
        // A client may prompt a reopened thread without loading or resuming it first.
        if let Err(e) = self.restore(&request.session_id).await {
            return responder.respond_with_error(e);
        }
        match self.run_turn(request, connection).await {
            Ok(stop) => responder.respond(PromptResponse::new(stop)),
            Err(e) if e.downcast_ref::<UnknownSession>().is_some() => {
                responder.respond_with_error(invalid(e.to_string()))
            }
            Err(e) => responder.respond_with_error(
                agent_client_protocol::Error::internal_error().data(format!("{e:#}")),
            ),
        }
    }

    async fn run_turn(
        &self,
        request: PromptRequest,
        connection: ConnectionTo<Client>,
    ) -> anyhow::Result<StopReason> {
        let session_id = request.session_id.clone();
        let caps = self.client_caps.lock().unwrap().clone();
        let blocks =
            prompt::resolve_resource_links(&request.prompt, &caps, &connection, &session_id).await;
        let prompt_text = prompt::prompt_to_text(&blocks, &self.profile.audio_hints());
        // Check the session out for the turn.
        let (
            cwd,
            roots,
            model,
            history,
            cancel,
            always_allowed,
            always_rejected,
            auto_approve,
            mcp,
            new_title,
        ) = {
            let mut sessions = self.sessions.lock().unwrap();
            let s = sessions
                .get_mut(&session_id)
                .ok_or_else(|| UnknownSession(session_id.to_string()))?;
            if s.busy {
                anyhow::bail!("a prompt is already running for session {session_id}");
            }
            s.busy = true;
            s.cancel = CancellationToken::new();
            s.updated_at = now_secs();
            let new_title = if s.title.is_none() && !prompt_text.trim().is_empty() {
                s.title = Some(session_title(&prompt_text));
                s.title.clone()
            } else {
                None
            };
            (
                s.cwd.clone(),
                s.roots.clone(),
                s.model.clone(),
                std::mem::take(&mut s.messages),
                s.cancel.clone(),
                s.always_allowed.clone(),
                s.always_rejected.clone(),
                s.auto_approve.clone(),
                s.mcp.clone(),
                new_title,
            )
        };
        if let Some(title) = new_title {
            let update = SessionInfoUpdate::new()
                .title(title)
                .updated_at(iso8601(now_secs()));
            let _ = connection.send_notification(SessionNotification::new(
                session_id.clone(),
                SessionUpdate::SessionInfoUpdate(update),
            ));
        }

        let mut messages = vec![json!({
            "role": "system",
            "content": self.system_prompt(&cwd, &roots, &mcp),
        })];
        messages.extend(history);

        let ctx = ToolCtx {
            connection: connection.clone(),
            session_id: session_id.clone(),
            cwd,
            roots,
            caps,
            cancel,
            auto_approve,
            always_allowed,
            always_rejected,
            mcp,
        };
        let result = self.turn(&ctx, &model, &mut messages, &blocks).await;

        // Check the history back in, even when the turn failed.
        let history = messages.split_off(1);
        let stored = {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.get_mut(&session_id).map(|s| {
                s.messages = history;
                s.busy = false;
                s.updated_at = now_secs();
                s.stored()
            })
        };
        if let Some(stored) = stored {
            let updated_at = stored.updated_at;
            let store = self.store.clone();
            let id = session_id.0.to_string();
            let saved = tokio::task::spawn_blocking(move || store.save(&id, &stored)).await;
            if let Ok(Err(e)) | Err(e) = saved.map_err(anyhow::Error::from) {
                tracing::warn!("saving session {session_id} failed: {e:#}");
            }
            let _ = connection.send_notification(SessionNotification::new(
                session_id.clone(),
                SessionUpdate::SessionInfoUpdate(
                    SessionInfoUpdate::new().updated_at(iso8601(updated_at)),
                ),
            ));
        }
        result
    }

    /// Everything inside a turn after the session is checked out.
    async fn turn(
        &self,
        ctx: &ToolCtx,
        model: &str,
        messages: &mut Vec<Value>,
        blocks: &[ContentBlock],
    ) -> anyhow::Result<StopReason> {
        let audio_paths = match self.scratch_dir(&ctx.session_id) {
            Some(dir) => prompt::save_audio_blocks(blocks, &dir).await?,
            None => Vec::new(),
        };
        let hints = self.profile.audio_hints();
        let slash = self.profile.expand_slash(
            &prompt::prompt_to_text(blocks, &hints),
            &SessionCtx { mcp: &ctx.mcp },
        );
        let content = prompt::prompt_to_content(blocks, &audio_paths, slash.as_deref(), &hints);
        messages.push(json!({ "role": "user", "content": content }));
        self.agent_loop(ctx, model, messages).await
    }

    async fn agent_loop(
        &self,
        ctx: &ToolCtx,
        model: &str,
        messages: &mut Vec<Value>,
    ) -> anyhow::Result<StopReason> {
        let llm = self.llm();
        let tool_defs = tools::definitions(&self.toolsets, &ctx.mcp);
        let notify = |update: SessionUpdate| {
            ctx.connection
                .send_notification(SessionNotification::new(ctx.session_id.clone(), update))
        };
        // `size` is the model's context window, which OpenAI-style endpoints don't report.
        let context_window = std::env::var(self.env.var("CONTEXT_WINDOW"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);

        for _ in 0..MAX_TURNS {
            // One message id per assistant message (and per thought), so clients can group
            // the streamed chunks.
            let message_id = new_message_id();
            let thought_id = new_message_id();
            let wire: Vec<Value> = messages.iter().map(wire_message).collect();
            let completion = tokio::select! {
                r = llm.complete(model, &wire, &tool_defs, |delta| {
                    let update = match delta {
                        Delta::Text(t) => SessionUpdate::AgentMessageChunk(
                            ContentChunk::new(t.to_string().into()).message_id(message_id.clone()),
                        ),
                        Delta::Reasoning(t) => SessionUpdate::AgentThoughtChunk(
                            ContentChunk::new(t.to_string().into()).message_id(thought_id.clone()),
                        ),
                    };
                    if let Err(e) = notify(update) {
                        tracing::warn!("failed to send update: {e}");
                    }
                }) => r?,
                () = ctx.cancel.cancelled() => return Ok(StopReason::Cancelled),
            };
            messages.push(completion.to_message());
            if let Some(usage) = completion.usage {
                let used = usage.used();
                notify(SessionUpdate::UsageUpdate(UsageUpdate::new(
                    used,
                    context_window.max(used),
                )))?;
            }

            if completion.tool_calls.is_empty() {
                return Ok(match completion.finish_reason.as_deref() {
                    Some("length") => StopReason::MaxTokens,
                    Some("content_filter") => StopReason::Refusal,
                    _ => StopReason::EndTurn,
                });
            }

            for tc in &completion.tool_calls {
                // Every tool call needs a matching tool message, even after cancellation.
                if ctx.cancel.is_cancelled() {
                    messages.push(tool_message(&tc.id, "Cancelled by user.", true));
                    continue;
                }
                let parsed: Result<Value, _> = serde_json::from_str(&tc.arguments);
                let args = parsed.as_ref().cloned().unwrap_or_else(|_| json!({}));
                let (title, kind, locations) =
                    tools::describe(&self.toolsets, &ctx.cwd, &ctx.roots, &tc.name, &args);
                notify(SessionUpdate::ToolCall(
                    ToolCall::new(tc.id.clone(), title)
                        .name(tc.name.clone())
                        .kind(kind)
                        .status(ToolCallStatus::InProgress)
                        .locations(locations)
                        .raw_input(args.clone()),
                ))?;

                let outcome: ToolOutcome = if parsed.is_err() {
                    ToolOutcome::err(format!("Invalid JSON arguments: {}", tc.arguments))
                } else {
                    tools::execute(
                        ctx,
                        &self.toolsets,
                        self.profile.as_ref(),
                        &tc.id,
                        &tc.name,
                        args,
                    )
                    .await
                };

                let mut fields = ToolCallUpdateFields::new()
                    .status(if outcome.failed {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    })
                    .content(outcome.content)
                    .raw_output(json!({ "output": outcome.text, "failed": outcome.failed }));
                if !outcome.locations.is_empty() {
                    fields = fields.locations(locations_of(&outcome.locations));
                }
                ctx.update(&tc.id, fields)?;
                messages.push(tool_message(&tc.id, &outcome.text, outcome.failed));
            }
            if ctx.cancel.is_cancelled() {
                return Ok(StopReason::Cancelled);
            }
        }
        Ok(StopReason::MaxTurnRequests)
    }
}

/// Marker error: the prompt named a session that doesn't exist.
#[derive(Debug)]
struct UnknownSession(String);

impl std::fmt::Display for UnknownSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown session {}", self.0)
    }
}

impl std::error::Error for UnknownSession {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_known_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn titles_are_trimmed_and_single_line() {
        assert_eq!(session_title("  hello\nworld"), "hello");
        let long = "x".repeat(200);
        let t = session_title(&long);
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn terminal_auth_support_from_capability_or_legacy_meta() {
        let caps = |v: Value| serde_json::from_value::<ClientCapabilities>(v).unwrap();
        assert!(!supports_terminal_auth(&caps(json!({}))));
        assert!(supports_terminal_auth(&caps(
            json!({"auth": {"terminal": true}})
        )));
        // The ACP registry validator's payload.
        assert!(supports_terminal_auth(&caps(
            json!({"terminal": true, "_meta": {"terminal_output": true, "terminal-auth": true}})
        )));
        assert!(!supports_terminal_auth(&caps(
            json!({"_meta": {"terminal-auth": false}})
        )));
    }

    #[test]
    fn version_negotiation_always_v1() {
        assert_eq!(negotiate_version(ProtocolVersion::V1), ProtocolVersion::V1);
        assert_eq!(
            negotiate_version(ProtocolVersion::from(7)),
            ProtocolVersion::V1
        );
        assert_eq!(
            negotiate_version(ProtocolVersion::from(0)),
            ProtocolVersion::V1
        );
    }

    #[test]
    fn private_failure_flag_never_reaches_the_wire() {
        let m = tool_message("c1", "boom", true);
        assert_eq!(m["x_failed"], true);
        let w = wire_message(&m);
        assert!(w.get("x_failed").is_none());
        assert_eq!(w["content"], "boom");
        assert!(tool_message("c1", "ok", false).get("x_failed").is_none());
    }

    #[test]
    fn user_text_handles_parts() {
        let m = json!({"role": "user", "content": [
            {"type": "text", "text": "hi"},
            {"type": "image_url", "image_url": {"url": "x"}}
        ]});
        assert_eq!(user_text(&m), "hi\n\n[image]");
        assert_eq!(user_text(&json!({"content": "plain"})), "plain");
    }
}
