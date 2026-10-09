//! Streaming client for OpenAI-compatible `/chat/completions` endpoints (Onde Cloud by default,
//! or any other with the `OPENAI_*` variables): text and reasoning deltas, tool-call
//! accumulation, token usage, and `GET /models`.
//!
//! Superseded by `onde-backend` once it exists (Onde Agent Platform §5.2); the agent depends
//! only on this module's public types so that swap stays local.

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

/// Onde Cloud's OpenAI-compatible endpoint.
pub const DEFAULT_BASE_URL: &str = "https://cloud.ondeinference.com/v1";
/// The model used when neither the profile nor the environment names one.
pub const DEFAULT_MODEL: &str = "onde-kkk";
/// The credential every Onde agent reads: `app-id:app-secret`.
pub const API_KEY_VAR: &str = "ONDE_API_KEY";
/// Base URL of a generic OpenAI API compatible endpoint when `OPENAI_BASE_URL` is unset.
pub const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Model for a generic endpoint when neither `<PREFIX>_MODEL` nor `OPENAI_MODEL` is set.
pub const OPENAI_DEFAULT_MODEL: &str = "gpt-4o-mini";

/// Where completions come from. Both speak the OpenAI chat completions API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Onde Cloud: `ONDE_API_KEY` (`app-id:app-secret`), `<PREFIX>_BASE_URL` or
    /// `ONDE_BASE_URL` for a local or staging deployment.
    Onde,
    /// Any OpenAI API compatible endpoint: `OPENAI_BASE_URL`, `OPENAI_API_KEY`, `OPENAI_MODEL`.
    OpenAi,
}

impl Provider {
    /// `onde` or `openai`, as written to `<PREFIX>_PROVIDER`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Onde => "onde",
            Self::OpenAi => "openai",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "onde" | "onde-cloud" | "ondeinference" => Some(Self::Onde),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    /// The variable holding this provider's key.
    pub fn key_var(self) -> &'static str {
        match self {
            Self::Onde => API_KEY_VAR,
            Self::OpenAi => "OPENAI_API_KEY",
        }
    }

    /// Name shown to people, e.g. in setup and errors.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Onde => "Onde Inference",
            Self::OpenAi => "the OpenAI API compatible endpoint",
        }
    }
}

/// Where an agent's settings come from: `<PREFIX>_BASE_URL`, `<PREFIX>_MODEL` and
/// `<PREFIX>_DATA_DIR` in the environment, then `<config dir>/<dir_name>/env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmEnv {
    /// Environment prefix, e.g. `SPLITFIRE`.
    pub prefix: &'static str,
    /// Directory name under the platform config dir, e.g. `splitfire-agent`.
    pub dir_name: &'static str,
    /// Model when `<PREFIX>_MODEL` is unset.
    pub default_model: &'static str,
}

