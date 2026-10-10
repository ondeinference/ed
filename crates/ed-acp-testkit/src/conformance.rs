//! The ACP v1 conformance suite (Onde Agent Platform §5.5). Each function drives the real
//! agent binary over stdio against the mock Onde endpoint. Run them all with
//! [`conformance_suite!`](crate::conformance_suite).
//!
//! The suite assumes the agent offers [`WorkspaceTools`] (`read_file`, `write_file`) and
//! terminal auth, as every Onde ACP agent does today.
//!
//! [`WorkspaceTools`]: https://docs.rs/ed-acp

use std::time::Duration;

use serde_json::{Value, json};

use crate::Target;
use crate::mock::{MODEL, Step, TEST_KEY, mock, temp_dir};
use crate::raw::{Perm, Raw, alive, error_code, read_pid, stub_server, tool_text, wait_dead};

pub async fn initialize_negotiates_v1_and_advertises_capabilities(t: &Target) {
    let data = temp_dir("sf-data");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    // A client from the future gets v1 back, not an echo of its own version.
    let r = agent
        .ok(
            "initialize",
            json!({"protocolVersion": 99, "clientCapabilities": {"auth": {"terminal": true}}}),
        )
        .await;
    assert_eq!(r["protocolVersion"], 1);
    let caps = &r["agentCapabilities"];
    assert_eq!(caps["loadSession"], true);
    assert_eq!(caps["promptCapabilities"]["image"], true);
    assert_eq!(caps["promptCapabilities"]["embeddedContext"], true);
    let sc = &caps["sessionCapabilities"];
    for k in ["list", "resume", "close", "delete", "additionalDirectories"] {
        assert!(sc.get(k).is_some(), "sessionCapabilities.{k} missing: {sc}");
    }
    assert_eq!(r["agentInfo"]["name"], t.name);
    assert!(
        caps["auth"].get("logout").is_some(),
        "auth.logout missing: {caps}"
    );
    // Terminal auth is offered to clients that declare they can run it...
    assert_eq!(r["authMethods"][0]["id"], "terminal-setup");
    assert_eq!(r["authMethods"][0]["args"][0], "--setup");
    agent.finish().await;

    let mut agent = Raw::spawn(&t.env(&data, &url));
    let r = agent
        .ok(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await;
    assert_eq!(r["protocolVersion"], 1);
    // ...and not to clients that can't.
    assert!(
        r.get("authMethods")
            .and_then(Value::as_array)
            .is_none_or(|m| m.is_empty()),
        "{r}"
    );
    agent.finish().await;
}

/// The ACP registry probes every listed agent with this exact `initialize` (copied from
/// `.github/workflows/client.py` in agentclientprotocol/registry), from an empty HOME with no
/// credentials, and rejects the agent if `authMethods` is empty. It signals terminal-auth
/// support only through `_meta`, never through `auth.terminal`.
pub async fn registry_validator_probe_sees_terminal_auth(t: &Target) {
    let data = temp_dir("sf-data");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut env = t.env(&data, &url);
    env.unset("ONDE_API_KEY");
    let mut agent = Raw::spawn(&env);
    let r = agent
        .ok(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientInfo": {"name": "ACP Registry Validator", "version": "1.0.0"},
                "clientCapabilities": {
                    "terminal": true,
                    "fs": {"readTextFile": true, "writeTextFile": true},
                    "_meta": {"terminal_output": true, "terminal-auth": true}
                }
            }),
        )
        .await;
    let methods = r["authMethods"].as_array().expect("authMethods");
    assert!(!methods.is_empty(), "registry validation would fail: {r}");
    assert_eq!(methods[0]["type"], "terminal", "{methods:?}");
    assert_eq!(methods[0]["args"][0], "--setup");
    agent.finish().await;
}

