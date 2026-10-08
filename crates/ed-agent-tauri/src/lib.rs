//! Tauri bindings for [Ed](ed_agent::Ed).
//!
//! Ed's core is framework-agnostic and reports through a
//! [`StatusSink`](ed_agent::StatusSink). This crate is the Tauri implementation
//! of that sink plus the commands a webview calls.
//!
//! # Wiring it up
//!
//! Build an [`Ed`](ed_agent::Ed) around a [`TauriSink`] and manage it during
//! setup:
//!
//! ```no_run
//! use ed_agent::Ed;
//! use ed_agent_tauri::{EdState, TauriSink};
//! use tauri::Manager;
//!
//! fn setup(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
//!     let ed = Ed::with_sink(TauriSink::new(app.handle().clone()));
//!     app.manage(EdState::new(ed));
//!     Ok(())
//! }
//! ```
//!
//! Then register the commands on the builder. This can't be a doc test — a
//! library has no `tauri.conf.json` for `generate_context!` to read:
//!
//! ```text
//! tauri::Builder::default()
//!     .setup(|app| setup(app))
//!     .invoke_handler(tauri::generate_handler![
//!         ed_agent_tauri::chat_get_status,
//!         ed_agent_tauri::chat_get_history,
//!         ed_agent_tauri::chat_clear_history,
//!         ed_agent_tauri::chat_send_message,
//!         ed_agent_tauri::chat_run,
//!         ed_agent_tauri::chat_cancel,
//!     ])
//!     .run(tauri::generate_context!())
//!     .expect("run");
//! ```
//!
//! Loading a model is deliberately not a command here: which model, and from
//! where, is an application decision. Call [`Ed::load`](ed_agent::Ed::load)
//! from your own command or during setup.
//!
//! # Several conversations
//!
//! For an app with a session list, build an [`EdSessions`] instead of an
//! [`EdState`]. You supply a [`SessionStore`](ed_agent::SessionStore); it owns
//! the live session, the cache, the logout fence and approvals:
//!
//! ```text
//! let sessions = EdSessions::builder(app.handle().clone(), MyStore::new())
//!     .executor(my_executor)
//!     .build();
//! app.manage(sessions);
//! ```
//!
//! Register `chat_list_sessions`, `chat_new_session`, `chat_switch_session`,
//! `chat_delete_session`, `chat_get_session_history`, `chat_submit` and
//! `chat_respond_approval`. `chat_submit` returns at once and the answer comes
//! as a `chat_reply` event `{id, session, reply, duration, error}`, with
//! `chat_text_delta {session, id, delta}` events while it generates.
//!
//! A host that has to rewrite the message first (to add context) or recover
//! from an engine failure wraps [`EdSessions::submit_with`] in a command of its
//! own. Call [`EdSessions::reset`] on logout or account switch.
//!
//! # Tools
//!
//! Tool calling works through this crate, but the host supplies the executor.
//! With [`EdSessions`] approvals are handled for you: [`TauriApprovals`] emits
//! `chat_approval_requested {request_id, session, call, risk}`, waits (120 s by
//! default) for `chat_respond_approval {request_id, decision}`, then emits
//! `chat_approval_resolved {request_id}`. A timeout or a reset denies.
//!
//! With [`EdState`], build the agent with
//! [`Ed::with_agent`](ed_agent::Ed::with_agent) and your own
//! [`ApprovalHandler`](ed_agent::ApprovalHandler); the default `Ed::with_sink`
//! rejects every tool and denies every approval.
//!
//! # Events
//!
//! The first two are named exactly as the existing applications already emit
//! them, so a frontend needs no change:
//!
//! - [`EVENT_CHAT_STATUS_CHANGED`] with a [`ChatStatusPayload`]
//! - [`EVENT_CHAT_REPLY`] with a [`ChatReply`](ed_agent::ChatReply)
//! - [`EVENT_CHAT_TOOL_REQUESTED`] with a [`ToolCall`](ed_agent::ToolCall)
//! - [`EVENT_CHAT_APPROVAL_REQUESTED`] with an
//!   [`ApprovalRequest`](ed_agent::ApprovalRequest)
//! - [`EVENT_CHAT_TOOL_STARTED`] with a [`ToolCall`](ed_agent::ToolCall)
//! - [`EVENT_CHAT_TOOL_FINISHED`] with a
//!   [`ToolExecutionResult`](ed_agent::ToolExecutionResult)
//! - [`EVENT_CHAT_AGENT_REPLY`] with an [`AgentReply`](ed_agent::AgentReply)
//! - [`EVENT_CHAT_WARNING`] with a string
//! - [`EVENT_CHAT_TEXT_DELTA`] with a [`TextDeltaPayload`]
//! - [`EVENT_CHAT_APPROVAL_RESOLVED`] with an [`ApprovalResolvedPayload`]

#![forbid(unsafe_code)]

mod approvals;
mod commands;
mod events;
mod scope;
mod sessions;
mod sink;

pub use approvals::{TauriApprovals, DEFAULT_APPROVAL_TIMEOUT};
pub use commands::{
    chat_cancel, chat_clear_history, chat_delete_session, chat_get_history,
    chat_get_session_history, chat_get_status, chat_list_sessions, chat_new_session,
    chat_respond_approval, chat_run, chat_send_message, chat_submit, chat_switch_session, EdState,
};
pub use events::{
    ApprovalRequestedPayload, ApprovalResolvedPayload, ChatStatusPayload, SubmitReplyPayload,
    TextDeltaPayload, EVENT_CHAT_AGENT_REPLY, EVENT_CHAT_APPROVAL_REQUESTED,
    EVENT_CHAT_APPROVAL_RESOLVED, EVENT_CHAT_REPLY, EVENT_CHAT_STATUS_CHANGED,
    EVENT_CHAT_TEXT_DELTA, EVENT_CHAT_TOOL_FINISHED, EVENT_CHAT_TOOL_REQUESTED,
    EVENT_CHAT_TOOL_STARTED, EVENT_CHAT_WARNING,
};
pub use sessions::{EdSessions, EdSessionsBuilder};
pub use sink::TauriSink;
