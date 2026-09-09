//! Event names and payloads emitted to the webview.

use ed_agent::EngineStatus;
use serde::{Deserialize, Serialize};

/// Emitted whenever the engine's lifecycle status changes.
///
/// The name matches what the existing applications already emit, so adopting
/// Ed needs no frontend change.
pub const EVENT_CHAT_STATUS_CHANGED: &str = "chat_status_changed";

/// Emitted once an inference turn completes, successfully or not.
pub const EVENT_CHAT_REPLY: &str = "chat_reply";

/// Emitted with a [`ToolCall`](ed_agent::ToolCall) when the model asks for a
/// tool, before any approval or execution.
pub const EVENT_CHAT_TOOL_REQUESTED: &str = "chat_tool_requested";

/// Emitted with an [`ApprovalRequest`](ed_agent::ApprovalRequest) when a
/// mutating tool is waiting on the user.
///
/// This is a notification, not the approval itself. Ed asks the
/// [`ApprovalHandler`](ed_agent::ApprovalHandler) the host passed to
/// [`Ed::with_agent`](ed_agent::Ed::with_agent) for the decision; this event
/// exists so the webview can put a sheet on screen while that call is pending.
pub const EVENT_CHAT_APPROVAL_REQUESTED: &str = "chat_approval_requested";

/// Emitted with a [`ToolCall`](ed_agent::ToolCall) once a tool is approved and
/// about to run.
pub const EVENT_CHAT_TOOL_STARTED: &str = "chat_tool_started";

/// Emitted with a [`ToolExecutionResult`](ed_agent::ToolExecutionResult) when a
/// tool finishes, whether it succeeded or failed.
pub const EVENT_CHAT_TOOL_FINISHED: &str = "chat_tool_finished";

/// Emitted with an [`AgentReply`](ed_agent::AgentReply) when a full agent turn
/// completes.
///
/// Distinct from [`EVENT_CHAT_REPLY`], which covers the single-shot
/// [`Ed::send`](ed_agent::Ed::send) path. A turn reports one or the other,
/// never both.
pub const EVENT_CHAT_AGENT_REPLY: &str = "chat_agent_reply";

/// Emitted with a string when Ed has something the user should see but the turn
/// is still viable, such as an unverified model or a spent tool-round budget.
pub const EVENT_CHAT_WARNING: &str = "chat_warning";

/// Payload for [`EVENT_CHAT_STATUS_CHANGED`].
///
/// `status` serialises to `"unloaded"`, `"loading"`, `"ready"`, `"generating"`,
/// or `"error"` — [`EngineStatus`] is `#[serde(rename_all = "snake_case")]`,
/// which is the same set of strings applications were previously declaring as
/// their own constants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStatusPayload {
    pub status: EngineStatus,
    pub model_name: Option<String>,
    pub error: Option<String>,
}
