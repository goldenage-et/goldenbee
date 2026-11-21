//! Opt-in generic file and search tools.
//!
//! File semantics come from the pinned native `adk_devtools` tools whenever
//! ADK supplies one. The runtime adds only what ADK does not provide for a
//! local workspace:
//!
//! - `multi_edit`: atomic sequential exact-string edits in one write, matching
//!   the community `multi_edit` contract;
//! - `apply_patch`: the Codex `*** Begin Patch` format, including add, update,
//!   delete, and move operations.
//!
//! Every tool is wrapped by [`ToolKernel`](super::kernel::ToolKernel) for
//! capability gating, timeout, cancellation, bounded output, evidence, and
//! audit.

use super::kernel::{ToolDescriptor, ToolEffect, ToolKernel, ToolRisk};
use adk_devtools::{EditFileTool, GlobTool, GrepTool, ReadFileTool, Workspace, WriteFileTool};
use adk_rust::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One generic tool a Worker Package can opt into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuiltinTool {
    /// ADK `read_file`.
    ReadFile,
    /// ADK `write_file`.
    WriteFile,
    /// ADK `edit_file`.
    EditFile,
    /// Runtime `multi_edit`, because ADK has no multi-edit tool.
    MultiEdit,
    /// Runtime `apply_patch`, because ADK exposes Codex patches only through its
    /// provider built-in declaration and the separate sandbox-runner session.
    ApplyPatch,
    /// ADK `glob`.
    Glob,
    /// ADK `grep`.
    Grep,
    /// Foreground contained command with streamed output.
    Shell,
    /// Interactive contained PTY session.
    Pty,
    /// Contained command started in the background.
    BackgroundProcess,
    /// Observe one owned background process.
    ProcessWait,
    /// Terminate one owned background process.
    ProcessKill,
    /// Bounded HTTP fetch under a destination policy.
    WebFetch,
    /// Bounded search through an operator-configured endpoint.
    WebSearch,
    /// Return a typed image descriptor for a workspace file.
    ViewImage,
    /// Persist and publish the per-task plan.
    WriteTodos,
    /// Ask the caller a question through the approval channel.
    AskUser,
}

/// Ownership family of a generic built-in capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCategory {
    /// Workspace file and search tools.
    File,
    /// Contained process and PTY tools.
    Process,
    /// Network tools under a destination policy.
    Web,
    /// Image, plan, and user-question tools.
    Interaction,
}

impl BuiltinTool {
    /// Every generic file/search capability.
    pub const FILES: [BuiltinTool; 7] = [
        Self::ReadFile,
        Self::WriteFile,
        Self::EditFile,
        Self::MultiEdit,
        Self::ApplyPatch,
        Self::Glob,
        Self::Grep,
    ];

    /// Every contained process and PTY capability.
    pub const PROCESS: [BuiltinTool; 5] = [
        Self::Shell,
        Self::Pty,
        Self::BackgroundProcess,
        Self::ProcessWait,
        Self::ProcessKill,
    ];

    /// Every network capability.
    pub const WEB: [BuiltinTool; 2] = [Self::WebFetch, Self::WebSearch];

    /// Every image, plan, and user-question capability.
    pub const INTERACTION: [BuiltinTool; 3] = [Self::ViewImage, Self::WriteTodos, Self::AskUser];

    /// Every generic capability.
    pub const ALL: [BuiltinTool; 17] = [
        Self::ReadFile,
        Self::WriteFile,
        Self::EditFile,
        Self::MultiEdit,
        Self::ApplyPatch,
        Self::Glob,
        Self::Grep,
        Self::Shell,
        Self::Pty,
        Self::BackgroundProcess,
        Self::ProcessWait,
        Self::ProcessKill,
        Self::WebFetch,
        Self::WebSearch,
        Self::ViewImage,
        Self::WriteTodos,
        Self::AskUser,
    ];

