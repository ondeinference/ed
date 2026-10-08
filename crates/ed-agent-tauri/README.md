# ed-agent-tauri

Tauri bindings for [Ed](https://crates.io/crates/ed-agent), the embeddable
local-model chat agent.

Ed's core is framework-agnostic and reports through a `StatusSink`. This crate
is the Tauri implementation of that sink plus the commands a webview calls.

```rust
use ed_agent::Ed;
use ed_agent_tauri::{EdState, TauriSink};
use tauri::Manager;

let ed = Ed::with_sink(TauriSink::new(app.handle().clone()));
app.manage(EdState::new(ed));
```

Then register `chat_get_status`, `chat_get_history`, `chat_clear_history`,
`chat_send_message`, `chat_run`, and `chat_cancel` on the builder's
`invoke_handler`.

These events reach the webview: `chat_status_changed`, `chat_reply`,
`chat_tool_requested`, `chat_approval_requested`, `chat_tool_started`,
`chat_tool_finished`, `chat_agent_reply`, and `chat_warning`.

## Several conversations

`EdSessions` is the multi-session form. Give it a `SessionStore` and it handles
switching, the in-memory cache, logout (`reset`) and approvals. Register
`chat_list_sessions`, `chat_new_session`, `chat_switch_session`,
`chat_delete_session`, `chat_get_session_history`, `chat_submit` and
`chat_respond_approval`.

`chat_submit` returns immediately; the answer is a `chat_reply` event
`{id, session, reply, duration, error}`, preceded by `chat_text_delta
{session, id, delta}` events. Approvals use `chat_approval_requested
{request_id, session, call, risk}` and `chat_approval_resolved {request_id}`;
answer with `chat_respond_approval`. To rewrite the message first, wrap
`EdSessions::submit_with` in your own command.

Loading a model is deliberately not a command here — which model, and from
where, is an application decision. Call `Ed::load` or `Ed::load_with` from your
own command.

Without `EdSessions`, tool calling needs both halves from you: build the agent with `Ed::with_agent` to pass a `ToolExecutor` and an `ApprovalHandler`.

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under MIT or Apache-2.0.
