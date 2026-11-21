//! Owner-coordinated retention and authorized erasure.
//!
//! Each authoritative store keeps its own lifecycle API. This module only
//! coordinates their outcomes, records durable progress so an interrupted
//! cleanup can resume, and reports obligations that were intentionally kept.
//! It is not a universal store, index, or deletion engine.

use crate::{now, CleanupJob, Error, Result, RetainedObligation, Runtime, State};
use chrono::{Duration, Utc};
use common::{scopes, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One owning store that could not complete a required lifecycle step.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlockedStep {
    /// Owning store, such as `memory` or `artifact`.
    pub store: String,
    /// Stable error code.
    pub code: String,
    /// Redacted explanation and recovery guidance.
    pub message: String,
}

/// Deletion report returned by retention or authorized erasure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleReport {
    /// Durable cleanup job identity.
    pub job_id: String,
    /// Whether this call resumed an unfinished job.
    pub resumed: bool,
    /// Whether every configured stage completed.
    pub completed: bool,
    /// Removed or changed record counts by owning data class.
    pub removed: BTreeMap<String, usize>,
    /// Records intentionally kept because a live obligation requires them.
    pub retained: Vec<RetainedObligation>,
    /// Stores that reported a limitation instead of a successful deletion.
    pub blocked: Vec<BlockedStep>,
    /// Streams whose retained history begins after their first sequence, so a
    /// reconnect at an older cursor receives explicit cursor-expiry guidance.
    pub cursor_expiry: usize,
}

/// One package-owned store that participates in coordinated lifecycle work.
///
/// The shared runtime never inspects the package's files or schema. The package
/// returns its own counts and errors, and the coordinator records them in the
/// same durable report as the shared stores.
#[async_trait::async_trait]
pub trait LifecycleOwner: Send + Sync {
    /// Remove content owned by one principal in one namespace.
    async fn erase_principal(&self, namespace: &str, principal: &str) -> Result<usize>;

    /// Remove content older than `cutoff_ms` under the package's own policy.
    async fn retention(&self, cutoff_ms: i64) -> Result<usize>;
}