    /// Stable tool name exposed to the model.
    pub fn id(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::WriteFile => "write_file",
            Self::EditFile => "edit_file",
            Self::MultiEdit => "multi_edit",
            Self::ApplyPatch => "apply_patch",
            Self::Glob => "glob",
            Self::Grep => "grep",
            Self::Shell => "shell",
            Self::Pty => "pty",
            Self::BackgroundProcess => "background_process",
            Self::ProcessWait => "process_wait",
            Self::ProcessKill => "process_kill",
            Self::WebFetch => "web_fetch",
            Self::WebSearch => "web_search",
            Self::ViewImage => "view_image",
            Self::WriteTodos => "write_todos",
            Self::AskUser => "ask_user",
        }
    }

    /// Permission exercised by the tool.
    pub fn permission(self) -> &'static str {
        match self {
            Self::ReadFile | Self::Glob | Self::Grep | Self::ViewImage => "workspace:read",
            Self::WriteFile | Self::EditFile | Self::MultiEdit | Self::ApplyPatch => {
                "workspace:write"
            }
            Self::Shell | Self::Pty | Self::BackgroundProcess | Self::ProcessKill => {
                "process:execute"
            }
            Self::ProcessWait => "process:read",
            Self::WebFetch => "web:fetch",
            Self::WebSearch => "web:search",
            Self::WriteTodos => "plan:write",
            Self::AskUser => "user:ask",
        }
    }

    /// Owning capability family.
    pub fn category(self) -> BuiltinCategory {
        match self {
            Self::ReadFile
            | Self::WriteFile
            | Self::EditFile
            | Self::MultiEdit
            | Self::ApplyPatch
            | Self::Glob
            | Self::Grep => BuiltinCategory::File,
            Self::Shell
            | Self::Pty
            | Self::BackgroundProcess
            | Self::ProcessWait
            | Self::ProcessKill => BuiltinCategory::Process,
            Self::WebFetch | Self::WebSearch => BuiltinCategory::Web,
            Self::ViewImage | Self::WriteTodos | Self::AskUser => BuiltinCategory::Interaction,
        }
    }

    /// Whether the tool requires an explicit package sandbox policy.
    pub fn requires_sandbox(self) -> bool {
        self.category() == BuiltinCategory::Process
    }
}

/// Build the registered tool set for one configured Worker.
///
/// Disabled capabilities are not returned, so they are neither advertised nor
/// bound to an agent. The kernel gate remains as the second barrier if a model
/// requests the disabled name directly.
pub fn tools(kernel: &ToolKernel) -> Vec<(String, Arc<dyn Tool>, Vec<String>)> {
    let Some(workspace) = kernel.devtools_workspace() else {
        return Vec::new();
    };
    let mut tools = Vec::new();
    for builtin in BuiltinTool::FILES {
        if !kernel.is_enabled(builtin.id()) {
            continue;
        }
        let id = builtin.id().to_string();
        let descriptor = descriptor(builtin);
        let inner: Arc<dyn Tool> = match builtin {
            BuiltinTool::ReadFile => Arc::new(ReadFileTool::new(workspace.clone())),
            BuiltinTool::WriteFile => Arc::new(WriteFileTool::new(workspace.clone())),
            BuiltinTool::EditFile => Arc::new(EditFileTool::new(workspace.clone())),
            BuiltinTool::MultiEdit => Arc::new(MultiEditTool::new(workspace.clone())),
            BuiltinTool::ApplyPatch => Arc::new(ApplyPatchTool::new(workspace.clone())),
            BuiltinTool::Glob => Arc::new(GlobTool::new(workspace.clone())),
            BuiltinTool::Grep => Arc::new(GrepTool::new(workspace.clone())),
            _ => continue,
        };
        tools.push((
            id,
            kernel.wrap(inner, descriptor),
            vec![builtin.permission().to_string()],
        ));
    }
    tools
}

