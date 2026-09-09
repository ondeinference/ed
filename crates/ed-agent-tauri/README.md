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

Loading a model is deliberately not a command here — which model, and from
where, is an application decision. Call `Ed::load` or `Ed::load_with` from your
own command.

Tool calling needs both halves from you. Build the agent with `Ed::with_agent`
to pass a `ToolExecutor` and an `ApprovalHandler`; `Ed::with_sink` rejects every
tool and denies every approval. Approvals are not a command either: answering
one means a webview round trip inside an awaited Rust call, and hosts differ
enough about how to wire that up that this crate emits
`chat_approval_requested` and leaves the answering to you.

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under MIT or Apache-2.0.