impl Runtime {
    /// Apply the configured retention policy.
    ///
    /// Stages cover conversations, graph checkpoints, task metadata, durable
    /// events, artifacts, ingested sources, native memory, native graph facts,
    /// audit, and telemetry. Unfinished recovery state is never discarded.
    pub async fn apply_retention(&self, principal: &common::Principal) -> Result<LifecycleReport> {
        principal.require(scopes::INSTANCE_ADMIN)?;
        let policy = self.config().retention.clone();
        let timestamp = now();
        let cutoff = |days: i64| timestamp - days.max(0) * 86_400_000;
        let job_id = "retention:instance".to_string();
        let (mut job, resumed) = self
            .begin_cleanup(&job_id, "retention", &principal.namespace, "")
            .await?;
        let mut report = LifecycleReport {
            job_id: job.id.clone(),
            resumed,
            removed: job.removed.clone(),
            retained: job.retained.clone(),
            ..LifecycleReport::default()
        };

        // Conversations. Unfinished task sessions are retained because the
        // native history is still required to resume them.
        if policy.sessions_days > 0 && !job.completed.contains(&"sessions".into()) {
            let preserve = self
                .store()
                .read(|state| {
                    state
                        .tasks
                        .values()
                        .filter(|task| !task.state.is_terminal())
                        .map(|task| task.session_id.clone())
                        .collect::<BTreeSet<_>>()
                })
                .await;
            match self.sessions().await {
                Some(sessions) => match sessions
                    .retention(cutoff(policy.sessions_days), &preserve)
                    .await
                {
                    Ok(count) => {
                        self.complete_stage(&mut job, "sessions", count, &mut report)
                            .await?;
                    }
                    Err(error) => {
                        self.block_stage(&mut job, "sessions", &error, &mut report)
                            .await?;
                    }
                },
                None => {
                    self.block_stage(
                        &mut job,
                        "sessions",
                        &Error::new(
                            "retention.store_not_composed",
                            "no native session store is attached to this runtime",
                        ),
                        &mut report,
                    )
                    .await?;
                }
            }
        }

        // Native graph checkpoints. A non-terminal task keeps its newest
        // checkpoint and reports the retained obligation.
        if policy.checkpoints_days > 0 && !job.completed.contains(&"checkpoints".into()) {
            let tasks = self
                .store()
                .read(|state| {
                    state
                        .tasks
                        .values()
                        .filter_map(|task| {
                            task.native_thread_id.clone().map(|thread| {
                                (task.id.clone(), thread, task.state, task.updated_at)
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let checkpointers = self.graph_checkpointers().await;
            let mut removed = 0usize;
            for (task_id, thread, state, updated_at) in tasks {
                if !state.is_terminal() {
                    let obligation = RetainedObligation {
                        class: "checkpoint".into(),
                        id: thread,
                        reason: "unfinished-task".into(),
                    };
                    job.retained.push(obligation.clone());
                    report.retained.push(obligation);
                    continue;
                }
                if updated_at >= cutoff(policy.checkpoints_days) {
                    continue;
                }
                if checkpointers.is_empty() {
                    self.block_stage(
                        &mut job,
                        "checkpoint",
                        &Error::new(
                            "retention.store_not_composed",
                            "no native checkpoint owner is attached to this runtime",
                        ),
                        &mut report,
                    )
                    .await?;
                    break;
                }
                for checkpointer in checkpointers.values() {
                    match checkpointer.delete_thread(&thread).await {
                        Ok(count) => removed += count,
                        Err(error) => {
                            self.block_stage(&mut job, "checkpoint", &error, &mut report)
                                .await?;
                            break;
                        }
                    }
                }
                let _ = task_id;
            }
            if !report.blocked.iter().any(|step| step.store == "checkpoint") {
                self.complete_stage(&mut job, "checkpoints", removed, &mut report)
                    .await?;
            }
        }

        // Turn-boundary file snapshots. They share the checkpoint window and
        // are removed only after their manifest records are gone; the owning
        // store then collects unreferenced blobs. An unfinished task keeps its
        // snapshots so recovery and rewind remain possible.
        if policy.checkpoints_days > 0 && !job.completed.contains(&"checkpoints.files".into()) {
            let preserve = self
                .store()
                .read(|state| {
                    state
                        .tasks
                        .values()
                        .filter(|task| !task.state.is_terminal())
                        .map(|task| task.id.clone())
                        .collect::<BTreeSet<_>>()
                })
                .await;
            match self
                .checkpoint_retention(cutoff(policy.checkpoints_days), &preserve)
                .await
            {
                Ok((records, blobs)) => {
                    if records > 0 {
                        report
                            .removed
                            .entry("checkpoint_files".into())
                            .and_modify(|value| *value += records)
                            .or_insert(records);
                    }
                    if blobs > 0 {
                        report
                            .removed
                            .entry("checkpoint_blobs".into())
                            .and_modify(|value| *value += blobs)
                            .or_insert(blobs);
                    }
                    self.complete_stage(
                        &mut job,
                        "checkpoints.files",
                        records + blobs,
                        &mut report,
                    )
                    .await?;
                }
                Err(error) => {
                    self.block_stage(&mut job, "checkpoint.files", &error, &mut report)
                        .await?;
                }
            }
        }

        // Task metadata. Only terminal work past the window is removed; this is
        // the same set whose checkpoints and events are already eligible.
        if policy.tasks_days > 0 && !job.completed.contains(&"tasks".into()) {
            let (removed, retained) = self
                .store()
                .write(|state| {
                    let terminal: Vec<String> = state
                        .tasks
                        .values()
                        .filter(|task| {
                            task.state.is_terminal() && task.updated_at < cutoff(policy.tasks_days)
                        })
                        .map(|task| task.id.clone())
                        .collect();
                    for id in &terminal {
                        state.tasks.remove(id);
                        state.events.remove(id);
                        state.task_index.retain(|_, value| value != id);
                        state
                            .approvals
                            .retain(|_, approval| &approval.task_id != id);
                        state
                            .operations
                            .retain(|_, operation| &operation.task_id != id);
                        state.traces.retain(|trace| &trace.task_id != id);
                    }
                    let retained = state
                        .tasks
                        .values()
                        .filter(|task| !task.state.is_terminal())
                        .map(|task| RetainedObligation {
                            class: "task".into(),
                            id: task.id.clone(),
                            reason: "unfinished-task".into(),
                        })
                        .collect::<Vec<_>>();
                    Ok((terminal.len(), retained))
                })
                .await?;
            job.retained.extend(retained.clone());
            report.retained.extend(retained);
            self.complete_stage(&mut job, "tasks", removed, &mut report)
                .await?;
        }

        // Durable events. Event history for unfinished tasks is preserved.
        if policy.events_days > 0 && !job.completed.contains(&"events".into()) {
            let (removed, cursor_expiry) = self
                .store()
                .write(|state| {
                    let terminal: BTreeSet<String> = state
                        .tasks
                        .values()
                        .filter(|task| task.state.is_terminal())
                        .map(|task| task.id.clone())
                        .collect();
                    let unfinished: BTreeSet<String> = state
                        .tasks
                        .values()
                        .filter(|task| !task.state.is_terminal())
                        .map(|task| task.id.clone())
                        .collect();
                    let mut removed = 0usize;
                    let mut cursor_expiry = 0usize;
                    state.events.retain(|task_id, _| {
                        terminal.contains(task_id) || unfinished.contains(task_id)
                    });
                    for (task_id, stream) in state.events.iter_mut() {
                        if unfinished.contains(task_id) {
                            continue;
                        }
                        let before = stream.len();
                        stream.retain(|event| event.at >= cutoff(policy.events_days));
                        removed += before - stream.len();
                        if before > stream.len()
                            && stream.first().is_some_and(|event| event.seq > 1)
                        {
                            cursor_expiry += 1;
                        }
                    }
                    Ok((removed, cursor_expiry))
                })
                .await?;
            report.cursor_expiry = cursor_expiry;
            self.complete_stage(&mut job, "events", removed, &mut report)
                .await?;
        }

        // Expired access grants are always removed, even when artifact content
        // retention is disabled for this deployment.
        if !job.completed.contains(&"grants".into()) {
            let expired = self
                .store()
                .write(|state| {
                    let before = state.grants.len();
                    state.grants.retain(|_, grant| grant.expires_at > timestamp);
                    Ok(before - state.grants.len())
                })
                .await?;
            self.complete_stage(&mut job, "grants", expired, &mut report)
                .await?;
        }

        // Artifact metadata and immutable content. A valid share grant or an
        // unfinished task that references the artifact keeps both.
        if policy.artifacts_days > 0 && !job.completed.contains(&"artifacts".into()) {
            let (candidates, retained) = self
                .store()
                .read(|state| {
                    let evidence: Vec<String> = state
                        .tasks
                        .values()
                        .filter(|task| !task.state.is_terminal())
                        .filter_map(|task| task.evidence.as_ref())
                        .map(|value| value.to_string())
                        .collect();
                    let mut candidates = Vec::new();
                    let mut retained = Vec::new();
                    for record in state.artifacts.values() {
                        let artifact = &record.artifact;
                        if artifact.created_at >= cutoff(policy.artifacts_days) {
                            continue;
                        }
                        let shared = state.grants.values().any(|grant| {
                            grant.artifact_id == artifact.id && grant.expires_at > timestamp
                        });
                        if shared {
                            retained.push(RetainedObligation {
                                class: "artifact".into(),
                                id: artifact.id.clone(),
                                reason: "outstanding-grant".into(),
                            });
                            continue;
                        }
                        if evidence.iter().any(|value| value.contains(&artifact.id)) {
                            retained.push(RetainedObligation {
                                class: "artifact".into(),
                                id: artifact.id.clone(),
                                reason: "unfinished-task-evidence".into(),
                            });
                            continue;
                        }
                        candidates.push((
                            artifact.id.clone(),
                            record.scope.clone(),
                            artifact.version,
                        ));
                    }
                    (candidates, retained)
                })
                .await;
            job.retained.extend(retained.clone());
            report.retained.extend(retained);
            let mut removed = 0usize;
            let mut blocked = false;
            for (id, scope, version) in candidates {
                match self.delete_native_artifact(&scope, version).await {
                    Ok(()) => {
                        self.store()
                            .write(|state| {
                                state.artifacts.remove(&id);
                                state.grants.retain(|_, grant| grant.artifact_id != id);
                                Ok(())
                            })
                            .await?;
                        removed += 1;
                    }
                    Err(error) => {
                        self.block_stage(&mut job, "artifact", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                }
            }
            if !blocked {
                self.complete_stage(&mut job, "artifacts", removed, &mut report)
                    .await?;
            }
        }

        // Ingested sources and their derived memory/graph rows. Document age is
        // the source-owner policy; derivatives follow the same decision.
        if policy.documents_days > 0 && !job.completed.contains(&"documents".into()) {
            let documents = self
                .store()
                .read(|state| {
                    state
                        .documents
                        .values()
                        .filter(|document| document.created_at < cutoff(policy.documents_days))
                        .map(|document| (document.id.clone(), document.owner.clone()))
                        .collect::<Vec<_>>()
                })
                .await;
            let mut removed = 0usize;
            let mut blocked = false;
            for (id, _owner) in documents {
                if let Some(memory) = self.memory().await {
                    if let Err(error) = memory.erase_document(&id).await {
                        self.block_stage(&mut job, "document", &error, &mut report)
                            .await?;
                        blocked = true;
                        continue;
                    }
                }
                if let Some(graph) = self.graph().await {
                    if let Err(error) = graph.erase_document(&id).await {
                        self.block_stage(&mut job, "document", &error, &mut report)
                            .await?;
                        blocked = true;
                        continue;
                    }
                }
                self.store()
                    .write(|state| {
                        state.documents.remove(&id);
                        Ok(())
                    })
                    .await?;
                removed += 1;
            }
            if !blocked {
                self.complete_stage(&mut job, "documents", removed, &mut report)
                    .await?;
            }
        }

        // Package-owned indexes and source bytes apply their own documented
        // retention while reporting into this coordinated result.
        if policy.documents_days > 0 && !job.completed.contains(&"packages".into()) {
            let owners = self.lifecycle_owners().await;
            let mut removed = 0usize;
            let mut blocked = false;
            for owner in owners {
                match owner.retention(cutoff(policy.documents_days)).await {
                    Ok(count) => removed += count,
                    Err(error) => {
                        self.block_stage(&mut job, "package", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                }
            }
            if !blocked {
                self.complete_stage(&mut job, "packages", removed, &mut report)
                    .await?;
            }
        }

        // Native long-term memory and graph facts.
        if policy.memory_days > 0 && !job.completed.contains(&"memory".into()) {
            let mut removed = 0usize;
            let mut blocked = false;
            if let Some(memory) = self.memory().await {
                match memory
                    .prune_before(Utc::now() - Duration::days(policy.memory_days.max(0)))
                    .await
                {
                    Ok(count) => removed = count,
                    Err(error) => {
                        self.block_stage(&mut job, "memory", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                }
            }
            if let Some(graph) = self.graph().await {
                match graph
                    .prune_before(Utc::now() - Duration::days(policy.memory_days.max(0)))
                    .await
                {
                    Ok(count) => removed += count,
                    Err(error) => {
                        self.block_stage(&mut job, "knowledge", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                }
            }
            if !blocked {
                self.complete_stage(&mut job, "memory", removed, &mut report)
                    .await?;
            }
        }

        // Audit and telemetry have independent windows. The lifecycle audit
        // record for this run is written after the stage so it cannot be
        // deleted by the run that produced it.
        if policy.audit_days > 0 && !job.completed.contains(&"audit".into()) {
            let removed = self
                .store()
                .write(|state| {
                    let before = state.audit.len();
                    state
                        .audit
                        .retain(|record| record.at >= cutoff(policy.audit_days));
                    Ok(before - state.audit.len())
                })
                .await?;
            self.complete_stage(&mut job, "audit", removed, &mut report)
                .await?;
        }
        if policy.traces_days > 0 && !job.completed.contains(&"telemetry".into()) {
            let removed = self
                .store()
                .write(|state| {
                    let before = state.traces.len();
                    state
                        .traces
                        .retain(|trace| trace.at >= cutoff(policy.traces_days));
                    Ok(before - state.traces.len())
                })
                .await?;
            self.complete_stage(&mut job, "telemetry", removed, &mut report)
                .await?;
        }

        report.completed = report.blocked.is_empty();
        if report.completed {
            self.finish_cleanup(&job.id).await?;
        }
        let _ = self
            .audit(
                &principal.id,
                "retention.apply",
                &job.id,
                if report.completed {
                    "applied"
                } else {
                    "blocked"
                },
                &redacted_counts(&report.removed),
            )
            .await;
        Ok(report)
    }

    /// Erase one principal's authoritative content, projections, indexes,
    /// caches, and access grants in the caller's namespace.
    ///
    /// A caller may erase itself; erasing another principal requires `admin`.
    /// A principal in a different namespace cannot address or inspect this
    /// target because the namespace comes from the authenticated caller.
    pub async fn erase_principal(
        &self,
        principal: &common::Principal,
        target: &str,
    ) -> Result<LifecycleReport> {
        principal.require(scopes::ADMIN)?;
        if target.trim().is_empty() {
            return Err(Error::new(
                "retention.target_required",
                "an erasure target principal is required",
            ));
        }
        if target != principal.id {
            principal.require(scopes::INSTANCE_ADMIN).or_else(|_| {
                // A namespace administrator may erase work in its own
                // namespace, but never a principal in another namespace.
                principal.require_namespace(&principal.namespace)
            })?;
        }
        let namespace = principal.namespace.clone();
        let target = target.to_string();
        let job_id = format!("erasure:{namespace}:{target}");
        let (mut job, resumed) = self
            .begin_cleanup(&job_id, "erasure", &namespace, &target)
            .await?;
        let mut report = LifecycleReport {
            job_id: job.id.clone(),
            resumed,
            removed: job.removed.clone(),
            retained: job.retained.clone(),
            ..LifecycleReport::default()
        };
        let native_user = crate::execution::runner::native_user_id(&namespace, &target)?;

        let task_ids_of_target = |state: &State| -> Vec<String> {
            state
                .tasks
                .values()
                .filter(|task| task.namespace == namespace && task.caller == target)
                .map(|task| task.id.clone())
                .collect()
        };
        let document_ids_of_target = |state: &State| -> Vec<String> {
            state
                .documents
                .values()
                .filter(|document| document.namespace == namespace && document.owner == target)
                .map(|document| document.id.clone())
                .collect()
        };

        // Conversations are owned by the native session store.
        if !job.completed.contains(&"sessions".into()) {
            match self.sessions().await {
                Some(sessions) => match sessions.erase_user(&native_user).await {
                    Ok(count) => {
                        self.complete_stage(&mut job, "sessions", count, &mut report)
                            .await?;
                    }
                    Err(error) => {
                        self.block_stage(&mut job, "sessions", &error, &mut report)
                            .await?;
                    }
                },
                None => {
                    self.block_stage(
                        &mut job,
                        "sessions",
                        &Error::new(
                            "erasure.store_not_composed",
                            "no native session store is attached to this runtime",
                        ),
                        &mut report,
                    )
                    .await?;
                }
            }
        }

        // Ingested sources and every derivative memory/graph row they produced.
        if !job.completed.contains(&"documents".into()) {
            let documents = self
                .store()
                .read(|state| document_ids_of_target(state))
                .await;
            let mut removed = 0usize;
            let mut blocked = false;
            for id in documents {
                if let Some(memory) = self.memory().await {
                    if let Err(error) = memory.erase_document(&id).await {
                        self.block_stage(&mut job, "document", &error, &mut report)
                            .await?;
                        blocked = true;
                        continue;
                    }
                }
                if let Some(graph) = self.graph().await {
                    if let Err(error) = graph.erase_document(&id).await {
                        self.block_stage(&mut job, "document", &error, &mut report)
                            .await?;
                        blocked = true;
                        continue;
                    }
                }
                self.store()
                    .write(|state| {
                        state.documents.remove(&id);
                        Ok(())
                    })
                    .await?;
                removed += 1;
            }
            if !blocked {
                self.complete_stage(&mut job, "documents", removed, &mut report)
                    .await?;
            }
        }

        // Package-owned indexes, source bytes, and derived rows. Each package
        // owns its schema and returns its own count or blocking error.
        if !job.completed.contains(&"packages".into()) {
            let owners = self.lifecycle_owners().await;
            let mut removed = 0usize;
            let mut blocked = false;
            for owner in owners {
                match owner.erase_principal(&namespace, &target).await {
                    Ok(count) => removed += count,
                    Err(error) => {
                        self.block_stage(&mut job, "package", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                }
            }
            if !blocked {
                self.complete_stage(&mut job, "packages", removed, &mut report)
                    .await?;
            }
        }

        // Native long-term memory and graph facts for exactly one principal.
        if !job.completed.contains(&"memory".into()) {
            let mut removed = 0usize;
            let mut blocked = false;
            match self.memory().await {
                Some(memory) => match memory.erase_principal(&namespace, &target).await {
                    Ok(()) => removed += 1,
                    Err(error) => {
                        self.block_stage(&mut job, "memory", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                },
                None => {
                    self.block_stage(
                        &mut job,
                        "memory",
                        &Error::new(
                            "erasure.store_not_composed",
                            "no native long-term memory store is attached to this runtime",
                        ),
                        &mut report,
                    )
                    .await?;
                    blocked = true;
                }
            }
            if let Some(graph) = self.graph().await {
                match graph.erase_principal(&namespace, &target).await {
                    Ok(()) => removed += 1,
                    Err(error) => {
                        self.block_stage(&mut job, "knowledge", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                }
            }
            if !blocked {
                self.complete_stage(&mut job, "memory", removed, &mut report)
                    .await?;
            }
        }

        // Native graph checkpoints. An unfinished task keeps its checkpoint so
        // recovery remains possible after the rest of the content is erased.
        if !job.completed.contains(&"checkpoints".into()) {
            let tasks = self
                .store()
                .read(|state| {
                    state
                        .tasks
                        .values()
                        .filter(|task| task.namespace == namespace && task.caller == target)
                        .filter_map(|task| {
                            task.native_thread_id
                                .clone()
                                .map(|thread| (task.id.clone(), thread, task.state))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let checkpointers = self.graph_checkpointers().await;
            let mut removed = 0usize;
            let mut blocked = false;
            for (task_id, thread, state) in tasks {
                if !state.is_terminal() {
                    let obligation = RetainedObligation {
                        class: "checkpoint".into(),
                        id: thread,
                        reason: "unfinished-task".into(),
                    };
                    job.retained.push(obligation.clone());
                    report.retained.push(obligation);
                    let _ = task_id;
                    continue;
                }
                if checkpointers.is_empty() {
                    self.block_stage(
                        &mut job,
                        "checkpoint",
                        &Error::new(
                            "erasure.store_not_composed",
                            "no native checkpoint owner is attached to this runtime",
                        ),
                        &mut report,
                    )
                    .await?;
                    blocked = true;
                    break;
                }
                for checkpointer in checkpointers.values() {
                    match checkpointer.delete_thread(&thread).await {
                        Ok(count) => removed += count,
                        Err(error) => {
                            self.block_stage(&mut job, "checkpoint", &error, &mut report)
                                .await?;
                            blocked = true;
                            break;
                        }
                    }
                }
            }
            if !blocked {
                self.complete_stage(&mut job, "checkpoints", removed, &mut report)
                    .await?;
            }
        }

        // Turn-boundary file snapshots for the erased principal. An unfinished
        // task keeps them as a retained obligation, matching native graph
        // checkpoints. Terminal tasks remove manifest records and then their
        // now-unreferenced blobs.
        if !job.completed.contains(&"checkpoints.files".into()) {
            let (erase, preserve) = self
                .store()
                .read(|state| {
                    let mut erase = BTreeSet::new();
                    let mut preserve = Vec::new();
                    for task in state
                        .tasks
                        .values()
                        .filter(|task| task.namespace == namespace && task.caller == target)
                    {
                        if task.state.is_terminal() {
                            erase.insert(task.id.clone());
                        } else {
                            preserve.push(task.id.clone());
                        }
                    }
                    (erase, preserve)
                })
                .await;
            for task_id in preserve {
                let obligation = RetainedObligation {
                    class: "checkpoint-file".into(),
                    id: task_id,
                    reason: "unfinished-task".into(),
                };
                job.retained.push(obligation.clone());
                report.retained.push(obligation);
            }
            match self.checkpoint_erase_tasks(&erase).await {
                Ok((records, blobs)) => {
                    self.complete_stage(
                        &mut job,
                        "checkpoints.files",
                        records + blobs,
                        &mut report,
                    )
                    .await?;
                }
                Err(error) => {
                    self.block_stage(&mut job, "checkpoint.files", &error, &mut report)
                        .await?;
                }
            }
        }

        // Artifacts and grants owned by the target. Native content is deleted
        // before metadata so a failed backend is reported, not marked erased.
        if !job.completed.contains(&"artifacts".into()) {
            let artifacts = self
                .store()
                .read(|state| {
                    state
                        .artifacts
                        .values()
                        .filter(|record| {
                            record.artifact.namespace == namespace
                                && record.artifact.owner == target
                        })
                        .map(|record| {
                            (
                                record.artifact.id.clone(),
                                record.scope.clone(),
                                record.artifact.version,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let mut removed = 0usize;
            let mut blocked = false;
            for (id, scope, version) in artifacts {
                match self.delete_native_artifact(&scope, version).await {
                    Ok(()) => {
                        self.store()
                            .write(|state| {
                                state.artifacts.remove(&id);
                                state.grants.retain(|_, grant| grant.artifact_id != id);
                                Ok(())
                            })
                            .await?;
                        removed += 1;
                    }
                    Err(error) => {
                        self.block_stage(&mut job, "artifact", &error, &mut report)
                            .await?;
                        blocked = true;
                    }
                }
            }
            if !blocked {
                self.complete_stage(&mut job, "artifacts", removed, &mut report)
                    .await?;
            }
        }

        // Harness projections, indexes, approvals, operations, traces, tokens,
        // and redacted audit. Unfinished tasks keep their recovery structure
        // with content stripped.
        if !job.completed.contains(&"metadata".into()) {
            let (removed, retained) = self
                .store()
                .write(|state| {
                    let task_ids = task_ids_of_target(state);
                    let task_set: BTreeSet<String> = task_ids.iter().cloned().collect();
                    let terminal: BTreeSet<String> = state
                        .tasks
                        .values()
                        .filter(|task| task_set.contains(&task.id) && task.state.is_terminal())
                        .map(|task| task.id.clone())
                        .collect();
                    let mut removed = BTreeMap::new();
                    let mut retained = Vec::new();
                    for id in &terminal {
                        state.tasks.remove(id);
                        state.events.remove(id);
                        state.task_index.retain(|_, value| value != id);
                    }
                    let mut scrubbed = 0usize;
                    for task in state.tasks.values_mut() {
                        if task_set.contains(&task.id) && !task.state.is_terminal() {
                            task.input = "[erased]".into();
                            task.output = None;
                            // Keep evidence references required for recovery.
                            scrubbed += 1;
                            retained.push(RetainedObligation {
                                class: "task".into(),
                                id: task.id.clone(),
                                reason: "unfinished-task".into(),
                            });
                        }
                    }
                    removed.insert("task_metadata".into(), terminal.len());
                    removed.insert("tasks_scrubbed".into(), scrubbed);
                    let before_events = state
                        .events
                        .iter()
                        .filter(|(id, _)| task_set.contains(*id))
                        .map(|(_, events)| events.len())
                        .sum::<usize>();
                    for (id, events) in state.events.iter_mut() {
                        if !task_set.contains(id) {
                            continue;
                        }
                        for event in events {
                            if !terminal.contains(id) {
                                event.payload = serde_json::json!({"erased": true});
                            }
                        }
                    }
                    let after_events = state
                        .events
                        .iter()
                        .filter(|(id, _)| task_set.contains(*id))
                        .map(|(_, events)| events.len())
                        .sum::<usize>();
                    removed.insert("events_removed".into(), before_events - after_events);

                    let mut approvals_removed = 0usize;
                    let mut approvals_retained = 0usize;
                    let mut keep = Vec::new();
                    for (id, mut approval) in std::mem::take(&mut state.approvals) {
                        if !task_set.contains(&approval.task_id) {
                            keep.push((id, approval));
                            continue;
                        }
                        if terminal.contains(&approval.task_id) {
                            approvals_removed += 1;
                            continue;
                        }
                        approval.target = "[erased]".into();
                        approval.risk = "[erased]".into();
                        approval.receipt = None;
                        approvals_retained += 1;
                        keep.push((id, approval));
                    }
                    state.approvals = keep.into_iter().collect();
                    removed.insert("approvals".into(), approvals_removed);
                    removed.insert("approvals_redacted".into(), approvals_retained);

                    let mut operations_removed = 0usize;
                    for operation in state.operations.values_mut() {
                        if task_set.contains(&operation.task_id) {
                            if terminal.contains(&operation.task_id) {
                                operations_removed += 1;
                            } else {
                                operation.external_ref = None;
                                operation.receipt = None;
                            }
                        }
                    }
                    state
                        .operations
                        .retain(|_, operation| !terminal.contains(&operation.task_id));
                    removed.insert("operations".into(), operations_removed);

                    let before_traces = state.traces.len();
                    state.traces.retain(|trace| {
                        !task_set.contains(&trace.task_id)
                            && !trace
                                .attributes
                                .values()
                                .any(|value| value.contains(&target))
                    });
                    removed.insert("traces".into(), before_traces - state.traces.len());

                    let before_tokens = state.tokens.len();
                    state.tokens.retain(|_, binding| {
                        !(binding.namespace == namespace && binding.id == target)
                    });
                    removed.insert("tokens".into(), before_tokens - state.tokens.len());

                    // Audit stays append-only and useful, but payload-bearing
                    // detail is redacted for the erased identity.
                    let mut redacted = 0usize;
                    for record in state.audit.iter_mut() {
                        if (record.target == target || record.principal == target)
                            && record.detail != "[redacted]"
                        {
                            record.detail = "[redacted]".into();
                            redacted += 1;
                        }
                    }
                    removed.insert("audit_redacted".into(), redacted);

                    Ok((removed, retained))
                })
                .await?;
            job.retained.extend(retained.clone());
            report.retained.extend(retained);
            let metadata_total = removed.values().sum::<usize>();
            for (class, count) in removed {
                report.removed.insert(class, count);
            }
            self.complete_stage(&mut job, "metadata", metadata_total, &mut report)
                .await?;
        }

        report.completed = report.blocked.is_empty();
        if report.completed {
            self.finish_cleanup(&job.id).await?;
        }
        let _ = self
            .audit(
                &principal.id,
                "principal.erase",
                &target,
                if report.completed {
                    "erased"
                } else {
                    "blocked"
                },
                &redacted_counts(&report.removed),
            )
            .await;
        Ok(report)
    }

    /// Count unfinished tasks that require explicit reconciliation.
    pub async fn recovery_tasks(&self) -> Vec<String> {
        self.store()
            .read(|state| {
                state
                    .tasks
                    .values()
                    .filter(|task| {
                        matches!(
                            task.state,
                            TaskState::Recovering | TaskState::Unknown | TaskState::Cancelling
                        )
                    })
                    .map(|task| task.id.clone())
                    .collect()
            })
            .await
    }

    /// Validate the persisted schema. Interrupted or unsupported data is
    /// quarantined, never silently migrated.
    pub async fn migrate(&self, principal: &common::Principal) -> Result<u32> {
        principal.require(scopes::INSTANCE_ADMIN)?;
        let schema = self.store().read(|state| state.schema).await;
        if schema > crate::DATA_SCHEMA {
            return Err(Error::new(
                "storage.schema_unsupported",
                "persisted data was written by a newer schema",
            ));
        }
        if schema < crate::DATA_SCHEMA {
            self.store()
                .write(|state| {
                    state.schema = crate::DATA_SCHEMA;
                    Ok(())
                })
                .await?;
        }
        Ok(crate::DATA_SCHEMA)
    }

    async fn begin_cleanup(
        &self,
        id: &str,
        kind: &str,
        namespace: &str,
        target: &str,
    ) -> Result<(CleanupJob, bool)> {
        let existing = self
            .store()
            .read(|state| state.cleanup.get(id).cloned())
            .await;
        if let Some(job) = existing {
            return Ok((job, true));
        }
        let job = CleanupJob {
            id: id.to_string(),
            kind: kind.to_string(),
            namespace: namespace.to_string(),
            target: target.to_string(),
            started_at: now(),
            updated_at: now(),
            completed: Vec::new(),
            blocked: Vec::new(),
            removed: BTreeMap::new(),
            retained: Vec::new(),
        };
        self.store()
            .write(|state| {
                state.cleanup.insert(id.to_string(), job.clone());
                Ok(())
            })
            .await?;
        Ok((job, false))
    }

    async fn complete_stage(
        &self,
        job: &mut CleanupJob,
        stage: &str,
        count: usize,
        report: &mut LifecycleReport,
    ) -> Result<()> {
        if count > 0 {
            report
                .removed
                .entry(stage.to_string())
                .and_modify(|value| *value += count)
                .or_insert(count);
            *job.removed.entry(stage.to_string()).or_insert(0) += count;
        }
        job.completed.push(stage.to_string());
        job.updated_at = now();
        merge_retained(&mut job.retained, &report.retained);
        self.persist_cleanup(job).await
    }

    async fn block_stage(
        &self,
        job: &mut CleanupJob,
        store: &str,
        error: &Error,
        report: &mut LifecycleReport,
    ) -> Result<()> {
        let label = format!("{store}:{}", error.code);
        if !job.blocked.contains(&label) {
            job.blocked.push(label);
            report.blocked.push(BlockedStep {
                store: store.to_string(),
                code: error.code.clone(),
                message: error.message.clone(),
            });
        }
        job.updated_at = now();
        merge_retained(&mut job.retained, &report.retained);
        self.persist_cleanup(job).await
    }

    async fn persist_cleanup(&self, job: &CleanupJob) -> Result<()> {
        self.store()
            .write(|state| {
                state.cleanup.insert(job.id.clone(), job.clone());
                Ok(())
            })
            .await
    }

    async fn finish_cleanup(&self, id: &str) -> Result<()> {
        self.store()
            .write(|state| {
                state.cleanup.remove(id);
                Ok(())
            })
            .await
    }
}

fn redacted_counts(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(name, count)| format!("{name}={count}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn merge_retained(into: &mut Vec<RetainedObligation>, from: &[RetainedObligation]) {
    for obligation in from {
        if !into.iter().any(|current| {
            current.class == obligation.class
                && current.id == obligation.id
                && current.reason == obligation.reason
        }) {
            into.push(obligation.clone());
        }
    }
}
