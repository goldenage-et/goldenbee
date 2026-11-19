//! Shared interactive-session operations.
//!
//! The line-oriented fallback and the full-screen chat both use these Gateway
//! helpers so Worker selection, capability lookup, and task completion behave
//! the same way in each interface.

use common::{ApiError, Task, WorkerSummary};
use sdk::Client;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Reachable Workers advertised by the Gateway.
pub async fn reachable(client: &Client) -> Result<Vec<WorkerSummary>, ApiError> {
    let workers = client.workers().await?;
    Ok(workers
        .into_iter()
        .filter(|worker| worker.reachable)
        .collect())
}

/// Agent capability identifiers advertised by one Worker.
pub fn capabilities(workers: &[WorkerSummary], worker: &str) -> Vec<String> {
    workers
        .iter()
        .find(|candidate| candidate.id == worker)
        .map(|candidate| {
            candidate
                .capabilities
                .iter()
                .map(|capability| capability.id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Pick the requested agent, or `general`, or the first advertised capability.
pub fn first_agent(
    workers: &[WorkerSummary],
    worker: &str,
    requested: Option<&str>,
) -> Result<String, ApiError> {
    let capabilities = capabilities(workers, worker);
    if let Some(requested) = requested {
        if capabilities.iter().any(|id| id == requested) {
            return Ok(requested.to_string());
        }
        return Err(ApiError::new(
            "cli.agent_not_found",
            format!("Worker '{worker}' has no agent capability '{requested}'"),
        ));
    }
    capabilities
        .iter()
        .find(|id| id.as_str() == "general")
        .cloned()
        .or_else(|| capabilities.first().cloned())
        .ok_or_else(|| {
            ApiError::new(
                "cli.agent_not_found",
                format!("Worker '{worker}' advertises no agent capability"),
            )
        })
}

/// Wait for a routed task to reach a terminal state.
pub async fn wait_terminal(client: &Client, task_id: &str) -> Result<Task, ApiError> {
    for _ in 0..2_000 {
        let projection = client.task(task_id).await?;
        if projection.task.state.is_terminal() {
            return Ok(projection.task);
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(ApiError::new(
        "cli.timeout",
        "task did not reach a terminal state",
    ))
}

/// Fresh interactive session identifier.
pub fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("cli-{}-{nanos}", std::process::id())
}

/// Render one error for a human, including recovery guidance when present.
pub fn error_text(error: &ApiError) -> String {
    if error.recovery.is_empty() {
        format!("{}: {}", error.code, error.message)
    } else {
        format!("{}: {}\n{}", error.code, error.message, error.recovery)
    }
}

/// Print an error to stderr in the CLI's stable format.
pub fn print_error(error: &ApiError) {
    eprintln!("gb-client: {}: {}", error.code, error.message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::CapabilityDescriptor;

    fn worker(id: &str, agents: &[&str]) -> WorkerSummary {
        WorkerSummary {
            id: id.to_string(),
            endpoint: format!("http://{id}"),
            instance: id.to_string(),
            package: "general".to_string(),
            version: "0.1.0".to_string(),
            capabilities: agents
                .iter()
                .map(|id| CapabilityDescriptor {
                    id: id.to_string(),
                    kind: "agent".to_string(),
                    description: String::new(),
                    permissions: Vec::new(),
                })
                .collect(),
            declared_capabilities: Vec::new(),
            approved_capabilities: Vec::new(),
            protocol: String::new(),
            locality: "local".to_string(),
            status: "healthy".to_string(),
            reachable: true,
            incarnation: "1".to_string(),
            incarnations: Vec::new(),
            capacity: common::Capacity {
                active: 0,
                limit: 1,
                queued: 0,
                queue_limit: 1,
            },
            lease_expires_at: 0,
            renewed_at: 0,
        }
    }

    #[test]
    fn agent_defaults_to_general_then_first_capability() {
        let workers = vec![worker("a", &["echo", "general"])];
        assert_eq!(first_agent(&workers, "a", None).unwrap(), "general");
        assert_eq!(first_agent(&workers, "a", Some("echo")).unwrap(), "echo");
        let only_echo = vec![worker("b", &["echo"])];
        assert_eq!(first_agent(&only_echo, "b", None).unwrap(), "echo");
        assert_eq!(
            first_agent(&only_echo, "b", Some("general"))
                .unwrap_err()
                .code,
            "cli.agent_not_found"
        );
        let none = vec![worker("c", &[])];
        assert_eq!(
            first_agent(&none, "c", None).unwrap_err().code,
            "cli.agent_not_found"
        );
    }

    #[test]
    fn errors_include_recovery_when_present() {
        let error = ApiError::new("cli.timeout", "task did not finish");
        let text = error_text(&error);
        assert!(text.contains("cli.timeout: task did not finish"));
    }
}
