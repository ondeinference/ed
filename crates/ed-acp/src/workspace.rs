//! Workspace tools: `read_file`, `write_file`, `edit_file`, `list_directory` and
//! `run_command`, routed through the ACP client (`fs/*`, `terminal/*`) when it advertises
//! support and falling back to the local filesystem and `sh -c` otherwise. Every tool takes an
//! optional `root` for multi-root workspaces. Writes and commands ask for approval.

use std::path::Path;
use std::time::Duration;

use agent_client_protocol::ErrorCode;
use agent_client_protocol::schema::v1::{
    CreateTerminalRequest, Diff, KillTerminalRequest, ReadTextFileRequest, ReleaseTerminalRequest,
    Terminal, TerminalOutputRequest, ToolCallContent, ToolCallLocation, ToolCallUpdateFields,
    ToolKind, WaitForTerminalExitRequest, WriteTextFileRequest,
};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{
    DescribeCtx, MAX_OUTPUT_BYTES, ToolCtx, ToolOutcome, Toolset, absolutize, base_for,
    function_def, resolve_in, truncate,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// The workspace tools as a [`Toolset`].
#[derive(Debug, Default, Clone, Copy)]
pub struct WorkspaceTools;

const NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_directory",
    "run_command",
];

fn tool_definitions() -> Vec<Value> {
    let f = |name: &str, desc: &str, params: Value| function_def(name, desc, params);
    let root_prop = || {
        json!({
            "type": "string",
            "description": "Workspace root to act in: absolute path of a session root or a directory beneath one. Defaults to the primary working directory."
        })
    };
    vec![
        f(
            "read_file",
            "Read a text file. Paths may be relative to the working directory.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "line": { "type": "integer", "description": "1-based line to start from" },
                    "limit": { "type": "integer", "description": "Max lines to read" },
                    "root": root_prop()
                },
                "required": ["path"]
            }),
        ),
        f(
            "write_file",
            "Create or overwrite a file with the given content.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" }, "content": { "type": "string" }, "root": root_prop() },
                "required": ["path", "content"]
            }),
        ),
        f(
            "edit_file",
            "Replace an exact, unique occurrence of old_string with new_string in a file.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "root": root_prop()
                },
                "required": ["path", "old_string", "new_string"]
            }),
        ),
        f(
            "list_directory",
            "List entries of a directory (directories end with '/').",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Defaults to the working directory" }, "root": root_prop() }
            }),
        ),
        f(
            "run_command",
            "Run a shell command (sh -c) in the working directory and return its output.",
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" }, "root": root_prop() },
                "required": ["command"]
            }),
        ),
    ]
}

#[async_trait]
impl Toolset for WorkspaceTools {
    fn definitions(&self) -> Vec<Value> {
        tool_definitions()
    }

    fn handles(&self, name: &str) -> bool {
        NAMES.contains(&name)
    }

    fn describe(
        &self,
        name: &str,
        args: &Value,
        ctx: &DescribeCtx<'_>,
    ) -> (String, ToolKind, Vec<ToolCallLocation>) {
        let str_arg = |k: &str| args.get(k).and_then(Value::as_str);
        let base =
            base_for(ctx.cwd, ctx.roots, str_arg("root")).unwrap_or_else(|_| ctx.cwd.to_path_buf());
        // ACP v1: ToolCallLocation.path must be an absolute path.
        let path = str_arg("path").map(|p| absolutize(&resolve_in(&base, p)));
        let loc = path
            .clone()
            .map(ToolCallLocation::new)
            .into_iter()
            .collect();
        let shown = path
            .as_deref()
            .map_or_else(|| base.display().to_string(), |p| p.display().to_string());
        match name {
            "read_file" => (format!("Read {shown}"), ToolKind::Read, loc),
            "write_file" => (format!("Write {shown}"), ToolKind::Edit, loc),
            "edit_file" => (format!("Edit {shown}"), ToolKind::Edit, loc),
            "list_directory" => (format!("List {shown}"), ToolKind::Search, loc),
            "run_command" => {
                let cmd = str_arg("command").unwrap_or("");
                let where_ = if base == ctx.cwd {
                    String::new()
                } else {
                    format!(" in {}", base.display())
                };
                (format!("`{cmd}`{where_}"), ToolKind::Execute, vec![])
            }
            _ => (name.to_string(), ToolKind::Other, vec![]),
        }
    }

