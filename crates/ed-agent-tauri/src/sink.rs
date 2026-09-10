//! Bridges Ed's notifications onto Tauri's event bus.

use ed_agent::{
    AgentReply, ApprovalRequest, ChatReply, EngineStatus, EventSink, ToolCall, ToolExecutionResult,
};
use log::warn;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::events::{
    ChatStatusPayload, EVENT_CHAT_AGENT_REPLY, EVENT_CHAT_APPROVAL_REQUESTED, EVENT_CHAT_REPLY,
    EVENT_CHAT_STATUS_CHANGED, EVENT_CHAT_TOOL_FINISHED, EVENT_CHAT_TOOL_REQUESTED,
    EVENT_CHAT_TOOL_STARTED, EVENT_CHAT_WARNING,
};

/// An [`EventSink`] that emits to the webview.
///
/// Emission failures are logged rather than propagated: a closing window
/// dropping its listeners is normal, and an inference turn that already
/// produced an answer shouldn't be reported as failed because nobody was
/// listening.
pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn emit(&self, event: &str, payload: impl Serialize + Clone) {
        if let Err(err) = self.app.emit(event, payload) {
            warn!("ed-tauri: could not emit {event}: {err}");
        }
    }
}

impl EventSink for TauriSink {
    fn status_changed(&self, status: EngineStatus, model_name: Option<&str>, error: Option<&str>) {
        self.emit(
            EVENT_CHAT_STATUS_CHANGED,
            ChatStatusPayload {
                status,
                model_name: model_name.map(str::to_owned),
                error: error.map(str::to_owned),
            },
        );
    }

    fn replied(&self, reply: &ChatReply) {
        self.emit(EVENT_CHAT_REPLY, reply);
    }

    fn tool_requested(&self, call: &ToolCall) {
        self.emit(EVENT_CHAT_TOOL_REQUESTED, call);
    }

    fn approval_requested(&self, request: &ApprovalRequest) {
        self.emit(EVENT_CHAT_APPROVAL_REQUESTED, request);
    }

    fn tool_started(&self, call: &ToolCall) {
        self.emit(EVENT_CHAT_TOOL_STARTED, call);
    }

    fn tool_finished(&self, result: &ToolExecutionResult) {
        self.emit(EVENT_CHAT_TOOL_FINISHED, result);
    }

    fn agent_replied(&self, reply: &AgentReply) {
        self.emit(EVENT_CHAT_AGENT_REPLY, reply);
    }

    fn warning(&self, message: &str) {
        self.emit(EVENT_CHAT_WARNING, message);
    }
}
