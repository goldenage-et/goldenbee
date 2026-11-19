//! Profile bindings to registered native workflow templates.
//!
//! The templates are the pinned `adk-agent` `WorkflowArchitectureTemplate`
//! presets. A profile binds its members and limits as template parameters and
//! this module lowers them to the native sequential, parallel, fan-out/fan-in,
//! review-loop, or existing loop agent. It is a binder and validator, not a
//! second workflow interpreter.

use adk_agent::team::WorkflowArchitectureTemplate;
use adk_rust::prelude::LoopAgent;
use adk_rust::Agent;
use common::{ApiError, ProfileRevision};
use std::sync::Arc;

/// Native workflow template names accepted by a profile revision.
pub const NATIVE_WORKFLOW_TEMPLATES: &[&str] = &[
    "single",
    "sequential",
    "parallel",
    "loop",
    "team",
    "fan-out-fan-in",
    "review-loop",
];

/// The template selected by a revision. An explicit `workflow` reference wins;
/// otherwise the existing composition name is the template name.
pub(crate) fn effective_template(revision: &ProfileRevision) -> &str {
    revision
        .workflow
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&revision.composition)
}

/// Validate that a revision binds valid parameters to a registered template.
///
/// This performs the same shape checks the native template compiler enforces,
/// but without constructing agents. It runs before a candidate revision is
/// appended or activated.
pub(crate) fn validate_workflow(revision: &ProfileRevision) -> std::result::Result<(), ApiError> {
    let requested = effective_template(revision);
    let template = canonical_template(requested).ok_or_else(|| {
        if revision.workflow.is_some() {
            ApiError::new(
                "profile.workflow_unknown",
                format!(
                    "workflow template '{requested}' is not registered; supported templates: {}",
                    NATIVE_WORKFLOW_TEMPLATES.join(", ")
                ),
            )
        } else {
            ApiError::new(
                "profile.composition_invalid",
                format!(
                    "unsupported composition '{requested}'; supported compositions: {}",
                    NATIVE_WORKFLOW_TEMPLATES.join(", ")
                ),
            )
        }
    })?;
    let parameters = if revision.workflow.is_some() {
        "profile.workflow_parameters"
    } else {
        "profile.composition_invalid"
    };
    let empty = if revision.workflow.is_some() {
        "profile.workflow_parameters"
    } else {
        "profile.composition_empty"
    };
    let mut seen = std::collections::BTreeSet::new();
    for member in &revision.members {
        if member.trim().is_empty() {
            return Err(ApiError::new(
                parameters,
                "workflow template members must not be empty",
            ));
        }
        if !seen.insert(member.as_str()) {
            return Err(ApiError::new(
                "profile.member_duplicate",
                format!("profile member '{member}' is selected twice"),
            ));
        }
    }
    match template {
        "single" => {
            if revision.members.len() > 1 {
                return Err(ApiError::new(
                    parameters,
                    "a single template accepts at most one member",
                ));
            }
        }
        "sequential" | "parallel" | "loop" | "team" => {
            if revision.members.is_empty() {
                return Err(ApiError::new(
                    empty,
                    "the selected workflow template requires at least one member",
                ));
            }
        }
        "fan-out-fan-in" => {
            if revision.members.len() < 2 {
                return Err(ApiError::new(
                    parameters,
                    "fan-out-fan-in requires at least one worker and one aggregator member",
                ));
            }
            let workers = revision.members.len() - 1;
            if revision.limits.max_fanout > 0 && workers > revision.limits.max_fanout as usize {
                return Err(ApiError::new(
                    "profile.fanout_exceeded",
                    "workflow fan-out exceeds the profile fanout limit",
                ));
            }
        }
        "review-loop" => {
            if revision.members.len() != 2 {
                return Err(ApiError::new(
                    parameters,
                    "review-loop requires exactly a producer and a reviewer member",
                ));
            }
        }
        _ => unreachable!("canonical workflow names are exhaustive"),
    }
    if matches!(template, "team" | "parallel")
        && revision.limits.max_fanout > 0
        && revision.members.len() > revision.limits.max_fanout as usize
    {
        return Err(ApiError::new(
            "profile.fanout_exceeded",
            "composition exceeds profile fanout limit",
        ));
    }
    Ok(())
}