impl LlmEnv {
    pub fn var(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.prefix)
    }

    /// Directory for the stored `env` file: `~/.config/<dir_name>` on Linux,
    /// `~/Library/Application Support/<dir_name>` on macOS, `%APPDATA%\\<dir_name>` on
    /// Windows. `<PREFIX>_DATA_DIR` overrides it (tests). `None` without a home directory.
    pub fn config_dir(&self) -> Option<std::path::PathBuf> {
        if let Some(dir) = std::env::var_os(self.var("DATA_DIR")).filter(|v| !v.is_empty()) {
            return Some(dir.into());
        }
        directories::BaseDirs::new().map(|dirs| {
            #[cfg(target_os = "linux")]
            let base = dirs.config_dir();
            #[cfg(not(target_os = "linux"))]
            let base = dirs.data_dir();
            base.join(self.dir_name)
        })
    }

    /// The stored `env` file `--setup` writes and `logout` removes.
    pub fn config_file(&self) -> Option<std::path::PathBuf> {
        self.config_dir().map(|d| d.join("env"))
    }

    /// Where the stored `env` file is looked for, most preferred first: [`Self::config_file`],
    /// then `~/.config/<dir_name>/env`, where agents on macOS kept it before they moved to the
    /// platform directory. Only the first is used when `<PREFIX>_DATA_DIR` is set.
    pub fn config_file_candidates(&self) -> Vec<std::path::PathBuf> {
        let mut paths: Vec<std::path::PathBuf> = self.config_file().into_iter().collect();
        let overridden = std::env::var_os(self.var("DATA_DIR")).is_some_and(|v| !v.is_empty());
        #[cfg(not(target_os = "windows"))]
        if !overridden && let Some(dirs) = directories::BaseDirs::new() {
            let legacy = dirs
                .home_dir()
                .join(".config")
                .join(self.dir_name)
                .join("env");
            if !paths.contains(&legacy) {
                paths.push(legacy);
            }
        }
        #[cfg(target_os = "windows")]
        let _ = overridden;
        paths
    }

    /// Where durable state (sessions, audio scratch) lives. Same directory as the `env` file.
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.config_dir()
            .unwrap_or_else(|| std::env::temp_dir().join(self.dir_name))
    }

    fn load_file_vars(&self) -> std::collections::HashMap<String, String> {
        let mut vars = std::collections::HashMap::new();
        let content = self
            .config_file_candidates()
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok());
        let Some(content) = content else {
            return vars;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            if let Some((k, v)) = line.split_once('=') {
                let mut v = v.trim();
                if v.len() >= 2
                    && ((v.starts_with('"') && v.ends_with('"'))
                        || (v.starts_with('\'') && v.ends_with('\'')))
                {
                    v = &v[1..v.len() - 1];
                }
                vars.insert(k.trim().to_string(), v.to_string());
            }
        }
        vars
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmConfig {
    pub provider: Provider,
    pub base_url: String,
    /// Sent as the bearer token. For Onde Cloud, `app-id:app-secret`.
    pub api_key: Option<String>,
    pub model: String,
}

