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
