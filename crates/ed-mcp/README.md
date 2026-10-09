# ed-mcp

MCP client glue over [`rmcp`](https://crates.io/crates/rmcp), the official Rust SDK, for
[Ed](https://crates.io/crates/ed-agent) agents.

`rmcp` is the client. This crate owns what every agent was writing around it: server
configuration (stdio child processes and streamable HTTP with an optional bearer token),
`mcp__<server>__<tool>` namespacing, per-server timeouts, progress and cancellation, and read-only
gating from `readOnlyHint`. A server that fails to start is logged and left out.

```rust
use ed_mcp::{ClientInfo, McpToolset, ServerSpec, ToolsetConfig};

let spec = ServerSpec::http("mail", "https://api.example.com/v1/mcp").with_bearer(token);
let tools = McpToolset::connect_specs(
    ToolsetConfig::new(ClientInfo::new("my-app", "1.0")),
    &[spec],
    &[],
)
.await;
```

## Features

- `acp`: map `McpServer` lists from an Agent Client Protocol `session/new` request.
- `agent`: `McpExecutor` serves a toolset to Ed as tools, with risk from `readOnlyHint` plus a
  host override list, and optional unprefixed names for single-server apps.
- `test-server`: a scriptable MCP server for tests.

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under MIT or Apache-2.0.
