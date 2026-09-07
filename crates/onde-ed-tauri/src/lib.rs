//! Tauri bindings for [Ed](onde_ed::Ed).
//!
//! Ed's core is framework-agnostic and reports through a
//! [`StatusSink`](onde_ed::StatusSink). This crate is the Tauri implementation
//! of that sink plus the commands a webview calls.
//!
//! # Wiring it up
//!
//! Build an [`Ed`](onde_ed::Ed) around a [`TauriSink`] and manage it during
//! setup:
//!
//! ```no_run
//! use onde_ed::Ed;
//! use onde_ed_tauri::{EdState, TauriSink};
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
//!         onde_ed_tauri::chat_get_status,
//!         onde_ed_tauri::chat_get_history,
//!         onde_ed_tauri::chat_clear_history,
//!         onde_ed_tauri::chat_send_message,
//!     ])
//!     .run(tauri::generate_context!())
//!     .expect("run");
//! ```
//!
//! Loading a model is deliberately not a command here: which model, and from
//! where, is an application decision. Call [`Ed::load`](onde_ed::Ed::load)
//! from your own command or during setup.
//!
//! # Events
//!
//! Two events reach the webview, named exactly as the existing applications
//! already emit them so a frontend needs no change:
//!
//! - [`EVENT_CHAT_STATUS_CHANGED`] with a [`ChatStatusPayload`]
//! - [`EVENT_CHAT_REPLY`] with a [`ChatReply`](onde_ed::ChatReply)

#![forbid(unsafe_code)]

mod commands;
mod events;
mod sink;

pub use commands::{
    chat_clear_history, chat_get_history, chat_get_status, chat_send_message, EdState,
};
pub use events::{ChatStatusPayload, EVENT_CHAT_REPLY, EVENT_CHAT_STATUS_CHANGED};
pub use sink::TauriSink;
