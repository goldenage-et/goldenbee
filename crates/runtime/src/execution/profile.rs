//! Profile-revision native agent construction.
//!
//! Sequential, parallel, and loop compositions use the pinned native workflow
//! agents. Team compositions lower a supervisor template to a validated
//! `TeamSpec` and compile it with the native `team-tools` delegation support.
//! No harness-owned orchestration interpreter is involved.

use crate::agents::definition::{skill_index, AgentSpec};
use crate::agents::{workflow, SkillAsset};
use crate::context::ContextBundle;
use crate::data::memory::{preload_callback, NativeMemory};
use crate::execution::plugins::AgentCallbacks;
use crate::execution::providers::{FallbackReporter, ProviderRegistry, ProviderRequirements};
use crate::mcp::McpRegistry;
use crate::worker::WorkerResult;
use adk_agent::team::{
    RelationshipKind, TeamArchitectureTemplate, TeamBudget, TeamFailurePolicy, TeamPolicy, TeamSpec,
};
use adk_rust::prelude::LlmAgentBuilder;
use adk_rust::Agent;
use common::ApiError;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Builds a native agent from an immutable profile revision and resolved
/// package assets.
pub type ProfileAgentBuilder = Arc<
    dyn Fn(
            &common::ProfileRevision,
            &str,
            Option<FallbackReporter>,
        ) -> std::result::Result<Arc<dyn Agent>, ApiError>
        + Send
        + Sync,
>;

