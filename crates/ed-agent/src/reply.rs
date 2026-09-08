//! The flattened result of one inference turn.

use serde::{Deserialize, Serialize};

/// The outcome of a single [`Ed::send`](crate::Ed::send), in one shape whether
/// it succeeded or failed.
///
/// Inference returns `Result<InferenceResult, InferenceError>`, but a frontend
/// receiving this over an event boundary wants a single payload it can render
/// either way. Every application built on `onde` was flattening it the same
/// way, so Ed does it once. `reply` and `error` are mutually exclusive:
/// exactly one is `Some`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatReply {
    /// The assistant's text, or `None` if inference failed.
    pub reply: Option<String>,
    /// Human-readable generation time (e.g. `"1.4s"`), or `None` on failure.
    pub duration: Option<String>,
    /// Why inference failed, or `None` on success.
    pub error: Option<String>,
}

impl ChatReply {
    pub(crate) fn ok(reply: String, duration: String) -> Self {
        Self {
            reply: Some(reply),
            duration: Some(duration),
            error: None,
        }
    }

    pub(crate) fn failed(error: impl std::fmt::Display) -> Self {
        Self {
            reply: None,
            duration: None,
            error: Some(error.to_string()),
        }
    }

    /// Whether this turn produced an answer.
    pub fn is_ok(&self) -> bool {
        self.reply.is_some()
    }
}
