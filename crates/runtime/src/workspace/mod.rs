//! Bounded, read-only inspection of the Worker workspace.
//!
//! This is a projection over the same directory used by the generic file
//! tools. It never writes, follows a symlink outside the root, or returns the
//! host workspace path.

use common::{ApiError, WorkspaceEntry, WorkspaceFile, WorkspaceTreePage};
use notify::{EventKind, RecursiveMode, Watcher};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One live workspace watcher. Dropping it stops recursive notifications.
pub struct WorkspaceWatch {
    _watcher: notify::RecommendedWatcher,
    receiver: tokio::sync::mpsc::Receiver<Vec<String>>,
}

impl WorkspaceWatch {
    /// Receive one coalesced batch of visible workspace-relative paths.
    pub async fn recv(&mut self) -> Option<Vec<String>> {
        self.receiver.recv().await
    }
}

/// One bounded raw preview response.
pub struct WorkspaceContent {
    /// Bytes in the requested range.
    pub bytes: Vec<u8>,
    /// Safe server-classified content type.
    pub content_type: String,
    /// First byte offset in the full file.
    pub start: u64,
    /// Last byte offset in the full file.
    pub end: u64,
    /// Full file size.
    pub total: u64,
    /// SHA-256 digest of the full file.
    pub digest: String,
}

/// Resolved workspace inspection policy and root.
#[derive(Debug, Clone)]
pub struct WorkspaceService {
    root: PathBuf,
    max_entries: usize,
    max_file_bytes: usize,
    max_preview_bytes: usize,
    show_hidden: bool,
    ignored: Vec<String>,
}

impl WorkspaceService {
    /// Build a service rooted at an existing directory.
    pub fn new(
        root: PathBuf,
        max_entries: usize,
        max_file_bytes: usize,
        max_preview_bytes: usize,
        show_hidden: bool,
        ignored: Vec<String>,
    ) -> Result<Self, ApiError> {
        let root = fs::canonicalize(&root).map_err(|_| {
            ApiError::new(
                "workspace.unavailable",
                "the Worker workspace directory is not available",
            )
        })?;
        if !root.is_dir() {
            return Err(ApiError::new(
                "workspace.unavailable",
                "the Worker workspace is not a directory",
            ));
        }
        Ok(Self {
            root,
            max_entries: max_entries.max(1),
            max_file_bytes: max_file_bytes.max(1),
            max_preview_bytes: max_preview_bytes.max(1),
            show_hidden,
            ignored,
        })
    }

    /// Workspace root. Callers must not serialize this value.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// List one directory level in stable lexical order.
    pub fn tree(
        &self,
        raw_path: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<WorkspaceTreePage, ApiError> {
        let path = self.resolve(raw_path)?;
        if !path.is_dir() {
            return Err(ApiError::new(
                "workspace.not_found",
                "the requested workspace path is not a directory",
            ));
        }
        let offset = match cursor {
            Some(value) => value
                .parse::<usize>()
                .map_err(|_| ApiError::new("workspace.limit", "the workspace cursor is invalid"))?,
            None => 0,
        };
        let limit = limit.clamp(1, self.max_entries);
        let mut entries = Vec::new();
        let directory = fs::read_dir(&path).map_err(|_| {
            ApiError::new(
                "workspace.not_found",
                "the requested workspace directory could not be read",
            )
        })?;
        for item in directory {
            let Ok(item) = item else { continue };
            let Ok(file_type) = item.file_type() else {
                continue;
            };
            // Symlinks are not listed. A linked directory could otherwise
            // escape the root or create a cycle, and a link is not part of the
            // durable workspace surface.
            if file_type.is_symlink() {
                continue;
            }
            let name = item.file_name().to_string_lossy().to_string();
            if self.ignored.iter().any(|ignored| ignored == &name) {
                continue;
            }
            let hidden = self.is_hidden(&name);
            if hidden && !self.show_hidden {
                continue;
            }
            if self.is_secret(&name) {
                continue;
            }
            let Ok(metadata) = item.metadata() else {
                continue;
            };
            let kind = if metadata.is_dir() {
                "directory"
            } else {
                "file"
            };
            let relative = path
                .strip_prefix(&self.root)
                .ok()
                .map(|value| value.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let entry_path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            entries.push(WorkspaceEntry {
                name,
                path: entry_path,
                kind: kind.into(),
                size: if metadata.is_file() {
                    metadata.len()
                } else {
                    0
                },
                modified_at: modified_at(&metadata),
                hidden,
            });
        }
        entries.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        });
        let total = entries.len();
        let page = entries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let next = offset.saturating_add(page.len());
        let truncated = next < total;
        Ok(WorkspaceTreePage {
            path: relative_path(&self.root, &path),
            entries: page,
            next_cursor: truncated.then(|| next.to_string()),
            truncated,
        })
    }

