//! The auth seam for products with a sign-in of their own.
//!
//! Without a provider the agent only knows terminal auth: a client runs `<agent> --setup`
//! and the key lands in the `env` file. A product whose credentials come from somewhere else
//! (a browser sign-in, a stored session) supplies an [`AuthProvider`] from
//! [`Profile::auth`](crate::Profile::auth) and the agent defers to it.

use agent_client_protocol::schema::v1::{AuthMethod, ClientCapabilities, RequestId};
use agent_client_protocol::{Client, ConnectionTo};
use async_trait::async_trait;

use crate::llm::LlmConfig;

/// What `authenticate` may use to reach the user.
pub struct AuthCtx<'a> {
    /// The client connection, for a URL-mode elicitation or a notification.
    pub connection: &'a ConnectionTo<Client>,
    /// The `authenticate` request's own id. No session exists yet, so it scopes an
    /// elicitation sent while the request is open.
    pub request_id: RequestId,
    /// What the client advertised in `initialize`, e.g. `elicitation.url`.
    pub client_caps: &'a ClientCapabilities,
}

impl AuthCtx<'_> {
    /// Whether the client can show a URL-mode elicitation.
    pub fn client_offers_url_elicitation(&self) -> bool {
        self.client_caps
            .elicitation
            .as_ref()
            .is_some_and(|caps| caps.url.is_some())
    }
}

#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// The method advertised in `initialize`, whatever the client's terminal capability,
    /// e.g. `AuthMethod::Agent(AuthMethodAgent::new("sigit", "Sign in to siGit Code"))`.
    fn method(&self) -> AuthMethod;

    /// The stored session as an endpoint, a bearer and a model, or `None` when signed out.
    /// Read before each session request, so a sign-in finished in this process or another
    /// is picked up without a restart.
    fn llm_config(&self) -> Option<LlmConfig>;

    /// Run the sign-in. An existing session passes. An `Err` is shown to the user as is.
    async fn authenticate(&self, ctx: AuthCtx<'_>) -> agent_client_protocol::Result<()>;

    /// Forget the stored session.
    async fn logout(&self) -> anyhow::Result<()>;
}
