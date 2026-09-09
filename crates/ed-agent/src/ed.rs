//! The agent handle.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use log::{error, info};
use onde::inference::types::{format_duration, ChatMessage};
use onde::inference::{
    ChatEngine, EngineInfo, EngineStatus, GgufModelConfig, InferenceError, SamplingConfig,
    ToolCallingSupport, ToolDefinition, ToolResult,
};

use crate::{
    tool::truncate_output, AgentConfig, AgentError, AgentReply, AgentToolDefinition,
    ApprovalDecision, ApprovalHandler, ApprovalRequest, ChatReply, DenyApprovals, EventSink,
    NoopSink, RejectingExecutor, ToolCall, ToolExecutionResult, ToolExecutor, ToolRisk,
};

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
    tools: tokio::sync::RwLock<HashMap<String, AgentToolDefinition>>,
    executor: Arc<dyn ToolExecutor>,
    approvals: Arc<dyn ApprovalHandler>,
    session_grants: tokio::sync::Mutex<HashSet<String>>,
    turn: tokio::sync::Mutex<()>,
    cancelled: AtomicBool,
    agent_config: AgentConfig,
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

impl<S: EventSink> Ed<S> {
    /// An agent that reports transitions and replies to `sink`.
    pub fn with_sink(sink: S) -> Self {
        Self::with_agent(
            sink,
            Arc::new(RejectingExecutor),
            Arc::new(DenyApprovals),
            AgentConfig::default(),
        )
    }

    /// Build an agent with host-defined tool execution and approval handling.
    pub fn with_agent(
        sink: S,
        executor: Arc<dyn ToolExecutor>,
        approvals: Arc<dyn ApprovalHandler>,
        config: AgentConfig,
    ) -> Self {
        Self {
            engine: ChatEngine::new(),
            sink,
            tools: tokio::sync::RwLock::new(HashMap::new()),
            executor,
            approvals,
            session_grants: tokio::sync::Mutex::new(HashSet::new()),
            turn: tokio::sync::Mutex::new(()),
            cancelled: AtomicBool::new(false),
            agent_config: config.validated(),
        }
    }

