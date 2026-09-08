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

Then register `chat_get_status`, `chat_get_history`, `chat_clear_history`, and
`chat_send_message` on the builder's `invoke_handler`. Two events reach the
webview: `chat_status_changed` and `chat_reply`.

Loading a model is deliberately not a command here — which model, and from
where, is an application decision. Call `Ed::load` or `Ed::load_with` from your
own command.

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under Apache-2.0.