    /// Read a bounded UTF-8 preview or return binary metadata.
    pub fn file(&self, raw_path: &str) -> Result<WorkspaceFile, ApiError> {
        let path = self.resolve(raw_path)?;
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        if self.is_secret(&name) {
            return Err(ApiError::new(
                "workspace.path_denied",
                "the requested workspace file is not browsable",
            ));
        }
        let metadata = fs::metadata(&path).map_err(|_| {
            ApiError::new(
                "workspace.not_found",
                "the requested workspace file was not found",
            )
        })?;
        if !metadata.is_file() {
            return Err(ApiError::new(
                "workspace.not_found",
                "the requested workspace path is not a file",
            ));
        }
        // A preview is not a download. Refuse pathological files before
        // allocating their bytes; ordinary text is bounded by max_file_bytes.
        const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
        if metadata.len() > MAX_READ_BYTES {
            return Err(ApiError::new(
                "workspace.too_large",
                "the requested workspace file exceeds the preview ceiling",
            ));
        }
        let bytes = fs::read(&path).map_err(|_| {
            ApiError::new(
                "workspace.not_found",
                "the requested workspace file could not be read",
            )
        })?;
        let digest = common::bytes_digest(&bytes);
        let binary = std::str::from_utf8(&bytes).is_err();
        let (text, truncated) = if binary {
            (None, false)
        } else {
            let text = String::from_utf8_lossy(&bytes).to_string();
            let truncated = text.len() > self.max_file_bytes;
            let text = if truncated {
                truncate_utf8(&text, self.max_file_bytes)
            } else {
                text
            };
            (Some(text), truncated)
        };
        let content_type = content_type_for(&path, binary);
        Ok(WorkspaceFile {
            path: relative_path(&self.root, &path),
            kind: "file".into(),
            size: metadata.len(),
            modified_at: modified_at(&metadata),
            digest: Some(digest),
            content_type,
            binary,
            truncated,
            text,
        })
    }

    /// Read bounded raw bytes for media/PDF/binary preview.
    ///
    /// HTML is deliberately reported as plain text so a direct content request
    /// can never be interpreted as active markup. SVG remains an image type and
    /// is intended for an `<img>` element, not inline document evaluation.
    pub fn content(
        &self,
        raw_path: &str,
        range: Option<&str>,
    ) -> Result<WorkspaceContent, ApiError> {
        let path = self.resolve(raw_path)?;
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        if self.is_secret(&name) {
            return Err(ApiError::new(
                "workspace.path_denied",
                "the requested workspace file is not browsable",
            ));
        }
        let metadata = fs::metadata(&path).map_err(|_| {
            ApiError::new(
                "workspace.not_found",
                "the requested workspace file was not found",
            )
        })?;
        if !metadata.is_file() {
            return Err(ApiError::new(
                "workspace.not_found",
                "the requested workspace path is not a file",
            ));
        }
        let total = metadata.len();
        if total > self.max_preview_bytes as u64 {
            return Err(ApiError::new(
                "workspace.too_large",
                "the requested workspace file exceeds the preview ceiling",
            ));
        }
        let bytes = fs::read(&path).map_err(|_| {
            ApiError::new(
                "workspace.not_found",
                "the requested workspace file could not be read",
            )
        })?;
        let digest = common::bytes_digest(&bytes);
        let (start, end) = parse_range(range, total)?;
        let selected = bytes
            .get(start as usize..end.saturating_add(1) as usize)
            .ok_or_else(|| {
                ApiError::new(
                    "workspace.range_invalid",
                    "the requested byte range is invalid",
                )
            })?
            .to_vec();
        Ok(WorkspaceContent {
            bytes: selected,
            content_type: content_type_for(&path, std::str::from_utf8(&bytes).is_err()),
            start,
            end,
            total,
            digest,
        })
    }

