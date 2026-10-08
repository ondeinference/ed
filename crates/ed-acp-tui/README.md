# ed-acp-tui

A terminal UI for any [Agent Client Protocol](https://agentclientprotocol.com) agent.

It is a regular ACP client: it launches the agent (usually the same binary with `--acp`) as a
subprocess and renders one session with ratatui, so it exercises the protocol path an editor
would. It serves `fs/read_text_file` and `fs/write_text_file` from local disk and answers
permission requests with `y` (allow), `a` (always), `n` (reject) or `r` (always reject).

## Copyright

© 2026 [Splitfire AB](https://5mb.app) ([Onde Inference](https://ondeinference.com)).
Licensed under MIT or Apache-2.0.
