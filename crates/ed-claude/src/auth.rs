//! [`ClaudeAuth`]: the Anthropic API key as an `ed-acp` [`AuthProvider`].

use std::path::PathBuf;

use agent_client_protocol::schema::v1::{AuthMethod, AuthMethodAgent, ErrorCode};
use async_trait::async_trait;
use ed_acp::{AuthCtx, AuthProvider, LlmConfig, Provider};

use crate::credentials;

/// The id of the auth method, as the client sends it in `authenticate`.
pub const METHOD_ID: &str = "anthropic-api-key";

/// Anthropic's OpenAI-compatible endpoint.
pub const BASE_URL: &str = "https://api.anthropic.com/v1";

/// The model a session starts on when none is stored.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";

/// The models offered in the picker. Fixed here because Anthropic's `/models` may want an
/// `x-api-key` header the compatibility layer doesn't take from a bearer.
pub const MODELS: [&str; 3] = [DEFAULT_MODEL, "claude-opus-5-5", "claude-haiku-5-5"];

/// The message `authenticate` fails with when no key is stored. A host UI that sees it asks the
/// user for a key and stores it with [`credentials::store_api_key`]; `ed-acp-tauri` supports
/// only URL elicitation, so the key can't be collected through the protocol.
pub const API_KEY_NEEDED: &str = "Add your Anthropic API key to use Claude.";

#[derive(Debug, Clone, Default)]
pub struct ClaudeAuth {
    base_url: Option<String>,
    dir: Option<PathBuf>,
}

impl ClaudeAuth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Talk to another endpoint, e.g. a proxy or a test server.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into().trim_end_matches('/').to_string());
        self
    }

    /// Read the key from `dir` instead of the process-wide [`credentials::dir`].
    pub fn with_dir(mut self, dir: PathBuf) -> Self {
        self.dir = Some(dir);
        self
    }

    fn stored(&self) -> Option<credentials::Stored> {
        match &self.dir {
            Some(dir) => credentials::load_from(dir),
            None => credentials::load(),
        }
    }
}

#[async_trait]
impl AuthProvider for ClaudeAuth {
    fn method(&self) -> AuthMethod {
        AuthMethod::Agent(
            AuthMethodAgent::new(METHOD_ID, "Use an Anthropic API key")
                .description("Claude through the Anthropic API, billed to your API key."),
        )
    }

    fn llm_config(&self) -> Option<LlmConfig> {
        let stored = self.stored()?;
        let model = stored
            .model
            .filter(|m| MODELS.contains(&m.as_str()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Some(LlmConfig {
            provider: Provider::OpenAi,
            base_url: self
                .base_url
                .clone()
                .unwrap_or_else(|| BASE_URL.to_string()),
            api_key: Some(stored.api_key),
            model,
        })
    }

    fn models(&self) -> Option<Vec<String>> {
        Some(MODELS.iter().map(|m| m.to_string()).collect())
    }

    async fn authenticate(&self, _ctx: AuthCtx<'_>) -> agent_client_protocol::Result<()> {
        if self.stored().is_some() {
            return Ok(());
        }
        Err(agent_client_protocol::Error::new(
            ErrorCode::InternalError.into(),
            API_KEY_NEEDED.to_string(),
        ))
    }

    async fn logout(&self) -> anyhow::Result<()> {
        let removed = match &self.dir {
            Some(dir) => credentials::clear_in(dir),
            None => credentials::clear(),
        };
        tracing::info!("logout: stored Anthropic key removed: {removed}");
        Ok(())
    }
}
