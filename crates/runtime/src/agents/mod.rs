//! Worker package identity, registration checks, and native composition.
//!
//! A package owns its manifest. The shared runtime owns the generic checker and
//! the native agent composition helper so independently built packages do not
//! copy either concern.

use common::ApiError;
use std::collections::BTreeSet;
use std::result::Result as StdResult;

pub use common::PackageManifest;

/// Registration checker for compiled package capabilities.
#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    capabilities: Vec<String>,
}

impl CapabilityRegistry {
    /// Register one compiled capability id, rejecting duplicates.
    pub fn register(&mut self, id: impl Into<String>) -> StdResult<(), ApiError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ApiError::new(
                "package.capability_invalid",
                "capability id must not be empty",
            ));
        }
        if self.capabilities.iter().any(|existing| existing == &id) {
            return Err(ApiError::new(
                "package.duplicate_capability",
                format!("capability '{id}' is registered twice"),
            ));
        }
        self.capabilities.push(id);
        Ok(())
    }

    /// Registered capability ids.
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    /// Verify every declared capability is implemented and no compiled
    /// capability is missing from the manifest.
    pub fn verify(&self, manifest: &PackageManifest) -> StdResult<(), ApiError> {
        manifest.validate()?;
        let declared: BTreeSet<_> = manifest
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        let registered: BTreeSet<_> = self
            .capabilities
            .iter()
            .map(|capability| capability.as_str())
            .collect();
        if declared != registered {
            return Err(ApiError::new(
                "package.capability_mismatch",
                "manifest capabilities do not match actual registrations",
            )
            .with(
                "declared",
                declared.iter().copied().collect::<Vec<_>>().join(","),
            )
            .with(
                "registered",
                registered.iter().copied().collect::<Vec<_>>().join(","),
            ));
        }
        Ok(())
    }
}

pub(crate) mod composition;
pub(crate) mod definition;
#[cfg(not(feature = "graph"))]
#[path = "graph_disabled.rs"]
pub mod graph;
#[cfg(feature = "graph")]
pub mod graph;
pub(crate) mod workflow;

pub use composition::compose_agent;
pub use definition::{AgentDefinition, AgentDescriptor, AgentOrigin, SkillAsset};

#[cfg(test)]
mod tests {
    use super::*;
    use common::CapabilityDescriptor;

    fn manifest(ids: &[&str]) -> PackageManifest {
        PackageManifest {
            name: "general".into(),
            version: "0.1.0".into(),
            protocol: common::PROTOCOL_VERSION.into(),
            runtime: "0.1.0".into(),
            adk: "2.3.0".into(),
            required_permissions: Vec::new(),
            features: Vec::new(),
            launcher_modes: Vec::new(),
            hooks: Vec::new(),
            capabilities: ids
                .iter()
                .map(|id| CapabilityDescriptor {
                    id: (*id).into(),
                    kind: "agent".into(),
                    description: "test".into(),
                    permissions: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn registry_rejects_duplicates() {
        let mut registry = CapabilityRegistry::default();
        registry.register("echo").unwrap();
        assert_eq!(
            registry.register("echo").unwrap_err().code,
            "package.duplicate_capability"
        );
    }

    #[test]
    fn manifest_matches_registrations() {
        let mut registry = CapabilityRegistry::default();
        registry.register("general").unwrap();
        registry.verify(&manifest(&["general"])).unwrap();
    }

    #[test]
    fn missing_registration_is_rejected() {
        let mut registry = CapabilityRegistry::default();
        registry.register("general").unwrap();
        let error = registry
            .verify(&manifest(&["general", "echo"]))
            .unwrap_err();
        assert_eq!(error.code, "package.capability_mismatch");
    }
}