    /// Associate telemetry with an Onde app id. See [`ChatEngine::with_app_id`].
    pub fn with_app_id(sink: S, onde_app_id: Option<String>) -> Self {
        Self {
            engine: ChatEngine::with_app_id(onde_app_id),
            sink,
            tools: tokio::sync::RwLock::new(HashMap::new()),
            executor: Arc::new(RejectingExecutor),
            approvals: Arc::new(DenyApprovals),
            session_grants: tokio::sync::Mutex::new(HashSet::new()),
            turn: tokio::sync::Mutex::new(()),
            cancelled: AtomicBool::new(false),
            agent_config: AgentConfig::default(),
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

    /// Run any load, reporting the transitions around it.
    ///
    /// Emits [`EngineStatus::Loading`] before `load` starts and then either
    /// [`Ready`](EngineStatus::Ready) or [`Error`](EngineStatus::Error). That
    /// sequence is the whole reason this method exists: calling a
    /// [`ChatEngine`] load path directly leaves the UI with no way to know a
    /// multi-minute download has begun.
    ///
    /// Ed takes a closure rather than wrapping each load path by name because
    /// `onde`'s load APIs take types Ed has no business in its public
    /// signature — `ChatEngine::load_assigned_model` takes an `Environment`
    /// from the smbCloud SDK, and *which* crate that type comes from changed
    /// between `onde` releases. Naming it here would drag that dependency into
    /// every host and pin them all to Ed's version of it. Passing a closure
    /// keeps Ed's dependency surface to `onde` alone and works for load paths
    /// that don't exist yet.
    ///
    /// ```no_run
    /// # use ed_agent::{Ed, GgufModelConfig};
    /// # async fn example(ed: Ed) {
    /// ed.load_with("Qwen 2.5", |engine| {
    ///     engine.load_gguf_model(GgufModelConfig::qwen25_1_5b(), None, None)
    /// })
    /// .await
    /// .expect("load");
    /// # }
    /// ```
    ///
    /// `model_name` is what the host wants the UI to show while loading; it is
    /// reported with each transition and needn't match what the engine ends up
    /// calling the model.
    pub async fn load_with<'a, F, Fut>(
        &'a self,
        model_name: &str,
        load: F,
    ) -> Result<Duration, InferenceError>
    where
        F: FnOnce(&'a ChatEngine) -> Fut,
        Fut: Future<Output = Result<Duration, InferenceError>>,
    {
        info!("ed: loading model {model_name}");
        self.sink
            .status_changed(EngineStatus::Loading, Some(model_name), None);

        match load(&self.engine).await {
            Ok(elapsed) => {
                info!("ed: loaded {model_name} in {}", format_duration(elapsed));
                self.sink
                    .status_changed(EngineStatus::Ready, Some(model_name), None);
                Ok(elapsed)
            }
            Err(err) => {
                error!("ed: failed to load {model_name}: {err}");
                let message = err.to_string();
                self.sink
                    .status_changed(EngineStatus::Error, Some(model_name), Some(&message));
                Err(err)
            }
        }
    }

    /// Load a GGUF model, reporting each transition.
    ///
    /// A convenience over [`load_with`](Ed::load_with) for the most common
    /// path. Downloads the weights first if they aren't in the local cache, so
    /// the `Loading` state can last a long time on a cold start.
    pub async fn load(
        &self,
        config: GgufModelConfig,
        system_prompt: Option<String>,
        sampling: Option<SamplingConfig>,
    ) -> Result<Duration, InferenceError> {
        let model_name = config.display_name.clone();
        self.load_with(&model_name, |engine| {
            engine.load_gguf_model(config, system_prompt, sampling)
        })
        .await
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

    /// Register a host capability that the model may request.
    pub async fn register_tool(&self, tool: AgentToolDefinition) -> Result<(), AgentError> {
        tool.validate()?;
        let mut tools = self.tools.write().await;
        if tools.contains_key(&tool.name) {
            return Err(AgentError::DuplicateTool {
                name: tool.name.clone(),
            });
        }
        tools.insert(tool.name.clone(), tool);
        Ok(())
    }

    pub async fn unregister_tool(&self, name: &str) -> bool {
        self.tools.write().await.remove(name).is_some()
    }

    pub async fn remove_all_tools(&self) {
        self.tools.write().await.clear();
        self.session_grants.lock().await.clear();
    }

    /// Request cancellation of the active agent turn.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    async fn execute_tool_call(
        &self,
        call: &ToolCall,
        registered: &HashMap<String, AgentToolDefinition>,
    ) -> ToolExecutionResult {
        let Some(definition) = registered.get(&call.name) else {
            return ToolExecutionResult {
                tool_call_id: call.id.clone(),
                content: format!("tool {:?} is not registered", call.name),
                is_error: true,
            };
        };

        if !matches!(
            serde_json::from_str::<serde_json::Value>(&call.arguments),
            Ok(serde_json::Value::Object(_))
        ) {
            return ToolExecutionResult {
                tool_call_id: call.id.clone(),
                content: "tool arguments must be a valid JSON object".to_string(),
                is_error: true,
            };
        }

        let approved = if definition.risk == ToolRisk::ReadOnly
            || self.session_grants.lock().await.contains(&call.name)
        {
            true
        } else {
            let request = ApprovalRequest {
                call: call.clone(),
                risk: definition.risk,
            };
            self.sink.approval_requested(&request);
            match self.approvals.approve(request).await {
                ApprovalDecision::AllowOnce => true,
                ApprovalDecision::AllowForSession => {
                    self.session_grants.lock().await.insert(call.name.clone());
                    true
                }
                ApprovalDecision::Deny => false,
            }
        };

        if !approved {
            return ToolExecutionResult {
                tool_call_id: call.id.clone(),
                content: format!("tool {:?} was denied by the user", call.name),
                is_error: true,
            };
        }

        self.sink.tool_started(call);
        match self.executor.execute(&call.name, &call.arguments).await {
            Ok(content) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                content: truncate_output(content, self.agent_config.max_tool_output_chars),
                is_error: false,
            },
            Err(error) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                content: truncate_output(
                    format!("tool execution failed: {error}"),
                    self.agent_config.max_tool_output_chars,
                ),
                is_error: true,
            },
        }
    }

    /// Run a complete local agent turn, including any requested host tools.
    pub async fn run(&self, message: impl Into<String>) -> Result<AgentReply, AgentError> {
        let _turn = self.turn.lock().await;
        self.cancelled.store(false, Ordering::Release);

        let registered = self.tools.read().await.clone();
        if registered.is_empty() {
            let reply = self.send(message).await;
            return match (reply.reply, reply.duration, reply.error) {
                (Some(text), Some(duration), None) => {
                    let agent_reply = AgentReply {
                        text,
                        duration_seconds: 0.0,
                        duration,
                        tool_rounds: 0,
                    };
                    self.sink.agent_replied(&agent_reply);
                    Ok(agent_reply)
                }
                (_, _, Some(reason)) => Err(AgentError::Inference { reason }),
                _ => Err(AgentError::EmptyReply),
            };
        }

        match self.engine.tool_calling_support().await {
            ToolCallingSupport::Unsupported => return Err(AgentError::UnsupportedModel),
            ToolCallingSupport::Unknown => self
                .sink
                .warning("the loaded model has not been verified for tool calling"),
            ToolCallingSupport::Supported => {}
        }

        let mut definitions: Vec<ToolDefinition> = registered
            .values()
            .map(|tool| ToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters_schema: tool.parameters_schema.clone(),
            })
            .collect();
        definitions.sort_by(|left, right| left.name.cmp(&right.name));