pub async fn auth_required_then_authenticate_reloads_config(t: &Target) {
    let data = temp_dir("sf-data");
    let (url, _) = mock(vec![Step::Final("hello")]).await;
    let mut env = t.env(&data, &url);
    env.unset("ONDE_API_KEY");
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let cwd = temp_dir("sf-work");

    // No key anywhere: session/new and session/prompt are refused with auth_required (-32000).
    let e = agent
        .call("session/new", json!({"cwd": cwd, "mcpServers": []}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");
    let e = agent
        .call("session/prompt", json!({"sessionId": "x", "prompt": []}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");
    // authenticate fails while there is still no key, and rejects unknown methods.
    let e = agent
        .call("authenticate", json!({"methodId": "terminal-setup"}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");
    let e = agent
        .call("authenticate", json!({"methodId": "nope"}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");

    // load/resume also answer auth_required, so an editor reopening a thread shows sign-in.
    let e = agent
        .call(
            "session/load",
            json!({"sessionId": "x", "cwd": cwd, "mcpServers": []}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");

    // `--setup` stored a key in the config file; authenticate picks it up without a restart.
    std::fs::write(data.join("env"), format!("ONDE_API_KEY={TEST_KEY}\n")).unwrap();
    agent
        .ok("authenticate", json!({"methodId": "terminal-setup"}))
        .await;
    let sid = agent.new_session(&cwd, json!([])).await;
    let r = agent.text_prompt(&sid, "hi").await;
    assert_eq!(r["stopReason"], "end_turn");

    // logout removes the stored key; the agent is signed out without a restart.
    agent.ok("logout", json!({})).await;
    assert!(!data.join("env").exists());
    let e = agent
        .call("session/new", json!({"cwd": cwd, "mcpServers": []}))
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32000, "{e}");

    // Zed's flow: terminal auth runs `--setup` in another process, then retries session/new
    // on this one without calling authenticate. The stored key must be picked up anyway.
    std::fs::write(data.join("env"), format!("ONDE_API_KEY={TEST_KEY}\n")).unwrap();
    agent.new_session(&cwd, json!([])).await;
    agent.finish().await;
}

pub async fn sessions_survive_a_killed_process_and_replay_tool_output(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    std::fs::write(work.join("notes.txt"), "chorus in Dm").unwrap();
    let (url, _) = mock(vec![
        Step::ToolCall {
            name: "read_file",
            arguments: json!({"path": "notes.txt"}),
        },
        Step::ToolCall {
            name: "read_file",
            arguments: json!({"path": "missing.txt"}),
        },
        Step::Final("The chorus is in Dm."),
    ])
    .await;
    let env = t.env(&data, &url);

    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let r = agent.text_prompt(&sid, "What key is the chorus in?").await;
    assert_eq!(r["stopReason"], "end_turn");
    // Title is pushed as a session_info_update.
    let infos = agent.updates("session_info_update");
    assert!(
        infos
            .iter()
            .any(|u| u["title"] == "What key is the chorus in?"),
        "no title update: {infos:?}"
    );
    // The process dies without a graceful close.
    agent.child.kill().await.unwrap();
    drop(agent);

    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let list = agent.ok("session/list", json!({})).await;
    let sessions = list["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{list}");
    assert_eq!(sessions[0]["sessionId"], sid.as_str());
    assert_eq!(sessions[0]["title"], "What key is the chorus in?");
    assert!(sessions[0]["updatedAt"].as_str().unwrap().ends_with('Z'));
    // cwd filter.
    let other = agent.ok("session/list", json!({"cwd": "/nowhere"})).await;
    assert!(other["sessions"].as_array().unwrap().is_empty());

    agent.notes.clear();
    agent
        .ok(
            "session/load",
            json!({"sessionId": sid, "cwd": work, "mcpServers": []}),
        )
        .await;
    agent.settle().await;
    let user = agent.updates("user_message_chunk");
    assert_eq!(user[0]["content"]["text"], "What key is the chorus in?");
    let said: String = agent
        .updates("agent_message_chunk")
        .iter()
        .filter_map(|u| u["content"]["text"].as_str())
        .collect();
    assert!(said.contains("The chorus is in Dm."), "{said}");
    let calls = agent.updates("tool_call");
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0]["status"], "completed");
    assert!(
        tool_text(&calls[0]).contains("chorus in Dm"),
        "{}",
        calls[0]
    );
    assert_eq!(
        calls[1]["status"], "failed",
        "failed call must replay as failed: {}",
        calls[1]
    );
    assert!(
        calls[0]["locations"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("notes.txt")
    );
    // Commands are advertised after the load response.
    let cmds = agent.updates("available_commands_update");
    assert!(!cmds.is_empty());
    agent.finish().await;
}

pub async fn resume_close_delete_and_unknown_sessions(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let work2 = temp_dir("sf-work");
    let (url, stats) = mock(vec![Step::Final("ok")]).await;
    let env = t.env(&data, &url);
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;

    let sid = agent.new_session(&work, json!([])).await;
    agent.text_prompt(&sid, "first").await;

    // Unknown sessions are invalid_params where nothing can be reopened. load/resume of a
    // well-formed id reopen it instead (covered below); a malformed id can't name a session.
    for (method, params) in [
        (
            "session/load",
            json!({"sessionId": "../nope", "cwd": work, "mcpServers": []}),
        ),
        (
            "session/resume",
            json!({"sessionId": "../nope", "cwd": work, "mcpServers": []}),
        ),
        (
            "session/load",
            json!({"sessionId": "relative-cwd", "cwd": "work", "mcpServers": []}),
        ),
        ("session/new", json!({"cwd": "work", "mcpServers": []})),
        ("session/close", json!({"sessionId": "nope"})),
        ("session/list", json!({"cursor": "stale"})),
        (
            "session/prompt",
            json!({"sessionId": "nope", "prompt": [{"type": "text", "text": "x"}]}),
        ),
        (
            "session/set_config_option",
            json!({"sessionId": "nope", "configId": "model", "value": "other-model"}),
        ),
    ] {
        let e = agent.call(method, params).await.unwrap_err();
        assert_eq!(error_code(&e), -32602, "{method}: {e}");
    }

    // Config options: model picker lists the endpoint's models; bad ids are rejected.
    let created = agent
        .ok("session/new", json!({"cwd": work, "mcpServers": []}))
        .await;
    let opts = created["configOptions"].as_array().unwrap();
    assert_eq!(opts[0]["id"], "model");
    assert_eq!(opts[0]["category"], "model");
    assert_eq!(opts[0]["currentValue"], MODEL);
    let values: Vec<&str> = opts[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    assert!(
        values.contains(&MODEL) && values.contains(&"other-model"),
        "{values:?}"
    );
    let sid2 = created["sessionId"].as_str().unwrap().to_string();
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "model", "value": "bogus"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "temperature", "value": "x"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");
    let r = agent
        .ok(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "model", "value": "other-model"}),
        )
        .await;
    assert_eq!(r["configOptions"][0]["currentValue"], "other-model");
    agent.text_prompt(&sid2, "second").await;
    let last = stats.payloads.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["model"], "other-model");

    // Resume rebinds the cwd without replaying anything.
    agent.notes.clear();
    agent
        .ok(
            "session/resume",
            json!({"sessionId": sid, "cwd": work2, "mcpServers": []}),
        )
        .await;
    assert!(
        agent.updates("user_message_chunk").is_empty(),
        "resume must not replay"
    );
    let listed = agent.ok("session/list", json!({"cwd": work2})).await;
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);

    // Close keeps the stored conversation; delete removes it.
    agent.ok("session/close", json!({"sessionId": sid})).await;
    let e = agent.call("session/close", json!({"sessionId": sid})).await;
    assert!(
        e.is_ok(),
        "close of a stored-only session is a no-op, not an error"
    );
    let all = agent.ok("session/list", json!({})).await;
    assert_eq!(all["sessions"].as_array().unwrap().len(), 2);
    agent.ok("session/delete", json!({"sessionId": sid})).await;
    let all = agent.ok("session/list", json!({})).await;
    assert_eq!(all["sessions"].as_array().unwrap().len(), 1);
    assert!(!data.join("sessions").join(format!("{sid}.json")).exists());
    // Delete is idempotent: a second delete, or one for a session that never existed, succeeds.
    agent.ok("session/delete", json!({"sessionId": sid})).await;
    agent
        .ok("session/delete", json!({"sessionId": "nope"}))
        .await;

    // A well-formed id with no saved history (a thread from a process that never saved it)
    // reopens empty under the same id, with a notice, instead of failing to launch.
    agent.notes.clear();
    let r = agent
        .ok(
            "session/load",
            json!({"sessionId": "lost-thread", "cwd": work, "mcpServers": []}),
        )
        .await;
    assert_eq!(r["configOptions"][0]["id"], "model");
    let notice = agent.updates("agent_message_chunk");
    assert_eq!(notice.len(), 1, "{notice:?}");
    assert!(
        notice[0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("starts fresh"),
        "{notice:?}"
    );
    assert!(notice[0]["messageId"].is_string());
    agent.text_prompt("lost-thread", "hello again").await;
    agent.finish().await;
}

pub async fn streaming_reports_message_ids_usage_and_inlines_file_links(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    std::fs::write(
        work.join("chart.md"),
        "Verse: Am F C G\nChorus: F G Em Am\nBridge: Dm E\n",
    )
    .unwrap();
    let (url, stats) = mock(vec![Step::Final("Looks like A minor.")]).await;
    let mut env = t.env(&data, &url);
    let window = env.var("CONTEXT_WINDOW");
    env.set(&window, "1000");
    let mut agent = Raw::spawn(&env);
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let chart = format!("file://{}?column=1#L2:2", work.join("chart.md").display());
    agent
        .prompt(
            &sid,
            json!([
                {"type": "text", "text": "What key is the chorus in?"},
                {"type": "resource_link", "name": "chart.md", "uri": chart},
            ]),
        )
        .await;

    // The selection link is inlined as just that line.
    let payload = stats.payloads.lock().unwrap()[0].clone();
    assert!(payload["stream_options"]["include_usage"] == true);
    let user = payload["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(user.contains("Chorus: F G Em Am"), "{user}");
    assert!(
        !user.contains("Verse:") && !user.contains("Bridge:"),
        "{user}"
    );

    // Every chunk of one assistant message carries the same message id.
    let chunks = agent.updates("agent_message_chunk");
    assert!(!chunks.is_empty());
    assert!(
        chunks.iter().all(|c| c["messageId"].is_string()),
        "{chunks:?}"
    );
    // Usage from the endpoint, sized by <PREFIX>_CONTEXT_WINDOW.
    let usage = agent.updates("usage_update");
    assert_eq!(usage.len(), 1, "{usage:?}");
    assert_eq!(
        (usage[0]["used"].as_u64(), usage[0]["size"].as_u64()),
        (Some(150), Some(1000))
    );
    agent.finish().await;
}

pub async fn permissions_reject_always_raw_output_and_auto_approve(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let write = |path: &'static str| Step::ToolCall {
        name: "write_file",
        arguments: json!({"path": path, "content": "x"}),
    };
    let (url, _) = mock(vec![write("a.txt"), write("b.txt"), Step::Final("done")]).await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    // A client that supports boolean config options gets the auto-approve toggle.
    let init = agent
        .ok(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {"session": {"configOptions": {"boolean": {}}}}}),
        )
        .await;
    assert_eq!(init["protocolVersion"], 1);
    let created = agent
        .ok("session/new", json!({"cwd": work, "mcpServers": []}))
        .await;
    agent.settle().await;
    let sid = created["sessionId"].as_str().unwrap().to_string();
    let opts = created["configOptions"].as_array().unwrap();
    assert_eq!(opts[1]["id"], "auto_approve");
    assert_eq!(opts[1]["type"], "boolean");
    assert_eq!(opts[1]["currentValue"], false);

    // "Always reject" is offered, and once chosen the tool is refused without asking again.
    agent.perm = Perm::RejectAlways;
    agent.text_prompt(&sid, "write two files").await;
    assert_eq!(
        agent.permissions.len(),
        1,
        "asked again after reject_always"
    );
    let kinds: Vec<&str> = agent.permissions[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"reject_always"), "{kinds:?}");
    assert!(!work.join("a.txt").exists() && !work.join("b.txt").exists());
    // Tool calls carry the tool name; updates carry the raw output the model saw.
    let calls = agent.updates("tool_call");
    assert!(calls.iter().all(|c| c["name"] == "write_file"), "{calls:?}");
    let done = agent.updates("tool_call_update");
    let failed: Vec<&Value> = done.iter().filter(|u| u["status"] == "failed").collect();
    assert_eq!(failed.len(), 2, "{done:?}");
    assert_eq!(failed[0]["rawOutput"]["failed"], true);
    assert!(
        failed[0]["rawOutput"]["output"]
            .as_str()
            .unwrap()
            .contains("rejected")
    );

    // Turning auto-approve on is broadcast as a config_option_update and skips the prompts.
    agent.notes.clear();
    let r = agent
        .ok(
            "session/set_config_option",
            json!({"sessionId": sid, "configId": "auto_approve", "type": "boolean", "value": true}),
        )
        .await;
    assert_eq!(r["configOptions"][1]["currentValue"], true);
    let broadcast = agent.updates("config_option_update");
    assert_eq!(broadcast.len(), 1, "{broadcast:?}");
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": sid, "configId": "auto_approve", "value": "yes"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");

    // A fresh session (no reject_always grant) now writes without asking.
    let sid2 = agent.new_session(&work, json!([])).await;
    agent
        .ok(
            "session/set_config_option",
            json!({"sessionId": sid2, "configId": "auto_approve", "type": "boolean", "value": true}),
        )
        .await;
    agent.permissions.clear();
    agent.text_prompt(&sid2, "write two files").await;
    assert!(agent.permissions.is_empty(), "{:?}", agent.permissions);
    assert!(work.join("a.txt").exists() && work.join("b.txt").exists());
    agent.finish().await;
}

pub async fn auto_approve_is_hidden_from_clients_without_boolean_options(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    agent.initialize().await;
    let created = agent
        .ok("session/new", json!({"cwd": work, "mcpServers": []}))
        .await;
    agent.settle().await;
    let opts = created["configOptions"].as_array().unwrap();
    assert_eq!(opts.len(), 1, "{opts:?}");
    let e = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": created["sessionId"], "configId": "auto_approve", "type": "boolean", "value": true}),
        )
        .await
        .unwrap_err();
    assert_eq!(error_code(&e), -32602, "{e}");
    agent.finish().await;
}

pub async fn content_filter_is_a_refusal_and_private_flags_stay_off_the_wire(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "read_file",
            arguments: json!({"path": "missing.txt"}),
        },
        Step::ContentFilter,
    ])
    .await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    agent.initialize().await;
    let sid = agent.new_session(&work, json!([])).await;
    let r = agent.text_prompt(&sid, "read it").await;
    assert_eq!(r["stopReason"], "refusal");
    let payloads = stats.payloads.lock().unwrap().clone();
    let second = payloads.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let tool = second.iter().find(|m| m["role"] == "tool").unwrap();
    assert!(
        tool["content"].as_str().unwrap().starts_with("Error:"),
        "{tool}"
    );
    assert!(
        tool.get("x_failed").is_none(),
        "private field leaked to the model: {tool}"
    );
    // The system prompt is rebuilt each turn and never stored with the history.
    assert_eq!(second[0]["role"], "system");
    let stored: Value = serde_json::from_slice(
        &std::fs::read(data.join("sessions").join(format!("{sid}.json"))).unwrap(),
    )
    .unwrap();
    assert!(
        stored["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system")
    );
    agent.finish().await;
}

