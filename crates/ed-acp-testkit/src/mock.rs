//! A scripted OpenAI-compatible streaming endpoint that stands in for Onde Cloud.
//!
//! It answers `GET /v1/models` with [`MODEL`] and `other-model`, records every
//! `/chat/completions` body, and replies with the next [`Step`] of its script, chosen by how many
//! tool results the request already carries. Final answers report usage when the request asks
//! for it with `stream_options.include_usage`, as Onde Cloud does.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Dummy Onde credentials; the mock asserts they arrive as the bearer token.
pub const TEST_KEY: &str = "test-app:test-secret";
pub const MODEL: &str = "scripted-test-model";

#[derive(Debug, Default)]
pub struct MockStats {
    pub requests: AtomicUsize,
    pub saw_bearer: Mutex<bool>,
    /// Every `/chat/completions` request body, in arrival order.
    pub payloads: Mutex<Vec<Value>>,
}

/// What the scripted model does on its next completion.
#[derive(Clone)]
pub enum Step {
    /// Emit a tool call, then wait for the tool result.
    ToolCall {
        name: &'static str,
        arguments: Value,
    },
    /// Emit final assistant text and finish.
    Final(&'static str),
    /// Stop with `finish_reason: content_filter`.
    ContentFilter,
}

#[derive(Clone)]
pub struct Script(pub Vec<Step>);

impl Script {
    /// Count completed tool results to know how far into the script we are.
    fn step_for(&self, messages: &[Value]) -> (usize, Step) {
        let done = messages.iter().filter(|m| m["role"] == "tool").count();
        (
            done,
            self.0
                .get(done)
                .unwrap_or_else(|| self.0.last().expect("empty script"))
                .clone(),
        )
    }
}

fn sse_chunk(
    content: Option<&str>,
    tool_call: Option<(&str, &str, &str)>,
    finish: Option<&str>,
) -> String {
    let mut delta = json!({});
    if let Some(c) = content {
        delta["content"] = json!(c);
    }
    if let Some((id, name, args)) = tool_call {
        delta["tool_calls"] = json!([{
            "index": 0,
            "id": id,
            "function": { "name": name, "arguments": args },
        }]);
    }
    let chunk = json!({ "choices": [{ "delta": delta, "finish_reason": finish }] });
    format!("data: {chunk}\n\n")
}

async fn read_http_request(socket: &mut tokio::net::TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end;
    let mut content_length = 0usize;
    loop {
        let n = socket.read(&mut tmp).await.unwrap();
        if n == 0 {
            panic!("connection closed before headers complete");
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            header_end = pos + 4;
            let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
            for line in headers.lines() {
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap();
                }
            }
            break;
        }
    }
    while buf.len() < header_end + content_length {
        let n = socket.read(&mut tmp).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let body = String::from_utf8_lossy(&buf[header_end..header_end + content_length]).to_string();
    (head, body)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn respond(socket: &mut tokio::net::TcpStream, content_type: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(resp.as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}

/// Spawn the mock server; returns its base URL (e.g. `http://127.0.0.1:PORT/v1`).
pub async fn start_mock_llm(script: Script, stats: Arc<MockStats>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let stats = stats.clone();
            let script = script.clone();
            tokio::spawn(async move {
                let (head, body) = read_http_request(&mut socket).await;
                let first = head.lines().next().unwrap_or("").to_string();
                if first.starts_with("GET /v1/models") {
                    let list =
                        json!({"object": "list", "data": [{"id": MODEL}, {"id": "other-model"}]});
                    return respond(&mut socket, "application/json", &list.to_string()).await;
                }
                stats.requests.fetch_add(1, Ordering::SeqCst);
                if head
                    .to_ascii_lowercase()
                    .contains(&format!("authorization: bearer {TEST_KEY}").to_ascii_lowercase())
                {
                    *stats.saw_bearer.lock().unwrap() = true;
                }
                assert!(
                    first.starts_with("POST /v1/chat/completions"),
                    "unexpected request: {first}"
                );
                let payload: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(payload["stream"], true);
                stats.payloads.lock().unwrap().push(payload.clone());
                let messages = payload["messages"].as_array().unwrap();
                let (done, step) = script.step_for(messages);

                let mut data = String::new();
                match step {
                    Step::ToolCall { name, arguments } => {
                        let id = format!("call_{}", done + 1);
                        data.push_str(&sse_chunk(
                            None,
                            Some((&id, name, &arguments.to_string())),
                            None,
                        ));
                        data.push_str(&sse_chunk(None, None, Some("tool_calls")));
                    }
                    Step::Final(text) => {
                        data.push_str(&sse_chunk(Some(text), None, None));
                        data.push_str(&sse_chunk(None, None, Some("stop")));
                        // `stream_options.include_usage`: a final chunk with no choices.
                        if payload.get("stream_options").is_some() {
                            let usage = json!({"choices": [], "usage": {
                                "prompt_tokens": 120, "completion_tokens": 30, "total_tokens": 150
                            }});
                            data.push_str(&format!("data: {usage}\n\n"));
                        }
                    }
                    Step::ContentFilter => {
                        data.push_str(&sse_chunk(None, None, Some("content_filter")));
                    }
                }
                data.push_str("data: [DONE]\n\n");
                respond(&mut socket, "text/event-stream", &data).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

pub fn temp_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Start a mock with `steps`; returns its base URL and the recorded requests.
pub async fn mock(steps: Vec<Step>) -> (String, Arc<MockStats>) {
    let stats = Arc::new(MockStats::default());
    let url = start_mock_llm(Script(steps), stats.clone()).await;
    (url, stats)
}