        let mut result = self
            .engine
            .send_message_with_tools(message.into(), &definitions)
            .await
            .map_err(|error| AgentError::Inference {
                reason: error.to_string(),
            })?;
        let mut duration_seconds = result.duration_secs;
        let mut rounds = 0_u8;

        while !result.tool_calls.is_empty() {
            rounds += 1;
            let mut tool_results = Vec::with_capacity(result.tool_calls.len());

            for (index, requested) in result.tool_calls.iter().enumerate() {
                if self.cancelled.load(Ordering::Acquire) {
                    for pending in &result.tool_calls[index..] {
                        tool_results.push(ToolResult {
                            tool_call_id: pending.id.clone(),
                            content: "tool was not executed because the user cancelled the turn"
                                .to_string(),
                        });
                    }
                    self.engine.record_tool_results(tool_results).await;
                    return Err(AgentError::Cancelled);
                }

                let call = ToolCall {
                    id: requested.id.clone(),
                    name: requested.function_name.clone(),
                    arguments: requested.arguments.clone(),
                };
                self.sink.tool_requested(&call);

                let execution = self.execute_tool_call(&call, &registered).await;
                self.sink.tool_finished(&execution);
                tool_results.push(ToolResult {
                    tool_call_id: execution.tool_call_id,
                    content: execution.content,
                });
            }

            if self.cancelled.load(Ordering::Acquire) {
                self.engine.record_tool_results(tool_results).await;
                return Err(AgentError::Cancelled);
            }

            let next_tools = if rounds < self.agent_config.max_tool_rounds {
                Some(definitions.as_slice())
            } else {
                None
            };
            result = self
                .engine
                .send_tool_results(tool_results, next_tools)
                .await
                .map_err(|error| AgentError::Inference {
                    reason: error.to_string(),
                })?;
            duration_seconds += result.duration_secs;
        }

        if result.text.trim().is_empty() {
            return Err(AgentError::EmptyReply);
        }
        let reply = AgentReply {
            text: result.text,
            duration_seconds,
            duration: format_duration(Duration::from_secs_f64(duration_seconds)),
            tool_rounds: rounds,
        };
        self.sink.agent_replied(&reply);
        Ok(reply)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;

    /// Records everything Ed reports, so a test can assert on the sequence of
    /// transitions rather than only the final state.
    #[derive(Default)]
    struct RecordingSink {
        statuses: Mutex<Vec<(EngineStatus, Option<String>)>>,
        errors: Mutex<Vec<Option<String>>>,
        replies: Mutex<Vec<ChatReply>>,
    }

    impl RecordingSink {
        fn statuses(&self) -> Vec<(EngineStatus, Option<String>)> {
            self.statuses.lock().unwrap().clone()
        }

        fn errors(&self) -> Vec<Option<String>> {
            self.errors.lock().unwrap().clone()
        }
    }

    impl EventSink for &'static RecordingSink {
        fn status_changed(
            &self,
            status: EngineStatus,
            model_name: Option<&str>,
            error: Option<&str>,
        ) {
            self.statuses
                .lock()
                .unwrap()
                .push((status, model_name.map(str::to_owned)));
            self.errors.lock().unwrap().push(error.map(str::to_owned));
        }

