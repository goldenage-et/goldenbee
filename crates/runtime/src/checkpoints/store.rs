//! Content-addressed blob storage for turn-boundary file snapshots.
//!
//! Snapshot bytes are owned here. The durable manifest lives in
//! [`crate::State::checkpoints`], so a restart can recover exactly which blobs
//! belong to which task turn. Blobs are addressed by SHA-256 and verified again
//! before any restore writes to the workspace.

use crate::core::store::BlobRef;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Directory below the Worker data directory that owns snapshot bytes.
pub const CHECKPOINT_DIR: &str = "checkpoints";
/// Maximum bytes captured for one file. A larger file is refused before the
/// mutating tool runs, because an unreversible edit would be presented as a
/// checkpointed one.
pub const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
const BLOB_DIR: &str = "blobs";

/// Content-addressed snapshot storage rooted at one Worker Instance's data
/// directory.
pub struct CheckpointStore {
    root: PathBuf,
    /// Serializes capture and blob collection so a sweep cannot delete a blob
    /// between its write and its manifest reference.
    gate: tokio::sync::Mutex<()>,
}

impl CheckpointStore {
    /// Open or create the snapshot store and prove it is writable.
    pub fn open(data_dir: &Path) -> crate::Result<Self> {
        let root = data_dir.join(CHECKPOINT_DIR);
        std::fs::create_dir_all(root.join(BLOB_DIR)).map_err(|error| {
            crate::Error::new(
                "checkpoint.storage_unavailable",
                format!("cannot create checkpoint directory: {error}"),
            )
        })?;
        Ok(Self {
            root,
            gate: tokio::sync::Mutex::new(()),
        })
    }

    /// Serialize captures with blob collection.
    pub(crate) fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }

    /// Root directory that owns checkpoint blobs.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory containing content-addressed blobs.
    pub fn blob_dir(&self) -> PathBuf {
        self.root.join(BLOB_DIR)
    }

    /// Capture the current bytes of one workspace-relative path.
    pub fn capture(&self, workspace: &Path, path: &str) -> crate::Result<BlobRef> {
        let resolved = confined_path(workspace, path)?;
        if !resolved.exists() {
            return Ok(absent());
        }
        let metadata = std::fs::symlink_metadata(&resolved).map_err(|error| {
            crate::Error::new(
                "checkpoint.path_unreadable",
                format!("cannot inspect workspace path: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            // Resolve the target and reject an escape instead of snapshotting
            // host content through a symlink.
            confined_path(workspace, path)?;
        }
        if !metadata.is_file() {
            return Err(crate::Error::new(
                "checkpoint.path_invalid",
                "only regular files can be checkpointed",
            ));
        }
        if metadata.len() > MAX_SNAPSHOT_BYTES {
            return Err(crate::Error::new(
                "checkpoint.too_large",
                "workspace file exceeds the checkpoint snapshot ceiling",
            ));
        }
        let bytes = std::fs::read(&resolved).map_err(|error| {
            crate::Error::new(
                "checkpoint.path_unreadable",
                format!("cannot read workspace file: {error}"),
            )
        })?;
        self.write_blob(&bytes)
    }

    /// Write bytes as one content-addressed blob. Idempotent.
    pub fn write_blob(&self, bytes: &[u8]) -> crate::Result<BlobRef> {
        let digest = common::bytes_digest(bytes);
        let path = self.blob_path(&digest)?;
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    crate::Error::new(
                        "checkpoint.storage_unavailable",
                        format!("cannot create blob directory: {error}"),
                    )
                })?;
            }
            atomic_write(&path, bytes).map_err(|error| {
                crate::Error::new(
                    "checkpoint.storage_unavailable",
                    format!("cannot persist snapshot blob: {error}"),
                )
            })?;
        }
        Ok(BlobRef {
            digest,
            bytes: bytes.len() as u64,
            present: true,
        })
    }

    /// Read a blob and verify its recorded digest and size before returning it.
    pub fn read_blob(&self, blob: &BlobRef) -> crate::Result<Vec<u8>> {
        if !blob.present {
            return Err(crate::Error::new(
                "checkpoint.invalid",
                "an absent snapshot has no content",
            ));
        }
        let path = self.blob_path(&blob.digest)?;
        let bytes = std::fs::read(&path).map_err(|error| {
            crate::Error::new(
                "checkpoint.corrupt",
                format!("snapshot blob is unreadable: {error}"),
            )
        })?;
        if bytes.len() as u64 != blob.bytes || common::bytes_digest(&bytes) != blob.digest {
            return Err(crate::Error::new(
                "checkpoint.corrupt",
                "snapshot blob does not match its recorded digest",
            ));
        }
        Ok(bytes)
    }

    /// Restore one workspace-relative path to a captured blob.
    ///
    /// Returns whether the on-disk content changed. An absent blob removes the
    /// file. All blob verification happens before the caller writes anything.
    pub fn restore_file(
        &self,
        workspace: &Path,
        path: &str,
        blob: &BlobRef,
    ) -> crate::Result<bool> {
        let resolved = confined_path(workspace, path)?;
        if !blob.present {
            if !resolved.exists() {
                return Ok(false);
            }
            std::fs::remove_file(&resolved).map_err(|error| {
                crate::Error::new(
                    "checkpoint.restore_failed",
                    format!("cannot remove restored path: {error}"),
                )
            })?;
            return Ok(true);
        }
        if resolved.exists() {
            let current = std::fs::read(&resolved).map_err(|error| {
                crate::Error::new(
                    "checkpoint.restore_failed",
                    format!("cannot read current workspace file: {error}"),
                )
            })?;
            if common::bytes_digest(&current) == blob.digest {
                return Ok(false);
            }
            if !resolved.is_file() {
                return Err(crate::Error::new(
                    "checkpoint.path_invalid",
                    "only regular files can be restored",
                ));
            }
        }
        let bytes = self.read_blob(blob)?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                crate::Error::new(
                    "checkpoint.restore_failed",
                    format!("cannot create restored directory: {error}"),
                )
            })?;
        }
        atomic_write(&resolved, &bytes).map_err(|error| {
            crate::Error::new(
                "checkpoint.restore_failed",
                format!("cannot write restored file: {error}"),
            )
        })?;
        Ok(true)
    }

    /// Delete every blob not referenced by the retained manifests.
    ///
    /// The caller holds the checkpoint gate, so a concurrent capture cannot
    /// write a blob between the reference snapshot and this sweep.
    pub fn gc(&self, referenced: &BTreeSet<String>) -> crate::Result<usize> {
        let dir = self.blob_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(crate::Error::new(
                    "checkpoint.storage_unavailable",
                    format!("cannot list snapshot blobs: {error}"),
                ))
            }
        };
        let mut removed = 0usize;
        for entry in entries {
            let entry = entry.map_err(|error| {
                crate::Error::new(
                    "checkpoint.storage_unavailable",
                    format!("cannot list snapshot blobs: {error}"),
                )
            })?;
            let shard = entry.path();
            if !shard.is_dir() {
                continue;
            }
            let shard_name = shard
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            for file in std::fs::read_dir(&shard).map_err(|error| {
                crate::Error::new(
                    "checkpoint.storage_unavailable",
                    format!("cannot list snapshot blobs: {error}"),
                )
            })? {
                let file = file.map_err(|error| {
                    crate::Error::new(
                        "checkpoint.storage_unavailable",
                        format!("cannot list snapshot blobs: {error}"),
                    )
                })?;
                let name = file.file_name().to_str().unwrap_or_default().to_string();
                let digest = format!("sha256:{shard_name}{name}");
                if referenced.contains(&digest) {
                    continue;
                }
                match std::fs::remove_file(file.path()) {
                    Ok(()) => removed += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(crate::Error::new(
                            "checkpoint.retention_failed",
                            format!("cannot remove snapshot blob: {error}"),
                        ))
                    }
                }
            }
        }
        Ok(removed)
    }

    fn blob_path(&self, digest: &str) -> crate::Result<PathBuf> {
        let hex = digest.strip_prefix("sha256:").ok_or_else(|| {
            crate::Error::new(
                "checkpoint.invalid",
                "snapshot digest has an unknown format",
            )
        })?;
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(crate::Error::new(
                "checkpoint.invalid",
                "snapshot digest is not a SHA-256 hex value",
            ));
        }
        Ok(self.blob_dir().join(&hex[..2]).join(&hex[2..]))
    }
}

