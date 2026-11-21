//! Turn-boundary file checkpoints, typed diff preview, and restore.
//!
//! A checkpoint is created at the mutating-tool boundary, before dispatch, and
//! committed after the tool returns. The canonical manifest is stored in the
//! runtime metadata store; snapshot bytes are content-addressed below the data
//! directory through [`store::CheckpointStore`]. Restore verifies every blob
//! before writing, converges under retry, and records task event and audit
//! evidence.

pub mod diff;
pub mod store;

pub use store::CheckpointStore;

use crate::core::store::{BlobRef, CheckpointFileRecord, CheckpointRecord, CheckpointRestore};
use crate::{now, Runtime};
use common::{
    scopes, CheckpointDiff, CheckpointFileView, CheckpointRestoreOutcome, CheckpointView,
    Principal, Task,
};
use serde_json::json;
use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

impl Runtime {
    /// Content-addressed snapshot store owned by this runtime.
    pub fn checkpoint_store(&self) -> std::sync::Arc<CheckpointStore> {
        self.checkpoint_store_ref().clone()
    }

    /// Register the workspace root used for restore. Worker startup sets this
    /// from the same operator configuration that binds the tool kernel.
    pub async fn set_tool_workspace(&self, workspace: Option<PathBuf>) {
        *self.tool_workspace_ref().write().await = workspace;
    }

    /// Registered workspace root used for restore.
    pub async fn tool_workspace(&self) -> Option<PathBuf> {
        self.tool_workspace_ref().read().await.clone()
    }

