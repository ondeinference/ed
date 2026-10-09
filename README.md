# Ed

**Embed Ed.** Your app gets a brain. Your users keep their data.

Ed is an agent you drop into an application. It loads an open-weights model,
keeps the conversation, calls only the host tools you register, and hands back
replies. The model runs on the user's device — there is no metered inference
API and private context does not need to leave the device.

## Why it exists

Seven applications needed the same thing: load a model, know whether it's ready,
keep a conversation, hand back a reply. Seven times somebody wrote it again. Two
of them ended up byte-identical in three files.

None of that code was interesting. It was the wiring between an inference engine
that reports status by polling and a user interface that needs to be told —
plus the flattening of `Result<InferenceResult, _>` into one payload a view can
render either way. Ed is that layer, written once.

It deliberately doesn't own sessions, persistence, model catalogues, or
downloads. Those differ per application and stay yours.

## Onde runs the model. Ed makes it a feature.

[Onde](https://ondeinference.com) is the inference engine: weights, quantisation,
Metal and CPU backends, tokens out. Ed is the part above it your users touch.

## See it live

- **[smbCloud MailX](https://smbcloudmail.com/en-us)** — siMail drafts replies,
  summarises long threads, and helps triage the inbox. The mail never leaves the
  machine to get read.
- **[Siti AI](https://github.com/getsigit/siti)** — a private assistant for
  macOS, iOS, and Android, and the open reference app: its source is public, so
  it's the codebase to read for a full integration.
- **[Pepak Basa Jawa Komplit](https://apps.apple.com/se/app/pepak-basa-jawa-komplit/id6752241812)**
  — Joko, a Javanese tutor who answers offline, which matters where the app is
  used.
- **[Rumi Learn Persian](https://apps.apple.com/se/app/rumi-l%C3%A4r-dig-persiska/id6753832408)**
  — Rumi, a Persian tutor, same model, same device.

## Quick start

```toml
[dependencies]
ed-agent = "1.2"
```

```rust
use ed_agent::{Ed, GgufModelConfig};

let ed = Ed::new();
ed.load(GgufModelConfig::qwen25_1_5b(), None, None).await?;

let reply = ed.send("Summarise this thread.").await;
println!("{}", reply.reply.unwrap_or_default());
```

Stream a reply with `Ed::stream` (plain chat) or let `Ed::run` forward deltas to
`EventSink::text_delta`. `Ed::restore_history` loads a saved conversation,
`Ed::generate` is a one-shot call that leaves the history alone, and
`Ed::set_system_prompt` / `set_sampling` change the model's settings. These
three need a loaded model. Persistence stays yours; implement `SessionStore`
over whatever you already have.

Construct with `Ed::with_agent` to provide the executor for registered tools
and the approval handler for mutating tools. Read-only tools run directly;
mutating tools must receive `AllowOnce` or `AllowForSession` first.

## Crates

| Crate | What it's for |
| --- | --- |
| [`ed-agent`](crates/ed-agent) | The agent. No framework dependency. |
| [`ed-agent-ffi`](crates/ed-agent-ffi) | UniFFI bridge used by the Swift XCFramework. |
| [`ed-agent-tauri`](crates/ed-agent-tauri) | Tauri bindings: a sink that emits to the webview, the commands it invokes, and `EdSessions` for apps with a chat history and webview-answered approvals. |

The Onde Agent Platform crates, for agents that editors drive over the Agent Client Protocol
(ACP). SplitFire Agent is built on them.

| Crate | What it's for |
| --- | --- |
| [`ed-acp`](crates/ed-acp) | An ACP v1 server: sessions, auth, the turn loop on Onde Cloud, tool approval, workspace tools and MCP. A product supplies a `Profile`. |
| [`ed-acp-tui`](crates/ed-acp-tui) | A terminal UI for any ACP agent. |
| [`ed-mcp`](crates/ed-mcp) | MCP client glue over `rmcp`, the official Rust SDK: stdio and streamable HTTP servers, bearer tokens, tool namespacing, timeouts, progress and cancellation. With the `agent` feature, `McpExecutor` serves a toolset to Ed as tools. |
| [`ed-onde-account`](crates/ed-onde-account) | Onde Inference account client: sign in, activate an app and get its Onde Cloud API key through ondeinference.com, with no Onde secrets in the host app. |
| [`ed-acp-testkit`](crates/ed-acp-testkit) | Mock Onde Cloud endpoint, raw ACP client, the ACP registry probe and the ACP v1 conformance suite. |

Free the intelligence.

## Copyright

© 2026 [Onde Inference](https://ondeinference.com).
Licensed under MIT or Apache-2.0.