fn descriptor(builtin: BuiltinTool) -> ToolDescriptor {
    let mut descriptor = ToolDescriptor::new(builtin.permission())
        .builtin()
        .typed_errors();
    match builtin {
        BuiltinTool::ReadFile | BuiltinTool::Glob | BuiltinTool::Grep => {
            descriptor = descriptor.risk(ToolRisk::Low).effect(ToolEffect::None);
        }
        BuiltinTool::WriteFile | BuiltinTool::EditFile | BuiltinTool::MultiEdit => {
            descriptor = descriptor
                .risk(ToolRisk::Medium)
                .effect(ToolEffect::FileWrite)
                .paths(path_argument);
        }
        BuiltinTool::ApplyPatch => {
            descriptor = descriptor
                .risk(ToolRisk::High)
                .effect(ToolEffect::FileWrite)
                .paths(patch_paths);
        }
        _ => {
            descriptor = descriptor.risk(ToolRisk::Medium).effect(ToolEffect::None);
        }
    }
    descriptor
}

fn path_argument(args: &Value) -> Vec<String> {
    args.get("path")
        .and_then(Value::as_str)
        .map(|path| vec![path.to_string()])
        .unwrap_or_default()
}

fn patch_paths(args: &Value) -> Vec<String> {
    let Some(patch) = args.get("patch").and_then(Value::as_str) else {
        return Vec::new();
    };
    patch
        .lines()
        .filter_map(|line| {
            for prefix in [
                "*** Add File: ",
                "*** Update File: ",
                "*** Delete File: ",
                "*** Move to: ",
            ] {
                if let Some(path) = line.strip_prefix(prefix) {
                    return Some(path.trim().to_string());
                }
            }
            None
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditOperation {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

/// Apply several exact-string edits to one file as one atomic write.
pub struct MultiEditTool {
    workspace: Workspace,
}

impl MultiEditTool {
    fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit"
    }

    fn description(&self) -> &str {
        "Apply multiple exact-string edits to one file in a single atomic write. \
         Edits are applied in order; each edit sees the result of the previous one. \
         If any edit fails, none are written."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path relative to the workspace root."
                },
                "edits": {
                    "type": "array",
                    "description": "Sequential edits to apply to the file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {
                                "type": "string",
                                "description": "Exact text to replace."
                            },
                            "new_string": {
                                "type": "string",
                                "description": "Replacement text. Must differ from old_string."
                            },
                            "replace_all": {
                                "type": "boolean",
                                "description": "Replace every occurrence (default false)."
                            }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path", "edits"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.is_writable() {
            return Err(AdkError::tool(
                "workspace is read-only; multi_edit is not permitted",
            ));
        }
        let path = require_str(&args, "path")?;
        let edits: Vec<EditOperation> = serde_json::from_value(
            args.get("edits")
                .cloned()
                .ok_or_else(|| AdkError::tool("missing required argument 'edits'"))?,
        )
        .map_err(|error| AdkError::tool(format!("invalid edits: {error}")))?;
        if edits.is_empty() {
            return Err(AdkError::tool("edits must contain at least one operation"));
        }
        let resolved = self.workspace.resolve(&path)?;
        if !resolved.is_file() {
            return Ok(json!({
                "error": "tool.not_found",
                "message": format!("file '{}' does not exist", path),
                "path": path,
            }));
        }
        let original = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|error| AdkError::tool(format!("cannot read '{path}': {error}")))?;
        let mut updated = original;
        let mut replacements = 0usize;
        for (index, edit) in edits.iter().enumerate() {
            if edit.old_string.is_empty() {
                return Err(AdkError::tool(format!(
                    "edit {index} has an empty old_string"
                )));
            }
            if edit.old_string == edit.new_string {
                return Err(AdkError::tool(format!(
                    "edit {index} has identical old_string and new_string"
                )));
            }
            let occurrences = updated.matches(&edit.old_string).count();
            if occurrences == 0 {
                return Ok(json!({
                    "error": "tool.edit_no_match",
                    "message": format!("edit {index} found no occurrence of old_string"),
                    "path": path,
                    "edit": index,
                }));
            }
            if occurrences > 1 && !edit.replace_all {
                return Ok(json!({
                    "error": "tool.edit_ambiguous",
                    "message": format!(
                        "edit {index} matches {occurrences} times; set replace_all=true or provide a more specific string"
                    ),
                    "path": path,
                    "edit": index,
                    "occurrences": occurrences,
                }));
            }
            updated = if edit.replace_all {
                updated.replace(&edit.old_string, &edit.new_string)
            } else {
                updated.replacen(&edit.old_string, &edit.new_string, 1)
            };
            replacements += if edit.replace_all { occurrences } else { 1 };
        }
        let bytes = updated.as_bytes().to_vec();
        let digest = common::bytes_digest(&bytes);
        write_contained(self.workspace.root().to_path_buf(), resolved.clone(), bytes).await?;
        Ok(json!({
            "path": self.workspace.display(&resolved),
            "edits": edits.len(),
            "replacements": replacements,
            "bytes_written": updated.len(),
            "digest": digest,
            "message": format!(
                "Applied {} edit(s) to {}",
                edits.len(),
                self.workspace.display(&resolved)
            ),
        }))
    }
}

