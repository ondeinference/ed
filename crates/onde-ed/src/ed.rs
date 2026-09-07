//! The agent handle.

use std::time::Duration;

use log::{error, info};
use onde::inference::types::{format_duration, ChatMessage};
use onde::inference::{
    ChatEngine, EngineInfo, EngineStatus, GgufModelConfig, InferenceError, SamplingConfig,
};

use crate::{ChatReply, NoopSink, StatusSink};

/// An embeddable chat agent over a single [`ChatEngine`].
///
/// Hold one per application. Ed is `Send + Sync` and every method takes
/// `&self`, so it can live behind a `OnceLock`, a framework's managed state,
/// or an `Arc` — whichever the host prefers. It is not cloneable by design:
/// one Ed owns one engine owns one loaded model, and copying that handle
/// around tends to mean two parts of an app disagree about which model is up.
pub struct Ed<S = NoopSink> {
    engine: ChatEngine,
    sink: S,
}

impl Ed<NoopSink> {
    /// An agent that reports nothing. Poll [`info`](Ed::info) for state.
    pub fn new() -> Self {
        Self::with_sink(NoopSink)
    }
}

impl Default for Ed<NoopSink> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: StatusSink> Ed<S> {
    /// An agent that reports transitions and replies to `sink`.
    pub fn with_sink(sink: S) -> Self {
        Self {
            engine: ChatEngine::new(),
            sink,
        }
    }

    /// Associate telemetry with an Onde app id. See [`ChatEngine::with_app_id`].
    pub fn with_app_id(sink: S, onde_app_id: Option<String>) -> Self {
        Self {
            engine: ChatEngine::with_app_id(onde_app_id),
            sink,
        }
    }

    /// The underlying engine.
    ///
    /// An escape hatch for the parts of `onde` Ed deliberately doesn't wrap —
    /// system prompts, alternative load paths, tool calling. Reach for it when
    /// you need them; anything you find yourself writing here for a second
    /// application is a candidate to move into Ed.
    pub fn engine(&self) -> &ChatEngine {
        &self.engine
    }

    /// Load a GGUF model, reporting each transition.
    ///
    /// Emits [`EngineStatus::Loading`] before starting and then either
    /// [`Ready`](EngineStatus::Ready) or [`Error`](EngineStatus::Error). That
    /// sequence is the whole reason this method exists: calling
    /// [`ChatEngine::load_gguf_model`] directly leaves the UI with no way to
    /// know a multi-minute download has begun.
    ///
    /// Downloads the weights first if they aren't in the local cache, so the
    /// `Loading` state can last a long time on a cold start.
    pub async fn load(
        &self,
        config: GgufModelConfig,
        system_prompt: Option<String>,
        sampling: Option<SamplingConfig>,
    ) -> Result<Duration, InferenceError> {
        let model_name = config.display_name.clone();
        info!("ed: loading model {model_name}");
        self.sink
            .status_changed(EngineStatus::Loading, Some(&model_name), None);

        match self
            .engine
            .load_gguf_model(config, system_prompt, sampling)
            .await
        {
            Ok(elapsed) => {
                info!("ed: loaded {model_name} in {}", format_duration(elapsed));
                self.sink
                    .status_changed(EngineStatus::Ready, Some(&model_name), None);
                Ok(elapsed)
            }
            Err(err) => {
                error!("ed: failed to load {model_name}: {err}");
                let message = err.to_string();
                self.sink
                    .status_changed(EngineStatus::Error, Some(&model_name), Some(&message));
                Err(err)
            }
        }
    }

    /// Unload the current model, returning its display name if one was loaded.
    pub async fn unload(&self) -> Option<String> {
        let unloaded = self.engine.unload_model().await;
        if let Some(name) = &unloaded {
            info!("ed: unloaded {name}");
        }
        self.sink
            .status_changed(EngineStatus::Unloaded, unloaded.as_deref(), None);
        unloaded
    }

    /// Run one inference turn.
    ///
    /// Never returns an error: a failure is reported in
    /// [`ChatReply::error`] so a caller has one shape to render either way.
    /// The reply also goes to the sink, so a host can render it from an event
    /// instead of awaiting this call — see [`StatusSink::replied`].
    pub async fn send(&self, message: impl Into<String>) -> ChatReply {
        let reply = match self.engine.send_message(message).await {
            Ok(result) => ChatReply::ok(result.text, result.duration_display),
            Err(err) => {
                error!("ed: inference failed: {err}");
                ChatReply::failed(err)
            }
        };
        self.sink.replied(&reply);
        reply
    }

    /// Current status, loaded model, footprint, and history length.
    pub async fn info(&self) -> EngineInfo {
        self.engine.info().await
    }

    /// Whether a model is loaded and ready.
    pub async fn is_loaded(&self) -> bool {
        self.engine.is_loaded().await
    }

    /// The conversation so far.
    pub async fn history(&self) -> Vec<ChatMessage> {
        self.engine.history().await
    }

    /// Clear the conversation, returning how many messages were dropped.
    ///
    /// Leaves the model loaded — this starts a new conversation, it doesn't
    /// tear down the engine.
    pub async fn clear_history(&self) -> usize {
        let cleared = self.engine.clear_history().await;
        info!("ed: cleared {cleared} messages");
        cleared
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records everything Ed reports, so a test can assert on the sequence of
    /// transitions rather than only the final state.
    #[derive(Default)]
    struct RecordingSink {
        statuses: Mutex<Vec<(EngineStatus, Option<String>)>>,
        replies: Mutex<Vec<ChatReply>>,
    }

    impl RecordingSink {
        fn statuses(&self) -> Vec<(EngineStatus, Option<String>)> {
            self.statuses.lock().unwrap().clone()
        }
    }

    impl StatusSink for &'static RecordingSink {
        fn status_changed(&self, status: EngineStatus, model_name: Option<&str>, _: Option<&str>) {
            self.statuses
                .lock()
                .unwrap()
                .push((status, model_name.map(str::to_owned)));
        }

        fn replied(&self, reply: &ChatReply) {
            self.replies.lock().unwrap().push(reply.clone());
        }
    }

    fn sink() -> &'static RecordingSink {
        Box::leak(Box::new(RecordingSink::default()))
    }

    #[tokio::test]
    async fn a_fresh_agent_reports_itself_unloaded_and_empty() {
        let ed = Ed::new();
        let info = ed.info().await;

        assert_eq!(info.status, EngineStatus::Unloaded);
        assert_eq!(info.model_name, None);
        assert_eq!(info.history_length, 0);
        assert!(!ed.is_loaded().await);
        assert!(ed.history().await.is_empty());
    }

    #[tokio::test]
    async fn unloading_reports_the_transition_even_with_nothing_loaded() {
        // The status still has to reach the host: a UI that asked to unload
        // needs to see `Unloaded` regardless of whether work was done, or the
        // view sits on a stale "ready" forever.
        let sink = sink();
        let ed = Ed::with_sink(sink);

        assert_eq!(ed.unload().await, None);
        assert_eq!(sink.statuses(), vec![(EngineStatus::Unloaded, None)]);
    }

    #[tokio::test]
    async fn clearing_an_empty_history_drops_nothing() {
        let ed = Ed::new();
        assert_eq!(ed.clear_history().await, 0);
    }

    #[tokio::test]
    async fn the_default_sink_swallows_reports_without_panicking() {
        // `Ed::new()` is the no-sink path; exercising it guards against a
        // future notification being added that assumes a real sink exists.
        let ed = Ed::default();
        ed.unload().await;
        assert_eq!(ed.info().await.status, EngineStatus::Unloaded);
    }
}
