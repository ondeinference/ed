//! Claude as an `ed-acp` agent, for hosts that run agents in process.
//!
//! Anthropic serves the OpenAI chat-completions API at `https://api.anthropic.com/v1/`, with
//! streaming and tool calls, so `ed-acp`'s own client, workspace tools, permissions and
//! `fs/*` work unchanged. [`ClaudeAuth`] supplies the endpoint, the user's API key and a model
//! through [`AuthProvider::llm_config`]. The environment is never read: the key and model come
//! from the file [`credentials`] keeps, so nothing collides with `OPENAI_*` or `ONDE_*`.
//!
//! This is the API-key mode only. A Claude.ai subscription goes through Anthropic's own
//! `claude` CLI, which a host runs as an external agent instead.

pub mod auth;
pub mod credentials;
pub mod profile;

pub use auth::{API_KEY_NEEDED, ClaudeAuth, DEFAULT_MODEL, MODELS};
pub use profile::{Claude, INFO};
