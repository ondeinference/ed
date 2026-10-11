//! The raw JSON-RPC test client and its environment.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// How long to wait for any one message from the agent.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// What the fake client does when the agent asks for permission.
#[derive(Debug, Clone, Copy)]
pub enum Perm {
    Allow,
    Reject,
    RejectAlways,
}

/// An ACP client speaking raw JSON-RPC to the agent's stdio, so tests see exactly what is on
/// the wire. It answers `session/request_permission` per [`Raw::perm`] and fails every other
/// agent-to-client request.
pub struct Raw {
    pub child: Child,
    stdin: Option<ChildStdin>,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    pub next_id: u64,
    /// Notifications (`session/update` etc.), in arrival order.
    pub notes: Vec<Value>,
    /// Permission requests seen, as the full JSON-RPC `params`.
    pub permissions: Vec<Value>,
    pub perm: Perm,
    queued: VecDeque<Value>,
}

/// The agent's command and environment for one test. The process gets nothing else from the
/// test's environment.
#[derive(Debug, Clone)]
pub struct Env {
    pub bin: PathBuf,
    pub prefix: &'static str,
    pub vars: Vec<(String, String)>,
}

impl Env {
    /// `<PREFIX>_<suffix>`.
    pub fn var(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.prefix)
    }
    pub fn set(&mut self, k: &str, v: impl Into<String>) -> &mut Self {
        self.vars.retain(|(n, _)| n != k);
        self.vars.push((k.to_string(), v.into()));
        self
    }
    pub fn unset(&mut self, k: &str) -> &mut Self {
        self.vars.retain(|(n, _)| n != k);
        self
    }
}

impl Raw {
    /// Start the agent with `--acp`.
    pub fn spawn(env: &Env) -> Raw {
        let mut cmd = Command::new(&env.bin);
        cmd.arg("--acp")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in &env.vars {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn agent");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        Raw {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
            notes: Vec::new(),
            permissions: Vec::new(),
            perm: Perm::Allow,
            queued: VecDeque::new(),
        }
    }

    pub async fn send(&mut self, msg: Value) {
        let stdin = self.stdin.as_mut().expect("stdin closed");
        stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }

    pub async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    pub async fn next_message(&mut self) -> Value {
        if let Some(m) = self.queued.pop_front() {
            return m;
        }
        let line = tokio::time::timeout(TIMEOUT, self.lines.next_line())
            .await
            .expect("timed out waiting for the agent")
            .unwrap()
            .expect("agent closed stdout");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {line}"))
    }

    /// Send a request and wait for its response, serving agent→client requests meanwhile.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let msg = self.next_message().await;
            if msg.get("method").is_some() {
                if msg.get("id").is_some() {
                    self.serve(&msg).await;
                } else {
                    self.notes.push(msg);
                }
                continue;
            }
            if msg["id"] == json!(id) {
                return match msg.get("error") {
                    Some(e) => Err(e.clone()),
                    None => Ok(msg["result"].clone()),
                };
            }
        }
    }

    pub async fn serve(&mut self, req: &Value) {
        let id = req["id"].clone();
        let reply = match req["method"].as_str().unwrap() {
            "session/request_permission" => {
                self.permissions.push(req["params"].clone());
                match self.perm {
                    Perm::Allow => {
                        json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}})
                    }
                    Perm::Reject => {
                        json!({"outcome": {"outcome": "selected", "optionId": "reject_once"}})
                    }
                    Perm::RejectAlways => {
                        json!({"outcome": {"outcome": "selected", "optionId": "reject_always"}})
                    }
                }
            }
            other => {
                self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unsupported {other}")}})).await;
                return;
            }
        };
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": reply}))
            .await;
    }

    /// Read whatever the agent sends right after a response (e.g. the commands update that
    /// follows `session/new`) until it has been quiet for a moment.
    pub async fn settle(&mut self) {
        while let Ok(line) =
            tokio::time::timeout(Duration::from_millis(300), self.lines.next_line()).await
        {
            let Ok(Some(line)) = line else { return };
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg.get("id").is_some() && msg.get("method").is_some() {
                self.serve(&msg).await;
            } else {
                self.notes.push(msg);
            }
        }
    }

    pub async fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .await
            .unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    pub async fn initialize(&mut self) -> Value {
        self.ok(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await
    }

    pub async fn new_session(&mut self, cwd: &Path, mcp: Value) -> String {
        let r = self
            .ok("session/new", json!({"cwd": cwd, "mcpServers": mcp}))
            .await;
        self.settle().await;
        r["sessionId"].as_str().unwrap().to_string()
    }

    pub async fn prompt(&mut self, sid: &str, blocks: Value) -> Value {
        self.ok(
            "session/prompt",
            json!({"sessionId": sid, "prompt": blocks}),
        )
        .await
    }

    pub async fn text_prompt(&mut self, sid: &str, text: &str) -> Value {
        self.prompt(sid, json!([{"type": "text", "text": text}]))
            .await
    }

    /// Notifications for a session update kind, e.g. `tool_call_update`.
    pub fn updates(&self, kind: &str) -> Vec<Value> {
        self.notes
            .iter()
            .filter(|n| {
                n["method"] == "session/update" && n["params"]["update"]["sessionUpdate"] == kind
            })
            .map(|n| n["params"]["update"].clone())
            .collect()
    }

    /// Close stdin and wait for the agent to exit on its own.
    pub async fn finish(mut self) {
        drop(self.stdin.take());
        let _ = tokio::time::timeout(TIMEOUT, self.child.wait())
            .await
            .expect("agent did not exit");
    }
}

/// The JSON-RPC error code.
pub fn error_code(e: &Value) -> i64 {
    e["code"].as_i64().unwrap()
}

/// The text content of a tool call or update.
pub fn tool_text(update: &Value) -> String {
    update["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.pointer("/content/text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Wait up to five seconds for `pid` to exit.
pub async fn wait_dead(pid: u32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

pub fn read_pid(file: &Path) -> u32 {
    std::fs::read_to_string(file)
        .expect("stub pid file")
        .trim()
        .parse()
        .unwrap()
}

/// An `mcpServers` entry for the test MCP server under `name`.
pub fn stub_server(stub: &Path, name: &str, pid_file: Option<&Path>) -> Value {
    let env: Vec<Value> = pid_file
        .map(|p| vec![json!({"name": "STUB_PID_FILE", "value": p})])
        .unwrap_or_default();
    json!({"name": name, "command": stub, "args": [], "env": env})
}
