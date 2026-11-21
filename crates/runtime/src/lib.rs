//! Shared Worker runtime and native ADK integration.
//!
//! The runtime owns Worker task admission, execution, sessions, profiles,
//! approvals, artifacts, memory, and effect receipts. Gateway control-plane
//! behavior belongs to `gateway-core`; package installation and host lifecycle
//! belong to the installer and instance manager.

mod agents;
pub mod browser;
pub mod checkpoints;
pub(crate) mod commands;
pub mod context;
mod core;
mod data;
mod execution;
pub mod headless;
pub mod master;
pub mod mcp;
pub mod peer;
pub mod plugins;
pub mod policy;
mod process;
mod sandbox;
pub mod serving;
mod sessions;
mod tasks;
mod tools;
pub mod worker;

/// Shared Worker runtime version advertised by package manifests.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version of the shared Worker runtime library.
///
/// Packages declare this value in signed release metadata instead of copying
/// their own package version, so an independently versioned package remains
/// compatible with a host policy that pins the runtime release.
pub const RUNTIME_VERSION: &str = VERSION;

pub use core::config::{
    AgentRequirement, ProviderProfile, RetentionPolicy, RuntimeConfig, DEFAULT_MAX_STREAMS,
    DEFAULT_STREAM_BUFFER, DEFAULT_STREAM_KEEPALIVE_MS,
};
pub use core::error::Result;
pub use core::ids::new_id;
pub use core::runtime::Runtime;
pub use core::store::{
    ArtifactRecord, ArtifactScope, AuditRecord, BlobRef, CheckpointFileRecord, CheckpointRecord,
    CheckpointRestore, CleanupJob, Counters, DocumentRecord, OperationRecord, ReleaseRecord,
    RetainedObligation, State, Store, DATA_SCHEMA,
};
pub use core::{config, crypto};
pub use data::artifacts::ArtifactContent;
pub use data::retention::{BlockedStep, LifecycleOwner, LifecycleReport};
pub use execution::operations::{
    BeginOutcome, EffectApprovalPolicy, EffectEvidence, EffectPolicy, EffectReconciler,
    EffectRecovery, EffectStatus, EffectTerminal, OperationView,
};
pub use execution::providers::{
    adapter_applies_reasoning, EnvironmentSecrets, FallbackDecision, MemorySecrets,
    ProviderRegistry, ProviderRequirements, ProviderView, SecretSource,
};
pub use execution::{ExecutionOutcome, Executor};
pub use headless::resolve_sources;
pub use master::{
    CriterionResult, MasterPlan, PlanChild, PlanEscalation, PlanEvaluation, PlanProvenance,
    PlanRecovery,
};
pub use peer::{DelegateRequest, DelegatedArtifact, PeerConfig, PeerDelegation, PeerRecord};
pub use policy::{
    ActionEffect, Decision as PermissionDecision, PermissionMode, PermissionPolicy,
    PermissionRequest, PermissionRule, PolicyDecision, RuleDecision,
};
pub use process::{
    ProcessError, ProcessMode, ProcessPolicy, ProcessRuntime, ProcessState, ProcessView,
};
pub use tasks::Recovery;

pub(crate) use common::ApiError;
pub(crate) use core::audit::push_audit;
pub(crate) use core::error::Error;
pub(crate) use core::ids::{now, profile_key};
pub(crate) use core::runtime::{PAUSE_RUNNING, PAUSE_WAITING};

/// Compatibility paths for the former `native` module tree.
pub mod native {
    pub use crate::agents::composition::effective_iterations;
    pub use crate::agents::graph::{
        DefinitionCheckpointer, GraphRegistration, GraphRuntime, GraphServices, GRAPH_STORE_FILE,
    };
    pub use crate::agents::{CapabilityRegistry, PackageManifest};
    pub use crate::data::artifacts::{ArtifactPayload, NativeArtifactStore, MAX_ARTIFACT_BYTES};
    pub use crate::data::graph::{NativeKnowledgeGraph, KNOWLEDGE_STORE_FILE};
    pub use crate::data::knowledge_tools::{
        ObserveKnowledgeTool, RecallKnowledgeTool, KNOWLEDGE_OBSERVE_TOOL, KNOWLEDGE_RECALL_TOOL,
    };
    pub use crate::data::memory::{
        preload_callback, NativeMemory, RememberMemoryTool, MEMORY_STORE_FILE,
    };
    pub use crate::execution::model::{model_for, DeterministicModel, HttpModel};
    pub use crate::execution::operations::{
        EffectApprovalPolicy, EffectEvidence, EffectPolicy, EffectReconciler, EffectRecovery,
        EffectStatus, EffectTerminal, OperationView,
    };
    pub use crate::execution::profile::ProfileAgentBuilder;
    pub use crate::execution::providers::{
        adapter_applies_reasoning, EnvironmentSecrets, FallbackDecision, MemorySecrets,
        ProviderRegistry, ProviderRequirements, ProviderView, SecretSource,
    };
    pub use crate::execution::runner::{decode_native_user_id, native_user_id, NativeExecutor};
    pub use crate::process::{
        ProcessError, ProcessMode, ProcessPolicy, ProcessRuntime, ProcessState, ProcessView,
    };
    pub use crate::sandbox::{
        ExecOutput, Sandbox, SandboxCommandTool, SandboxError, SandboxLimits, SandboxPolicy,
    };
    pub use crate::sessions::{FileSessionService, SESSION_STORE_FILE};
    pub use crate::tools::EffectTool;
    pub use crate::tools::WebPolicy;
    pub use common::KnowledgeFact;

    pub mod artifact {
        pub use crate::data::artifacts::{
            part_payload, ArtifactPayload, NativeArtifactStore, ARTIFACT_STORE_DIR,
            MAX_ARTIFACT_BYTES,
        };
    }
    pub mod executor {
        pub use crate::execution::profile::ProfileAgentBuilder;
        pub use crate::execution::runner::{decode_native_user_id, native_user_id, NativeExecutor};
    }
    pub mod memory {
        pub use crate::data::memory::{
            preload_callback, NativeMemory, RememberMemoryTool, MEMORY_STORE_FILE,
        };
    }
    pub mod model {
        pub use crate::execution::model::*;
    }
    pub mod package {
        pub use crate::agents::composition::effective_iterations;
        pub use crate::agents::{compose_agent, CapabilityRegistry, PackageManifest};
    }
    pub mod proof {
        pub use crate::execution::proof::*;
    }
    pub mod sandbox {
        pub use crate::sandbox::*;
    }
    pub mod session {
        pub use crate::sessions::{FileSessionService, SESSION_STORE_FILE, SESSION_STORE_SCHEMA};
    }
}
