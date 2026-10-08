//! A test MCP server over stdio (newline-delimited JSON-RPC). Stands in for third-party servers
//! and for built-ins such as `demucs --mcp`: it ignores its arguments. A product exposes it as a
//! binary whose `main` calls [`run`] and points `ed_acp_testkit::Target::mcp_stub` at it. Behind the
//! `test-server` feature.
//!
//! Set `STUB_PID_FILE` to have it write its process id, so a test can check it was stopped.
//!
//! Tools: `echo` (read-only), `slow_progress` (three progress notifications),
//! `wait_for_cancel` (answers only after `notifications/cancelled`), and the demucs-shaped
//! `list_models` / `separate_stems`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn tools() -> Value {
    json!([
        {
            "name": "echo",
            "description": "Echo the text back.",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "slow_progress",
            "description": "Report progress three times, then finish.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "wait_for_cancel",
            "description": "Blocks until the call is cancelled.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "list_models",
            "description": "List stem separation models.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "separate_stems",
            "description": "Pretend to separate stems.",
            "inputSchema": {"type": "object", "properties": {"input": {"type": "string"}, "model": {"type": "string"}}, "required": ["input"]}
        }
    ])
}

/// Serve on stdin/stdout until stdin closes.
pub async fn run() {
    // Lets tests find out whether this process is still alive after the agent closes it.
    if let Some(path) = std::env::var_os("STUB_PID_FILE") {
        let _ = std::fs::write(path, std::process::id().to_string());
    }
    let out = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let cancelled: Arc<Mutex<HashSet<String>>> = Arc::default();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    let send = {
        let out = out.clone();
        move |msg: Value| {
            let out = out.clone();
            async move {
                let mut o = out.lock().await;
                let _ = o.write_all(format!("{msg}\n").as_bytes()).await;
                let _ = o.flush().await;
            }
        }
    };

    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            if method == "notifications/cancelled"
                && let Some(rid) = params.get("requestId")
            {
                cancelled.lock().unwrap().insert(rid.to_string());
            }
            continue;
        };
        match method.as_str() {
            "initialize" => {
                let version = params
                    .get("protocolVersion")
                    .cloned()
                    .unwrap_or(json!("2025-06-18"));
                send(json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "stub", "version": "0.0.0"}
                }}))
                .await;
            }
            "ping" => send(json!({"jsonrpc": "2.0", "id": id, "result": {}})).await,
            "tools/list" => {
                send(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools()}})).await
            }
            "tools/call" => {
                let send = send.clone();
                let cancelled = cancelled.clone();
                tokio::spawn(async move {
                    let name = params
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    let token = params.pointer("/_meta/progressToken").cloned();
                    let progress = |n: u32, msg: &str| {
                        token.as_ref().map(|t| {
                            json!({"jsonrpc": "2.0", "method": "notifications/progress",
                                   "params": {"progressToken": t, "progress": n, "total": 100, "message": msg}})
                        })
                    };
                    let result = match name.as_str() {
                        "echo" => {
                            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
                            json!({"content": [{"type": "text", "text": format!("echo: {text}")}], "isError": false})
                        }
                        "slow_progress" | "separate_stems" => {
                            for (n, m) in [(10, "starting"), (50, "halfway"), (90, "almost")] {
                                if let Some(p) = progress(n, m) {
                                    send(p).await;
                                }
                                tokio::time::sleep(Duration::from_millis(40)).await;
                            }
                            if name == "slow_progress" {
                                json!({"content": [{"type": "text", "text": "done"}], "isError": false})
                            } else {
                                let input = args
                                    .get("input")
                                    .and_then(Value::as_str)
                                    .unwrap_or("/tmp/x.wav");
                                let dir = format!("{}_stems", input.trim_end_matches(".wav"));
                                let stems: Vec<Value> = ["drums", "bass", "vocals", "other"]
                                    .iter()
                                    .map(|s| json!({"id": s, "path": format!("{dir}/{s}.wav")}))
                                    .collect();
                                json!({
                                    "content": [{"type": "text", "text": format!("Separated into {} stems", stems.len())}],
                                    "structuredContent": {"stems": stems},
                                    "isError": false
                                })
                            }
                        }
                        "list_models" => json!({
                            "content": [{"type": "text", "text": "htdemucs 80 MB"}],
                            "structuredContent": {"models": [
                                {"id": "htdemucs", "size_mb": 80, "cached": false},
                                {"id": "htdemucs_6s", "size_mb": 55, "cached": true}
                            ]},
                            "isError": false
                        }),
                        "wait_for_cancel" => {
                            let key = id.to_string();
                            while !cancelled.lock().unwrap().contains(&key) {
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                            json!({"content": [{"type": "text", "text": "cancelled"}], "isError": true})
                        }
                        other => {
                            send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": format!("unknown tool {other}")}})).await;
                            return;
                        }
                    };
                    send(json!({"jsonrpc": "2.0", "id": id, "result": result})).await;
                });
            }
            other => {
                send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})).await;
            }
        }
    }
}
