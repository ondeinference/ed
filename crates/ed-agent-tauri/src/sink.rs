//! Bridges Ed's notifications onto Tauri's event bus.

use ed_agent::{ChatReply, EngineStatus, StatusSink};
use log::warn;
use tauri::{AppHandle, Emitter};

use crate::events::{ChatStatusPayload, EVENT_CHAT_REPLY, EVENT_CHAT_STATUS_CHANGED};

/// A [`StatusSink`] that emits to the webview.
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
}

impl StatusSink for TauriSink {
    fn status_changed(&self, status: EngineStatus, model_name: Option<&str>, error: Option<&str>) {
        let payload = ChatStatusPayload {
            status,
            model_name: model_name.map(str::to_owned),
            error: error.map(str::to_owned),
        };
        if let Err(err) = self.app.emit(EVENT_CHAT_STATUS_CHANGED, payload) {
            warn!("ed-tauri: could not emit {EVENT_CHAT_STATUS_CHANGED}: {err}");
        }
    }

    fn replied(&self, reply: &ChatReply) {
        if let Err(err) = self.app.emit(EVENT_CHAT_REPLY, reply) {
            warn!("ed-tauri: could not emit {EVENT_CHAT_REPLY}: {err}");
        }
    }
}