pub async fn client_supplied_mcp_server_is_offered_gated_and_streams_progress(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let pid_file = data.join("stub.pid");
    let (url, stats) = mock(vec![
        Step::ToolCall {
            name: "mcp__stub__echo",
            arguments: json!({"text": "hi"}),
        },
        Step::ToolCall {
            name: "mcp__stub__slow_progress",
            arguments: json!({}),
        },
        Step::Final("all done"),
    ])
    .await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    agent.initialize().await;
    let sid = agent
        .new_session(
            &work,
            json!([stub_server(&t.mcp_stub, "stub", Some(&pid_file))]),
        )
        .await;
    let r = agent.text_prompt(&sid, "use the stub").await;
    assert_eq!(r["stopReason"], "end_turn");

    // Offered: the namespaced tools ride along with the built-ins.
    let payload = stats.payloads.lock().unwrap()[0].clone();
    let names: Vec<String> = payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"mcp__stub__echo".to_string()), "{names:?}");
    assert_eq!(names.len(), t.base_tool_count + 5, "{names:?}");

    // Gated: only the tool without readOnlyHint asked for permission.
    assert_eq!(agent.permissions.len(), 1, "{:?}", agent.permissions);
    let perm = &agent.permissions[0];
    assert_eq!(perm["toolCall"]["toolCallId"], "call_2");

    // Callable, with the result fed back to the model.
    let payloads = stats.payloads.lock().unwrap().clone();
    let tools: Vec<&Value> = payloads[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect();
    assert_eq!(tools[0]["content"], "echo: hi");

    // Progress: MCP notifications/progress became tool_call_update content.
    let progress: Vec<String> = agent
        .updates("tool_call_update")
        .iter()
        .filter(|u| u["toolCallId"] == "call_2")
        .map(tool_text)
        .collect();
    assert!(
        progress.iter().any(|t| t == "halfway (50%)"),
        "{progress:?}"
    );
    let done = agent
        .updates("tool_call_update")
        .into_iter()
        .rfind(|u| u["toolCallId"] == "call_2")
        .unwrap();
    assert_eq!(done["status"], "completed");

    // Children are killed when the session closes.
    let pid = read_pid(&pid_file);
    assert!(alive(pid));
    agent.ok("session/close", json!({"sessionId": sid})).await;
    assert!(
        wait_dead(pid).await,
        "stub server still running after session/close"
    );
    agent.finish().await;
}

