//! `Claude` against a mock OpenAI-compatible endpoint: the stored key becomes the bearer, a
//! streamed tool-call turn round-trips, and the process environment is never consulted.

use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, StopReason, TextContent,
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
use ed_acp::{AuthProvider, ServeOptions};
use ed_acp_testkit::{MockStats, Script, Step, TEST_KEY, start_mock_llm, temp_dir};
use ed_claude::{Claude, ClaudeAuth, DEFAULT_MODEL, MODELS, credentials};
use serde_json::json;

/// Held by every test that changes the process environment or runs the agent, which reads it,
/// so a `set_var` never overlaps another thread's `getenv`.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn no_key_means_no_llm_config_even_with_provider_env_set() {
    let _env = ENV.blocking_lock();
    let dir = temp_dir("ed-claude-nokey");
    // SAFETY: ENV is held, and the other test that reads the environment waits for it.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "sk-from-env");
        std::env::set_var("ED_CLAUDE_API_KEY", "sk-from-env");
        std::env::set_var("ONDE_API_KEY", "app:secret");
    }
    let auth = ClaudeAuth::new().with_dir(dir.clone());
    assert!(auth.llm_config().is_none());
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ED_CLAUDE_API_KEY");
        std::env::remove_var("ONDE_API_KEY");
    }
}

#[test]
fn llm_config_uses_the_stored_key_and_model() {
    let dir = temp_dir("ed-claude-config");
    let auth = ClaudeAuth::new().with_dir(dir.clone());
    credentials::store_in_for_tests(&dir, "sk-ant-1", Some("claude-opus-5-5"));
    let config = auth.llm_config().expect("config");
    assert_eq!(config.base_url, "https://api.anthropic.com/v1");
    assert_eq!(config.api_key.as_deref(), Some("sk-ant-1"));
    assert_eq!(config.model, "claude-opus-5-5");

    // A model outside the offered list falls back to the default.
    credentials::store_in_for_tests(&dir, "sk-ant-1", Some("gpt-4o"));
    assert_eq!(auth.llm_config().unwrap().model, DEFAULT_MODEL);
    assert_eq!(auth.models().unwrap(), MODELS.map(String::from).to_vec());
}

#[tokio::test]
async fn a_streamed_tool_call_turn_round_trips() {
    let _env = ENV.lock().await;
    let data = temp_dir("ed-claude-data");
    let key_dir = temp_dir("ed-claude-keys");
    let work = temp_dir("ed-claude-work");
    std::fs::write(work.join("hello.txt"), "hi").unwrap();
    // SAFETY: ENV is held for the whole turn; only the data dir, no credentials.
    unsafe { std::env::set_var("ED_CLAUDE_DATA_DIR", &data) };
    credentials::store_in_for_tests(&key_dir, TEST_KEY, None);

    let stats = Arc::new(MockStats::default());
    let script = Script(vec![
        Step::ToolCall {
            name: "list_directory",
            arguments: json!({"path": "."}),
        },
        Step::Final("done"),
    ]);
    let base = start_mock_llm(script, stats.clone()).await;
    let auth = ClaudeAuth::new().with_base_url(base).with_dir(key_dir);

    let (agent_end, client_end) = Channel::duplex();
    let opts = ServeOptions {
        yolo: true,
        surface: "app",
    };
    tokio::spawn(ed_acp::serve(
        Arc::new(Claude::without_shell().with_auth(auth)),
        opts,
        agent_end,
    ));

    let stop = Client
        .builder()
        .connect_with(client_end, |conn: ConnectionTo<Agent>| async move {
            conn.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = conn
                .send_request(NewSessionRequest::new(work.clone()))
                .block_task()
                .await?;
            let reply = conn
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![ContentBlock::Text(TextContent::new("list the files"))],
                ))
                .block_task()
                .await?;
            Ok(reply.stop_reason)
        })
        .await
        .expect("turn");

    assert_eq!(stop, StopReason::EndTurn);
    assert!(*stats.saw_bearer.lock().unwrap(), "the key is the bearer");
    let payloads = stats.payloads.lock().unwrap();
    assert_eq!(payloads.len(), 2, "tool call, then the answer");
    assert_eq!(payloads[0]["model"], DEFAULT_MODEL);
    let names: Vec<_> = payloads[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"edit_file".to_string()));
    assert!(!names.contains(&"run_command".to_string()), "no shell");
}
