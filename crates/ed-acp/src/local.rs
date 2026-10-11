//! [`Provider::Local`](crate::llm::Provider::Local): the turn loop on an on-device model, with
//! onde.
//!
//! The rest of `ed-acp` keeps the conversation as OpenAI chat-completions JSON and calls
//! [`LlmClient::complete`](crate::llm::LlmClient::complete) once per model turn with all of it.
//! This module converts that JSON to onde's [`ChatTurn`]s and runs
//! [`ChatEngine::complete`], which takes the whole conversation each call and leaves the
//! engine's own history alone. One engine serves the process; it loads the requested model on
//! first use (downloading it the first time) and swaps it when the session picks another.
//!
//! onde doesn't stream tool-aware turns yet, so a turn's reasoning and text are reported
//! through `on_delta` once the turn ends.
//!
//! Without the `local` feature the provider is still selectable but every turn fails with an
//! error that says how to enable it.

#[cfg(feature = "local")]
pub use enabled::{catalog, complete};

#[cfg(not(feature = "local"))]
pub use disabled::{catalog, complete};

#[cfg(not(feature = "local"))]
mod disabled {
    use serde_json::Value;

    use crate::llm::{Completion, Delta, ModelInfo};

    pub fn catalog() -> Vec<ModelInfo> {
        Vec::new()
    }

    pub async fn complete(
        _model: &str,
        _messages: &[Value],
        _tools: &Value,
        _on_delta: impl FnMut(Delta<'_>),
    ) -> anyhow::Result<Completion> {
        anyhow::bail!(
            "this agent was built without on-device inference; rebuild it with ed-acp's `local` \
             feature, or choose another provider"
        )
    }
}

#[cfg(feature = "local")]
mod enabled {
    use std::sync::OnceLock;

    use anyhow::{Context, Result, anyhow, bail};
    use onde::inference::models::{SUPPORTED_MODEL_INFO, TOOL_CALLING_MODELS};
    use onde::inference::{
        ChatEngine, ChatTurn, GgufModelConfig, ToolCallRequest as OndeCall, ToolCallingSupport,
        ToolDefinition,
    };
    use serde_json::Value;
    use tokio::sync::Mutex;

    use crate::llm::{Completion, Delta, ModelInfo, ToolCallRequest, Usage};

    /// The process's engine and the model loaded in it. The mutex is held across a turn, so
    /// sessions take turns on the one model instead of swapping it under each other.
    struct Local {
        engine: ChatEngine,
        loaded: Mutex<Option<String>>,
    }

    fn local() -> &'static Local {
        static LOCAL: OnceLock<Local> = OnceLock::new();
        LOCAL.get_or_init(|| Local {
            engine: ChatEngine::new(),
            loaded: Mutex::new(None),
        })
    }

    /// onde's tool-calling models, the ones an agent can drive.
    pub fn catalog() -> Vec<ModelInfo> {
        TOOL_CALLING_MODELS
            .iter()
            .map(|id| ModelInfo {
                id: (*id).to_string(),
                owned_by: SUPPORTED_MODEL_INFO
                    .iter()
                    .find(|info| info.id == *id)
                    .map(|info| format!("{} (on device)", info.name)),
            })
            .collect()
    }

