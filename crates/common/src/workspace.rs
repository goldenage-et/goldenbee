//! Read-only Worker workspace inspection wire types.
//!
//! Paths are always workspace-relative. These contracts never expose the host
//! workspace root, absolute paths, symlink targets, or file bytes beyond the
//! bounded text ceiling.

use serde::{Deserialize, Serialize};

/// One directory entry in a bounded workspace tree page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    /// Base name, never a path.
    pub name: String,
    /// Workspace-relative path using `/` separators.
    pub path: String,
    /// `file` or `directory`.
    pub kind: String,
    /// File size in bytes. Zero for directories.
    #[serde(default)]
    pub size: u64,
    /// Last modification time in Unix milliseconds.
    #[serde(default)]
    pub modified_at: i64,
    /// Whether the entry is a dotfile or otherwise hidden by policy.
    #[serde(default)]
    pub hidden: bool,
}

/// One bounded directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTreePage {
    /// Workspace-relative directory path. Empty for the workspace root.
    #[serde(default)]
    pub path: String,
    /// Entries in stable lexical order.
    #[serde(default)]
    pub entries: Vec<WorkspaceEntry>,
    /// Opaque cursor for the next page, when the directory was bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Whether any entries were omitted from this page.
    #[serde(default)]
    pub truncated: bool,
}

/// One bounded file read or metadata projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFile {
    /// Workspace-relative path using `/` separators.
    pub path: String,
    /// `file`.
    pub kind: String,
    /// File size in bytes.
    #[serde(default)]
    pub size: u64,
    /// Last modification time in Unix milliseconds.
    #[serde(default)]
    pub modified_at: i64,
    /// SHA-256 digest as `sha256:<hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Whether the content is not valid UTF-8 text.
    #[serde(default)]
    pub binary: bool,
    /// Whether text was truncated at the configured ceiling.
    #[serde(default)]
    pub truncated: bool,
    /// Bounded UTF-8 text. Absent for binary files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}
