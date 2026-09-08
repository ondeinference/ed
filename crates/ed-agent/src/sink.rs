//! Where Ed reports what just happened.

use onde::inference::EngineStatus;

use crate::ChatReply;

/// Receives lifecycle transitions and completed replies.
///
/// `onde` reports status by polling
/// [`info()`](onde::inference::ChatEngine::info), which is the wrong shape for
/// a UI: nothing tells the view that a load finished, so hosts end up either
/// timing a poll loop or threading their own event emission through every call
/// site. Ed pushes instead, and a host implements this once to bridge into
/// whatever its UI listens on.
///
/// Both methods are synchronous and should stay cheap — they are called while
/// Ed is mid-operation. Hand off to a channel or an event bus rather than
/// blocking. Implementations must be `Send + Sync` because a load or a send
/// may be driven from any task.
pub trait StatusSink: Send + Sync + 'static {
    /// The engine moved to a new lifecycle state.
    ///
    /// `model_name` is the model involved where one is known, and `error`
    /// carries the reason when `status` is [`EngineStatus::Error`].
    fn status_changed(&self, status: EngineStatus, model_name: Option<&str>, error: Option<&str>);

    /// An inference turn finished, successfully or not.
    ///
    /// Delivered as a notification rather than only as a return value because
    /// inference is slow: a host that awaits the call from a UI callback can
    /// have that callback collected before it resolves. Emitting lets the view
    /// pick the result up independently.
    fn replied(&self, reply: &ChatReply);
}

/// A [`StatusSink`] that drops everything.
///
/// The default for [`Ed::new`](crate::Ed::new), and what you want in tests or
/// a host that only ever polls [`Ed::info`](crate::Ed::info).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSink;

impl StatusSink for NoopSink {
    fn status_changed(&self, _: EngineStatus, _: Option<&str>, _: Option<&str>) {}
    fn replied(&self, _: &ChatReply) {}
}