pub async fn mcp_children_die_when_the_agent_exits(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let pid_file = data.join("stub.pid");
    let (url, _) = mock(vec![Step::Final("x")]).await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    agent.initialize().await;
    agent
        .new_session(
            &work,
            json!([stub_server(&t.mcp_stub, "stub", Some(&pid_file))]),
        )
        .await;
    let pid = read_pid(&pid_file);
    assert!(alive(pid));
    agent.finish().await;
    assert!(wait_dead(pid).await, "stub server outlived the agent");
}

pub async fn broken_mcp_server_is_not_fatal(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, stats) = mock(vec![Step::Final("fine")]).await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    agent.initialize().await;
    let sid = agent
        .new_session(
            &work,
            json!([
                {"name": "ghost", "command": "/definitely/not/here", "args": [], "env": []},
                {"name": "http", "type": "http", "url": "http://127.0.0.1:1/mcp", "headers": []}
            ]),
        )
        .await;
    let r = agent.text_prompt(&sid, "hello").await;
    assert_eq!(r["stopReason"], "end_turn");
    let payload = stats.payloads.lock().unwrap()[0].clone();
    assert_eq!(
        payload["tools"].as_array().unwrap().len(),
        t.base_tool_count
    );
    agent.finish().await;
}

pub async fn cancelling_a_turn_cancels_the_mcp_call(t: &Target) {
    let data = temp_dir("sf-data");
    let work = temp_dir("sf-work");
    let (url, _) = mock(vec![
        Step::ToolCall {
            name: "mcp__stub__wait_for_cancel",
            arguments: json!({}),
        },
        Step::Final("should not get here"),
    ])
    .await;
    let mut agent = Raw::spawn(&t.env(&data, &url));
    agent.initialize().await;
    let sid = agent
        .new_session(&work, json!([stub_server(&t.mcp_stub, "stub", None)]))
        .await;
    // Fire the prompt without waiting, then cancel once the call is in flight.
    let id = agent.next_id;
    agent.next_id += 1;
    agent
        .send(
            json!({"jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {"sessionId": sid, "prompt": [{"type": "text", "text": "wait"}]}}),
        )
        .await;
    let mut cancelled = false;
    let stop = loop {
        let msg = agent.next_message().await;
        if msg.get("method").is_some() {
            if msg.get("id").is_some() {
                agent.serve(&msg).await;
                // The permission request means the tool call is about to start.
                tokio::time::sleep(Duration::from_millis(300)).await;
                agent
                    .notify("session/cancel", json!({"sessionId": sid}))
                    .await;
                cancelled = true;
            } else {
                agent.notes.push(msg);
            }
        } else if msg["id"] == json!(id) {
            break msg["result"]["stopReason"].clone();
        }
    };
    assert!(cancelled, "the call should have asked for permission first");
    assert_eq!(stop, "cancelled");
    let last = agent
        .updates("tool_call_update")
        .into_iter()
        .last()
        .unwrap();
    assert_eq!(last["status"], "failed");
    agent.finish().await;
}