#[derive(Debug, Clone)]
enum PatchOp {
    Add {
        path: String,
        content: String,
    },
    Update {
        path: String,
        destination: Option<String>,
        hunks: Vec<PatchHunk>,
    },
    Delete {
        path: String,
    },
}

#[derive(Debug, Clone)]
struct PatchHunk {
    lines: Vec<PatchLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatchLine {
    Context(String),
    Remove(String),
    Add(String),
}

/// Apply a Codex-style `*** Begin Patch` document.
///
/// Supported operations are `*** Add File:`, `*** Update File:` (with optional
/// `*** Move to:`), and `*** Delete File:`. Update chunks are separated by
/// `@@`; context, removal, and addition lines are matched exactly.
pub struct ApplyPatchTool {
    workspace: Workspace,
}

impl ApplyPatchTool {
    fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a patch document to the workspace. Use `*** Begin Patch`, then \
         `*** Add File:`, `*** Update File:` (optionally `*** Move to:`), or \
         `*** Delete File:`. Update chunks use `@@` with context, `-`, and `+` lines."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Patch document in the Codex apply_patch format."
                }
            },
            "required": ["patch"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.is_writable() {
            return Err(AdkError::tool(
                "workspace is read-only; apply_patch is not permitted",
            ));
        }
        let patch = require_str(&args, "patch")?;
        let operations = parse_patch(&patch).map_err(AdkError::tool)?;
        let mut writes: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        let mut removals: Vec<PathBuf> = Vec::new();
        let mut files = Vec::new();
        for operation in &operations {
            match operation {
                PatchOp::Add { path, content } => {
                    let resolved = self.workspace.resolve(path)?;
                    writes.push((resolved.clone(), content.as_bytes().to_vec()));
                    files.push(json!({
                        "path": self.workspace.display(&resolved),
                        "change": "add",
                        "bytes": content.len(),
                    }));
                }
                PatchOp::Delete { path } => {
                    let resolved = self.workspace.resolve(path)?;
                    if !resolved.is_file() {
                        return Ok(json!({
                            "error": "tool.not_found",
                            "message": format!("file '{path}' does not exist"),
                            "path": path,
                        }));
                    }
                    removals.push(resolved.clone());
                    files.push(json!({
                        "path": self.workspace.display(&resolved),
                        "change": "delete",
                    }));
                }
                PatchOp::Update {
                    path,
                    destination,
                    hunks,
                } => {
                    let resolved = self.workspace.resolve(path)?;
                    let original = tokio::fs::read_to_string(&resolved)
                        .await
                        .map_err(|error| {
                            AdkError::tool(format!("cannot read '{path}': {error}"))
                        })?;
                    let updated = apply_hunks(&original, hunks).map_err(|message| {
                        AdkError::tool(format!("patch failed for '{path}': {message}"))
                    })?;
                    let target = match destination {
                        Some(destination) => self.workspace.resolve(destination)?,
                        None => resolved.clone(),
                    };
                    writes.push((target.clone(), updated.as_bytes().to_vec()));
                    if destination.is_some() && target != resolved {
                        removals.push(resolved.clone());
                    }
                    files.push(json!({
                        "path": self.workspace.display(&target),
                        "change": if destination.is_some() { "move" } else { "modify" },
                        "bytes": updated.len(),
                    }));
                }
            }
        }
        for (path, bytes) in &writes {
            write_contained(
                self.workspace.root().to_path_buf(),
                path.clone(),
                bytes.clone(),
            )
            .await?;
        }
        for path in &removals {
            tokio::fs::remove_file(path)
                .await
                .map_err(|error| AdkError::tool(format!("cannot remove file: {error}")))?;
        }
        Ok(json!({
            "files": files,
            "count": files.len(),
            "message": format!("Applied patch to {} file(s)", files.len()),
        }))
    }
}