fn absent() -> BlobRef {
    BlobRef {
        digest: String::new(),
        bytes: 0,
        present: false,
    }
}

/// Resolve a workspace-relative path and reject absolute, parent, and symlink
/// escapes. The returned path is lexical beneath the canonical workspace root.
fn confined_path(workspace: &Path, raw: &str) -> crate::Result<PathBuf> {
    let relative = Path::new(raw);
    if raw.trim().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(crate::Error::new(
            "checkpoint.path_denied",
            "checkpoint paths must stay inside the workspace",
        ));
    }
    let root = std::fs::canonicalize(workspace).map_err(|error| {
        crate::Error::new(
            "checkpoint.workspace_unavailable",
            format!("cannot resolve the checkpoint workspace: {error}"),
        )
    })?;
    let joined = root.join(relative);
    let mut probe = joined.as_path();
    while !probe.exists() {
        probe = probe.parent().ok_or_else(|| {
            crate::Error::new(
                "checkpoint.path_denied",
                "checkpoint path has no resolvable parent inside the workspace",
            )
        })?;
    }
    let canonical = std::fs::canonicalize(probe).map_err(|error| {
        crate::Error::new(
            "checkpoint.workspace_unavailable",
            format!("cannot resolve a checkpoint path: {error}"),
        )
    })?;
    if !canonical.starts_with(&root) {
        return Err(crate::Error::new(
            "checkpoint.path_denied",
            "checkpoint path resolves outside the workspace",
        ));
    }
    if joined.exists() {
        let target = std::fs::canonicalize(&joined).map_err(|error| {
            crate::Error::new(
                "checkpoint.workspace_unavailable",
                format!("cannot resolve a checkpoint path: {error}"),
            )
        })?;
        if !target.starts_with(&root) {
            return Err(crate::Error::new(
                "checkpoint.path_denied",
                "checkpoint path resolves outside the workspace",
            ));
        }
    }
    Ok(joined)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
