//! Durable event → shared JSONL item projection.
//!
//! One event vocabulary stays authoritative. This module only *projects* the
//! task event stream into the Codex-style started/updated/completed item state
//! machine under Goldenbee's task vocabulary. It never appends an event.

use crate::Runtime;
use common::{JsonlItem, Principal, StreamEvent};
use serde_json::Value;

/// Which state-machine move an event represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemChange {
    Started,
    Updated,
    Completed,
}

/// Project one task event into an item change, when the event maps to an item.
pub(crate) async fn project(
    runtime: &Runtime,
    principal: &Principal,
    event: &StreamEvent,
) -> Option<(ItemChange, JsonlItem)> {
    let payload = &event.payload;
    match event.kind.as_str() {
        "tool.call" | "tool.started" => {
            let id = text(payload, "id")
                .or_else(|| text(payload, "call_id"))
                .unwrap_or_else(|| format!("tool-{}", event.seq));
            let name = text(payload, "name")
                .or_else(|| text(payload, "tool"))
                .unwrap_or_else(|| "tool".into());
            Some((
                ItemChange::Started,
                JsonlItem::ToolCall {
                    id,
                    name,
                    server: text(payload, "server"),
                    agent: text(payload, "agent"),
                    ok: None,
                    error_code: None,
                },
            ))
        }
        "tool.result" | "tool.completed" => {
            let id = text(payload, "id")
                .or_else(|| text(payload, "call_id"))
                .unwrap_or_else(|| format!("tool-{}", event.seq));
            let name = text(payload, "name")
                .or_else(|| text(payload, "tool"))
                .unwrap_or_else(|| "tool".into());
            Some((
                ItemChange::Completed,
                JsonlItem::ToolCall {
                    id,
                    name,
                    server: text(payload, "server"),
                    agent: text(payload, "agent"),
                    ok: payload.get("ok").and_then(Value::as_bool),
                    error_code: text(payload, "error_code").filter(|code| !code.is_empty()),
                },
            ))
        }
        "approval.requested" | "approval.pending" => {
            let id = text(payload, "approval_id").unwrap_or_else(|| format!("evt-{}", event.seq));
            Some((
                ItemChange::Started,
                JsonlItem::Approval {
                    id,
                    operation: text(payload, "operation").unwrap_or_else(|| "approval".into()),
                    target: text(payload, "target").unwrap_or_default(),
                    risk: text(payload, "risk"),
                    decision: (event.kind == "approval.pending").then(|| "pending".to_string()),
                },
            ))
        }
        "approval.approved" | "approval.rejected" | "approval.revoked" | "approval.expired" => {
            let id = text(payload, "approval_id").unwrap_or_else(|| format!("evt-{}", event.seq));
            let decision = event
                .kind
                .strip_prefix("approval.")
                .unwrap_or("decided")
                .to_string();
            Some((
                ItemChange::Completed,
                JsonlItem::Approval {
                    id,
                    operation: text(payload, "operation").unwrap_or_else(|| "approval".into()),
                    target: text(payload, "target").unwrap_or_default(),
                    risk: text(payload, "risk"),
                    decision: Some(decision),
                },
            ))
        }
        "checkpoint.committed" => {
            let id = text(payload, "checkpoint_id")
                .unwrap_or_else(|| format!("checkpoint-{}", event.seq));
            let diff = runtime.checkpoint_diff(principal, &id).await.ok();
            Some((
                ItemChange::Completed,
                JsonlItem::Diff {
                    id,
                    status: text(payload, "status"),
                    diff,
                },
            ))
        }
        "checkpoint.restored" => {
            let id = text(payload, "checkpoint_id")
                .unwrap_or_else(|| format!("checkpoint-{}", event.seq));
            let diff = runtime.checkpoint_diff(principal, &id).await.ok();
            Some((
                ItemChange::Completed,
                JsonlItem::Diff {
                    id,
                    status: Some("restored".into()),
                    diff,
                },
            ))
        }
        "model.delta" | "model.delta.complete" | "graph.output" => {
            let body = text(payload, "text")?;
            let author = text(payload, "author").unwrap_or_else(|| "agent".into());
            Some((
                if event.kind == "model.delta" {
                    ItemChange::Updated
                } else {
                    ItemChange::Completed
                },
                JsonlItem::AgentMessage {
                    id: format!("message:{author}"),
                    text: body,
                },
            ))
        }
        "model.thinking" | "model.thinking.complete" => {
            let body = text(payload, "text")?;
            let author = text(payload, "author").unwrap_or_else(|| "agent".into());
            Some((
                if event.kind == "model.thinking" {
                    ItemChange::Updated
                } else {
                    ItemChange::Completed
                },
                JsonlItem::Reasoning {
                    id: format!("reasoning:{author}"),
                    text: body,
                },
            ))
        }
        _ => None,
    }
}

fn text(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}
