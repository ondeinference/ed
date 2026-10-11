//! Several conversations over one Ed.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ed_agent::{
    AgentConfig, AgentError, AgentReply, ApprovalDecision, ChatMessage, Ed, RejectingExecutor,
    SessionError, SessionMeta, SessionStore, ToolExecutor,
};
use log::{error, info, warn};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

use crate::approvals::{TauriApprovals, DEFAULT_APPROVAL_TIMEOUT};
use crate::events::{SubmitReplyPayload, EVENT_CHAT_REPLY};
use crate::scope::TurnScope;
use crate::sink::TauriSink;

#[derive(Default)]
struct State {
    active: Option<String>,
    /// Histories of sessions that were switched away from, so switching back
    /// doesn't hit the store.
    cache: HashMap<String, Vec<ChatMessage>>,
}

struct Shared {
    app: AppHandle,
    ed: Arc<Ed<TauriSink>>,
    store: Arc<dyn SessionStore>,
    approvals: Arc<TauriApprovals>,
    scope: TurnScope,
    /// Bumped by [`EdSessions::reset`]. A turn captures it at dispatch and
    /// refuses to save or emit if it changed by completion, so a reply that
    /// finishes after logout or an account switch can't land in the next
    /// account's storage.
    generation: AtomicU64,
    state: Mutex<State>,
    error_message: Option<String>,
    new_id: Box<dyn Fn() -> String + Send + Sync>,
}

/// Multi-session chat state: one [`Ed`], many conversations.
///
/// Register it with `app.manage(sessions)` and the `chat_*_session` commands.
/// It owns which session is live in the engine, an in-memory cache, the
/// generation fence, and approvals; persistence stays with the host's
/// [`SessionStore`].
///
/// Cheap to clone.
#[derive(Clone)]
pub struct EdSessions {
    shared: Arc<Shared>,
}

/// Configuration for [`EdSessions`].
pub struct EdSessionsBuilder {
    app: AppHandle,
    store: Arc<dyn SessionStore>,
    executor: Arc<dyn ToolExecutor>,
    config: AgentConfig,
    onde_app_id: Option<String>,
    approval_timeout: Duration,
    error_message: Option<String>,
    new_id: Box<dyn Fn() -> String + Send + Sync>,
}

impl EdSessionsBuilder {
    /// Run tools through `executor`. Defaults to rejecting every tool.
    pub fn executor(mut self, executor: Arc<dyn ToolExecutor>) -> Self {
        self.executor = executor;
        self
    }

    pub fn config(mut self, config: AgentConfig) -> Self {
        self.config = config;
        self
    }

    /// Associate telemetry with an Onde app id.
    pub fn onde_app_id(mut self, id: impl Into<String>) -> Self {
        self.onde_app_id = Some(id.into());
        self
    }

    /// How long an approval waits for the user. Default 120 seconds.
    pub fn approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }

    /// Show this to the user instead of the error text when a turn fails. The
    /// real error is still logged.
    pub fn error_message(mut self, message: impl Into<String>) -> Self {
        self.error_message = Some(message.into());
        self
    }

    /// How [`EdSessions::new_session`] names a session. Defaults to a random UUID. A store that
    /// treats some ids as ephemeral (by prefix, say) can mint matching ones here.
    pub fn id_generator(mut self, generator: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.new_id = Box::new(generator);
        self
    }

    pub fn build(self) -> EdSessions {
        let scope = TurnScope::default();
        let approvals = Arc::new(TauriApprovals::scoped(
            self.app.clone(),
            scope.clone(),
            self.approval_timeout,
        ));
        let sink = TauriSink::scoped(self.app.clone(), scope.clone(), false);
        let ed = Ed::with_agent_and_app_id(
            sink,
            self.executor,
            approvals.clone(),
            self.config,
            self.onde_app_id,
        );
        EdSessions {
            shared: Arc::new(Shared {
                app: self.app,
                ed: Arc::new(ed),
                store: self.store,
                approvals,
                scope,
                generation: AtomicU64::new(0),
                state: Mutex::new(State::default()),
                error_message: self.error_message,
                new_id: self.new_id,
            }),
        }
    }
}

impl EdSessions {
    pub fn builder(app: AppHandle, store: impl SessionStore) -> EdSessionsBuilder {
        EdSessionsBuilder {
            app,
            store: Arc::new(store),
            executor: Arc::new(RejectingExecutor),
            config: AgentConfig::default(),
            onde_app_id: None,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            error_message: None,
            new_id: Box::new(|| uuid::Uuid::new_v4().to_string()),
        }
    }

    /// The agent, for loading a model and anything else Ed exposes.
    pub fn ed(&self) -> &Arc<Ed<TauriSink>> {
        &self.shared.ed
    }

    pub fn approvals(&self) -> &Arc<TauriApprovals> {
        &self.shared.approvals
    }