fn require_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AdkError::tool(format!("missing required string argument '{key}'")))
}

fn parse_patch(text: &str) -> std::result::Result<Vec<PatchOp>, String> {
    let mut lines = text.lines().peekable();
    let mut operations = Vec::new();
    let mut started = false;
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        if line == "*** Begin Patch" {
            started = true;
            continue;
        }
        if line == "*** End Patch" {
            started = false;
            continue;
        }
        if !started {
            return Err("patch must start with '*** Begin Patch'".into());
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let mut content = String::new();
            while let Some(next) = lines.peek() {
                if next.starts_with("*** ") {
                    break;
                }
                let Some(next) = lines.next() else { break };
                let Some(text) = next.strip_prefix('+') else {
                    return Err(format!("add file line must start with '+': {next}"));
                };
                content.push_str(text);
                content.push('\n');
            }
            operations.push(PatchOp::Add {
                path: path.trim().to_string(),
                content,
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            operations.push(PatchOp::Delete {
                path: path.trim().to_string(),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let mut destination = None;
            if let Some(next) = lines.peek() {
                if let Some(target) = next.strip_prefix("*** Move to: ") {
                    destination = Some(target.trim().to_string());
                    lines.next();
                }
            }
            let mut hunks = Vec::new();
            while let Some(next) = lines.peek() {
                if next.starts_with("*** ") {
                    break;
                }
                let Some(next) = lines.next() else { break };
                if next == "@@" || next.starts_with("@@ ") {
                    let mut hunk = PatchHunk { lines: Vec::new() };
                    while let Some(candidate) = lines.peek() {
                        if *candidate == "*** End of File" {
                            lines.next();
                            break;
                        }
                        if candidate.starts_with("@@") || candidate.starts_with("*** ") {
                            break;
                        }
                        let Some(candidate) = lines.next() else { break };
                        if candidate == "\\ No newline at end of file" {
                            continue;
                        }
                        let mut chars = candidate.chars();
                        let prefix = chars.next().unwrap_or(' ');
                        let text = chars.as_str().to_string();
                        match prefix {
                            ' ' => hunk.lines.push(PatchLine::Context(text)),
                            '-' => hunk.lines.push(PatchLine::Remove(text)),
                            '+' => hunk.lines.push(PatchLine::Add(text)),
                            _ => return Err(format!("invalid patch line '{candidate}'")),
                        }
                    }
                    hunks.push(hunk);
                } else if next.trim().is_empty() {
                    continue;
                } else {
                    return Err(format!("unexpected update line '{next}'"));
                }
            }
            operations.push(PatchOp::Update {
                path: path.trim().to_string(),
                destination,
                hunks,
            });
            continue;
        }
        return Err(format!("unexpected patch line '{line}'"));
    }
    if started {
        return Err("patch is missing '*** End Patch'".into());
    }
    if operations.is_empty() {
        return Err("patch contains no file operations".into());
    }
    Ok(operations)
}

fn apply_hunks(original: &str, hunks: &[PatchHunk]) -> std::result::Result<String, String> {
    let trailing = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    let mut cursor = 0usize;
    for hunk in hunks {
        let old: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(text) | PatchLine::Remove(text) => Some(text.as_str()),
                PatchLine::Add(_) => None,
            })
            .collect();
        let new: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(text) | PatchLine::Add(text) => Some(text.clone()),
                PatchLine::Remove(_) => None,
            })
            .collect();
        let position = if old.is_empty() {
            cursor
        } else {
            find_lines(&lines, &old, cursor)
                .ok_or_else(|| format!("context did not match at or after line {}", cursor + 1))?
        };
        lines.splice(position..position + old.len(), new);
        cursor = position
            + hunk
                .lines
                .iter()
                .filter(|line| !matches!(line, PatchLine::Remove(_)))
                .count();
    }
    let mut output = lines.join("\n");
    if trailing || !output.is_empty() {
        output.push('\n');
    }
    Ok(output)
}

