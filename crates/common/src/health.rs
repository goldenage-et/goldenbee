//! Worker health and correlated operational evidence.

use crate::limits::Capacity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Process lifecycle phase.
///
/// This is deliberately separate from subsystem health and from individual
/// task state. A live process can be recovering, warming, ready, or draining
/// while an execution subsystem is degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    /// Configuration, storage, or composition is still opening.
    Initializing,
    /// Durable work is being inspected and reconciled.
    Recovering,
    /// Recovery metadata is settled and startup hooks are running.
    Warming,
    /// Admission and the listener are ready for new work.
    Ready,
    /// New admission is closed; admitted work is finishing or being stopped.
    Draining,
    /// All owned resources have stopped cleanly.
    Stopped,
    /// All owned resources stopped, but a required step reported a failure.
    StoppedDegraded,
    /// Startup failed before readiness.
    Failed,
}

impl LifecyclePhase {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Recovering => "recovering",
            Self::Warming => "warming",
            Self::Ready => "ready",
            Self::Draining => "draining",
            Self::Stopped => "stopped",
            Self::StoppedDegraded => "stopped_degraded",
            Self::Failed => "failed",
        }
    }

    /// Whether the phase is a terminal process phase.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::StoppedDegraded | Self::Failed)
    }

    /// Whether the phase is allowed to admit new work.
    pub fn admits_work(self) -> bool {
        matches!(self, Self::Ready)
    }
}

impl std::fmt::Display for LifecyclePhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Health response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    /// Typed lifecycle phase. `phase` carries the same stable wire name.
    #[serde(default = "default_phase")]
    pub lifecycle: LifecyclePhase,
    /// Ordered phases observed by this process, including the current one.
    #[serde(default)]
    pub phase_history: Vec<String>,
    /// `initializing`, `recovering`, `warming`, `ready`, `draining`,
    /// `stopped`, `stopped_degraded`, or `failed`.
    pub phase: String,
    /// Instance identity.
    pub instance: String,
    /// Installed package identity.
    #[serde(default)]
    pub package: String,
    /// Installed package version.
    #[serde(default)]
    pub version: String,
    /// Whether this process currently accepts new work.
    #[serde(default)]
    pub ready: bool,
    /// Why readiness is false, when it is false.
    #[serde(default)]
    pub readiness_reason: String,
    /// `open` or `closed`.
    #[serde(default = "default_admission")]
    pub admission: String,
    /// Process incarnation.
    pub incarnation: String,
    /// Configuration revision.
    pub config_revision: u64,
    /// Resolved asset revision.
    #[serde(default)]
    pub asset_revision: u64,
    /// Supported capability IDs.
    pub capabilities: Vec<String>,
    /// Capacity.
    pub capacity: Capacity,
    /// Subsystem health. Provider reachability never changes liveness.
    pub subsystems: BTreeMap<String, String>,
    /// Explicit degradations. Empty is a clean, fully observed state.
    #[serde(default)]
    pub degraded: Vec<String>,
    /// Bounded operational metrics. Labels never carry task, session, secret,
    /// or native object values.
    #[serde(default)]
    pub metrics: Vec<crate::telemetry::MetricSample>,
    /// Uptime in milliseconds.
    pub uptime_ms: u64,
}

fn default_phase() -> LifecyclePhase {
    LifecyclePhase::Initializing
}

fn default_admission() -> String {
    "closed".into()
}

/// Correlated operational evidence sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSample {
    /// Trace identity.
    pub trace_id: String,
    /// Task identity.
    pub task_id: String,
    /// Span name.
    pub span: String,
    /// Duration.
    pub duration_ms: u64,
    /// Redacted attributes.
    pub attributes: BTreeMap<String, String>,
    /// Time.
    pub at: i64,
}
