//! Native execution boundary and adapters.

pub mod budget;
pub mod model;
pub(crate) mod operations;
pub mod plugins;
pub mod profile;
pub(crate) mod proof;
pub mod providers;
pub mod runner;

use crate::Runtime;
use common::{ApiError, Task, TaskState, Usage};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// A native graph checkpoint that can be resumed or read as a terminal result.
#[derive(Debug, Clone)]
pub struct GraphResume {
    /// Native thread identity. This is the session thread under `Agent::run`.
    pub thread_id: String,
    /// Frontier still pending at the committed checkpoint.
    pub pending_nodes: Vec<String>,
    /// Native super-step count at the checkpoint.
    pub step: usize,
    /// Canonical native graph state.
    pub state: Value,
    /// Final output already committed in the state, when present.
    pub output: Option<String>,
}

/// Result of inspecting a task's native checkpoint before resuming it.
#[derive(Debug, Clone)]
pub enum ResumeInspection {
    /// The executor does not own a resumable native checkpoint for this task.
    Unsupported,
    /// No checkpoint was committed for the task's native thread.
    Missing,
    /// The checkpoint exists but cannot be decoded.
    Corrupt {
        /// Actionable native error detail.
        detail: String,
    },
    /// The checkpoint belongs to a different graph definition.
    Incompatible {
        /// Actionable native error detail.
        detail: String,
    },
    /// A committed checkpoint with nodes still pending.
    Paused(GraphResume),
    /// A committed terminal checkpoint; the graph must not run again.
    Complete(GraphResume),
}

/// Outcome returned by a native executor.
#[derive(Debug, Clone)]
pub struct ExecutionOutcome {
    /// Output text, possibly partial.
    pub output: Option<String>,
    /// Truthful usage.
    pub usage: Usage,
    /// Definite failure.
    pub error: Option<ApiError>,
    /// Explicit state override. `None` means success or failure from `error`.
    pub state: Option<TaskState>,
    /// Confirmed native invocation identity.
    pub native_invocation_id: Option<String>,
    /// Confirmed native graph thread identity.
    pub native_thread_id: Option<String>,
    /// Confirmed native A2A task identity.
    pub native_a2a_task_id: Option<String>,
    /// Explicit uncertainty; never collapsed into success.
    pub uncertainty: Option<String>,
}

impl ExecutionOutcome {
    /// A successful outcome.
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: Some(output.into()),
            usage: Usage::default(),
            error: None,
            state: Some(TaskState::Succeeded),
            native_invocation_id: None,
            native_thread_id: None,
            native_a2a_task_id: None,
            uncertainty: None,
        }
    }

    /// A definite failure.
    pub fn failure(error: ApiError) -> Self {
        Self {
            output: None,
            usage: Usage::default(),
            error: Some(error),
            state: Some(TaskState::Failed),
            native_invocation_id: None,
            native_thread_id: None,
            native_a2a_task_id: None,
            uncertainty: None,
        }
    }

    /// An explicitly uncertain outcome.
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            output: None,
            usage: Usage::default(),
            error: None,
            state: Some(TaskState::Unknown),
            native_invocation_id: None,
            native_thread_id: None,
            native_a2a_task_id: None,
            uncertainty: Some(reason.into()),
        }
    }

    /// A cooperative cancellation outcome.
    pub fn cancelled() -> Self {
        Self {
            output: None,
            usage: Usage::default(),
            error: None,
            state: Some(TaskState::Cancelled),
            native_invocation_id: None,
            native_thread_id: None,
            native_a2a_task_id: None,
            uncertainty: None,
        }
    }
}

/// Native execution boundary.
#[async_trait::async_trait]
pub trait Executor: Send + Sync {
    /// Execute a durably accepted task.
    async fn execute(
        &self,
        runtime: Runtime,
        task: Task,
        cancel: CancellationToken,
    ) -> ExecutionOutcome;
    /// Request cancellation of an active task. Returning `true` confirms it.
    async fn cancel(&self, task_id: &str) -> bool;

    /// Inspect a task's native checkpoint before dispatch.
    ///
    /// Executors without native resumable state return
    /// `ResumeInspection::Unsupported`.
    async fn inspect_resume(&self, _task: &Task) -> ResumeInspection {
        ResumeInspection::Unsupported
    }
}
