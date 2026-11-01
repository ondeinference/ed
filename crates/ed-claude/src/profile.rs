//! Claude's [`Profile`]: who the agent is, its prompt, and the workspace tools.

use std::sync::Arc;

use async_trait::async_trait;
use ed_acp::{
    AgentInfo, AuthProvider, DescribeCtx, LlmEnv, Profile, PromptCtx, ToolCtx, ToolOutcome,
    Toolset, WorkspaceTools,
};
use serde_json::Value;

use crate::auth::{ClaudeAuth, DEFAULT_MODEL};

pub const INFO: AgentInfo = AgentInfo {
    name: "claude",
    display_name: "Claude",
    version: env!("CARGO_PKG_VERSION"),
    env: LlmEnv {
        prefix: "ED_CLAUDE",
        dir_name: "edclaude",
        default_model: DEFAULT_MODEL,
    },
};

const NO_SHELL_NOTE: &str = "\n\nThis host cannot run commands: there is no run_command tool, \
    and no git or shell. When something needs a command, say which one and let the user run it.";

/// Claude as an `ed-acp` agent that uses the user's Anthropic API key.
#[derive(Debug, Clone)]
pub struct Claude {
    shell: bool,
    auth: ClaudeAuth,
}

impl Claude {
    /// With the workspace tools, `run_command` included.
    pub fn new() -> Self {
        Self {
            shell: true,
            auth: ClaudeAuth::new(),
        }
    }

    /// For hosts that cannot start processes (an iOS app, a sandboxed Mac app): the workspace
    /// tools without `run_command`, and a prompt that says so.
    pub fn without_shell() -> Self {
        Self {
            shell: false,
            auth: ClaudeAuth::new(),
        }
    }

    /// Use `auth` instead of the default, e.g. one pointed at another endpoint.
    pub fn with_auth(mut self, auth: ClaudeAuth) -> Self {
        self.auth = auth;
        self
    }
}

impl Default for Claude {
    fn default() -> Self {
        Self::new()
    }
}

/// `WorkspaceTools` minus `run_command`.
struct FileTools;

#[async_trait]
impl Toolset for FileTools {
    fn definitions(&self) -> Vec<Value> {
        WorkspaceTools
            .definitions()
            .into_iter()
            .filter(|d| d["function"]["name"] != "run_command")
            .collect()
    }

    fn handles(&self, name: &str) -> bool {
        name != "run_command" && WorkspaceTools.handles(name)
    }

    fn describe(
        &self,
        name: &str,
        args: &Value,
        ctx: &DescribeCtx<'_>,
    ) -> (
        String,
        agent_client_protocol::schema::v1::ToolKind,
        Vec<agent_client_protocol::schema::v1::ToolCallLocation>,
    ) {
        WorkspaceTools.describe(name, args, ctx)
    }

    async fn execute(
        &self,
        ctx: &ToolCtx,
        tool_call_id: &str,
        name: &str,
        args: Value,
    ) -> anyhow::Result<ToolOutcome> {
        if name == "run_command" {
            anyhow::bail!("This host cannot run commands.");
        }
        WorkspaceTools.execute(ctx, tool_call_id, name, args).await
    }
}

#[async_trait]
impl Profile for Claude {
    fn info(&self) -> AgentInfo {
        INFO
    }

    fn system_prompt(&self, ctx: &PromptCtx<'_>) -> String {
        let tools = if self.shell {
            "read_file, write_file, edit_file, list_directory and run_command"
        } else {
            "read_file, write_file, edit_file and list_directory"
        };
        let mut prompt = format!(
            "You are Claude, an autonomous coding agent working inside the user's editor.\n\
             Use your tools to inspect and change the project: read files before editing them, \
             prefer edit_file for small changes, and use write_file to create files. Your tools \
             are {tools}, plus those of any connected MCP servers. Keep answers concise and use \
             Markdown."
        );
        if !self.shell {
            prompt.push_str(NO_SHELL_NOTE);
        }
        prompt.push_str(&format!("\n\nWorking directory: {}", ctx.cwd.display()));
        if ctx.roots.len() > 1 {
            let roots: Vec<String> = ctx.roots.iter().map(|r| r.display().to_string()).collect();
            prompt.push_str(&format!("\nProject roots: {}", roots.join(", ")));
        }
        prompt
    }

    fn auth(&self) -> Option<Arc<dyn AuthProvider>> {
        Some(Arc::new(self.auth.clone()))
    }

    fn toolsets(&self) -> Vec<Arc<dyn Toolset>> {
        if self.shell {
            vec![Arc::new(WorkspaceTools)]
        } else {
            vec![Arc::new(FileTools)]
        }
    }
}