/// Name reserved for the native team coordinator. It is an internal child and
/// is never registered as a public package capability.
pub const COORDINATOR: &str = "coordinator";

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_profile_agent(
    revision: &common::ProfileRevision,
    agents: &BTreeMap<String, Arc<dyn Agent>>,
    specs: &BTreeMap<String, AgentSpec>,
    skills: &[SkillAsset],
    effect_tools: &BTreeSet<String>,
    callbacks: Option<&(dyn Fn() -> AgentCallbacks + Send + Sync)>,
    memory: Option<&Arc<NativeMemory>>,
    context: &ContextBundle,
    providers: &Arc<ProviderRegistry>,
    reporter: Option<FallbackReporter>,
    mcp: Option<&Arc<McpRegistry>>,
) -> WorkerResult<Arc<dyn Agent>> {
    if revision.members.is_empty() {
        return Err(ApiError::new(
            if revision.composition == "single" {
                "profile.composition_invalid"
            } else {
                "profile.composition_empty"
            },
            "a composed profile revision must select native members",
        ));
    }
    let mut unique = BTreeSet::new();
    for member in &revision.members {
        if member == COORDINATOR {
            return Err(ApiError::new(
                "profile.member_reserved",
                format!("'{COORDINATOR}' is reserved for the native team coordinator"),
            ));
        }
        if !unique.insert(member.as_str()) {
            return Err(ApiError::new(
                "profile.member_duplicate",
                format!("profile member '{member}' is selected twice"),
            ));
        }
    }
    let requirements = ProviderRequirements {
        modalities: vec!["text".into()],
        tools: revision
            .members
            .iter()
            .filter_map(|member| specs.get(member))
            .map(|spec| spec.tools.len() + spec.toolsets.len())
            .sum::<usize>(),
        max_tokens: revision.limits.max_tokens,
    };
    let model = providers
        .resolve_with_observer(&revision.model, revision.revision, &requirements, reporter)
        .map_err(|error| {
            ApiError::new(
                "profile.model_unresolved",
                format!(
                    "profile model reference '{}' failed: {}",
                    revision.model,
                    common::redact(&error.message)
                ),
            )
        })?
        .model;
    let max_iterations = crate::agents::composition::effective_iterations(&revision.limits);
    let coordinator_skills = if !revision.skills.is_empty() {
        revision.skills.clone()
    } else {
        let mut selected = BTreeSet::new();
        for member in &revision.members {
            if let Some(spec) = specs.get(member) {
                selected.extend(spec.skills.iter().cloned());
            }
        }
        selected.into_iter().collect()
    };
    let mut leaves = Vec::new();
    for member in &revision.members {
        if let Some(spec) = specs.get(member) {
            let instruction = context.apply_to_instruction(&leaf_instruction(revision, member));
            let mut builder = LlmAgentBuilder::new(member)
                .description(spec.description.clone())
                .instruction(instruction)
                .model(model.clone())
                .max_iterations(max_iterations);
            let selected_skills = if revision.skills.is_empty() {
                &spec.skills
            } else {
                &revision.skills
            };
            if let Some(index) = skill_index(skills, selected_skills)? {
                builder = builder.with_skills(index);
            }
            for tool in &spec.tools {
                if effect_tools.contains(tool.name()) {
                    builder = builder.require_tool_confirmation(tool.name().to_string());
                }
                builder = builder.tool(tool.clone());
            }
            for toolset in &spec.toolsets {
                builder = builder.toolset(toolset.clone());
            }
            for server in &spec.mcp_servers {
                if !revision.mcp.iter().any(|allowed| allowed == server) {
                    continue;
                }
                let registry = mcp.ok_or_else(|| {
                    ApiError::new(
                        "profile.mcp_unavailable",
                        "this Worker has no MCP registry for the pinned profile",
                    )
                })?;
                let grants: BTreeSet<String> = revision.grants.iter().cloned().collect();
                builder = builder.toolset(registry.view(server, grants));
            }
            let agent_callbacks = callbacks_with_memory(callbacks, memory, member)?;
            builder = agent_callbacks.apply(builder);
            leaves.push(Arc::new(builder.build().map_err(|error| {
                ApiError::new(
                    "profile.composition_failed",
                    format!("profile member '{member}' failed to build: {error}"),
                )
            })?) as Arc<dyn Agent>);
            continue;
        }
        // A manually registered native agent (including a prebuilt native
        // workflow) is used exactly as registered. Its own prompt and tools stay
        // authoritative; the profile can select it but cannot rewrite it.
        let agent = agents.get(member).cloned().ok_or_else(|| {
            ApiError::new(
                "profile.composition_failed",
                format!("profile member '{member}' has no native agent"),
            )
        })?;
        leaves.push(agent);
    }
    let template = workflow::effective_template(revision);
    match workflow::canonical_template(template) {
        Some("single") => Ok(leaves
            .into_iter()
            .next()
            .expect("non-empty members produce a leaf")),
        Some("team") => build_team(
            revision,
            &model,
            leaves,
            skills,
            &coordinator_skills,
            max_iterations,
            callbacks,
            memory,
            context,
        ),
        Some(_) => workflow::compile_workflow(revision, leaves, max_iterations),
        None => Err(ApiError::new(
            "profile.workflow_unknown",
            format!("workflow template '{template}' is not registered"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_team(
    revision: &common::ProfileRevision,
    model: &Arc<dyn adk_rust::Llm>,
    leaves: Vec<Arc<dyn Agent>>,
    skills: &[SkillAsset],
    coordinator_skills: &[String],
    max_iterations: u32,
    callbacks: Option<&(dyn Fn() -> AgentCallbacks + Send + Sync)>,
    memory: Option<&Arc<NativeMemory>>,
    context: &ContextBundle,
) -> WorkerResult<Arc<dyn Agent>> {
    let mut instruction = context.apply_to_instruction(&revision.instruction);
    instruction.push_str(
        "\nYou are the team coordinator. Delegate exact work to the specialists and return the aggregated result.",
    );
    let mut builder = LlmAgentBuilder::new(COORDINATOR)
        .description("Coordinator for a native ADK agent team")
        .instruction(instruction)
        .model(model.clone())
        .max_iterations(max_iterations);
    if let Some(index) = skill_index(skills, coordinator_skills)? {
        builder = builder.with_skills(index);
    }
    let agent_callbacks = callbacks_with_memory(callbacks, memory, COORDINATOR)?;
    builder = agent_callbacks.apply(builder);
    let coordinator = Arc::new(builder.build().map_err(|error| {
        ApiError::new(
            "profile.composition_failed",
            format!("profile coordinator failed to build: {error}"),
        )
    })?) as Arc<dyn Agent>;

    let template = TeamArchitectureTemplate::Supervisor {
        name: "team".into(),
        coordinator: COORDINATOR.into(),
        specialists: revision.members.clone(),
        relationship: RelationshipKind::Delegate,
        policy: team_policy(&revision.limits),
    };
    let spec: TeamSpec = template.lower().map_err(|error| {
        ApiError::new(
            "profile.composition_failed",
            format!("native team template did not lower: {error}"),
        )
    })?;
    let mut bound = Vec::with_capacity(leaves.len() + 1);
    bound.push(coordinator);
    bound.extend(leaves);
    let compiled = spec.compile(bound).map_err(|error| {
        ApiError::new(
            "profile.composition_failed",
            format!("native team did not compile: {error}"),
        )
    })?;
    Ok(Arc::new(compiled))
}

/// Map exact profile limits onto the native team policy. Unset values keep the
/// pinned upstream defaults; configured values never become an invalid zero.
fn team_policy(limits: &common::Limits) -> TeamPolicy {
    let depth = |value: u32, fallback: u32| {
        if value == 0 {
            fallback
        } else {
            value
        }
    };
    TeamPolicy {
        max_transfer_depth: depth(limits.max_depth, 8),
        max_delegation_depth: depth(limits.max_depth, 4),
        max_concurrent_delegations: if limits.max_fanout == 0 {
            4
        } else {
            limits.max_fanout as usize
        },
        failure: TeamFailurePolicy::Propagate,
        budget: TeamBudget {
            max_model_requests: (limits.max_model_calls > 0)
                .then_some(limits.max_model_calls as u64),
            max_tool_calls: (limits.max_tool_calls > 0).then_some(limits.max_tool_calls as u64),
            max_tokens: (limits.max_tokens > 0).then_some(limits.max_tokens),
            max_wall_time_ms: (limits.max_duration_ms > 0).then_some(limits.max_duration_ms),
            ..TeamBudget::default()
        },
        ..TeamPolicy::default()
    }
}

fn callbacks_with_memory(
    callbacks: Option<&(dyn Fn() -> AgentCallbacks + Send + Sync)>,
    memory: Option<&Arc<NativeMemory>>,
    member: &str,
) -> WorkerResult<AgentCallbacks> {
    let mut agent_callbacks = callbacks.map(|factory| factory()).unwrap_or_default();
    if let Some(memory) = memory {
        let preload = preload_callback(memory.clone()).map_err(|error| {
            ApiError::new(
                "profile.memory_callback_failed",
                format!("profile member '{member}' could not bind memory recall: {error}"),
            )
        })?;
        agent_callbacks = agent_callbacks.with_before_model(preload);
    }
    Ok(agent_callbacks)
}

fn leaf_instruction(revision: &common::ProfileRevision, member: &str) -> String {
    if revision.members.len() == 1 {
        revision.instruction.clone()
    } else {
        format!(
            "{}\nYou are the '{member}' specialist. Return only your own result.",
            revision.instruction
        )
    }
}
