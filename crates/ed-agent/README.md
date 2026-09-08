# ed-agent

**Embed Ed.** Your app gets a brain. Your users keep their data.

Ed is a chat agent you drop into an application. It sits one layer above the
[`onde`](https://crates.io/crates/onde) inference SDK: `onde` gives you an
engine to load a model into and send messages to, and Ed adds the part every
application built on it was writing for itself.

- **Lifecycle orchestration.** Loading a model is a sequence of state changes,
  not one call. Ed drives `Loading -> Ready` (or `-> Error`) and reports each
  transition.
- **Notification.** `onde` exposes status by polling. A UI needs to be told. Ed
  pushes transitions and replies to a `StatusSink` you implement.
- **A flattened reply.** Inference returns `Result<InferenceResult, _>`; a
  frontend wants one shape covering both outcomes. That's `ChatReply`.

Ed deliberately does not own sessions, persistence, model catalogues, or
downloads. Those differ per application and belong to you.

## Usage

```rust
use ed_agent::{Ed, GgufModelConfig};

let ed = Ed::new();
ed.load(GgufModelConfig::qwen25_1_5b(), None, None).await?;

let reply = ed.send("Summarise this thread.").await;
println!("{}", reply.reply.unwrap_or_default());
```

To hear about state changes, implement `StatusSink` and construct with
`Ed::with_sink`. For Tauri applications,
[`ed-agent-tauri`](https://crates.io/crates/ed-agent-tauri) provides that
implementation plus the commands a webview invokes.

## Any load path, same transitions

`Ed::load` covers GGUF. For anything else — an assigned model from a server, a
UQFF build, a load API that doesn't exist yet — hand the call to `load_with` and
Ed reports the transitions around it:

```rust
ed.load_with("Onde model", |engine| {
    engine.load_assigned_model(env, app_id, secret, prompt, sampling)
})
.await?;
```

Ed takes a closure rather than wrapping each path by name on purpose. Those APIs
take types that would drag their crates into Ed's public signature and pin every
host to Ed's version of them — and at least one of those types has already moved
between `onde` releases. A closure keeps Ed's dependency surface to `onde` alone.

## Platforms

Ed has no `cfg(target_os)` gates. `onde` already splits real and fallback
implementations internally, so this crate compiles everywhere and surfaces
`onde`'s error on a platform it can't run inference on.

Be careful adding platform gates in a host: a gate narrower than `onde`'s own
support list silently disables chat on a platform that would have worked.

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under Apache-2.0.
