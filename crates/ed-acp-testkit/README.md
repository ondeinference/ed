# ed-acp-testkit

Test support for Onde ACP agents, used as a dev-dependency.

- A mock Onde Cloud endpoint (streaming, tool calls, usage, `content_filter`).
- A raw JSON-RPC client that drives the real agent binary over stdio.
- The ACP registry validator's `initialize` probe.
- The ACP v1 conformance suite, adopted with one macro call in an integration test:
  `ed_acp_testkit::conformance_suite!(target);`

The scriptable MCP server for tests is in [`ed-mcp`](https://crates.io/crates/ed-mcp) behind its
`test-server` feature.

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under MIT or Apache-2.0.
