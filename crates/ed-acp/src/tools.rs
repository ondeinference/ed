//! The tool layer: what a tool gets ([`ToolCtx`]), what it returns ([`ToolOutcome`]), the
//! approval flow, ACP path rules, and dispatch to product [`Toolset`]s and MCP servers.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    ClientCapabilities, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, SessionId, SessionNotification, SessionUpdate, ToolCallContent,
    ToolCallLocation, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Client, ConnectionTo};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use ed_mcp::McpToolset;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{McpCall, Profile};

/// Cap on tool output sent to the model and on client terminal output.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// Everything a tool needs to act on behalf of one session.
pub struct ToolCtx {
    pub connection: ConnectionTo<Client>,
    pub session_id: SessionId,
    /// Primary working directory: the base for relative paths and `run_command`.
    pub cwd: PathBuf,
    /// Additional workspace roots the model may address with the `root` parameter.
    pub roots: Vec<PathBuf>,
    pub caps: ClientCapabilities,
    pub cancel: CancellationToken,
    /// Approve tool calls without asking: `--yolo`, or the session's `auto_approve` option.
    pub auto_approve: Arc<AtomicBool>,
    /// Tool names the user chose "always allow" for in this session.
    pub always_allowed: Arc<Mutex<HashSet<String>>>,
    /// Tool names the user chose "always reject" for in this session.
    pub always_rejected: Arc<Mutex<HashSet<String>>>,
    /// Tools from MCP servers available to this session.
    pub mcp: Arc<McpToolset>,
}

/// What a tool call produced.
pub struct ToolOutcome {
    /// Text returned to the model.
    pub text: String,
    /// Rich content shown to the user in the client.
    pub content: Vec<ToolCallContent>,
    pub failed: bool,
    /// Files the tool produced or touched, surfaced as tool-call locations.
    pub locations: Vec<PathBuf>,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            content: vec![ToolCallContent::from(text.clone())],
            text,
            failed: false,
            locations: Vec::new(),
        }
    }
    pub fn err(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            content: vec![ToolCallContent::from(text.clone())],
            text,
            failed: true,
            locations: Vec::new(),
        }
    }
}

/// An OpenAI `tools` entry.
pub fn function_def(name: &str, desc: &str, params: Value) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": desc, "parameters": params } })
}

/// The session's directories, for describing a tool call without running it (also used when
/// `session/load` replays history).
pub struct DescribeCtx<'a> {
    pub cwd: &'a Path,
    pub roots: &'a [PathBuf],
}

/// A group of tools a product offers to the model.
#[async_trait]
pub trait Toolset: Send + Sync {
    /// OpenAI `tools` entries, built with [`function_def`].
    fn definitions(&self) -> Vec<Value>;
    /// Whether `name` is one of this toolset's tools.
    fn handles(&self, name: &str) -> bool;
    /// Title, kind and affected locations shown in the client when the call starts. Locations
    /// must be absolute ([`absolutize`]).
    fn describe(
        &self,
        name: &str,
        args: &Value,
        ctx: &DescribeCtx<'_>,
    ) -> (String, ToolKind, Vec<ToolCallLocation>);
    /// Run the tool. A mutating tool asks first with [`ToolCtx::permit`]. An `Err` is shown to
    /// the model as `Error: …` and the call is marked failed.
    async fn execute(
        &self,
        ctx: &ToolCtx,
        tool_call_id: &str,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome>;
}

/// All tool definitions for one session, in OpenAI `tools` format: product toolsets first,
/// then MCP tools sorted by name.
pub fn definitions(toolsets: &[Arc<dyn Toolset>], mcp: &McpToolset) -> Value {
    let mut defs: Vec<Value> = toolsets.iter().flat_map(|t| t.definitions()).collect();
    defs.extend(
        mcp.tools()
            .into_iter()
            .map(|t| function_def(&t.name, &t.description, t.parameters.clone())),
    );
    Value::Array(defs)
}

/// Title, kind and affected locations shown in the client when the call starts.
pub fn describe(
    toolsets: &[Arc<dyn Toolset>],
    cwd: &Path,
    roots: &[PathBuf],
    name: &str,
    args: &Value,
) -> (String, ToolKind, Vec<ToolCallLocation>) {
    if let Some((server, tool)) = ed_mcp::split_qualified(name) {
        let loc = ["input", "path", "file"]
            .iter()
            .filter_map(|k| args.get(*k).and_then(Value::as_str))
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| ToolCallLocation::new(absolutize(&p)))
            .collect();
        return (format!("{server}: {tool}"), ToolKind::Other, loc);
    }
    let ctx = DescribeCtx { cwd, roots };
    match toolsets.iter().find(|t| t.handles(name)) {
        Some(t) => t.describe(name, args, &ctx),
        None => (name.to_string(), ToolKind::Other, vec![]),
    }
}

