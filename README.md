<p align="center">
  <img src="https://raw.githubusercontent.com/ondeinference/onde/refs/heads/main/assets/onde-inference-logo.svg" alt="Onde Inference" width="96">
</p>

<h1 align="center">Ed</h1>

<p align="center">
  Embed Ed. Your app gets a brain. Your users keep their data.
</p>

<p align="center">
  <a href="https://crates.io/crates/ed-agent"><img src="https://img.shields.io/crates/v/ed-agent?style=flat-square&labelColor=17211D&color=235843&label=ed-agent" alt="ed-agent on crates.io"></a>
  <a href="https://crates.io/crates/ed-acp"><img src="https://img.shields.io/crates/v/ed-acp?style=flat-square&labelColor=17211D&color=235843&label=ed-acp" alt="ed-acp on crates.io"></a>
  <a href="https://github.com/ondeinference/ed-swift"><img src="https://img.shields.io/github/v/release/ondeinference/ed-swift?style=flat-square&labelColor=17211D&color=235843&label=Swift" alt="Swift"></a>
  <a href="https://github.com/ondeinference/ed/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/ondeinference/ed/ci.yml?style=flat-square&labelColor=17211D&color=235843&label=CI" alt="CI"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-235843?style=flat-square&labelColor=17211D" alt="License"></a>
</p>

Ed is the agent layer above [Onde](https://ondeinference.com). It loads an
open-weights model, keeps the conversation, calls only the host tools you
register and hands back replies. The model runs on the user's device, so there
is no metered inference API and private context stays put. The `ed-acp` crates
build ACP agents (Onde Code, SplitFire Agent, siGit) on the same base.

Ed doesn't own sessions, persistence, model catalogues or downloads. Those
differ per application and stay yours.

## Install

```sh
cargo add ed-agent   # embedded chat
cargo add ed-acp     # ACP agent
```

For Swift, add the package from [`ondeinference/ed-swift`](https://github.com/ondeinference/ed-swift).

## Quick start

```rust
use ed_agent::{Ed, GgufModelConfig};

let ed = Ed::new();
ed.load(GgufModelConfig::qwen25_1_5b(), None, None).await?;

let reply = ed.send("Summarise this thread.").await;
println!("{}", reply.reply.unwrap_or_default());
```

Streaming, history, tools and approvals are covered in [`crates/ed-agent`](crates/ed-agent).

## Use it your way

- **In your app:** [`ed-agent`](crates/ed-agent), plus [`ed-agent-tauri`](crates/ed-agent-tauri) for Tauri and [`ed-agent-ffi`](crates/ed-agent-ffi) for Swift.
- **As an ACP agent:** [`ed-acp`](crates/ed-acp) with a `Profile`, and [`ed-acp-tui`](crates/ed-acp-tui) for a terminal UI.
- **Inside a Tauri app:** [`ed-acp-tauri`](crates/ed-acp-tauri) runs an ACP agent in process or an external stdio agent.
- **With Claude:** [`ed-claude`](crates/ed-claude/README.md), a profile on the Anthropic API with the user's key.
- **With MCP servers:** [`ed-mcp`](crates/ed-mcp). Onde accounts: [`ed-onde-account`](crates/ed-onde-account). Tests: [`ed-acp-testkit`](crates/ed-acp-testkit).

## In production

[smbCloud MailX](https://smbcloudmail.com/en-us), [Siti AI](https://github.com/getsigit/siti) (the open reference app),
[Pepak Basa Jawa Komplit](https://apps.apple.com/se/app/pepak-basa-jawa-komplit/id6752241812) and
[Rumi](https://apps.apple.com/se/app/rumi-l%C3%A4r-dig-persiska/id6753832408).

## Docs

Each crate has its own README:
[`ed-agent`](crates/ed-agent/README.md),
[`ed-agent-tauri`](crates/ed-agent-tauri/README.md),
[`ed-agent-ffi`](crates/ed-agent-ffi),
[`ed-acp`](crates/ed-acp/README.md),
[`ed-acp-tui`](crates/ed-acp-tui/README.md),
[`ed-acp-tauri`](crates/ed-acp-tauri),
[`ed-claude`](crates/ed-claude/README.md),
[`ed-mcp`](crates/ed-mcp/README.md),
[`ed-onde-account`](crates/ed-onde-account/README.md),
[`ed-acp-testkit`](crates/ed-acp-testkit/README.md).

## Contributing

Open pull requests against `development`. Before you push:

```sh
cargo fmt --all -- --check && cargo clippy --locked --workspace --all-targets -- -D warnings && cargo test --locked --workspace
```

## License

MIT or Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

## Copyright

Free the intelligence.

© 2026 [Onde Inference](https://ondeinference.com).