    /// The current generation. Capture it before slow work and compare with
    /// [`is_current`](EdSessions::is_current) afterwards.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::SeqCst)
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.generation() == generation
    }

    /// The id of the session live in the engine, if any.
    pub async fn active(&self) -> Option<String> {
        self.shared.state.lock().await.active.clone()
    }

    /// Stored sessions, most recently updated first.
    pub async fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
        self.shared.store.list().await
    }

    /// Make `session` the live conversation.
    ///
    /// The outgoing session's history is kept in memory; the incoming one
    /// comes from memory, then the store. An unknown id starts empty.
    pub async fn activate(&self, session: &str) {
        let shared = &self.shared;
        let mut state = shared.state.lock().await;
        if state.active.as_deref() == Some(session) {
            return;
        }
        if let Some(previous) = state.active.take() {
            let live = shared.ed.history().await;
            state.cache.insert(previous, live);
        }
        let stored = match state.cache.remove(session) {
            Some(messages) => messages,
            None => match shared.store.load(session).await {
                Ok(messages) => messages.unwrap_or_default(),
                Err(err) => {
                    error!("ed-tauri: could not load session {session}: {err}");
                    Vec::new()
                }
            },
        };
        shared.ed.restore_history(stored).await;
        state.active = Some(session.to_owned());
    }

    /// Start an empty session and make it live. Returns its id. The store
    /// hears about it when the first turn completes.
    pub async fn new_session(&self) -> String {
        let id = (self.shared.new_id)();
        self.activate(&id).await;
        id
    }

    /// The messages of `session`, activating it.
    pub async fn history(&self, session: &str) -> Vec<ChatMessage> {
        self.activate(session).await;
        self.shared.ed.history().await
    }

    /// The messages of `session` without switching to it: the live history if it is the active
    /// session, else the cache, else the store. For rendering a session that is not on screen,
    /// without disturbing a turn that is running in another.
    pub async fn peek(&self, session: &str) -> Vec<ChatMessage> {
        let shared = &self.shared;
        let state = shared.state.lock().await;
        if state.active.as_deref() == Some(session) {
            return shared.ed.history().await;
        }
        if let Some(cached) = state.cache.get(session) {
            return cached.clone();
        }
        drop(state);
        match shared.store.load(session).await {
            Ok(messages) => messages.unwrap_or_default(),
            Err(err) => {
                error!("ed-tauri: could not load session {session}: {err}");
                Vec::new()
            }
        }
    }

    /// Delete a session from the store and memory. If it was live, the engine
    /// is cleared and no session is live until the next activate.
    pub async fn delete(&self, session: &str) -> Result<(), SessionError> {
        let shared = &self.shared;
        let result = shared.store.delete(session).await;
        let mut state = shared.state.lock().await;
        state.cache.remove(session);
        if state.active.as_deref() == Some(session) {
            state.active = None;
            shared.ed.clear_history().await;
        }
        result
    }

    /// Drop all in-memory state and fence off in-flight turns. For logout and
    /// account switches: the next account must never see this one's chats.
    /// Stored sessions are left alone.
    pub async fn reset(&self) {
        let shared = &self.shared;
        shared.generation.fetch_add(1, Ordering::SeqCst);
        shared.ed.cancel();
        shared.approvals.cancel_all().await;
        let mut state = shared.state.lock().await;
        state.cache.clear();
        state.active = None;
        shared.ed.clear_history().await;
    }

    /// Save the live history under `session`, unless `generation` is stale.
    pub async fn save(&self, session: &str, generation: u64) {
        let shared = &self.shared;
        let live = shared.ed.history().await;
        let mut state = shared.state.lock().await;
        if !self.is_current(generation) {
            return;
        }
        if let Err(err) = shared.store.save(session, &live).await {
            error!("ed-tauri: could not save session {session}: {err}");
        }
        state.cache.insert(session.to_owned(), live);
    }

    /// Run one agent turn in `session` and deliver the result as a
    /// `chat_reply` event.
    ///
    /// Returns as soon as the session is active and the turn is spawned. A
    /// long-pending invoke can be dropped by a mobile webview, so the answer
    /// travels by event: `{id, session, reply, duration, error}`.
    pub async fn submit(&self, session: &str, id: &str, message: impl Into<String>) {
        let message = message.into();
        self.submit_with(session, id, move |ed| async move { ed.run(message).await })
            .await;
    }

    /// Like [`submit`](EdSessions::submit), with the turn supplied by the
    /// host: rewrite the message, or recover and retry on a known engine
    /// failure. The closure runs on the spawned task.
    pub async fn submit_with<F, Fut>(&self, session: &str, id: &str, turn: F)
    where
        F: FnOnce(Arc<Ed<TauriSink>>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<AgentReply, AgentError>> + Send + 'static,
    {
        self.activate(session).await;
        let generation = self.generation();
        let this = self.clone();
        let session = session.to_owned();
        let id = id.to_owned();
        info!("ed-tauri: dispatch session={session} id={id}");

        tokio::spawn(async move {
            this.shared.scope.set(&session, &id);
            let result = turn(this.shared.ed.clone()).await;
            this.shared.scope.clear();

            if !this.is_current(generation) {
                info!("ed-tauri: reply dropped, the account context changed (session={session})");
                return;
            }
            this.save(&session, generation).await;
            if !this.is_current(generation) {
                return;
            }

            let payload = match result {
                Ok(reply) => SubmitReplyPayload {
                    id,
                    session,
                    reply: Some(reply.text),
                    duration: Some(reply.duration),
                    error: None,
                },
                Err(err) => {
                    error!("ed-tauri: turn failed: {err}");
                    SubmitReplyPayload {
                        id,
                        session,
                        reply: None,
                        duration: None,
                        error: Some(
                            this.shared
                                .error_message
                                .clone()
                                .unwrap_or_else(|| err.to_string()),
                        ),
                    }
                }
            };
            if let Err(err) = this.shared.app.emit(EVENT_CHAT_REPLY, payload) {
                warn!("ed-tauri: could not emit chat_reply: {err}");
            }
        });
    }

    /// Answer a pending approval.
    pub async fn respond_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), String> {
        self.shared.approvals.respond(request_id, decision).await
    }
}