    /// Start a recursive, filtered filesystem watcher for this workspace.
    ///
    /// The watcher is best-effort: filesystem notifications are not durable and
    /// a reconnect must refresh the tree. It is still bounded: a slow Client
    /// drops batches rather than growing an unbounded queue.
    pub fn watch(&self) -> Result<WorkspaceWatch, ApiError> {
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        let service = self.clone();
        let mut watcher = notify::recommended_watcher(
            move |result: std::result::Result<notify::Event, notify::Error>| {
                let Ok(event) = result else { return };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                let paths = event
                    .paths
                    .iter()
                    .filter_map(|path| service.visible_relative(path))
                    .collect::<Vec<_>>();
                if !paths.is_empty() {
                    let _ = sender.try_send(paths);
                }
            },
        )
        .map_err(|_| {
            ApiError::new(
                "workspace.unavailable",
                "the workspace watcher could not be created",
            )
        })?;
        watcher
            .watch(&self.root, RecursiveMode::Recursive)
            .map_err(|_| {
                ApiError::new(
                    "workspace.unavailable",
                    "the workspace watcher could not start",
                )
            })?;
        Ok(WorkspaceWatch {
            _watcher: watcher,
            receiver,
        })
    }

    fn visible_relative(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return None;
            };
            let name = name.to_string_lossy();
            if self.ignored.iter().any(|ignored| ignored == &name) {
                return None;
            }
            if self.is_hidden(&name) && !self.show_hidden {
                return None;
            }
            if self.is_secret(&name) {
                return None;
            }
        }
        let relative = relative.to_string_lossy().replace('\\', "/");
        (!relative.is_empty()).then_some(relative)
    }

    fn resolve(&self, raw: &str) -> Result<PathBuf, ApiError> {
        let raw = raw.trim();
        let relative = if raw.is_empty() || raw == "." {
            PathBuf::new()
        } else {
            let candidate = Path::new(raw);
            if candidate.is_absolute()
                || candidate
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(ApiError::new(
                    "workspace.path_denied",
                    "the workspace path must be relative and contained",
                ));
            }
            candidate.to_path_buf()
        };
        let joined = self.root.join(relative);
        let resolved = fs::canonicalize(&joined).map_err(|_| {
            ApiError::new(
                "workspace.not_found",
                "the requested workspace path was not found",
            )
        })?;
        if !resolved.starts_with(&self.root) {
            return Err(ApiError::new(
                "workspace.path_denied",
                "the requested workspace path escapes the workspace",
            ));
        }
        Ok(resolved)
    }

    fn is_hidden(&self, name: &str) -> bool {
        name.starts_with('.') && name != "." && name != ".."
    }

    fn is_secret(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        lower == ".env"
            || lower.starts_with(".env.")
            || lower.contains("secret")
            || lower.contains("credential")
            || lower.contains("token")
            || lower == "id_rsa"
            || lower == "id_ed25519"
    }
}

fn parse_range(raw: Option<&str>, total: u64) -> Result<(u64, u64), ApiError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok((0, total.saturating_sub(1)));
    };
    let value = raw.trim().strip_prefix("bytes=").ok_or_else(|| {
        ApiError::new(
            "workspace.range_invalid",
            "only single byte ranges are supported",
        )
    })?;
    if value.contains(',') {
        return Err(ApiError::new(
            "workspace.range_invalid",
            "multiple byte ranges are not supported",
        ));
    }
    let (start, end) = value.split_once('-').ok_or_else(|| {
        ApiError::new(
            "workspace.range_invalid",
            "the requested byte range is invalid",
        )
    })?;
    let start = start.trim().parse::<u64>().map_err(|_| {
        ApiError::new(
            "workspace.range_invalid",
            "the requested byte range is invalid",
        )
    })?;
    let end = if end.trim().is_empty() {
        total.saturating_sub(1)
    } else {
        end.trim().parse::<u64>().map_err(|_| {
            ApiError::new(
                "workspace.range_invalid",
                "the requested byte range is invalid",
            )
        })?
    };
    if total == 0 || start >= total || start > end {
        return Err(ApiError::new(
            "workspace.range_invalid",
            "the requested byte range is outside the file",
        ));
    }
    Ok((start, end.min(total - 1)))
}

