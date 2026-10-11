//! An [`McpToolset`] as [Ed](ed_agent) tools. Behind the `agent` feature.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use ed_agent::{AgentToolDefinition, Ed, EventSink, ToolExecutor, ToolRisk};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::{McpTool, McpToolset};

/// How MCP tools are presented to Ed.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    read_only: HashSet<String>,
    unprefixed: bool,
}

impl ToolPolicy {
    /// Default policy: tool names are `mcp__<server>__<tool>`, and a tool is read-only only when
    /// its server sets `readOnlyHint: true`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Treat these tools as read-only (no approval) whatever their server says. Names are the
    /// server's own tool names, or the names the model sees. For servers that do not set
    /// `readOnlyHint`.
    pub fn read_only(mut self, names: &[&str]) -> Self {
        self.read_only.extend(names.iter().map(|n| (*n).to_owned()));
        self
    }

    /// Show the model the server's own tool names, without the `mcp__<server>__` prefix. For apps
    /// with one server whose tool names are already in the system prompt. If two servers use the
    /// same tool name the first one wins.
    pub fn unprefixed(mut self) -> Self {
        self.unprefixed = true;
        self
    }

    fn risk(&self, tool: &McpTool, exposed: &str) -> ToolRisk {
        if tool.read_only || self.read_only.contains(exposed) || self.read_only.contains(&tool.tool)
        {
            ToolRisk::ReadOnly
        } else {
            ToolRisk::Mutating
        }
    }
}

struct Loaded {
    toolset: Arc<McpToolset>,
    /// Name the model sees -> qualified name in the toolset.
    names: HashMap<String, String>,
    definitions: Vec<AgentToolDefinition>,
}

/// Runs the tools of an [`McpToolset`] for Ed.
///
/// Cheap to clone. Hand one clone to [`Ed::with_agent`] as the executor, and keep another to
/// connect and disconnect servers as the user signs in and out:
///
/// ```ignore
/// let mcp = McpExecutor::new(ToolPolicy::new().read_only(&["list_messages"]).unprefixed());
/// let ed = Ed::with_agent(sink, Arc::new(mcp.clone()), approvals, config);
/// mcp.set_toolset(Some(toolset));
/// mcp.sync(&ed).await;
/// ```
#[derive(Clone)]
pub struct McpExecutor {
    policy: Arc<ToolPolicy>,
    loaded: Arc<RwLock<Option<Loaded>>>,
    registered: Arc<Mutex<Vec<String>>>,
}

impl McpExecutor {
    pub fn new(policy: ToolPolicy) -> Self {
        Self {
            policy: Arc::new(policy),
            loaded: Arc::default(),
            registered: Arc::default(),
        }
    }

    /// Use `toolset`, or none, and shut the previous toolset down. Call
    /// [`sync`](McpExecutor::sync) afterwards. A tool call still running on the old toolset
    /// finishes first; its connections close when it does.
    pub async fn set_toolset(&self, toolset: Option<McpToolset>) {
        let new = toolset.map(|toolset| {
            let mut names = HashMap::new();
            let mut definitions = Vec::new();
            for tool in toolset.tools() {
                let exposed = if self.policy.unprefixed {
                    tool.tool.clone()
                } else {
                    tool.name.clone()
                };
                if names.contains_key(&exposed) {
                    tracing::warn!(
                        "duplicate tool name `{exposed}` after prefix removal; keeping the first"
                    );
                    continue;
                }
                let description = if self.policy.unprefixed {
                    tool.description
                        .strip_prefix(&format!("[{} MCP server] ", tool.server))
                        .unwrap_or(&tool.description)
                        .to_owned()
                } else {
                    tool.description.clone()
                };
                let description = if description.trim().is_empty() {
                    format!("The `{}` tool.", tool.tool)
                } else {
                    description
                };
                let schema = tool.parameters.to_string();
                definitions.push(match self.policy.risk(tool, &exposed) {
                    ToolRisk::ReadOnly => {
                        AgentToolDefinition::read_only(exposed.clone(), description, schema)
                    }
                    ToolRisk::Mutating => {
                        AgentToolDefinition::mutating(exposed.clone(), description, schema)
                    }
                });
                names.insert(exposed, tool.name.clone());
            }
            Loaded {
                toolset: Arc::new(toolset),
                names,
                definitions,
            }
        });
        let old = std::mem::replace(&mut *self.loaded.write().await, new);
        if let Some(old) = old
            && let Some(toolset) = Arc::into_inner(old.toolset)
        {
            toolset.shutdown().await;
        }
    }

    /// The tools Ed should offer, for the connected toolset (empty when none).
    pub async fn definitions(&self) -> Vec<AgentToolDefinition> {
        self.loaded
            .read()
            .await
            .as_ref()
            .map(|l| l.definitions.clone())
            .unwrap_or_default()
    }

    /// Make `ed`'s registered tools match the connected toolset: the ones this executor registered
    /// before are removed, the current ones added.
    pub async fn sync<S: EventSink>(&self, ed: &Ed<S>) {
        let mut registered = self.registered.lock().await;
        for name in registered.drain(..) {
            ed.unregister_tool(&name).await;
        }
        for definition in self.definitions().await {
            let name = definition.name.clone();
            match ed.register_tool(definition).await {
                Ok(()) => registered.push(name),
                Err(error) => tracing::warn!("could not register MCP tool `{name}`: {error}"),
            }
        }
    }
}

#[async_trait]
impl ToolExecutor for McpExecutor {
    async fn execute(&self, tool_name: &str, arguments: &str) -> Result<String, String> {
        let args: Value =
            serde_json::from_str(arguments).map_err(|e| format!("bad arguments: {e}"))?;
        // Clone out of the lock: a call can run for a long time and must not hold up a
        // disconnect.
        let (toolset, qualified) = {
            let guard = self.loaded.read().await;
            let loaded = guard.as_ref().ok_or("no MCP server is connected")?;
            let qualified = loaded
                .names
                .get(tool_name)
                .ok_or_else(|| format!("unknown tool `{tool_name}`"))?
                .clone();
            (loaded.toolset.clone(), qualified)
        };
        let outcome = toolset
            .call(&qualified, args, &CancellationToken::new(), &|_| {})
            .await
            .map_err(|e| e.to_string())?;
        if outcome.failed {
            Err(outcome.text)
        } else {
            Ok(outcome.text)
        }
    }
}
