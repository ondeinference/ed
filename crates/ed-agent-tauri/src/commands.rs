//! The commands a webview invokes.

use ed_agent::{AgentReply, ChatMessage, ChatReply, Ed, EngineInfo};
use tauri::State;

use crate::sink::TauriSink;

/// Managed state holding the agent.
///
/// Register one during `setup` with `app.manage(EdState::new(ed))`. Ed is
/// deliberately not a global here: an application that switches accounts, or
/// runs more than one window against separate engines, needs to control the
/// lifetime, and a `static` takes that away.
pub struct EdState {
    ed: Ed<TauriSink>,
}

impl EdState {
    pub fn new(ed: Ed<TauriSink>) -> Self {
        Self { ed }
    }

    /// The agent, for commands the host defines itself — loading a model,
    /// setting a system prompt, anything Ed exposes but this crate doesn't
    /// wrap.
    pub fn ed(&self) -> &Ed<TauriSink> {
        &self.ed
    }
}

/// Current status, loaded model, footprint, and history length.
#[tauri::command]
pub async fn chat_get_status(state: State<'_, EdState>) -> Result<EngineInfo, String> {
    Ok(state.ed.info().await)
}

/// The conversation so far.
#[tauri::command]
pub async fn chat_get_history(state: State<'_, EdState>) -> Result<Vec<ChatMessage>, String> {
    Ok(state.ed.history().await)
}

/// Clear the conversation, returning how many messages were dropped. The model
/// stays loaded.
#[tauri::command]
pub async fn chat_clear_history(state: State<'_, EdState>) -> Result<usize, String> {
    Ok(state.ed.clear_history().await)
}

/// Run one inference turn.
///
/// The reply is also emitted as [`EVENT_CHAT_REPLY`](crate::EVENT_CHAT_REPLY).
/// Prefer listening for that over awaiting this call: on a slow model the
/// webview can garbage-collect the invoke callback before generation
/// finishes, and the answer is lost even though inference succeeded.
#[tauri::command]
pub async fn chat_send_message(
    state: State<'_, EdState>,
    message: String,
) -> Result<ChatReply, String> {
    Ok(state.ed.send(message).await)
}

/// Run a full agent turn, including any tools the host registered.
///
/// Reports progress as it goes: `chat_tool_requested`,
/// `chat_approval_requested`, `chat_tool_started`, `chat_tool_finished`, and
/// finally `chat_agent_reply`. As with [`chat_send_message`], prefer the event
/// over awaiting this call, since an agent turn is slower still.
///
/// Unlike [`chat_send_message`] this returns an `Err` on failure rather than
/// folding it into the payload: an agent turn has failure modes a chat reply
/// doesn't (a model that can't call tools, a cancelled turn, a spent round
/// budget), and flattening them all into an error string would throw away the
/// distinction the frontend needs to respond to.
#[tauri::command]
pub async fn chat_run(state: State<'_, EdState>, message: String) -> Result<AgentReply, String> {
    state
        .ed
        .run(message)
        .await
        .map_err(|error| error.to_string())
}

/// Cancel the agent turn in progress, if there is one.
#[tauri::command]
pub async fn chat_cancel(state: State<'_, EdState>) -> Result<(), String> {
    state.ed.cancel();
    Ok(())
}