        fn replied(&self, reply: &ChatReply) {
            self.replies.lock().unwrap().push(reply.clone());
        }
    }

    fn sink() -> &'static RecordingSink {
        Box::leak(Box::new(RecordingSink::default()))
    }

    struct CountingExecutor(AtomicUsize);

    #[async_trait::async_trait]
    impl ToolExecutor for CountingExecutor {
        async fn execute(&self, tool_name: &str, _: &str) -> Result<String, String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(format!("{tool_name} completed"))
        }
    }

    struct FixedApproval(ApprovalDecision);

    #[async_trait::async_trait]
    impl ApprovalHandler for FixedApproval {
        async fn approve(&self, _: ApprovalRequest) -> ApprovalDecision {
            self.0
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "call-1".to_owned(),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
        }
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

    #[tokio::test]
    async fn a_successful_load_reports_loading_then_ready() {
        // The ordering is the feature: a UI that only ever sees the final
        // state has no way to put up a spinner for a load that can take
        // minutes.
        let sink = sink();
        let ed = Ed::with_sink(sink);

        let elapsed = ed
            .load_with("Test model", |_| async { Ok(Duration::from_secs(2)) })
            .await
            .expect("the closure succeeded");

        assert_eq!(elapsed, Duration::from_secs(2));
        assert_eq!(
            sink.statuses(),
            vec![
                (EngineStatus::Loading, Some("Test model".to_owned())),
                (EngineStatus::Ready, Some("Test model".to_owned())),
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_load_reports_loading_then_error_with_the_reason() {
        let sink = sink();
        let ed = Ed::with_sink(sink);

        let err = ed
            .load_with("Test model", |_| async {
                Err(InferenceError::Other {
                    reason: "no weights".to_owned(),
                })
            })
            .await
            .expect_err("the closure failed");

        assert!(err.to_string().contains("no weights"));
        assert_eq!(
            sink.statuses(),
            vec![
                (EngineStatus::Loading, Some("Test model".to_owned())),
                (EngineStatus::Error, Some("Test model".to_owned())),
            ]
        );
        assert_eq!(sink.errors(), vec![None, Some("no weights".to_owned())]);
    }

    #[tokio::test]
    async fn read_only_tools_run_without_an_approval_round_trip() {
        let executor = Arc::new(CountingExecutor(AtomicUsize::new(0)));
        let ed = Ed::with_agent(
            NoopSink,
            executor.clone(),
            Arc::new(FixedApproval(ApprovalDecision::Deny)),
            AgentConfig::default(),
        );
        ed.register_tool(AgentToolDefinition::read_only(
            "clock",
            "Read the clock",
            r#"{"type":"object"}"#,
        ))
        .await
        .unwrap();
        let tools = ed.tools.read().await.clone();

        let result = ed.execute_tool_call(&call("clock"), &tools).await;

        assert!(!result.is_error);
        assert_eq!(executor.0.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn denied_mutating_tools_never_reach_the_executor() {
        let executor = Arc::new(CountingExecutor(AtomicUsize::new(0)));
        let ed = Ed::with_agent(
            NoopSink,
            executor.clone(),
            Arc::new(FixedApproval(ApprovalDecision::Deny)),
            AgentConfig::default(),
        );
        ed.register_tool(AgentToolDefinition::mutating(
            "save_note",
            "Save a note",
            r#"{"type":"object"}"#,
        ))
        .await
        .unwrap();
        let tools = ed.tools.read().await.clone();

        let result = ed.execute_tool_call(&call("save_note"), &tools).await;

        assert!(result.is_error);
        assert!(result.content.contains("denied"));
        assert_eq!(executor.0.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn a_session_grant_applies_to_later_calls_of_the_same_tool() {
        let executor = Arc::new(CountingExecutor(AtomicUsize::new(0)));
        let ed = Ed::with_agent(
            NoopSink,
            executor.clone(),
            Arc::new(FixedApproval(ApprovalDecision::AllowForSession)),
            AgentConfig::default(),
        );
        ed.register_tool(AgentToolDefinition::mutating(
            "save_note",
            "Save a note",
            r#"{"type":"object"}"#,
        ))
        .await
        .unwrap();
        let tools = ed.tools.read().await.clone();

        ed.execute_tool_call(&call("save_note"), &tools).await;
        ed.execute_tool_call(&call("save_note"), &tools).await;

        assert_eq!(executor.0.load(Ordering::Relaxed), 2);
        assert!(ed.session_grants.lock().await.contains("save_note"));
    }

    #[tokio::test]
    async fn unknown_tools_return_an_error_result_instead_of_executing() {
        let executor = Arc::new(CountingExecutor(AtomicUsize::new(0)));
        let ed = Ed::with_agent(
            NoopSink,
            executor.clone(),
            Arc::new(FixedApproval(ApprovalDecision::AllowOnce)),
            AgentConfig::default(),
        );

        let result = ed
            .execute_tool_call(&call("missing"), &HashMap::new())
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("not registered"));
        assert_eq!(executor.0.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn tool_arguments_must_be_a_json_object() {
        let executor = Arc::new(CountingExecutor(AtomicUsize::new(0)));
        let ed = Ed::with_agent(
            NoopSink,
            executor.clone(),
            Arc::new(FixedApproval(ApprovalDecision::AllowOnce)),
            AgentConfig::default(),
        );
        ed.register_tool(AgentToolDefinition::read_only(
            "clock",
            "Read the clock",
            r#"{"type":"object"}"#,
        ))
        .await
        .unwrap();
        let tools = ed.tools.read().await.clone();
        let mut invalid = call("clock");
        invalid.arguments = "[]".to_owned();

        let result = ed.execute_tool_call(&invalid, &tools).await;

        assert!(result.is_error);
        assert!(result.content.contains("JSON object"));
        assert_eq!(executor.0.load(Ordering::Relaxed), 0);
    }
}