fn content_type_for(path: &Path, binary: bool) -> String {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.ends_with(".html") || name.ends_with(".htm") {
        return "text/plain; charset=utf-8".into();
    }
    let guessed = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string();
    if guessed == "application/octet-stream" && !binary {
        return "text/plain; charset=utf-8".into();
    }
    guessed
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string()
}

fn modified_at(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

fn truncate_utf8(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(root: &Path) -> WorkspaceService {
        WorkspaceService::new(
            root.to_path_buf(),
            50,
            8,
            1024 * 1024,
            false,
            vec![".git".into()],
        )
        .unwrap()
    }

    #[test]
    fn lists_files_and_directories_without_host_paths() {
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir(data.path().join("notes")).unwrap();
        std::fs::write(data.path().join("index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(data.path().join("notes/one.txt"), "one").unwrap();
        let service = service(data.path());
        let root = service.tree("", 50, None).unwrap();
        assert_eq!(root.path, "");
        assert_eq!(root.entries[0].path, "notes");
        assert!(root.entries.iter().any(|entry| entry.path == "index.html"));
        let nested = service.tree("notes", 50, None).unwrap();
        assert_eq!(nested.entries[0].path, "notes/one.txt");
        assert!(!serde_json::to_string(&root)
            .unwrap()
            .contains(data.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn hides_dotfiles_secrets_and_ignored_directories() {
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join(".env"), "SECRET=1").unwrap();
        std::fs::write(data.path().join("id_rsa"), "key").unwrap();
        std::fs::create_dir(data.path().join(".git")).unwrap();
        std::fs::write(data.path().join("index.html"), "ok").unwrap();
        let service = service(data.path());
        let root = service.tree("", 50, None).unwrap();
        assert_eq!(root.entries.len(), 1);
        assert_eq!(root.entries[0].path, "index.html");
        assert_eq!(
            service.file(".env").unwrap_err().code,
            "workspace.path_denied"
        );
    }

    #[test]
    fn rejects_parent_absolute_and_symlink_escape() {
        let data = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), data.path().join("link"))
            .unwrap();
        let service = service(data.path());
        assert_eq!(
            service.tree("../", 50, None).unwrap_err().code,
            "workspace.path_denied"
        );
        assert_eq!(
            service.tree("/", 50, None).unwrap_err().code,
            "workspace.path_denied"
        );
        #[cfg(unix)]
        assert_eq!(
            service.file("link").unwrap_err().code,
            "workspace.path_denied"
        );
    }

    #[tokio::test]
    async fn watcher_emits_visible_relative_paths() {
        let data = tempfile::tempdir().unwrap();
        let service = service(data.path());
        let mut watch = service.watch().unwrap();
        std::fs::write(data.path().join("changed.txt"), "changed").unwrap();
        let batch = tokio::time::timeout(std::time::Duration::from_secs(3), watch.recv())
            .await
            .expect("watcher emits")
            .expect("watcher batch");
        assert!(batch.iter().any(|path| path == "changed.txt"), "{batch:?}");
        std::fs::write(data.path().join(".env"), "SECRET=1").unwrap();
        let filtered = service.visible_relative(&data.path().join(".env"));
        assert!(filtered.is_none());
    }

    #[test]
    fn bounds_text_and_never_returns_binary_bytes() {
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("large.txt"), "abcdefghij").unwrap();
        std::fs::write(data.path().join("binary.bin"), [0_u8, 159, 146, 150]).unwrap();
        let service = service(data.path());
        let large = service.file("large.txt").unwrap();
        assert!(large.truncated);
        assert_eq!(large.text.as_deref(), Some("abcdefgh"));
        let binary = service.file("binary.bin").unwrap();
        assert!(binary.binary);
        assert!(binary.text.is_none());
    }
}