impl LlmConfig {
    /// Settings from the environment, falling back to the stored `env` file.
    ///
    /// The provider is `<PREFIX>_PROVIDER` (`onde` or `openai`) when set, else Onde Cloud
    /// whenever `ONDE_API_KEY` is set, else a generic endpoint when `OPENAI_API_KEY` is, else
    /// Onde Cloud. Onde Cloud reads only `ONDE_API_KEY`, and its own URL unless
    /// `<PREFIX>_BASE_URL` or `ONDE_BASE_URL` names another deployment, so stray `OPENAI_*`
    /// variables can't redirect it or send it the wrong key. A generic endpoint reads
    /// `OPENAI_BASE_URL`, `OPENAI_API_KEY` and `OPENAI_MODEL`. `<PREFIX>_MODEL` picks the
    /// model for either.
    ///
    /// Empty values are ignored: GUI launchers (e.g. Zed via launchd) often export
    /// variables set to "", which must not mask the config file.
    pub fn from_env(env: &LlmEnv) -> Self {
        let file_vars = env.load_file_vars();
        Self::from_lookup(env, |name| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| file_vars.get(name).cloned())
        })
    }

    /// Build a config from an arbitrary variable lookup (tests, setup).
    pub fn from_lookup(env: &LlmEnv, lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| lookup(name).filter(|v| !v.is_empty());
        let provider = match get(&env.var("PROVIDER")) {
            Some(name) => Provider::parse(&name).unwrap_or_else(|| {
                tracing::warn!("unknown {} {name:?}; using onde", env.var("PROVIDER"));
                Provider::Onde
            }),
            None if get(API_KEY_VAR).is_some() => Provider::Onde,
            None if get("OPENAI_API_KEY").is_some() => Provider::OpenAi,
            None => Provider::Onde,
        };
        let (base_url, model) = match provider {
            Provider::Onde => (
                get(&env.var("BASE_URL"))
                    .or_else(|| get("ONDE_BASE_URL"))
                    .unwrap_or_else(|| DEFAULT_BASE_URL.into()),
                get(&env.var("MODEL")).unwrap_or_else(|| env.default_model.into()),
            ),
            Provider::OpenAi => (
                get("OPENAI_BASE_URL").unwrap_or_else(|| OPENAI_DEFAULT_BASE_URL.into()),
                get(&env.var("MODEL"))
                    .or_else(|| get("OPENAI_MODEL"))
                    .unwrap_or_else(|| OPENAI_DEFAULT_MODEL.into()),
            ),
        };
        Self {
            provider,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: get(provider.key_var()),
            model,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: LlmEnv = LlmEnv {
        prefix: "SPLITFIRE",
        dir_name: "splitfire-agent",
        default_model: DEFAULT_MODEL,
    };

    fn config(vars: &[(&str, &str)]) -> LlmConfig {
        LlmConfig::from_lookup(&ENV, |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn defaults_point_at_onde_cloud() {
        let c = config(&[("ONDE_API_KEY", "app:secret")]);
        assert_eq!(c.base_url, "https://cloud.ondeinference.com/v1");
        assert_eq!(c.api_key.as_deref(), Some("app:secret"));
        assert_eq!(c.model, "onde-kkk");
        assert_eq!(config(&[]).api_key, None);
        assert_eq!(config(&[("ONDE_API_KEY", "")]).api_key, None);
    }

    #[test]
    fn base_url_and_model_overrides_apply() {
        let c = config(&[
            ("ONDE_API_KEY", "app:secret"),
            ("SPLITFIRE_MODEL", "onde-prism"),
            ("SPLITFIRE_BASE_URL", "http://x/v1/"),
        ]);
        assert_eq!(
            (c.base_url.as_str(), c.model.as_str()),
            ("http://x/v1", "onde-prism")
        );
    }

    #[test]
    fn onde_base_url_points_onde_at_another_deployment() {
        let c = config(&[
            ("ONDE_API_KEY", "app:secret"),
            ("ONDE_BASE_URL", "http://localhost:8090/v1/"),
        ]);
        assert_eq!(c.base_url, "http://localhost:8090/v1");
        let both = config(&[
            ("ONDE_BASE_URL", "http://a/v1"),
            ("SPLITFIRE_BASE_URL", "http://b/v1"),
        ]);
        assert_eq!(both.base_url, "http://b/v1");
    }

    #[test]
    fn openai_key_selects_a_generic_endpoint() {
        let c = config(&[
            ("OPENAI_API_KEY", "sk"),
            ("OPENAI_MODEL", "my-model"),
            ("OPENAI_BASE_URL", "http://x/v1/"),
            ("ONDE_BASE_URL", "http://onde/v1"),
        ]);
        assert_eq!(
            (
                c.provider,
                c.base_url.as_str(),
                c.api_key.as_deref(),
                c.model.as_str()
            ),
            (Provider::OpenAi, "http://x/v1", Some("sk"), "my-model")
        );
        let plain = config(&[("OPENAI_API_KEY", "sk")]);
        assert_eq!(
            (plain.base_url.as_str(), plain.model.as_str()),
            ("https://api.openai.com/v1", "gpt-4o-mini")
        );
        assert_eq!(config(&[]).provider, Provider::Onde);
    }

    #[test]
    fn onde_wins_over_openai_variables() {
        let c = config(&[
            ("ONDE_API_KEY", "app:secret"),
            ("OPENAI_API_KEY", "sk-other"),
            ("OPENAI_BASE_URL", "http://elsewhere/v1"),
            ("OPENAI_MODEL", "gpt-x"),
        ]);
        assert_eq!(c.provider, Provider::Onde);
        assert_eq!(c.api_key.as_deref(), Some("app:secret"));
        assert_eq!(c.base_url, "https://cloud.ondeinference.com/v1");
        assert_eq!(c.model, "onde-kkk");
    }

    #[test]
    fn explicit_provider_wins_over_detected_keys() {
        let c = config(&[
            ("ONDE_API_KEY", "app:secret"),
            ("OPENAI_API_KEY", "sk"),
            ("SPLITFIRE_PROVIDER", "openai"),
            ("SPLITFIRE_MODEL", "m"),
        ]);
        assert_eq!(
            (c.provider, c.api_key.as_deref(), c.model.as_str()),
            (Provider::OpenAi, Some("sk"), "m")
        );
        let onde = config(&[("SPLITFIRE_PROVIDER", "onde"), ("OPENAI_API_KEY", "sk")]);
        assert_eq!((onde.provider, onde.api_key), (Provider::Onde, None));
    }

    /// One test touches the process environment, so it can't race another.
    #[test]
    fn key_from_env_then_file_ignoring_empty_env() {
        let dir = std::env::temp_dir().join(format!("sf-llm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("env"),
            "# comment\nexport ONDE_API_KEY=\"file:key\"\nSPLITFIRE_MODEL=m1\n",
        )
        .unwrap();
        // SAFETY: only this test mutates these variables.
        unsafe {
            std::env::set_var("SPLITFIRE_DATA_DIR", &dir);
            std::env::set_var("ONDE_API_KEY", "");
        }
        assert_eq!(ENV.config_file_candidates(), vec![dir.join("env")]);
        let c = LlmConfig::from_env(&ENV);
        assert_eq!(c.api_key.as_deref(), Some("file:key"));
        assert_eq!(c.model, "m1");
        unsafe { std::env::set_var("ONDE_API_KEY", "env:key") };
        assert_eq!(
            LlmConfig::from_env(&ENV).api_key.as_deref(),
            Some("env:key")
        );
        unsafe {
            std::env::remove_var("ONDE_API_KEY");
            std::env::remove_var("SPLITFIRE_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn usage_tolerates_missing_total() {
        let chunk: Chunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
        )
        .unwrap();
        assert_eq!(chunk.usage.unwrap().used(), 10);
        let full = Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 5,
        };
        assert_eq!(full.used(), 5);
    }

    #[test]
    fn parses_models_response() {
        let body = r#"{
            "object": "list",
            "data": [
                {"id": "onde-balanced", "object": "model", "created": 1700000000, "owned_by": "onde"},
                {"id": "custom-model"}
            ]
        }"#;
        let parsed: ModelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.data.len(), 2);
        assert_eq!(parsed.data[0].owned_by.as_deref(), Some("onde"));
        assert_eq!(parsed.data[1].owned_by, None);
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// Provider extras that must be echoed back (e.g. Gemini's `thought_signature`).
    pub extra_content: Option<Value>,
}

#[derive(Debug, Default)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCallRequest>,
    pub finish_reason: Option<String>,
    /// Token usage, when the endpoint reports it (`stream_options.include_usage`).
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl Usage {
    /// Tokens in the context after this completion. Some servers leave `total_tokens` at 0.
    pub fn used(&self) -> u64 {
        self.total_tokens
            .max(self.prompt_tokens + self.completion_tokens)
    }
}

impl Completion {
    /// The assistant message to append to the conversation history.
    pub fn to_message(&self) -> Value {
        let mut msg = json!({ "role": "assistant", "content": self.content });
        if !self.tool_calls.is_empty() {
            msg["tool_calls"] = self
                .tool_calls
                .iter()
                .map(|tc| {
                    let mut call = json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments },
                    });
                    if let Some(extra) = &tc.extra_content {
                        call["extra_content"] = extra.clone();
                    }
                    call
                })
                .collect();
        }
        msg
    }
}