    /// Capture the pre-turn state of every path a mutating tool will touch.
    ///
    /// A failure is fail-closed: the caller must not dispatch the mutation,
    /// because an unreversible change would otherwise be presented as
    /// checkpointed.
    pub(crate) async fn checkpoint_before(
        &self,
        task: &Task,
        turn: &str,
        workspace: &Path,
        paths: &[String],
    ) -> crate::Result<String> {
        let store = self.checkpoint_store();
        let _gate = store.gate().lock().await;
        let mut captured = Vec::new();
        for path in unique_paths(paths) {
            captured.push((path.clone(), store.capture(workspace, &path)?));
        }
        let id = checkpoint_id(&task.id, turn);
        let now = now();
        let (created, files) = self
            .store()
            .write(|state| {
                let fresh = !state.checkpoints.contains_key(&id);
                let record =
                    state
                        .checkpoints
                        .entry(id.clone())
                        .or_insert_with(|| CheckpointRecord {
                            id: id.clone(),
                            task_id: task.id.clone(),
                            namespace: task.namespace.clone(),
                            caller: task.caller.clone(),
                            session_id: task.session_id.clone(),
                            turn: turn.to_string(),
                            attempt: task.attempt,
                            status: "open".into(),
                            created_at: now,
                            updated_at: now,
                            files: Vec::new(),
                            restores: Vec::new(),
                        });
                for (path, blob) in &captured {
                    match record.files.iter_mut().find(|file| &file.path == path) {
                        Some(file) => {
                            file.after = None;
                        }
                        None => record.files.push(CheckpointFileRecord {
                            path: path.clone(),
                            before: blob.clone(),
                            after: None,
                        }),
                    }
                }
                record.status = "open".into();
                record.updated_at = now;
                let summary = record
                    .files
                    .iter()
                    .map(|file| {
                        json!({
                            "path": file.path,
                            "before_digest": file.before.digest,
                            "before_bytes": file.before.bytes,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok((fresh, summary))
            })
            .await
            .map_err(api_error)?;
        let payload = json!({
            "checkpoint_id": id,
            "task_id": task.id,
            "turn": turn,
            "attempt": task.attempt,
            "files": files,
            "redaction": {"paths": "workspace-relative", "blobs": "content-addressed"},
        });
        let _ = self
            .append_event(
                &task.id,
                if created {
                    "checkpoint.created"
                } else {
                    "checkpoint.updated"
                },
                payload,
                true,
            )
            .await;
        if created {
            let _ = self
                .audit(
                    &task.caller,
                    "checkpoint.create",
                    &id,
                    "created",
                    &format!("task={} turn={turn}", task.id),
                )
                .await;
        }
        Ok(id)
    }

    /// Capture the post-turn state and close the checkpoint.
    ///
    /// A post-capture failure leaves the checkpoint `partial`: rewind evidence
    /// is still valid, while diff and post-state restore fail closed.
    pub(crate) async fn checkpoint_after(
        &self,
        checkpoint_id: &str,
        workspace: &Path,
        paths: &[String],
    ) -> crate::Result<()> {
        let store = self.checkpoint_store();
        let _gate = store.gate().lock().await;
        let mut captured = Vec::new();
        let mut failure = None;
        for path in unique_paths(paths) {
            match store.capture(workspace, &path) {
                Ok(blob) => captured.push((path, blob)),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let now = now();
        let result = self
            .store()
            .write(|state| {
                let record = state.checkpoints.get_mut(checkpoint_id).ok_or_else(|| {
                    crate::ApiError::new(
                        "checkpoint.not_found",
                        "checkpoint manifest does not exist",
                    )
                })?;
                for (path, blob) in &captured {
                    match record.files.iter_mut().find(|file| &file.path == path) {
                        Some(file) => file.after = Some(blob.clone()),
                        None => record.files.push(CheckpointFileRecord {
                            path: path.clone(),
                            before: BlobRef {
                                digest: String::new(),
                                bytes: 0,
                                present: false,
                            },
                            after: Some(blob.clone()),
                        }),
                    }
                }
                record.updated_at = now;
                record.status =
                    if failure.is_some() || record.files.iter().any(|file| file.after.is_none()) {
                        "partial".into()
                    } else {
                        "closed".into()
                    };
                Ok(())
            })
            .await;
        if let Err(error) = &result {
            return Err(api_error(crate::ApiError::new(
                error.code.clone(),
                error.message.clone(),
            )));
        }
        let task_id = self
            .store()
            .read(|state| {
                state
                    .checkpoints
                    .get(checkpoint_id)
                    .map(|record| record.task_id.clone())
            })
            .await;
        if let Some(task_id) = task_id {
            let payload = json!({
                "checkpoint_id": checkpoint_id,
                "task_id": task_id,
                "turn": self
                    .store()
                    .read(|state| state.checkpoints.get(checkpoint_id).map(|record| record.turn.clone()))
                    .await,
                "status": self
                    .store()
                    .read(|state| state.checkpoints.get(checkpoint_id).map(|record| record.status.clone()))
                    .await,
                "error": failure.as_ref().map(|error| error.code.clone()),
                "redaction": {"paths": "workspace-relative", "blobs": "content-addressed"},
            });
            let _ = self
                .append_event(&task_id, "checkpoint.committed", payload, true)
                .await;
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Checkpoints for one task, oldest first.
    pub async fn checkpoints_for_task(
        &self,
        principal: &Principal,
        task_id: &str,
    ) -> crate::Result<Vec<CheckpointView>> {
        principal.require(scopes::TASK_READ)?;
        let task = self.task(principal, task_id).await?;
        let records = self
            .store()
            .read(|state| {
                let mut records = state
                    .checkpoints
                    .values()
                    .filter(|record| record.task_id == task.id)
                    .cloned()
                    .collect::<Vec<_>>();
                records.sort_by_key(|record| record.created_at);
                records
            })
            .await;
        let store = self.checkpoint_store();
        records.iter().map(|record| view(record, &store)).collect()
    }

    /// One checkpoint by identity, scoped to its owning task.
    pub async fn checkpoint(
        &self,
        principal: &Principal,
        checkpoint_id: &str,
    ) -> crate::Result<CheckpointView> {
        principal.require(scopes::TASK_READ)?;
        let record = self.checkpoint_record(principal, checkpoint_id).await?;
        view(&record, &self.checkpoint_store())
    }

    /// Typed diff preview for one committed checkpoint.
    pub async fn checkpoint_diff(
        &self,
        principal: &Principal,
        checkpoint_id: &str,
    ) -> crate::Result<CheckpointDiff> {
        principal.require(scopes::TASK_READ)?;
        let record = self.checkpoint_record(principal, checkpoint_id).await?;
        let store = self.checkpoint_store();
        let mut files = Vec::new();
        let mut additions = 0usize;
        let mut deletions = 0usize;
        for file in &record.files {
            let after = file.after.as_ref().ok_or_else(|| {
                crate::Error::new(
                    "checkpoint.incomplete",
                    "checkpoint has not captured every post-turn file",
                )
            })?;
            let before = read_optional(&store, &file.before)?;
            let after = read_optional(&store, after)?;
            let Some(diff) = diff::file_diff(&file.path, before.as_deref(), after.as_deref())
            else {
                continue;
            };
            additions += diff.additions;
            deletions += diff.deletions;
            files.push(diff);
        }
        Ok(CheckpointDiff {
            checkpoint_id: record.id,
            task_id: record.task_id,
            turn: record.turn,
            files,
            additions,
            deletions,
            truncated: false,
        })
    }

    /// Rewind, undo, or re-apply one checkpoint, for the whole turn or one file
    /// selection.
    ///
    /// Idempotent under retry: the same request converges on the same content,
    /// and a repeated apply reports `already_applied`. A corrupted or missing
    /// snapshot fails closed before any workspace write.
    pub async fn restore_checkpoint(
        &self,
        principal: &Principal,
        checkpoint_id: &str,
        request: &common::CheckpointRestoreRequest,
    ) -> crate::Result<CheckpointRestoreOutcome> {
        principal.require(scopes::TASK_SUBMIT)?;
        let record = self.checkpoint_record(principal, checkpoint_id).await?;
        let mode = match request.mode.as_str() {
            "rewind" | "undo" => "rewind",
            "restore" | "redo" => "restore",
            other => {
                return Err(crate::Error::new(
                    "validation.checkpoint_mode",
                    format!("checkpoint restore mode '{other}' is not supported"),
                ))
            }
        };
        let workspace = self.tool_workspace().await.ok_or_else(|| {
            crate::Error::new(
                "checkpoint.workspace_unavailable",
                "this runtime has no registered tool workspace to restore",
            )
        })?;
        let selected = select_files(&record, request.files.as_deref())?;
        let targets = selected
            .iter()
            .map(|path| {
                let file = record
                    .files
                    .iter()
                    .find(|file| &file.path == path)
                    .expect("selected files exist");
                let blob = match mode {
                    "rewind" => file.before.clone(),
                    _ => file.after.clone().ok_or_else(|| {
                        crate::Error::new(
                            "checkpoint.incomplete",
                            "checkpoint has no post-turn snapshot to restore",
                        )
                    })?,
                };
                Ok((path.clone(), blob))
            })
            .collect::<crate::Result<Vec<_>>>()?;
        let restore_id = restore_id(checkpoint_id, mode, &targets);
        let store = self.checkpoint_store();
        let _gate = store.gate().lock().await;
        // Verify every selected blob before the first write. Corruption is a
        // fail-closed condition, never a partial restore.
        for (_, blob) in &targets {
            if blob.present {
                store.read_blob(blob)?;
            }
        }
        let mut changed = Vec::new();
        for (path, blob) in &targets {
            if store.restore_file(&workspace, path, blob)? {
                changed.push(path.clone());
            }
        }
        let at = now();
        self.store()
            .write(|state| {
                if let Some(record) = state.checkpoints.get_mut(checkpoint_id) {
                    if !record
                        .restores
                        .iter()
                        .any(|restore| restore.id == restore_id)
                    {
                        record.restores.push(CheckpointRestore {
                            id: restore_id.clone(),
                            mode: mode.to_string(),
                            files: selected.clone(),
                            at,
                        });
                    }
                    record.updated_at = at;
                }
                Ok(())
            })
            .await
            .map_err(api_error)?;
        let already_applied = changed.is_empty();
        let payload = json!({
            "checkpoint_id": checkpoint_id,
            "task_id": record.task_id,
            "turn": record.turn,
            "mode": mode,
            "files": selected,
            "changed": changed,
            "already_applied": already_applied,
            "restore_id": restore_id,
            "redaction": {"paths": "workspace-relative"},
        });
        let _ = self
            .append_event(&record.task_id, "checkpoint.restored", payload, true)
            .await;
        let _ = self
            .audit(
                &principal.id,
                "checkpoint.restore",
                checkpoint_id,
                if already_applied {
                    "already"
                } else {
                    "restored"
                },
                &format!("mode={mode} changed={}", changed.len()),
            )
            .await;
        Ok(CheckpointRestoreOutcome {
            checkpoint_id: checkpoint_id.to_string(),
            task_id: record.task_id,
            mode: mode.to_string(),
            files: selected,
            changed,
            already_applied,
            at,
        })
    }

    /// Remove file checkpoints older than `cutoff_ms`, then collect their
    /// unreferenced blobs. Unfinished tasks are preserved.
    pub async fn checkpoint_retention(
        &self,
        cutoff_ms: i64,
        preserve: &BTreeSet<String>,
    ) -> crate::Result<(usize, usize)> {
        let store = self.checkpoint_store();
        let _gate = store.gate().lock().await;
        let (removed, referenced) = self
            .store()
            .write(|state| {
                let before = state.checkpoints.len();
                state.checkpoints.retain(|_, record| {
                    preserve.contains(&record.task_id) || record.updated_at >= cutoff_ms
                });
                let referenced = referenced_blobs(state);
                Ok((before - state.checkpoints.len(), referenced))
            })
            .await
            .map_err(api_error)?;
        let blobs = store.gc(&referenced)?;
        Ok((removed, blobs))
    }

    /// Remove every file checkpoint owned by the selected tasks, then collect
    /// unreferenced blobs.
    pub async fn checkpoint_erase_tasks(
        &self,
        tasks: &BTreeSet<String>,
    ) -> crate::Result<(usize, usize)> {
        let store = self.checkpoint_store();
        let _gate = store.gate().lock().await;
        let (removed, referenced) = self
            .store()
            .write(|state| {
                let before = state.checkpoints.len();
                state
                    .checkpoints
                    .retain(|_, record| !tasks.contains(&record.task_id));
                let referenced = referenced_blobs(state);
                Ok((before - state.checkpoints.len(), referenced))
            })
            .await
            .map_err(api_error)?;
        let blobs = store.gc(&referenced)?;
        Ok((removed, blobs))
    }

    async fn checkpoint_record(
        &self,
        principal: &Principal,
        checkpoint_id: &str,
    ) -> crate::Result<CheckpointRecord> {
        let record = self
            .store()
            .read(|state| state.checkpoints.get(checkpoint_id).cloned())
            .await
            .ok_or_else(|| {
                crate::Error::new("checkpoint.not_found", "checkpoint does not exist")
            })?;
        // Resolving the owning task through the normal task view enforces
        // namespace and caller scoping for the checkpoint record.
        self.task(principal, &record.task_id).await?;
        Ok(record)
    }
}

fn checkpoint_id(task_id: &str, turn: &str) -> String {
    let digest = common::bytes_digest(format!("{task_id}\u{1f}{turn}").as_bytes());
    format!(
        "cp_{}",
        digest
            .trim_start_matches("sha256:")
            .chars()
            .take(32)
            .collect::<String>()
    )
}

fn unique_paths(paths: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for path in paths {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        if seen.insert(path.to_string()) {
            unique.push(path.to_string());
        }
    }
    unique
}

fn view(record: &CheckpointRecord, store: &CheckpointStore) -> crate::Result<CheckpointView> {
    let mut files = Vec::new();
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for file in &record.files {
        let before = read_optional(store, &file.before)?;
        let after = match &file.after {
            Some(after) => Some(read_optional(store, after)?),
            None => None,
        };
        let (change, file_additions, file_deletions) = match (&file.after, &after) {
            (Some(_), Some(after_bytes)) => {
                match diff::file_diff(&file.path, before.as_deref(), after_bytes.as_deref()) {
                    Some(diff) => (diff.change, diff.additions, diff.deletions),
                    None => ("unchanged".to_string(), 0, 0),
                }
            }
            (None, _) => (
                if record.status == "partial" {
                    "unknown".to_string()
                } else {
                    "pending".to_string()
                },
                0,
                0,
            ),
            (Some(_), None) => ("unchanged".to_string(), 0, 0),
        };
        additions += file_additions;
        deletions += file_deletions;
        files.push(CheckpointFileView {
            path: file.path.clone(),
            change,
            before_digest: file.before.present.then(|| file.before.digest.clone()),
            after_digest: file
                .after
                .as_ref()
                .and_then(|after| after.present.then(|| after.digest.clone())),
            before_bytes: file.before.bytes,
            after_bytes: file.after.as_ref().map(|after| after.bytes).unwrap_or(0),
            additions: file_additions,
            deletions: file_deletions,
        });
    }
    Ok(CheckpointView {
        id: record.id.clone(),
        task_id: record.task_id.clone(),
        session_id: record.session_id.clone(),
        turn: record.turn.clone(),
        attempt: record.attempt,
        status: record.status.clone(),
        created_at: record.created_at,
        updated_at: record.updated_at,
        files,
        additions,
        deletions,
    })
}

fn read_optional(store: &CheckpointStore, blob: &BlobRef) -> crate::Result<Option<Vec<u8>>> {
    if blob.present {
        Ok(Some(store.read_blob(blob)?))
    } else {
        Ok(None)
    }
}

fn select_files(
    record: &CheckpointRecord,
    requested: Option<&[String]>,
) -> crate::Result<Vec<String>> {
    let select = match requested {
        Some(files) => files.to_vec(),
        None => record.files.iter().map(|file| file.path.clone()).collect(),
    };
    let mut selected = Vec::new();
    for path in select {
        let normalized = normalize_relative(&path)?;
        if !record.files.iter().any(|file| file.path == normalized) {
            return Err(crate::Error::new(
                "checkpoint.file_unknown",
                "the selected file is not part of this checkpoint",
            ));
        }
        if !selected.contains(&normalized) {
            selected.push(normalized);
        }
    }
    if selected.is_empty() {
        return Err(crate::Error::new(
            "validation.checkpoint_files",
            "a restore must select at least one file",
        ));
    }
    Ok(selected)
}

fn normalize_relative(path: &str) -> crate::Result<String> {
    let path = path.trim();
    let candidate = Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(crate::Error::new(
            "validation.checkpoint_files",
            "checkpoint file selections must be workspace-relative",
        ));
    }
    Ok(path.to_string())
}

fn restore_id(checkpoint_id: &str, mode: &str, targets: &[(String, BlobRef)]) -> String {
    let mut sorted = targets.to_vec();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let digests = sorted
        .iter()
        .map(|(path, blob)| (path.clone(), blob.digest.clone(), blob.present))
        .collect::<Vec<_>>();
    let digest = common::canonical_digest(&(checkpoint_id, mode, digests));
    format!(
        "rs_{}",
        digest
            .trim_start_matches("sha256:")
            .chars()
            .take(32)
            .collect::<String>()
    )
}

fn referenced_blobs(state: &crate::State) -> BTreeSet<String> {
    let mut referenced = BTreeSet::new();
    for record in state.checkpoints.values() {
        for file in &record.files {
            if file.before.present {
                referenced.insert(file.before.digest.clone());
            }
            if let Some(after) = &file.after {
                if after.present {
                    referenced.insert(after.digest.clone());
                }
            }
        }
    }
    referenced
}

fn api_error(error: crate::ApiError) -> crate::Error {
    crate::Error::new(error.code, error.message)
}
