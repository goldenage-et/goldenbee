//! Command invocation over the owning Worker instance.
//!
//! Neutral commands run against durable Runtime state. Worker commands validate
//! their arguments, then submit through the same durable task admission path.
//! The command's narrowed agent is prebuilt from declared assets, so admission
//! and execution enforce the narrowing without a second executor.

use super::{neutral, CommandRegistry};
use crate::Runtime;
use common::{
    command_kinds, scopes, ApiError, CommandDescriptor, CommandRequest, CommandResponse, Principal,
    SubmitTaskRequest,
};

/// Validate and execute one command for the authenticated principal.
pub(crate) async fn invoke(
    registry: &CommandRegistry,
    runtime: &Runtime,
    principal: &Principal,
    name: &str,
    request: CommandRequest,
) -> Result<CommandResponse, ApiError> {
    let descriptor = registry.descriptor(name).cloned().ok_or_else(|| {
        ApiError::new(
            "command.not_found",
            format!("command '{name}' is not defined"),
        )
    })?;
    if registry.is_neutral(name) {
        neutral::reject_arguments(&request.arguments)?;
        let result = neutral::execute(name, registry, runtime, principal, &request).await?;
        return Ok(CommandResponse {
            command: name.into(),
            kind: command_kinds::NEUTRAL.into(),
            result,
            task: None,
        });
    }
    worker(registry, runtime, principal, name, descriptor, request).await
}

/// Discoverable command inventory for one revision.
pub(crate) async fn descriptors(registry: &CommandRegistry) -> Vec<CommandDescriptor> {
    registry.descriptors().to_vec()
}

async fn worker(
    registry: &CommandRegistry,
    runtime: &Runtime,
    principal: &Principal,
    name: &str,
    descriptor: CommandDescriptor,
    request: CommandRequest,
) -> Result<CommandResponse, ApiError> {
    principal.require(scopes::TASK_SUBMIT)?;
    if !request.arguments.is_empty() {
        return Err(ApiError::new(
            "command.arguments_unexpected",
            "Worker commands take positional input, not named arguments",
        ));
    }
    let command = registry.command(name).ok_or_else(|| {
        ApiError::new(
            "command.not_found",
            format!("command '{name}' is not defined"),
        )
    })?;
    let input = request.input.unwrap_or_default();
    if input.len() > 1_000_000 {
        return Err(ApiError::new(
            "command.input_too_large",
            "command input exceeds the 1 MB management limit",
        ));
    }
    let prepared = registry.prepare(name, &input)?;
    let target = command
        .agent
        .clone()
        .unwrap_or_else(|| registry.default_agent().to_string());
    if let Some(agent) = &request.agent {
        if agent != &target {
            return Err(ApiError::new(
                "command.agent_mismatch",
                format!("command '{name}' targets agent '{target}', not '{agent}'"),
            )
            .with("target", target));
        }
    }
    if request.session_id.trim().is_empty()
        || request.session_id.len() > 128
        || request
            .session_id
            .chars()
            .any(|character| character.is_control() || character == '/' || character == '\\')
    {
        return Err(ApiError::new(
            "command.session_invalid",
            "session_id must be a valid opaque identifier",
        ));
    }
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
        return Err(ApiError::new(
            "command.idempotency_key_required",
            "Worker command invocation requires a 1-200 character idempotency_key",
        ));
    }
    let namespace = principal.namespace.clone();
    let response = runtime
        .submit_task(
            principal,
            SubmitTaskRequest {
                namespace: Some(namespace),
                agent: format!("command:{name}"),
                session_id: request.session_id,
                input,
                idempotency_key: request.idempotency_key,
                profile: None,
                team: None,
                worker: None,
                limits: request.limits,
            },
        )
        .await?;
    Ok(CommandResponse {
        command: name.into(),
        kind: command_kinds::WORKER.into(),
        result: serde_json::json!({
            "arguments": prepared.arguments,
            "allowed_tools": descriptor.allowed_tools,
            "model": descriptor.model,
        }),
        task: Some(response),
    })
}
