//! Approvals answered by the webview.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use ed_agent::{ApprovalDecision, ApprovalHandler, ApprovalRequest};
use log::{error, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex};

use crate::events::{
    ApprovalRequestedPayload, ApprovalResolvedPayload, EVENT_CHAT_APPROVAL_REQUESTED,
    EVENT_CHAT_APPROVAL_RESOLVED,
};
use crate::scope::TurnScope;

/// How long to wait for the user before denying.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

/// An [`ApprovalHandler`] that asks the webview.
///
/// For each request it emits `chat_approval_requested`, waits for the webview
/// to call `chat_respond_approval` (or [`respond`](TauriApprovals::respond)),
/// and emits `chat_approval_resolved` when the wait ends, however it ends.
/// A timeout, a closed window or [`cancel_all`](TauriApprovals::cancel_all)
/// all mean *deny*.
type Emit = Box<dyn Fn(&str, serde_json::Value) -> Result<(), String> + Send + Sync>;

pub struct TauriApprovals {
    emit: Emit,
    scope: TurnScope,
    timeout: Duration,
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
}

impl TauriApprovals {
    pub fn new(app: AppHandle, timeout: Duration) -> Self {
        Self::scoped(app, TurnScope::default(), timeout)
    }

    pub(crate) fn scoped(app: AppHandle, scope: TurnScope, timeout: Duration) -> Self {
        let emit: Emit =
            Box::new(move |event, payload| app.emit(event, payload).map_err(|err| err.to_string()));
        Self::with_emitter(emit, scope, timeout)
    }

    fn with_emitter(emit: Emit, scope: TurnScope, timeout: Duration) -> Self {
        Self {
            emit,
            scope,
            timeout,
            pending: Mutex::new(HashMap::new()),
        }
    }

    fn emit(&self, event: &str, payload: impl Serialize) -> Result<(), String> {
        let value = serde_json::to_value(payload).map_err(|err| err.to_string())?;
        (self.emit)(event, value)
    }

    /// Answer a pending request. Errors if it already timed out or was
    /// answered.
    pub async fn respond(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), String> {
        let gone = || "This approval request is no longer pending.".to_string();
        let sender = self
            .pending
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(gone)?;
        sender.send(decision).map_err(|_| gone())
    }

    /// Deny everything that is waiting. Use it when the conversation the
    /// requests belong to goes away.
    pub async fn cancel_all(&self) {
        let pending: Vec<_> = self.pending.lock().await.drain().collect();
        for (_, sender) in pending {
            // Each waiting `approve` call emits its own resolved event once it
            // sees the denial, so every request closes exactly once.
            let _ = sender.send(ApprovalDecision::Deny);
        }
    }
}

#[async_trait]
impl ApprovalHandler for TauriApprovals {
    async fn approve(&self, request: ApprovalRequest) -> ApprovalDecision {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), sender);

        let (session, _) = self.scope.get();
        let payload = ApprovalRequestedPayload {
            request_id: request_id.clone(),
            session,
            call: request.call,
            risk: request.risk,
        };
        if let Err(err) = self.emit(EVENT_CHAT_APPROVAL_REQUESTED, payload) {
            self.pending.lock().await.remove(&request_id);
            error!("ed-tauri: could not request approval: {err}");
            return ApprovalDecision::Deny;
        }

        let decision = match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => ApprovalDecision::Deny,
            Err(_) => {
                warn!("ed-tauri: approval {request_id} timed out; denying");
                ApprovalDecision::Deny
            }
        };
        self.pending.lock().await.remove(&request_id);
        if let Err(err) = self.emit(
            EVENT_CHAT_APPROVAL_RESOLVED,
            ApprovalResolvedPayload { request_id },
        ) {
            error!("ed-tauri: could not close approval: {err}");
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ed_agent::{ToolCall, ToolRisk};

    use super::*;

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            call: ToolCall {
                id: "1".into(),
                name: "send_message".into(),
                arguments: "{}".into(),
            },
            risk: ToolRisk::Mutating,
        }
    }

    fn approvals(timeout: Duration) -> Arc<TauriApprovals> {
        Arc::new(TauriApprovals::with_emitter(
            Box::new(|_, _| Ok(())),
            TurnScope::default(),
            timeout,
        ))
    }

    async fn pending_id(approvals: &TauriApprovals) -> String {
        loop {
            if let Some(id) = approvals.pending.lock().await.keys().next().cloned() {
                return id;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn the_webview_can_allow() {
        let approvals = approvals(Duration::from_secs(5));
        let waiting = tokio::spawn({
            let approvals = approvals.clone();
            async move { approvals.approve(request()).await }
        });
        let id = pending_id(&approvals).await;

        approvals
            .respond(&id, ApprovalDecision::AllowForSession)
            .await
            .unwrap();

        assert_eq!(waiting.await.unwrap(), ApprovalDecision::AllowForSession);
        assert!(approvals
            .respond(&id, ApprovalDecision::Deny)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cancel_all_denies_everything_pending() {
        let approvals = approvals(Duration::from_secs(5));
        let waiting = tokio::spawn({
            let approvals = approvals.clone();
            async move { approvals.approve(request()).await }
        });
        pending_id(&approvals).await;

        approvals.cancel_all().await;

        assert_eq!(waiting.await.unwrap(), ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn an_unanswered_request_times_out_as_a_denial() {
        let approvals = approvals(Duration::from_millis(20));
        assert_eq!(approvals.approve(request()).await, ApprovalDecision::Deny);
        assert!(approvals.pending.lock().await.is_empty());
    }
}
