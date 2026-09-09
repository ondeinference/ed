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
//! # Tools
//!
//! Tool calling works through this crate, but the host owns both halves of it.
//! Build the agent with [`Ed::with_agent`](ed_agent::Ed::with_agent) to supply
//! a [`ToolExecutor`](ed_agent::ToolExecutor) and an
//! [`ApprovalHandler`](ed_agent::ApprovalHandler); the default `Ed::with_sink`
//! rejects every tool and denies every approval, which is the right default but
//! not a working agent.
//!
//! Approvals are deliberately not a command. Answering one means a webview
//! round trip in the middle of an awaited Rust call, and how a host wants to
//! wire that up (a channel, a shared map, a modal it blocks on) differs enough
//! that guessing here would be worse than leaving it. What this crate does give
//! you is [`EVENT_CHAT_APPROVAL_REQUESTED`], so the sheet can go up at the
//! right moment.
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

#![forbid(unsafe_code)]

mod commands;
mod events;
mod sink;

pub use commands::{
    chat_cancel, chat_clear_history, chat_get_history, chat_get_status, chat_run,
    chat_send_message, EdState,
};
pub use events::{
    ChatStatusPayload, EVENT_CHAT_AGENT_REPLY, EVENT_CHAT_APPROVAL_REQUESTED, EVENT_CHAT_REPLY,
    EVENT_CHAT_STATUS_CHANGED, EVENT_CHAT_TOOL_FINISHED, EVENT_CHAT_TOOL_REQUESTED,
    EVENT_CHAT_TOOL_STARTED, EVENT_CHAT_WARNING,
};
pub use sink::TauriSink;
