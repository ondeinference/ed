# Ed

**Embed Ed.** Your app gets a brain. Your users keep their data.

Ed is a chat agent you drop into an application. It loads an open-weights model,
tells your interface when that model is ready, keeps the conversation, and hands
back replies. The model runs on the user's device — there is no API key, no
metered token, no round trip, and nothing to leak.

## Why it exists

Four applications needed the same thing: load a model, know whether it's ready,
keep a conversation, hand back a reply. Four times somebody wrote it again. Two
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
ed-agent = "0.1"
```

```rust
use ed_agent::{Ed, GgufModelConfig};

let ed = Ed::new();
ed.load(GgufModelConfig::qwen25_1_5b(), None, None).await?;

let reply = ed.send("Summarise this thread.").await;
println!("{}", reply.reply.unwrap_or_default());
```

That's a working agent. To hear about state changes rather than polling for
them, implement `StatusSink` and build with `Ed::with_sink`.

## Crates

| Crate | What it's for |
| --- | --- |
| [`ed-agent`](crates/ed-agent) | The agent. No framework dependency. |
| [`ed-agent-tauri`](crates/ed-agent-tauri) | Tauri bindings: a sink that emits to the webview, plus the commands it invokes. |

Free the intelligence.

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under Apache-2.0.