    async fn execute(
        &self,
        ctx: &ToolCtx,
        tool_call_id: &str,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome> {
        match name {
            "read_file" => ctx.read_file(args).await,
            "write_file" => ctx.write_file(tool_call_id, args).await,
            "edit_file" => ctx.edit_file(tool_call_id, args).await,
            "list_directory" => ctx.list_directory(args).await,
            "run_command" => ctx.run_command(tool_call_id, args).await,
            other => Err(anyhow!("unknown tool `{other}`")),
        }
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    line: Option<u32>,
    limit: Option<u32>,
    root: Option<String>,
}
#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
    root: Option<String>,
}
#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    root: Option<String>,
}
#[derive(Deserialize)]
struct ListArgs {
    path: Option<String>,
    root: Option<String>,
}
#[derive(Deserialize)]
struct CommandArgs {
    command: String,
    root: Option<String>,
}

impl ToolCtx {
    pub(crate) async fn read_file(&self, args: Value) -> Result<ToolOutcome> {
        let a: ReadArgs = serde_json::from_value(args)?;
        let path = self.resolve_with(a.root.as_deref(), &a.path)?;
        let text = if self.caps.fs.read_text_file {
            // ACP v1 fs/read_text_file requires an absolute path.
            let mut req = ReadTextFileRequest::new(self.session_id.clone(), absolutize(&path));
            if let Some(l) = a.line {
                req = req.line(l);
            }
            if let Some(l) = a.limit {
                req = req.limit(l);
            }
            self.connection
                .send_request(req)
                .block_task()
                .await?
                .content
        } else {
            let full = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("reading {}", path.display()))?;
            let skip = a.line.map_or(0, |l| l.saturating_sub(1) as usize);
            let take = a.limit.map_or(usize::MAX, |l| l as usize);
            full.lines()
                .skip(skip)
                .take(take)
                .collect::<Vec<_>>()
                .join("\n")
        };
        // Show the user a short summary; give the model the full text.
        let mut out = ToolOutcome::ok(format!("{} lines", text.lines().count()));
        out.text = truncate(text);
        Ok(out)
    }

    /// Read the current file contents for diffs/edits; `Ok(None)` if it doesn't exist.
    /// A client-fs read failure is an error, distinct from a missing file (ACP
    /// `ResourceNotFound`), so callers don't mistake a failed read for a new file.
    pub(crate) async fn read_existing(&self, path: &Path) -> Result<Option<String>> {
        if self.caps.fs.read_text_file {
            let req = ReadTextFileRequest::new(self.session_id.clone(), absolutize(path));
            match self.connection.send_request(req).block_task().await {
                Ok(r) => Ok(Some(r.content)),
                Err(e) if e.code == ErrorCode::ResourceNotFound => Ok(None),
                Err(e) => Err(anyhow!("client read of {} failed: {e}", path.display())),
            }
        } else {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => Ok(Some(content)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(anyhow!("reading {}: {e}", path.display())),
            }
        }
    }

    pub(crate) async fn write_text(&self, path: &Path, content: &str) -> Result<()> {
        if self.caps.fs.write_text_file {
            let req = WriteTextFileRequest::new(self.session_id.clone(), absolutize(path), content);
            self.connection.send_request(req).block_task().await?;
        } else {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(path, content).await?;
        }
        Ok(())
    }

    pub(crate) async fn write_file(&self, id: &str, args: Value) -> Result<ToolOutcome> {
        let a: WriteArgs = serde_json::from_value(args)?;
        let path = self.resolve_with(a.root.as_deref(), &a.path)?;
        let old = self.read_existing(&path).await?;
        // ACP v1: Diff.path must be an absolute file path.
        let diff = Diff::new(absolutize(&path), a.content.clone()).old_text(old);
        self.apply_edit(id, "write_file", &path, &a.content, diff)
            .await
    }

    pub(crate) async fn edit_file(&self, id: &str, args: Value) -> Result<ToolOutcome> {
        let a: EditArgs = serde_json::from_value(args)?;
        let path = self.resolve_with(a.root.as_deref(), &a.path)?;
        let old = self
            .read_existing(&path)
            .await?
            .ok_or_else(|| anyhow!("{} does not exist", path.display()))?;
        match old.matches(&a.old_string).count() {
            0 => bail!("old_string not found in {}", path.display()),
            1 => {}
            n => bail!(
                "old_string occurs {n} times in {}; make it unique",
                path.display()
            ),
        }
        let new = old.replacen(&a.old_string, &a.new_string, 1);
        let diff = Diff::new(absolutize(&path), new.clone()).old_text(old);
        self.apply_edit(id, "edit_file", &path, &new, diff).await
    }

    pub(crate) async fn apply_edit(
        &self,
        id: &str,
        tool: &str,
        path: &Path,
        content: &str,
        diff: Diff,
    ) -> Result<ToolOutcome> {
        let diff = ToolCallContent::from(diff);
        if !self.permit(id, tool, vec![diff.clone()]).await? {
            return Ok(ToolOutcome::err("User rejected this change."));
        }
        self.write_text(path, content).await?;
        Ok(ToolOutcome {
            text: format!("Wrote {}", path.display()),
            content: vec![diff],
            failed: false,
            locations: Vec::new(),
        })
    }

    pub(crate) async fn list_directory(&self, args: Value) -> Result<ToolOutcome> {
        let a: ListArgs = serde_json::from_value(args)?;
        let base = self.base_for(a.root.as_deref())?;
        let path = a
            .path
            .map_or_else(|| base.clone(), |p| resolve_in(&base, &p));
        let mut entries = Vec::new();
        let mut rd = tokio::fs::read_dir(&path)
            .await
            .with_context(|| format!("listing {}", path.display()))?;
        while let Some(e) = rd.next_entry().await? {
            let mut name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().await.is_ok_and(|t| t.is_dir()) {
                name.push('/');
            }
            entries.push(name);
        }
        entries.sort();
        Ok(ToolOutcome::ok(entries.join("\n")))
    }

    pub(crate) async fn run_command(&self, id: &str, args: Value) -> Result<ToolOutcome> {
        let a: CommandArgs = serde_json::from_value(args)?;
        let base = self.base_for(a.root.as_deref())?;
        if !self.permit(id, "run_command", vec![]).await? {
            return Ok(ToolOutcome::err("User rejected running this command."));
        }
        if self.caps.terminal {
            self.run_in_client_terminal(id, &a.command, &base).await
        } else {
            self.run_locally(&a.command, &base).await
        }
    }

    pub(crate) async fn run_in_client_terminal(
        &self,
        id: &str,
        command: &str,
        cwd: &Path,
    ) -> Result<ToolOutcome> {
        let sid = self.session_id.clone();
        let req = CreateTerminalRequest::new(sid.clone(), "sh")
            .args(vec!["-c".into(), command.into()])
            // ACP v1: the terminal cwd must be an absolute path.
            .cwd(absolutize(cwd))
            .output_byte_limit(MAX_OUTPUT_BYTES as u64);
        let terminal_id = self
            .connection
            .send_request(req)
            .block_task()
            .await?
            .terminal_id;
        // Embed the live terminal in the tool call so the user can watch it.
        let content = vec![ToolCallContent::Terminal(Terminal::new(
            terminal_id.clone(),
        ))];
        self.update(id, ToolCallUpdateFields::new().content(content.clone()))?;

        let wait = self
            .connection
            .send_request(WaitForTerminalExitRequest::new(
                sid.clone(),
                terminal_id.clone(),
            ))
            .block_task();
        let exit = tokio::select! {
            r = wait => Some(r?.exit_status),
            () = tokio::time::sleep(COMMAND_TIMEOUT) => None,
            () = self.cancel.cancelled() => None,
        };
        if exit.is_none() {
            let _ = self
                .connection
                .send_request(KillTerminalRequest::new(sid.clone(), terminal_id.clone()))
                .block_task()
                .await;
        }
        let output = self
            .connection
            .send_request(TerminalOutputRequest::new(sid.clone(), terminal_id.clone()))
            .block_task()
            .await?;
        let _ = self
            .connection
            .send_request(ReleaseTerminalRequest::new(sid, terminal_id))
            .block_task()
            .await;

        let status = match &exit {
            Some(s) => match (s.exit_code, &s.signal) {
                (Some(c), _) => format!("exit code {c}"),
                (None, Some(sig)) => format!("killed by {sig}"),
                _ => "exited".into(),
            },
            None if self.cancel.is_cancelled() => "cancelled".into(),
            None => format!("timed out after {}s", COMMAND_TIMEOUT.as_secs()),
        };
        let failed = !matches!(exit.as_ref().and_then(|s| s.exit_code), Some(0));
        let trunc = if output.truncated {
            "\n[output truncated]"
        } else {
            ""
        };
        Ok(ToolOutcome {
            text: format!("{}{trunc}\n[{status}]", output.output),
            content,
            failed,
            locations: Vec::new(),
        })
    }

    pub(crate) async fn run_locally(&self, command: &str, cwd: &Path) -> Result<ToolOutcome> {
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::select! {
            r = tokio::time::timeout(COMMAND_TIMEOUT, child) => match r {
                Ok(r) => r?,
                Err(_) => return Ok(ToolOutcome::err(format!("Command timed out after {}s", COMMAND_TIMEOUT.as_secs()))),
            },
            () = self.cancel.cancelled() => return Ok(ToolOutcome::err("Command cancelled")),
        };
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let status = out
            .status
            .code()
            .map_or_else(|| "killed by signal".into(), |c| format!("exit code {c}"));
        let text = format!("{}\n[{status}]", truncate(text));
        let mut outcome = ToolOutcome::ok(format!("```\n{text}\n```"));
        outcome.text = text;
        outcome.failed = !out.status.success();
        Ok(outcome)
    }
}
