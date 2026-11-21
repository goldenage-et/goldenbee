//! Deterministic context manifest recorded on every admitted task stream.
//!
//! The manifest contains source identities and digests, never prompt or memory
//! text. Two runs under the same configuration, profile revision, and asset
//! revision therefore produce the same manifest digest, which is what makes
//! context assembly inspectable across restarts.

use crate::context::memory::ContextBundle;
use common::{ContextCompaction, Limits};
use serde_json::{json, Map, Value};

/// Inputs for one task's context manifest.
pub struct ManifestInput<'a> {
    /// Assembled memory bundle for the active asset revision.
    pub bundle: &'a ContextBundle,
    /// Agent selected by the task.
    pub agent: &'a str,
    /// Frozen profile revision.
    pub profile_revision: u64,
    /// Frozen asset revision.
    pub asset_revision: u64,
    /// Attempt number under the task identity.
    pub attempt: u32,
    /// Digest of the undecorated prompt or profile instruction.
    pub prompt_digest: String,
    /// Digest of the instruction the model actually receives.
    pub instruction_digest: String,
    /// Skills selected for the invocation.
    pub skills: Vec<String>,
    /// Tools bound to the invocation.
    pub tools: Vec<String>,
    /// Frozen effective task limits.
    pub limits: &'a Limits,
    /// Configured compaction, when any.
    pub compaction: Option<&'a ContextCompaction>,
    /// Whether this invocation carries a requested manual compaction.
    pub forced_compaction: bool,
}

/// Build the durable manifest payload, including its self-verifying digest.
pub fn value(input: ManifestInput<'_>) -> Value {
    let mut skills = input.skills;
    skills.sort();
    skills.dedup();
    let mut tools = input.tools;
    tools.sort();
    tools.dedup();
    let mut body = Map::new();
    body.insert("agent".into(), json!(input.agent));
    body.insert("profile_revision".into(), json!(input.profile_revision));
    body.insert("asset_revision".into(), json!(input.asset_revision));
    body.insert("attempt".into(), json!(input.attempt));
    body.insert("prompt_digest".into(), json!(input.prompt_digest));
    body.insert("instruction_digest".into(), json!(input.instruction_digest));
    body.insert("skills".into(), json!(skills));
    body.insert("tools".into(), json!(tools));
    body.insert("limits".into(), json!(input.limits));
    body.insert(
        "memory_config_digest".into(),
        json!(input.bundle.config_digest),
    );
    body.insert("memory_digest".into(), json!(input.bundle.digest));
    body.insert("memory_files".into(), json!(input.bundle.files));
    body.insert("memory_diagnostics".into(), json!(input.bundle.diagnostics));
    body.insert(
        "compaction".into(),
        input
            .compaction
            .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
            .unwrap_or(Value::Null),
    );
    body.insert("forced_compaction".into(), json!(input.forced_compaction));
    body.insert(
        "manual_compaction".into(),
        json!(if input.forced_compaction {
            "next_model_turn"
        } else {
            "not_requested"
        }),
    );
    let digest = common::canonical_digest(&Value::Object(body.clone()));
    body.insert("digest".into(), json!(digest));
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::memory::{MemoryDiagnostic, MemoryFile};

    fn bundle() -> ContextBundle {
        ContextBundle {
            files: vec![MemoryFile {
                source: "project".into(),
                precedence: 10,
                path: "AGENTS.md".into(),
                digest: "sha256:abc".into(),
                bytes: 12,
                imported: false,
            }],
            diagnostics: vec![MemoryDiagnostic {
                code: "context.memory_missing".into(),
                source: "project".into(),
                path: "optional.md".into(),
                message: "configured but missing".into(),
            }],
            text: "runtime marker text".into(),
            digest: "sha256:memory".into(),
            config_digest: "sha256:config".into(),
        }
    }

    fn input<'a>(bundle: &'a ContextBundle, limits: &'a Limits) -> ManifestInput<'a> {
        ManifestInput {
            bundle,
            agent: "general",
            profile_revision: 2,
            asset_revision: 3,
            attempt: 1,
            prompt_digest: "sha256:prompt".into(),
            instruction_digest: "sha256:instruction".into(),
            skills: vec!["tone".into(), "tone".into()],
            tools: vec!["read_file".into()],
            limits,
            compaction: None,
            forced_compaction: false,
        }
    }

    #[test]
    fn manifest_is_stable_and_never_copies_memory_text() {
        let bundle = bundle();
        let limits = Limits::default();
        let first = value(input(&bundle, &limits));
        let second = value(input(&bundle, &limits));
        assert_eq!(first["digest"], second["digest"]);
        assert_eq!(first["memory_files"][0]["source"], "project");
        assert_eq!(
            first["memory_diagnostics"][0]["code"],
            "context.memory_missing"
        );
        assert_eq!(first["skills"], serde_json::json!(["tone"]));
        assert!(
            !first.to_string().contains("runtime marker text"),
            "manifest must carry digests, not memory content"
        );
        assert!(first["digest"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:")));
    }
}