/// Run one tool call: a product toolset's, or an MCP server's. Never fails; errors become a
/// failed outcome the model sees.
pub async fn execute(
    ctx: &ToolCtx,
    toolsets: &[Arc<dyn Toolset>],
    profile: &dyn Profile,
    tool_call_id: &str,
    name: &str,
    args: Value,
) -> ToolOutcome {
    let result = if ed_mcp::split_qualified(name).is_some() {
        execute_mcp(ctx, profile, tool_call_id, name, args).await
    } else if let Some(t) = toolsets.iter().find(|t| t.handles(name)) {
        t.execute(ctx, tool_call_id, name, args).await
    } else {
        Err(anyhow!("unknown tool `{name}`"))
    };
    result.unwrap_or_else(|e| ToolOutcome::err(format!("Error: {e:#}")))
}

async fn execute_mcp(
    ctx: &ToolCtx,
    profile: &dyn Profile,
    tool_call_id: &str,
    name: &str,
    args: Value,
) -> Result<ToolOutcome> {
    let Some(tool) = ctx.mcp.tool(name) else {
        bail!("unknown tool `{name}`");
    };
    if !tool.read_only {
        let call = McpCall {
            tool,
            args: &args,
            connection: ctx.mcp.connection(name),
        };
        let preview = match profile.mcp_permission_preview(call).await {
            Some(text) => text,
            None => default_mcp_preview(tool, &args),
        };
        let preview = vec![ToolCallContent::from(preview)];
        if !ctx.permit(tool_call_id, name, preview).await? {
            return Ok(ToolOutcome::err("User rejected running this tool."));
        }
    }
    let on_progress = |line: String| {
        let _ = ctx.update(
            tool_call_id,
            ToolCallUpdateFields::new().content(vec![ToolCallContent::from(line)]),
        );
    };
    let out = ctx.mcp.call(name, args, &ctx.cancel, &on_progress).await?;
    let mut outcome = if out.failed {
        ToolOutcome::err(truncate(out.text))
    } else {
        ToolOutcome::ok(truncate(out.text))
    };
    if let Some(structured) = &out.structured {
        outcome.locations = profile
            .mcp_result_locations(tool, structured)
            .into_iter()
            .filter(|p| p.is_absolute())
            .map(|p| absolutize(&p))
            .collect();
    }
    Ok(outcome)
}

/// The permission prompt for an MCP tool when the product has nothing better to say.
fn default_mcp_preview(tool: &ed_mcp::McpTool, args: &Value) -> String {
    let mut text = format!("Run `{}` on the {} MCP server.", tool.tool, tool.server);
    if args.as_object().is_some_and(|o| !o.is_empty()) {
        let shown = serde_json::to_string_pretty(args).unwrap_or_default();
        text.push_str(&format!("\nArguments:\n```json\n{}\n```", truncate(shown)));
    }
    text
}

/// The base directory for a tool call's `root` parameter: a workspace root or any directory
/// beneath one (so the model can pass e.g. `root/crate/src`). Paths that escape a root via
/// `..` are rejected.
pub fn base_for(cwd: &Path, roots: &[PathBuf], root: Option<&str>) -> Result<PathBuf> {
    let Some(root) = root else {
        return Ok(cwd.to_path_buf());
    };
    // Lexical normalization resolves `..`, so `root/../elsewhere` can't pass the prefix check.
    let candidate = normalize(Path::new(root));
    let all_roots = all_roots(cwd, roots);
    for ws_root in &all_roots {
        if candidate == *ws_root {
            return Ok(ws_root.clone());
        }
        // Allow any directory below a root (e.g. a crate dir like `root/crate/src`),
        // as long as it doesn't escape the root via `..`.
        if let Ok(rest) = candidate.strip_prefix(ws_root)
            && rest.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return Ok(candidate);
        }
    }
    let known = all_roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "unknown root `{}` (workspace roots: {known})",
        candidate.display()
    )
}

fn all_roots(cwd: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
    std::iter::once(cwd.to_path_buf())
        .chain(roots.iter().filter(|r| *r != cwd).cloned())
        .collect()
}

impl ToolCtx {
    pub fn base_for(&self, root: Option<&str>) -> Result<PathBuf> {
        base_for(&self.cwd, &self.roots, root)
    }

    pub fn resolve_with(&self, root: Option<&str>, path: &str) -> Result<PathBuf> {
        Ok(resolve_in(&self.base_for(root)?, path))
    }

