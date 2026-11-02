# ed-claude

Claude as an [ACP](https://agentclientprotocol.com) coding agent that runs inside your app.

It is an [`ed-acp`](https://crates.io/crates/ed-acp) `Profile`: Claude's name, prompt, workspace
tools and an Anthropic API-key sign-in. It talks to the Anthropic API with the user's own key,
so you ship no key and pay for no inference. Every session gets ed-acp's tool approvals, `fs/*`
routed through the client, MCP servers and durable sessions.

```rust
use std::sync::Arc;
use agent_client_protocol::Channel;
use ed_acp::ServeOptions;
use ed_claude::{credentials, Claude};

// Once, at startup. In a sandboxed app, a dir inside the container.
credentials::set_dir(app_config_dir.join("edclaude"));

// The key the user pasted into your settings screen.
credentials::store_api_key(&api_key, None)?;

// Serve the agent in process. Run your ACP client on `client_end`.
let (agent_end, client_end) = Channel::duplex();
tokio::spawn(ed_acp::serve(
    Arc::new(Claude::new()),
    ServeOptions { yolo: false, surface: "app" },
    agent_end,
));
```

- `Claude::new()` has `read_file`, `write_file`, `edit_file`, `list_directory` and `run_command`.
  `Claude::without_shell()` drops `run_command` and tells the model so, for hosts that can't
  start processes (iOS, Mac App Store builds).
- Models: `claude-sonnet-5-5` (default), `claude-opus-5-5` and `claude-haiku-5-5`, offered
  in the ACP model picker.
- The key lives in one file at mode 0600 in the dir you set. The crate never reads the process
  environment, so `OPENAI_*`, `ONDE_*` and `ED_CLAUDE_*` variables don't leak in.
- Without a stored key, `authenticate` fails with `API_KEY_NEEDED`. Show your key prompt when
  you see it. `logout` deletes the stored key.
- `ClaudeAuth::with_base_url` points the agent at a proxy or a test server. Test with
  [`ed-acp-testkit`](https://crates.io/crates/ed-acp-testkit)'s mock endpoint.

This crate uses API keys only. For a Claude subscription, run Anthropic's `claude` CLI as an
external agent instead, for example through [`ed-acp-tauri`](https://crates.io/crates/ed-acp-tauri).

Used in production by [siGit](https://sigit.si).

Claude and Anthropic are trademarks of Anthropic, PBC. This crate is not made or endorsed by
Anthropic. Use of the Anthropic API follows Anthropic's terms.

## Copyright

© 2026 [Onde Inference](https://ondeinference.com).
Licensed under MIT or Apache-2.0.
