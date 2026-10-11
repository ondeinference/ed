//! The commands a webview invokes.

use std::sync::Arc;

use ed_agent::{AgentReply, ApprovalDecision, ChatMessage, ChatReply, Ed, EngineInfo, SessionMeta};
use tauri::State;

use crate::sessions::EdSessions;
use crate::sink::TauriSink;

/// Managed state holding the agent.
///
/// Register one during `setup` with `app.manage(EdState::new(ed))`. Ed is
/// deliberately not a global here: an application that switches accounts, or
/// runs more than one window against separate engines, needs to control the
/// lifetime, and a `static` takes that away.
pub struct EdState {
    ed: Arc<Ed<TauriSink>>,
}

impl EdState {
    pub fn new(ed: Ed<TauriSink>) -> Self {
        Self { ed: Arc::new(ed) }
    }

    /// The agent, for commands the host defines itself — loading a model,
    /// setting a system prompt, anything Ed exposes but this crate doesn't
    /// wrap.
    pub fn ed(&self) -> &Ed<TauriSink> {
        &self.ed
    }

    /// A shared handle to the agent, for work moved onto a spawned task.
    pub fn shared(&self) -> Arc<Ed<TauriSink>> {
        self.ed.clone()
    }

    /// Run an agent turn on a spawned task and deliver the result as a
    /// `chat_reply` event `{id, session, reply, duration, error}`, with
    /// `session` empty. Returns immediately.
    ///
    /// For single-conversation apps. A long-pending invoke can be dropped by a
    /// mobile webview, so the answer travels by event. Text deltas arrive as
    /// `chat_text_delta` while it runs.
    pub fn submit(&self, app: tauri::AppHandle, id: String, message: String) {
        use tauri::Emitter;
        let ed = self.ed.clone();
        tokio::spawn(async move {
            let payload = match ed.run(message).await {
                Ok(reply) => crate::SubmitReplyPayload {
                    id,
                    session: String::new(),
                    reply: Some(reply.text),
                    duration: Some(reply.duration),
                    error: None,
                },
                Err(err) => crate::SubmitReplyPayload {
                    id,
                    session: String::new(),
                    reply: None,
                    duration: None,
                    error: Some(err.to_string()),
                },
            };
            if let Err(err) = app.emit(crate::EVENT_CHAT_REPLY, payload) {
                log::warn!("ed-tauri: could not emit chat_reply: {err}");
            }
        });
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

/// Stored sessions, most recently updated first.
#[tauri::command]
pub async fn chat_list_sessions(state: State<'_, EdSessions>) -> Result<Vec<SessionMeta>, String> {
    state.list().await.map_err(|e| e.to_string())
}

/// Start an empty session and make it live. Returns its id.
#[tauri::command]
pub async fn chat_new_session(state: State<'_, EdSessions>) -> Result<String, String> {
    Ok(state.new_session().await)
}

/// Make `session` live and return its messages.
#[tauri::command]
pub async fn chat_switch_session(
    state: State<'_, EdSessions>,
    session: String,
) -> Result<Vec<ChatMessage>, String> {
    Ok(state.history(&session).await)
}

#[tauri::command]
pub async fn chat_delete_session(
    state: State<'_, EdSessions>,
    session: String,
) -> Result<(), String> {
    state.delete(&session).await.map_err(|e| e.to_string())
}

/// The messages of `session`, without switching to it.
#[tauri::command]
pub async fn chat_get_session_history(
    state: State<'_, EdSessions>,
    session: String,
) -> Result<Vec<ChatMessage>, String> {
    Ok(state.peek(&session).await)
}

/// Run a turn in `session`. Returns at once; the result is a `chat_reply`
/// event carrying `id`.
#[tauri::command]
pub async fn chat_submit(
    state: State<'_, EdSessions>,
    session: String,
    id: String,
    message: String,
) -> Result<(), String> {
    state.submit(&session, &id, message).await;
    Ok(())
}

/// Answer a `chat_approval_requested` event.
#[tauri::command]
pub async fn chat_respond_approval(
    state: State<'_, EdSessions>,
    request_id: String,
    decision: ApprovalDecision,
) -> Result<(), String> {
    state.respond_approval(&request_id, decision).await
}