    /// Ask the user for permission. Returns `Ok(false)` if rejected or cancelled. Honours
    /// "always allow" and "always reject" choices for `tool` and the session's auto-approve.
    pub async fn permit(
        &self,
        id: &str,
        tool: &str,
        content: Vec<ToolCallContent>,
    ) -> Result<bool> {
        if self.always_rejected.lock().unwrap().contains(tool) {
            return Ok(false);
        }
        if self.auto_approve.load(Ordering::Relaxed)
            || self.always_allowed.lock().unwrap().contains(tool)
        {
            return Ok(true);
        }
        let mut fields = ToolCallUpdateFields::new();
        if !content.is_empty() {
            fields = fields.content(content);
        }
        let req = RequestPermissionRequest::new(
            self.session_id.clone(),
            ToolCallUpdate::new(id.to_string(), fields),
            vec![
                PermissionOption::new("allow_once", "Allow", PermissionOptionKind::AllowOnce),
                PermissionOption::new(
                    "allow_always",
                    "Always allow",
                    PermissionOptionKind::AllowAlways,
                ),
                PermissionOption::new("reject_once", "Reject", PermissionOptionKind::RejectOnce),
                PermissionOption::new(
                    "reject_always",
                    "Always reject",
                    PermissionOptionKind::RejectAlways,
                ),
            ],
        );
        let resp = tokio::select! {
            r = self.connection.send_request(req).block_task() => r?,
            () = self.cancel.cancelled() => return Ok(false),
        };
        Ok(match resp.outcome {
            RequestPermissionOutcome::Selected(sel) => match &*sel.option_id.0 {
                "allow_always" => {
                    self.always_allowed.lock().unwrap().insert(tool.to_string());
                    true
                }
                "allow_once" => true,
                "reject_always" => {
                    self.always_rejected
                        .lock()
                        .unwrap()
                        .insert(tool.to_string());
                    false
                }
                _ => false,
            },
            _ => false,
        })
    }

    pub fn update(&self, id: &str, fields: ToolCallUpdateFields) -> Result<()> {
        let update = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(id.to_string(), fields));
        self.connection
            .send_notification(SessionNotification::new(self.session_id.clone(), update))?;
        Ok(())
    }
}

/// Join `path` onto `base`, keeping absolute paths as-is.
pub fn resolve_in(base: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Cut `s` to [`MAX_OUTPUT_BYTES`] on a character boundary, marking the cut.
pub fn truncate(mut s: String) -> String {
    if s.len() > MAX_OUTPUT_BYTES {
        let mut cut = MAX_OUTPUT_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n[truncated]");
    }
    s
}

/// Make `path` absolute and free of `.`/`..` components, as the ACP v1 spec requires
/// absolute paths everywhere. Existing paths are canonicalized (resolving symlinks
/// and giving the client the true on-disk location); paths that don't exist yet
/// are normalized lexically with the existing parent canonicalized when possible.
pub fn absolutize(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    // Not on disk (yet): normalize lexically, anchoring at the canonicalized parent.
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let base = parent.and_then(|p| std::fs::canonicalize(p).ok());
    let file_name = path.file_name().map(|f| f.to_os_string());
    match (base, file_name) {
        (Some(mut b), Some(f)) => {
            b.push(f);
            b
        }
        _ => normalize(path),
    }
}

/// Lexically normalize a path: resolve `.` and `..` without touching the filesystem.
/// Absolute paths stay absolute; relative paths are made absolute against the cwd.
pub fn normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for c in absolute.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str().to_os_string()),
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod fs_path_tests {
    use super::*;

    #[test]
    fn normalize_resolves_dot_dot_lexically() {
        assert_eq!(normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/a/./b")), PathBuf::from("/a/b"));
        assert_eq!(normalize(Path::new("/a/b/..")), PathBuf::from("/a"));
    }

    #[test]
    fn normalize_makes_relative_paths_absolute() {
        let n = normalize(Path::new("src/../README.md"));
        assert!(n.is_absolute());
        assert!(n.ends_with("README.md"));
        assert!(!n.components().any(|c| c == Component::ParentDir));
    }

    #[test]
    fn absolutize_canonicalizes_existing_paths() {
        let n = absolutize(&std::env::temp_dir());
        assert!(n.is_absolute());
        assert!(!n.components().any(|c| c == Component::ParentDir));
    }

    #[test]
    fn absolutize_normalizes_missing_paths() {
        let n = absolutize(Path::new("/definitely/does/not/../exist.txt"));
        assert_eq!(n, PathBuf::from("/definitely/does/exist.txt"));
    }

    #[test]
    fn base_for_rejects_escapes_and_accepts_subdirs() {
        let cwd = PathBuf::from("/ws/a");
        let roots = vec![PathBuf::from("/ws/b")];
        assert_eq!(base_for(&cwd, &roots, None).unwrap(), cwd);
        assert_eq!(
            base_for(&cwd, &roots, Some("/ws/b/sub/../x")).unwrap(),
            PathBuf::from("/ws/b/x")
        );
        assert!(base_for(&cwd, &roots, Some("/ws/b/../c")).is_err());
        assert!(base_for(&cwd, &roots, Some("/etc")).is_err());
    }
}