/// Compile a non-single, non-team workflow template around native leaf agents.
///
/// Leaves are supplied in `revision.members` order and must expose the matching
/// native agent names, which is how the pinned template compiler binds them.
pub(crate) fn compile_workflow(
    revision: &ProfileRevision,
    leaves: Vec<Arc<dyn Agent>>,
    max_iterations: u32,
) -> std::result::Result<Arc<dyn Agent>, ApiError> {
    let template = canonical_template(effective_template(revision)).ok_or_else(|| {
        ApiError::new(
            "profile.workflow_unknown",
            format!(
                "workflow template '{}' is not registered",
                effective_template(revision)
            ),
        )
    })?;
    match template {
        "sequential" => compile_template(
            WorkflowArchitectureTemplate::Sequential {
                name: "sequential".into(),
                steps: revision.members.clone(),
            },
            leaves,
        ),
        "parallel" => compile_template(
            WorkflowArchitectureTemplate::Parallel {
                name: "parallel".into(),
                members: revision.members.clone(),
                shared_state: false,
            },
            leaves,
        ),
        "fan-out-fan-in" => {
            let aggregator = revision.members.last().cloned().ok_or_else(|| {
                ApiError::new(
                    "profile.workflow_parameters",
                    "fan-out-fan-in requires an aggregator member",
                )
            })?;
            compile_template(
                WorkflowArchitectureTemplate::FanOutFanIn {
                    name: "fan-out-fan-in".into(),
                    workers: revision.members[..revision.members.len() - 1].to_vec(),
                    aggregator,
                    shared_state: false,
                },
                leaves,
            )
        }
        "review-loop" => compile_template(
            WorkflowArchitectureTemplate::ReviewLoop {
                name: "review-loop".into(),
                producer: revision.members.first().cloned().ok_or_else(|| {
                    ApiError::new(
                        "profile.workflow_parameters",
                        "review-loop requires a producer member",
                    )
                })?,
                reviewer: revision.members.get(1).cloned().ok_or_else(|| {
                    ApiError::new(
                        "profile.workflow_parameters",
                        "review-loop requires a reviewer member",
                    )
                })?,
                max_iterations: max_iterations.max(1),
            },
            leaves,
        ),
        "loop" => Ok(Arc::new(
            LoopAgent::new("loop", leaves).with_max_iterations(max_iterations.clamp(1, 10)),
        )),
        "single" => Err(ApiError::new(
            "profile.composition_invalid",
            "a single template is not a composed workflow",
        )),
        "team" => Err(ApiError::new(
            "profile.composition_invalid",
            "a team template is compiled through the native team compiler",
        )),
        _ => unreachable!("canonical workflow names are exhaustive"),
    }
}

fn compile_template(
    template: WorkflowArchitectureTemplate,
    leaves: Vec<Arc<dyn Agent>>,
) -> std::result::Result<Arc<dyn Agent>, ApiError> {
    template.compile(leaves).map_err(|error| {
        ApiError::new(
            "profile.workflow_compile_failed",
            format!("native workflow template did not compile: {error}"),
        )
    })
}

pub(crate) fn canonical_template(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "single" => Some("single"),
        "sequential" => Some("sequential"),
        "parallel" => Some("parallel"),
        "loop" => Some("loop"),
        "team" | "supervisor" => Some("team"),
        "fan-out-fan-in" | "fanout-fan-in" | "fan-out" | "fanout" => Some("fan-out-fan-in"),
        "review-loop" | "review" => Some("review-loop"),
        _ => None,
    }
}
