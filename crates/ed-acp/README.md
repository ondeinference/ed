# ed-acp

An [Agent Client Protocol](https://agentclientprotocol.com) v1 server for Onde agents.

It is the part of an ACP agent that is the same for every product: `initialize` and version
negotiation, auth, durable sessions, the model picker, prompt content, the turn loop against Onde
Cloud (or any OpenAI API compatible endpoint) with streaming, tool approval, workspace tools routed through the client, and MCP servers
(stdio and streamable HTTP) through [`ed-mcp`](https://crates.io/crates/ed-mcp). A product
supplies a `Profile`: its name, prompt, tools, slash commands and built-in MCP servers.

Test an agent built on it with [`ed-acp-testkit`](https://crates.io/crates/ed-acp-testkit), and
drive it from a terminal with [`ed-acp-tui`](https://crates.io/crates/ed-acp-tui).

## Copyright

© 2026 [Onde Inference](https://ondeinference.com).
Licensed under MIT or Apache-2.0.