    pub async fn complete(
        model: &str,
        messages: &[Value],
        tools: &Value,
        mut on_delta: impl FnMut(Delta<'_>),
    ) -> Result<Completion> {
        let turns = to_turns(messages)?;
        let tools = to_tools(tools);
        let local = local();
        let mut loaded = local.loaded.lock().await;
        if loaded.as_deref() != Some(model) {
            let config = GgufModelConfig::from_supported_model_id(model)
                .ok_or_else(|| anyhow!("{model} is not an on-device model onde supports"))?;
            if onde::inference::models::tool_calling_support(model) != ToolCallingSupport::Supported
            {
                bail!("{model} can't call tools reliably, so it can't run an agent");
            }
            on_delta(Delta::Reasoning(&format!(
                "Loading {} on this device. The first use downloads it.\n",
                config.display_name
            )));
            tracing::info!("loading on-device model {model}");
            *loaded = None;
            local
                .engine
                .load_gguf_model(config, None, None)
                .await
                .with_context(|| format!("loading {model}"))?;
            *loaded = Some(model.to_string());
        }
        let result = local
            .engine
            .complete(turns, &tools, None)
            .await
            .context("on-device inference")?;
        drop(loaded);

        if let Some(reasoning) = &result.reasoning {
            on_delta(Delta::Reasoning(reasoning));
        }
        if !result.text.is_empty() {
            on_delta(Delta::Text(&result.text));
        }
        Ok(Completion {
            content: result.text,
            tool_calls: result
                .tool_calls
                .into_iter()
                .map(|call| ToolCallRequest {
                    id: call.id,
                    name: call.function_name,
                    arguments: call.arguments,
                    extra_content: None,
                })
                .collect(),
            finish_reason: Some(result.finish_reason),
            usage: Some(Usage {
                prompt_tokens: result.prompt_tokens as u64,
                completion_tokens: result.completion_tokens as u64,
                total_tokens: (result.prompt_tokens + result.completion_tokens) as u64,
            }),
        })
    }

    /// OpenAI chat messages, as the turn loop keeps them, to onde turns.
    pub(super) fn to_turns(messages: &[Value]) -> Result<Vec<ChatTurn>> {
        messages
            .iter()
            .map(|message| {
                let role = message.get("role").and_then(Value::as_str).unwrap_or("");
                let content = text_of(message.get("content"));
                Ok(match role {
                    "system" | "developer" => ChatTurn::System(content),
                    "user" => ChatTurn::User(content),
                    "assistant" => ChatTurn::Assistant {
                        content,
                        tool_calls: message
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .map(|calls| calls.iter().map(to_call).collect())
                            .unwrap_or_default(),
                    },
                    "tool" => ChatTurn::Tool {
                        tool_call_id: message
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        content,
                    },
                    other => bail!("unexpected message role {other:?}"),
                })
            })
            .collect()
    }

    fn to_call(call: &Value) -> OndeCall {
        let field = |pointer: &str| {
            call.pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        OndeCall {
            id: field("/id"),
            function_name: field("/function/name"),
            arguments: field("/function/arguments"),
        }
    }

    /// A message's text: a string, or the text parts of a content array. On-device models here
    /// are text-only, so other parts become a short placeholder.
    fn text_of(content: Option<&Value>) -> String {
        match content {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .map(|part| match part.get("type").and_then(Value::as_str) {
                    Some("text") => part
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    Some(kind) => format!("[{kind} omitted: the on-device model reads text only]"),
                    None => String::new(),
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    /// OpenAI `tools` entries to onde tool definitions.
    pub(super) fn to_tools(tools: &Value) -> Vec<ToolDefinition> {
        tools
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| {
                        let function = tool.get("function")?;
                        Some(ToolDefinition {
                            name: function.get("name")?.as_str()?.to_string(),
                            description: function
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            parameters_schema: function
                                .get("parameters")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({"type": "object"}))
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(all(test, feature = "local"))]
mod tests {
    use onde::inference::{ChatTurn, ToolCallRequest};
    use serde_json::json;

    use super::enabled::{catalog, to_tools, to_turns};
    use crate::llm::LOCAL_DEFAULT_MODEL;

    #[test]
    fn the_default_model_is_in_the_catalog() {
        assert!(catalog().iter().any(|m| m.id == LOCAL_DEFAULT_MODEL));
        assert!(catalog().iter().all(|m| {
            m.owned_by
                .as_deref()
                .is_some_and(|o| o.ends_with("(on device)"))
        }));
    }

    #[test]
    fn messages_become_turns() {
        let messages = vec![
            json!({"role": "system", "content": "You are SplitFire."}),
            json!({"role": "user", "content": [
                {"type": "text", "text": "What key is this?"},
                {"type": "image_url", "image_url": {"url": "data:..."}}
            ]}),
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "analyze_audio", "arguments": "{\"path\":\"a.wav\"}"}}
            ]}),
            json!({"role": "tool", "tool_call_id": "call_1", "content": "Key: A minor"}),
            json!({"role": "assistant", "content": "A minor."}),
        ];
        let turns = to_turns(&messages).unwrap();
        assert_eq!(
            turns,
            vec![
                ChatTurn::System("You are SplitFire.".into()),
                ChatTurn::User(
                    "What key is this?\n[image_url omitted: the on-device model reads text only]"
                        .into()
                ),
                ChatTurn::Assistant {
                    content: String::new(),
                    tool_calls: vec![ToolCallRequest {
                        id: "call_1".into(),
                        function_name: "analyze_audio".into(),
                        arguments: "{\"path\":\"a.wav\"}".into(),
                    }],
                },
                ChatTurn::Tool {
                    tool_call_id: "call_1".into(),
                    content: "Key: A minor".into(),
                },
                ChatTurn::Assistant {
                    content: "A minor.".into(),
                    tool_calls: vec![],
                },
            ]
        );
        assert!(to_turns(&[json!({"role": "narrator", "content": "x"})]).is_err());
    }

    #[test]
    fn tools_become_definitions() {
        let tools = json!([
            {"type": "function", "function": {
                "name": "theory_chord", "description": "Spell a chord.",
                "parameters": {"type": "object", "properties": {"symbol": {"type": "string"}}}
            }},
            {"type": "function", "function": {"name": "no_params"}},
            {"type": "not-a-function"}
        ]);
        let defs = to_tools(&tools);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "theory_chord");
        assert_eq!(defs[0].description, "Spell a chord.");
        let schema: serde_json::Value = serde_json::from_str(&defs[0].parameters_schema).unwrap();
        assert_eq!(schema["properties"]["symbol"]["type"], "string");
        assert_eq!(defs[1].parameters_schema, r#"{"type":"object"}"#);
    }
}