fn find_lines(lines: &[String], needle: &[&str], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(lines.len()));
    }
    if needle.len() > lines.len() {
        return None;
    }
    (from..=lines.len() - needle.len()).find(|&index| {
        lines[index..index + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(line, expected)| line == expected)
    })
}

async fn write_contained(root: PathBuf, path: PathBuf, bytes: Vec<u8>) -> Result<()> {
    tokio::task::spawn_blocking(move || write_contained_sync(&root, &path, &bytes))
        .await
        .map_err(|error| AdkError::tool(format!("write task failed: {error}")))?
        .map_err(AdkError::tool)
}

fn write_contained_sync(root: &Path, path: &Path, bytes: &[u8]) -> std::result::Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("path '{}' has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create '{}': {error}", parent.display()))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("cannot resolve '{}': {error}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        return Err(format!(
            "path '{}' escapes the workspace root",
            path.display()
        ));
    }
    let target = canonical_parent.join(
        path.file_name()
            .ok_or_else(|| format!("path '{}' has no file name", path.display()))?,
    );
    let target = if std::fs::symlink_metadata(&target)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        let resolved = target
            .canonicalize()
            .map_err(|error| format!("cannot resolve symlink: {error}"))?;
        if !resolved.starts_with(root) {
            return Err(format!(
                "path '{}' escapes the workspace root",
                path.display()
            ));
        }
        resolved
    } else {
        target
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&target)
        .map_err(|error| format!("cannot open '{}': {error}", target.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write '{}': {error}", target.display()))?;
    file.flush()
        .map_err(|error| format!("cannot flush '{}': {error}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_patch_add_update_delete_parses() {
        let patch = r#"*** Begin Patch
*** Add File: a.txt
+one
+two
*** Update File: b.txt
*** Move to: c.txt
@@
 keep
-old
+new
*** Delete File: d.txt
*** End Patch"#;
        let operations = parse_patch(patch).expect("patch parses");
        assert_eq!(operations.len(), 3);
    }

    #[test]
    fn update_hunks_apply_exact_context() {
        let hunks = vec![PatchHunk {
            lines: vec![
                PatchLine::Context("keep".into()),
                PatchLine::Remove("old".into()),
                PatchLine::Add("new".into()),
            ],
        }];
        assert_eq!(
            apply_hunks("keep\nold\n", &hunks).expect("hunk applies"),
            "keep\nnew\n"
        );
    }

    #[test]
    fn rejected_patch_has_no_operations() {
        assert!(parse_patch("*** Update File: a\n").is_err());
    }
}
