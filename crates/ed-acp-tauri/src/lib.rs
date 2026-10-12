//! An [`ed_acp`] agent, hosted inside a Tauri app.
//!
//! A sandboxed App Store build can't launch an ACP agent as a subprocess the way an editor
//! does, so the app runs it in-process: [`AgentState`] serves the agent's [`Profile`] with
//! `ed_acp::serve` over an in-memory channel and talks to it as a regular ACP client. Nothing
//! about the protocol changes; only the transport does.
//!
//! The frontend sees ACP as Tauri commands and events. Session updates are forwarded
//! untouched, so the page renders the ACP schema directly. This crate holds everything that
//! doesn't depend on the app; each app passes a [`HostConfig`] and wraps [`AgentState`]'s
//! methods in its own `#[tauri::command]`s.
//!
//! The client advertises `fs.readTextFile`, `fs.writeTextFile` and `elicitation.url`. Reads
//! and writes are served inside the session's [`Roots`]. A URL elicitation (a product's own
//! sign-in page) is opened in the system browser through `tauri-plugin-opener`, which the app
//! has to register. The client advertises no `terminal`, so the agent runs commands itself,
//! after asking for permission.

mod client;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed_acp::agent_client_protocol::schema::v1::McpServer;
use ed_acp::{normalize, AgentInfo, Profile};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tsync::tsync;

pub use client::{clear_api_key, set_api_key, status, AgentState};

/// Returned by commands when the agent isn't signed in or has no API key. The page offers
/// sign-in when it sees this.
pub const AUTH_REQUIRED: &str = "auth_required";

/// Names of the Tauri events the host emits to the webview.
#[derive(Clone, Copy)]
pub struct Events {
    /// ACP `session/update` notifications, forwarded untouched.
    pub session_update: &'static str,
    /// `{ requestId, request }` for a permission prompt.
    pub permission_requested: &'static str,
    /// An [`AgentTurnEndedPayload`].
    pub turn_ended: &'static str,
}

/// The directories a session may use: its working directory and extra roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    pub cwd: PathBuf,
    pub extra: Vec<PathBuf>,
}

impl Roots {
    /// The app's own scratch directory (`<app data>/agent`), created if missing, plus `extra`.
    /// What a host that has no project folder of its own wants as `cwd`.
    pub fn app_scratch(app: &AppHandle, extra: Vec<PathBuf>) -> Result<Self, String> {
        let cwd = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("No app data directory: {e}"))?
            .join("agent");
        std::fs::create_dir_all(&cwd).map_err(|e| format!("{}: {e}", cwd.display()))?;
        Ok(Self { cwd, extra })
    }

    /// A project folder as the working directory. It must be absolute and an existing
    /// directory: it becomes the boundary the agent's file tools stay inside.
    pub fn project(cwd: &Path) -> Result<Self, String> {
        if !cwd.is_absolute() {
            return Err(format!("{} is not an absolute path.", cwd.display()));
        }
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory.", cwd.display()));
        }
        Ok(Self {
            cwd: cwd.to_path_buf(),
            extra: Vec::new(),
        })
    }

    /// Whether `path` is absolute and inside one of the roots, after removing `.` and `..`,
    /// so the agent can't walk out of them.
    pub fn contains(&self, path: &Path) -> bool {
        if !path.is_absolute() {
            return false;
        }
        let path = normalize(path);
        std::iter::once(&self.cwd)
            .chain(&self.extra)
            .any(|root| path.starts_with(normalize(root)))
    }
}

/// Everything about the app that the host can't know.
#[derive(Clone)]
pub struct HostConfig {
    /// Reported to the agent as `clientInfo.name`.
    pub client_name: &'static str,
    /// `clientInfo.version`.
    pub client_version: &'static str,
    /// `<PREFIX>_SURFACE` for the agent's prompt: `app`, neither an editor nor its TUI.
    pub surface: &'static str,
    /// The agent's profile, without what the platform can't run.
    pub profile: fn() -> Arc<dyn Profile>,
    /// The roots for a session. The argument is the folder the frontend asked for, if any;
    /// `Roots::app_scratch` is the answer for an app without project folders.
    pub roots: fn(&AppHandle, Option<&Path>) -> Result<Roots, String>,
    /// The MCP servers for a new session, given the signed-in user's access token when the app
    /// has one its agent should present. A profile's own built-in servers need none.
    pub mcp_servers: fn(&AppHandle, Option<String>) -> Vec<McpServer>,
    pub events: Events,
}

impl HostConfig {
    /// The profile's identity: where its env file lives and which variables it reads.
    pub fn info(&self) -> AgentInfo {
        (self.profile)().info()
    }
}

/// Response returned by `agent_status`.
#[tsync]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    /// `false` on platforms the agent isn't built for.
    pub available: bool,
    /// Whether the agent has credentials: a signed-in session or an API key.
    pub has_key: bool,
    /// The model the agent will use.
    pub model: String,
}

/// One earlier conversation, as `agent_list_sessions` returns it.
#[tsync]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionSummary {
    pub session_id: String,
    /// Set by the agent once the conversation has a first exchange.
    pub title: Option<String>,
    /// ISO 8601, when the conversation last changed.
    pub updated_at: Option<String>,
}

/// Response returned by `agent_open_session`.
#[tsync]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionOpened {
    pub session_id: String,
    /// `true` when an earlier conversation was loaded. Its history arrives as
    /// `agent_session_update` events before this response.
    pub restored: bool,
    /// The session's ACP `SessionConfigOption`s (model picker, auto-approve), as the agent
    /// sent them. `Value` is any JSON.
    pub config_options: Vec<serde_json::Value>,
}

/// Payload emitted with `agent_turn_ended`.
#[tsync]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurnEndedPayload {
    pub session_id: String,
    /// ACP stop reason (`end_turn`, `cancelled`, `max_tokens`, …), or `None`
    /// on error.
    pub stop_reason: Option<String>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_keep_the_agent_inside() {
        let roots = Roots {
            cwd: PathBuf::from("/data/agent"),
            extra: vec![PathBuf::from("/data/audio")],
        };
        assert!(roots.contains(Path::new("/data/agent/notes.md")));
        assert!(roots.contains(Path::new("/data/audio/song/bass.wav")));
        assert!(!roots.contains(Path::new("/data/agent/../secrets")));
        assert!(!roots.contains(Path::new("/data/agentx/file")));
        assert!(!roots.contains(Path::new("relative/file")));
    }

    #[test]
    fn a_project_root_must_be_an_absolute_existing_directory() {
        let dir = std::env::temp_dir();
        assert_eq!(Roots::project(&dir).unwrap().cwd, dir);
        assert!(Roots::project(Path::new("relative")).is_err());
        assert!(Roots::project(&dir.join("no-such-folder-ed-acp-tauri")).is_err());
    }
}
