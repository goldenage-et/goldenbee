//! Typed file checkpoint and diff views.
//!
//! These are management wire types. They expose workspace-relative paths and
//! content digests only; no host path, blob filename, or raw snapshot bytes
//! cross the runtime boundary.

use serde::{Deserialize, Serialize};

/// One turn-boundary checkpoint for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointView {
    /// Stable checkpoint identity.
    pub id: String,
    /// Owning task.
    pub task_id: String,
    /// Native session identity.
    pub session_id: String,
    /// Native invocation that bounds this checkpoint.
    pub turn: String,
    /// Task attempt that produced the checkpoint.
    pub attempt: u32,
    /// `open`, `closed`, or `partial`.
    pub status: String,
    /// Creation time.
    pub created_at: i64,
    /// Last update time.
    pub updated_at: i64,
    /// Files touched during this turn, with workspace-relative paths.
    pub files: Vec<CheckpointFileView>,
    /// Total added lines across files.
    pub additions: usize,
    /// Total removed lines across files.
    pub deletions: usize,
}

/// One file captured by a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFileView {
    /// Workspace-relative path.
    pub path: String,
    /// `added`, `modified`, `deleted`, or `unchanged`.
    pub change: String,
    /// Content digest before the turn, when the file existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_digest: Option<String>,
    /// Content digest after the turn, when it could be observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_digest: Option<String>,
    /// Size before the turn.
    #[serde(default)]
    pub before_bytes: u64,
    /// Size after the turn.
    #[serde(default)]
    pub after_bytes: u64,
    /// Added lines.
    #[serde(default)]
    pub additions: usize,
    /// Removed lines.
    #[serde(default)]
    pub deletions: usize,
}

/// Typed diff preview for one checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointDiff {
    /// Checkpoint identity.
    pub checkpoint_id: String,
    /// Owning task.
    pub task_id: String,
    /// Native invocation that bounds the checkpoint.
    pub turn: String,
    /// Changed files only.
    pub files: Vec<FileDiff>,
    /// Total added lines.
    pub additions: usize,
    /// Total removed lines.
    pub deletions: usize,
    /// Whether any file diff was bounded.
    pub truncated: bool,
}

/// One changed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    /// Workspace-relative path.
    pub path: String,
    /// `added`, `modified`, or `deleted`.
    pub change: String,
    /// Added lines.
    pub additions: usize,
    /// Removed lines.
    pub deletions: usize,
    /// Whether the file is not valid UTF-8 text.
    pub binary: bool,
    /// Unified-diff hunks, empty for binary files.
    pub hunks: Vec<DiffHunk>,
}

/// One unified-diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    /// First line of the hunk in the old file, 1-based.
    pub old_start: usize,
    /// Old hunk length.
    pub old_lines: usize,
    /// First line of the hunk in the new file, 1-based.
    pub new_start: usize,
    /// New hunk length.
    pub new_lines: usize,
    /// Ordered hunk lines.
    pub lines: Vec<DiffLine>,
}

/// One line in a diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    /// `context`, `add`, or `remove`.
    pub kind: String,
    /// Line text without its terminator.
    pub text: String,
}

impl FileDiff {
    /// Total changed lines in this file.
    pub fn changed_lines(&self) -> usize {
        self.additions + self.deletions
    }
}

/// Checkpoint restore request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRestoreRequest {
    /// `rewind`/`undo` restore the pre-turn state; `restore` re-applies the
    /// post-turn state.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Workspace-relative file selection. Omitted means the whole turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

fn default_mode() -> String {
    "rewind".into()
}

/// Result of a checkpoint restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRestoreOutcome {
    /// Checkpoint identity.
    pub checkpoint_id: String,
    /// Owning task.
    pub task_id: String,
    /// Applied mode.
    pub mode: String,
    /// Files selected by the request.
    pub files: Vec<String>,
    /// Files whose content changed on this call.
    pub changed: Vec<String>,
    /// True when the target state was already present.
    pub already_applied: bool,
    /// Restore time.
    pub at: i64,
}
