//! General Worker Package composition.
//!
//! One [`runtime::worker::Worker`] definition supplies package identity,
//! compiled capability registrations, native callbacks/plugins, and Worker
//! hooks. Agents, prompts, and skills come from the operator-selected
//! configuration; only the sandbox command tool and the manual diagnostic agent
//! are compiled registrations.

mod agent;
mod artifact;
mod callbacks;
mod hooks;
mod package;
mod sandbox;

pub use artifact::ArtifactTool;
pub use callbacks::native_callbacks;
pub use hooks::{audit_plugin, PackageHooks};
pub use package::{manifest, worker};
pub use sandbox::{default_sandbox_workspace, sandbox_policy, sandbox_tool};
