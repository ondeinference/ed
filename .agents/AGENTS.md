# onde-ed – Agent Instructions

## What this repo is
`onde-ed` is a **Rust Cargo workspace** providing the embeddable agentic core
used across Onde products (macOS, iOS, Tauri, CLI).  The workspace is organised
into focused crates; do not merge them.

---

## Crate map

| Crate | Purpose |
|---|---|
| `ed-agent` | Core library: `Ed` struct, lifecycle, `send`/`stream`/`run`, tool approval, `EventSink`, `SessionStore` seams |
| `ed-acp` | ACP v1 server: `Profile` trait, `serve`/`serve_stdio`, sessions, LLM streaming, tool routing, MCP |
| `ed-mcp` | MCP integration over `rmcp`: tool namespacing (`mcp__<server>__<tool>`, max 64 bytes), `SharedServer`, `McpExecutor`, timeouts, cancellation |
| `ed-acp-tui` | ratatui TUI frontend for ACP agents over stdio |
| `ed-acp-testkit` | Mock Onde Cloud endpoint + conformance test suite |
| `ed-agent-ffi` | UniFFI bridge → Apple XCFramework (`FfiEd`, `FfiToolExecutor`, `FfiApprovalHandler`, `FfiEventListener`) |
| `ed-agent-tauri` | Tauri command bindings that emit events to the webview |

---

## Non-negotiable architecture rules

1. **One `Ed` instance, one engine, one model.**  `Ed` is `Send + Sync`, not
   `Clone`.  Never try to clone or share it across threads without `Arc`.

2. **`Profile` is the product seam.**  A product crate (or binary) implements
   `Profile` (`info`, `system_prompt`, `toolsets`, `builtin_mcp_servers`).
   `ed-acp` handles the ACP server, session management, and LLM glue.  Do not
   put product-specific logic inside `ed-acp`.

3. **Tool risk gates approval.**  `ToolRisk::ReadOnly` vs `ToolRisk::Mutating`
   determines whether the approval handler is invoked.  Per-session grants are
   tracked via `ApprovalDecision::AllowForSession`.  Do not bypass this gate.

4. **MCP tool names ≤ 64 bytes.**  Names are sanitised to
   `mcp__<server>__<tool>`.  Anything longer is truncated.  Do not assume the
   original tool name survived intact.

5. **FFI types are wrappers, not source of truth.**  `FfiEd` delegates to
   `Ed`; `FfiToolExecutor` bridges a foreign callback into the
   `ToolExecutor` trait.  Keep FFI types thin.

6. **Tauri and FFI are separate bindings.**  `ed-agent-tauri` and
   `ed-agent-ffi` both wrap `ed-agent` but must not depend on each other.

---

## LLM providers

Configured via `ed-acp/src/llm.rs`.  Three variants: `Onde`, `OpenAi`, `Local`.

| Env var | Effect |
|---|---|
| `<PREFIX>_PROVIDER` | `onde` / `openai` / `local` |
| `ONDE_API_KEY` | Cloud key for `Onde` provider |
| `OPENAI_API_KEY` | Key for `OpenAi` provider |

Default cloud model: `onde-kkk`.  
Default local model: `Qwen3-4B` (desktop) / `Qwen3-1.7B` (mobile).

`LlmEnv` drives config-dir, env-file loading, and variable namespacing per
product.  Do not hard-code env vars; always go through `LlmEnv`.

---

## Working in this repo

### Before changing anything
- Read the relevant `lib.rs` and the trait/struct you are modifying first.
- Check whether a public API is re-exported from `ed-agent`'s `lib.rs` before
  adding a new one; keep the public surface minimal.
- If touching `ed-agent-ffi`, remember it must compile for Apple targets
  (macOS + iOS); avoid anything that does not cross the FFI boundary cleanly.

### Making changes
- Keep crate boundaries: adding a dependency from a lower-level crate
  (`ed-agent`) on a higher-level one (`ed-acp`) is forbidden.
- New tool definitions belong in the crate that owns the domain, exposed via
  `ToolExecutor` or MCP server, not inlined into `Ed`.
- Prefer `async_trait` + `Arc<dyn Trait>` for seams; avoid generics on `Ed`
  itself.
- When adding an FFI type, add the matching `From<Ffi*>` / `From<*>` impls
  immediately so both sides stay in sync.

### After changes
- Run `cargo check --workspace` to catch compile errors across all crates.
- Run `cargo test --workspace` before committing.
- For FFI changes, note that a full XCFramework build requires the Apple SDK
  toolchain; CI handles that — local `cargo check` is sufficient to validate
  Rust correctness.

### Branches
- Base branch is **`development`**. Open all PRs against `development`, not `main`.
- Feature branches: `feature/<short-description>`.
- Stacked PRs are fine; always target the parent feature branch, not `development`, when stacking.

### Commit messages
- Scope to the affected crate(s): `ed-agent: …`, `ed-acp: …`, `ed-mcp: …`.
- Keep the subject line under 72 chars.
- Always end the commit with:
  ```
  Co-Authored-By: siGit Code v1.6.6-acp <noreply@sigit.si>
  ```

---

## Things to avoid
- Do not add `#[allow(unused)]` blanket suppressions — fix the unused item.
- Do not store `tokio::runtime::Runtime` inside an FFI object; use `block_on`
  only at the outermost FFI boundary.
- Do not panic inside async tasks — convert errors to `AgentError` and surface
  them through the event sink or return type.
- Do not hard-code model names anywhere outside `ed-acp/src/llm.rs`.
- Do not bypass `ToolRisk` approval for any mutating tool, even in tests — use
  an `AllowAll` test double instead.
