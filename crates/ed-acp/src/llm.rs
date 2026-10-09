//! Streaming client for Onde Cloud's OpenAI-compatible `/chat/completions` endpoint: text and
//! reasoning deltas, tool-call accumulation, token usage, and `GET /models`.
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

    /// Where durable state (sessions, audio scratch) lives. Same directory as the `env` file.
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.config_dir()
            .unwrap_or_else(|| std::env::temp_dir().join(self.dir_name))
    }

    fn load_file_vars(&self) -> std::collections::HashMap<String, String> {
        let mut vars = std::collections::HashMap::new();
        let content = self
            .config_file()
            .and_then(|p| std::fs::read_to_string(p).ok());
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
    pub base_url: String,
    /// Onde credentials: `app-id:app-secret`, sent as the bearer token.
    pub api_key: Option<String>,
    pub model: String,
}

impl LlmConfig {
    /// `ONDE_API_KEY` from the environment, falling back to the stored `env` file.
    /// `<PREFIX>_BASE_URL` and `<PREFIX>_MODEL` override the endpoint and default model.
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
        Self {
            base_url: get(&env.var("BASE_URL"))
                .unwrap_or_else(|| DEFAULT_BASE_URL.into())
                .trim_end_matches('/')
                .to_string(),
            api_key: get(API_KEY_VAR),
            model: get(&env.var("MODEL")).unwrap_or_else(|| env.default_model.into()),
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
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments },
                    })
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
        let Some(key) = &self.config.api_key else {
            bail!("no ONDE_API_KEY configured");
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
            .context("reaching Onde Cloud")?;
        let status = resp.status();
        if matches!(status.as_u16(), 401 | 403) {
            bail!("Onde Cloud rejected the API key ({status})");
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Onde Cloud returned {status}: {text}");
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
                "No API key configured. Set ONDE_API_KEY in the environment or run the agent's `--setup`"
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