/// Streamed pieces surfaced to the caller as they arrive.
pub enum Delta<'a> {
    Text(&'a str),
    Reasoning(&'a str),
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: ChunkDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    content: Option<String>,
    // Non-standard but common (DeepSeek, vLLM, OpenRouter, llama.cpp).
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: usize,
    id: Option<String>,
    function: Option<FunctionDelta>,
    extra_content: Option<Value>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

/// A model from `GET /v1/models`. Extra fields in the response are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Debug, Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
        }
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Whether an API key is configured.
    pub fn has_api_key(&self) -> bool {
        self.config.api_key.is_some()
    }

    /// Verify the configured key with a minimal chat completion.
    /// Returns `Err` when no key is set, the key is rejected, or the endpoint is unreachable.
    pub async fn check_auth(&self) -> Result<()> {
        let provider = self.config.provider;
        let Some(key) = &self.config.api_key else {
            bail!("no {} configured", provider.key_var());
        };
        let body = json!({
            "model": self.config.model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
            "stream": false,
        });
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.config.base_url))
            .json(&body)
            .bearer_auth(key)
            .send()
            .await
            .with_context(|| format!("reaching {}", provider.display_name()))?;
        let status = resp.status();
        if matches!(status.as_u16(), 401 | 403) {
            bail!(
                "{} rejected the API key ({status})",
                provider.display_name()
            );
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("{} returned {status}: {text}", provider.display_name());
        }
        Ok(())
    }

    /// List the models the configured endpoint serves (`GET {base_url}/models`).
    pub async fn models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.config.base_url);
        let mut req = self.http.get(&url);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        let mut resp = req.send().await.context("sending models request")?;
        // Some endpoints (Onde Cloud) list models publicly but reject an invalid key, so a
        // bad key shouldn't hide the catalog: retry without auth.
        if self.config.api_key.is_some() && matches!(resp.status().as_u16(), 401 | 403) {
            resp = self
                .http
                .get(&url)
                .send()
                .await
                .context("sending models request")?;
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("models endpoint returned {status}: {text}");
        }
        let mut models = resp
            .json::<ModelsResponse>()
            .await
            .context("parsing models response")?
            .data;
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// Run one streaming chat completion, invoking `on_delta` for every text fragment.
    pub async fn complete(
        &self,
        model: &str,
        messages: &[Value],
        tools: &Value,
        mut on_delta: impl FnMut(Delta<'_>),
    ) -> Result<Completion> {
        if self.config.api_key.is_none() {
            bail!(
                "No API key configured. Set {} in the environment or run the agent's `--setup`",
                self.config.provider.key_var()
            );
        }
        let mut body = json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        let send = |body: &Value| {
            let mut req = self
                .http
                .post(format!("{}/chat/completions", self.config.base_url))
                .json(body);
            if let Some(key) = &self.config.api_key {
                req = req.bearer_auth(key);
            }
            req.send()
        };
        let mut resp = send(&body)
            .await
            .context("sending chat completion request")?;
        // Not every OpenAI-compatible server knows `stream_options`; retry without it.
        if matches!(resp.status().as_u16(), 400 | 422) {
            body.as_object_mut().unwrap().remove("stream_options");
            resp = send(&body)
                .await
                .context("sending chat completion request")?;
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("LLM endpoint returned {status}: {text}");
        }

        let mut out = Completion::default();
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(bytes) = stream.next().await {
            buf.extend_from_slice(&bytes.context("reading response stream")?);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(out);
                }
                let chunk: Chunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("skipping unparseable chunk ({e}): {data}");
                        continue;
                    }
                };
                if chunk.usage.is_some() {
                    out.usage = chunk.usage;
                }
                for choice in chunk.choices {
                    let d = choice.delta;
                    if let Some(r) = d.reasoning_content.as_deref().or(d.reasoning.as_deref())
                        && !r.is_empty()
                    {
                        on_delta(Delta::Reasoning(r));
                    }
                    if let Some(t) = d.content.as_deref()
                        && !t.is_empty()
                    {
                        out.content.push_str(t);
                        on_delta(Delta::Text(t));
                    }
                    for tc in d.tool_calls {
                        if out.tool_calls.len() <= tc.index {
                            out.tool_calls
                                .resize(tc.index + 1, ToolCallRequest::default());
                        }
                        let slot = &mut out.tool_calls[tc.index];
                        if let Some(id) = tc.id {
                            slot.id = id;
                        }
                        if tc.extra_content.is_some() {
                            slot.extra_content = tc.extra_content;
                        }
                        if let Some(f) = tc.function {
                            if let Some(n) = f.name {
                                slot.name.push_str(&n);
                            }
                            if let Some(a) = f.arguments {
                                slot.arguments.push_str(&a);
                            }
                        }
                    }
                    if choice.finish_reason.is_some() {
                        out.finish_reason = choice.finish_reason;
                    }
                }
            }
        }
        Ok(out)
    }
}
