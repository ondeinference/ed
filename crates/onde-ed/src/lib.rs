//! Ed: an embeddable, local-LLM-first chat agent.
//!
//! Ed sits one layer above the [`onde`] inference SDK. `onde` gives you a
//! [`ChatEngine`](onde::inference::ChatEngine) you can load a model into and
//! send messages to; Ed adds the part every application built on it was
//! writing for itself:
//!
//! - **Lifecycle orchestration.** Loading a model is a sequence of state
//!   changes, not one call. Ed drives `Loading -> Ready` (or `-> Error`) and
//!   reports each transition.
//! - **Notification.** `onde` exposes status by polling
//!   [`info()`](onde::inference::ChatEngine::info). A UI needs to be told.
//!   Ed pushes transitions and replies to a [`StatusSink`] the host implements.
//! - **A flattened reply.** Inference returns `Result<InferenceResult, _>`;
//!   a frontend wants one shape covering both outcomes. That's [`ChatReply`].
//!
//! Ed deliberately does *not* own sessions, persistence, model catalogues, or
//! downloads. Those differ per application and belong to the host.
//!
//! # Platforms
//!
//! Ed has no `cfg(target_os)` gates. `onde` already splits real and fallback
//! implementations internally, so this crate compiles everywhere and simply
//! surfaces `onde`'s error on a platform it can't run inference on. Do not add
//! platform gates here, and be careful about adding them in a host: a gate
//! narrower than `onde`'s own support list silently disables chat on a
//! platform that would have worked.
//!
//! # Example
//!
//! ```no_run
//! use onde_ed::{Ed, GgufModelConfig};
//!
//! # async fn example() {
//! let ed = Ed::new();
//! let config = GgufModelConfig::qwen25_1_5b();
//! ed.load(config, None, None).await.expect("load");
//!
//! let reply = ed.send("Summarise this thread.").await;
//! println!("{}", reply.reply.unwrap_or_default());
//! # }
//! ```

#![forbid(unsafe_code)]

mod ed;
mod reply;
mod sink;

pub use ed::Ed;
pub use reply::ChatReply;
pub use sink::{NoopSink, StatusSink};

// Re-exported so a host depends on `onde-ed` alone and never has to match a
// second `onde` version against this crate's.
pub use onde::inference::types::ChatMessage;
pub use onde::inference::{
    ChatEngine, EngineInfo, EngineStatus, GgufModelConfig, InferenceError, SamplingConfig,
};
