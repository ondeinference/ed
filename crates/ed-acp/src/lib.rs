//! An Agent Client Protocol (ACP) v1 server for Onde agents.
//!
//! `ed-acp` is the part of an ACP agent that is the same for every product: `initialize` and
//! version negotiation, terminal auth, `authenticate` and `logout`, durable sessions (`list`,
//! `load` with full replay, `resume`, `close`, `delete`), the model picker and auto-approve
//! config options, slash-command advertisement, prompt content (embedded and linked files,
//! images, audio), the turn loop against Onde Cloud (or any OpenAI API compatible endpoint)
//! with streaming, message ids and usage, tool approval, the workspace tools routed through the
//! client's `fs/*` and `terminal/*`, and MCP servers through [`ed_mcp`].
//!
//! A product supplies a [`Profile`]: its name, prompt, tools, slash commands and built-in MCP
//! servers. Everything in the Onde Agent Platform §5.5 conformance table is implemented here
//! once.
//!
//! ```ignore
//! let profile = Arc::new(MyProfile);
//! ed_acp::serve_stdio(profile, ServeOptions::from_env(&info)).await?;
//! ```

mod agent;
pub mod cli;
pub mod content;
pub mod llm;
mod server;
pub mod store;
pub mod tools;
mod workspace;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_client_protocol::schema::v1::{AvailableCommand, PromptCapabilities};
use async_trait::async_trait;
use ed_mcp::{Connection, McpTool, McpToolset, SharedServer};
use serde_json::Value;

pub use agent_client_protocol;
pub use content::AudioHints;
pub use ed_mcp;
pub use llm::{LlmConfig, LlmEnv, Provider};
pub use server::{ServeOptions, serve_stdio};
pub use tools::{
    DescribeCtx, ToolCtx, ToolOutcome, Toolset, absolutize, function_def, normalize, resolve_in,
    truncate,
};
pub use workspace::WorkspaceTools;

/// Who the agent is.
#[derive(Debug, Clone, Copy)]
pub struct AgentInfo {
    /// Binary and protocol name, e.g. `splitfire-agent`. Reported as `agentInfo.name` and in
    /// `clientInfo` to MCP servers.
    pub name: &'static str,
    /// Name shown to people, e.g. `SplitFire`.
    pub display_name: &'static str,
    pub version: &'static str,
    /// Where settings and credentials come from.
    pub env: LlmEnv,
}

/// What the system prompt is built from, every turn.
pub struct PromptCtx<'a> {
    /// `acp` or `tui`; see [`ServeOptions::surface`].
    pub surface: &'a str,
    pub cwd: &'a Path,
    pub roots: &'a [PathBuf],
    /// The session's MCP tools, so the prompt can mention optional capabilities only when
    /// they are connected.
    pub mcp: &'a McpToolset,
}

/// What slash commands depend on.
pub struct SessionCtx<'a> {
    pub mcp: &'a McpToolset,
}

/// What a slash command answered without the model can report.
pub struct CommandCtx<'a> {
    pub mcp: &'a McpToolset,
    /// The session's model.
    pub model: &'a str,
    /// The models offered in the model picker.
    pub models: &'a [String],
}

/// An MCP call awaiting approval.
pub struct McpCall<'a> {
    pub tool: &'a McpTool,
    pub args: &'a Value,
    /// The server, for products that consult it (e.g. to report a download size).
    pub connection: Option<&'a Connection>,
}

/// The product-specific part of an ACP agent.
#[async_trait]
pub trait Profile: Send + Sync + 'static {
    fn info(&self) -> AgentInfo;

    /// The system prompt for one turn.
    fn system_prompt(&self, ctx: &PromptCtx<'_>) -> String;

    /// The tools the model may call, besides MCP tools. Include [`WorkspaceTools`] to offer
    /// file and shell access.
    fn toolsets(&self) -> Vec<Arc<dyn Toolset>>;

    /// Commands advertised with `available_commands_update` after a session is created or
    /// reopened.
    fn slash_commands(&self, _ctx: &SessionCtx<'_>) -> Vec<AvailableCommand> {
        Vec::new()
    }

    /// When `text` starts with one of this profile's commands, the instruction the model gets
    /// in its place.
    fn expand_slash(&self, _text: &str, _ctx: &SessionCtx<'_>) -> Option<String> {
        None
    }

    /// When `text` is one of this profile's commands that needs no model (`/models`, `/setup`),
    /// the reply shown in the thread. It is not added to the conversation.
    fn answer_slash(&self, _text: &str, _ctx: &CommandCtx<'_>) -> Option<String> {
        None
    }

    /// Process-wide MCP servers offered to every session unless the client supplies one of
    /// the same name.
    fn builtin_mcp_servers(&self) -> Vec<&'static SharedServer> {
        Vec::new()
    }

    /// Advertised in `initialize`. Audio is accepted only when this says so.
    fn prompt_capabilities(&self) -> PromptCapabilities {
        PromptCapabilities::new().embedded_context(true).image(true)
    }

    /// How audio links and attachments are described to the model.
    fn audio_hints(&self) -> AudioHints {
        AudioHints::default()
    }

    /// The permission prompt for a mutating MCP tool. `None` uses the default: the tool, its
    /// server and its arguments.
    async fn mcp_permission_preview(&self, _call: McpCall<'_>) -> Option<String> {
        None
    }

    /// Files an MCP result produced, from its `structuredContent`, shown as tool-call
    /// locations. Relative paths are dropped.
    fn mcp_result_locations(&self, _tool: &McpTool, _structured: &Value) -> Vec<PathBuf> {
        Vec::new()
    }
}
