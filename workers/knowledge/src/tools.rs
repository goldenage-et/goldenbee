//! Native tools exposing package-owned ingestion, retrieval, and removal.
//!
//! Identity comes from the pinned ADK tool context. A caller cannot name
//! another namespace: the namespace is decoded from the native user id that
//! the shared runtime bound at admission.

use crate::service::KnowledgeCell;
use adk_rust::{Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Ingest one authorized source into the namespace native RAG index.
pub struct KnowledgeIngestTool {
    cell: KnowledgeCell,
}

impl KnowledgeIngestTool {
    /// Bind the tool to the startup-owned package service.
    pub fn new(cell: KnowledgeCell) -> Self {
        Self { cell }
    }
}

#[async_trait]
impl Tool for KnowledgeIngestTool {
    fn name(&self) -> &str {
        "knowledge_ingest"
    }

    fn description(&self) -> &str {
        "Ingest one authorized UTF-8 text or Markdown source and return its durable revision and citation provenance"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Source path, absolute or relative to the configured source root"
                },
                "title": {
                    "type": "string",
                    "description": "Optional display title"
                },
                "allowed": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional allowed principal list. Empty means the whole namespace."
                }
            },
            "required": ["uri"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let Some(uri) = args.get("uri").and_then(Value::as_str) else {
            return Ok(json!({
                "error": "knowledge.source_invalid",
                "message": "Missing required parameter 'uri'"
            }));
        };
        let title = args.get("title").and_then(Value::as_str);
        let allowed = args
            .get("allowed")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut guard = self.cell.lock().await;
        let Some(service) = guard.as_mut() else {
            return Ok(json!({
                "error": "knowledge.unavailable",
                "message": "knowledge service did not start"
            }));
        };
        let (namespace, principal) = match service.identity(ctx.user_id()) {
            Ok(identity) => identity,
            Err(error) => return Ok(error.to_json()),
        };
        match service
            .ingest(&principal, &namespace, uri, title, allowed)
            .await
        {
            Ok(outcome) => Ok(json!({
                "namespace": outcome.job.namespace,
                "job_id": outcome.job.job_id,
                "status": outcome.job.status.as_str(),
                "source_uri": outcome.job.source_uri,
                "source_digest": outcome.job.source_digest,
                "source_revision": outcome.job.source_revision,
                "title": outcome.job.title,
                "media_type": outcome.job.media_type,
                "allowed": outcome.job.allowed,
                "chunk_count": outcome.job.chunk_count,
                "index_collection": outcome.job.index_collection,
                "converged": outcome.converged,
                "coverage": {
                    "source_bytes": outcome.source_bytes,
                    "covered_bytes": outcome.covered_bytes,
                    "uncovered_bytes": outcome.uncovered_bytes,
                },
                "citation": format!(
                    "{}@{}#{}_0",
                    outcome.job.source_uri, outcome.job.source_digest, outcome.job.job_id
                )
            })),
            Err(error) => Ok(error.to_json()),
        }
    }
}

/// Query one namespace and return only ACL-authorized citations.
pub struct KnowledgeAskTool {
    cell: KnowledgeCell,
}

impl KnowledgeAskTool {
    /// Bind the tool to the startup-owned package service.
    pub fn new(cell: KnowledgeCell) -> Self {
        Self { cell }
    }
}

#[async_trait]
impl Tool for KnowledgeAskTool {
    fn name(&self) -> &str {
        "knowledge_ask"
    }

    fn description(&self) -> &str {
        "Query the namespace knowledge index and return cited chunks from current authorized sources"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language retrieval query"
                },
                "top_k": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum authorized citations to return"
                }
            },
            "required": ["query"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return Ok(json!({
                "error": "knowledge.query_invalid",
                "message": "Missing required parameter 'query'"
            }));
        };
        let top_k = args
            .get("top_k")
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        let mut guard = self.cell.lock().await;
        let Some(service) = guard.as_mut() else {
            return Ok(json!({
                "error": "knowledge.unavailable",
                "message": "knowledge service did not start"
            }));
        };
        let (namespace, principal) = match service.identity(ctx.user_id()) {
            Ok(identity) => identity,
            Err(error) => return Ok(error.to_json()),
        };
        match service.query(&principal, &namespace, query, top_k).await {
            Ok(outcome) => {
                let citations: Vec<Value> = outcome
                    .citations
                    .iter()
                    .map(|citation| {
                        json!({
                            "citation_id": citation.citation_id,
                            "document_id": citation.document_id,
                            "source_uri": citation.source_uri,
                            "source_digest": citation.source_digest,
                            "source_revision": citation.source_revision,
                            "chunk_id": citation.chunk_id,
                            "text": citation.text,
                            "score": citation.score,
                            "span": {"start": citation.span_start, "end": citation.span_end},
                            "token": citation.token(),
                        })
                    })
                    .collect();
                Ok(json!({
                    "namespace": namespace,
                    "query": query,
                    "results": outcome.citations.len(),
                    "citations": citations,
                    "stale": outcome.stale,
                    "citation_format": "[<source_uri>@<source_digest>#<chunk_id>]"
                }))
            }
            Err(error) => Ok(error.to_json()),
        }
    }
}

/// Mark an authorized source revision removed and drop its chunks.
pub struct KnowledgeRemoveTool {
    cell: KnowledgeCell,
}

impl KnowledgeRemoveTool {
    /// Bind the tool to the startup-owned package service.
    pub fn new(cell: KnowledgeCell) -> Self {
        Self { cell }
    }
}

#[async_trait]
impl Tool for KnowledgeRemoveTool {
    fn name(&self) -> &str {
        "knowledge_remove"
    }

    fn description(&self) -> &str {
        "Remove every indexed revision of one authorized source URI and keep a durable tombstone"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Source path, absolute or relative to the configured source root"
                }
            },
            "required": ["uri"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let Some(uri) = args.get("uri").and_then(Value::as_str) else {
            return Ok(json!({
                "error": "knowledge.source_invalid",
                "message": "Missing required parameter 'uri'"
            }));
        };
        let mut guard = self.cell.lock().await;
        let Some(service) = guard.as_mut() else {
            return Ok(json!({
                "error": "knowledge.unavailable",
                "message": "knowledge service did not start"
            }));
        };
        let (namespace, principal) = match service.identity(ctx.user_id()) {
            Ok(identity) => identity,
            Err(error) => return Ok(error.to_json()),
        };
        match service.remove(&principal, &namespace, uri).await {
            Ok(jobs) => Ok(json!({
                "namespace": namespace,
                "removed": jobs.iter().map(|job| json!({
                    "job_id": job.job_id,
                    "source_uri": job.source_uri,
                    "source_digest": job.source_digest,
                    "status": job.status.as_str(),
                    "removed_at": job.removed_at,
                })).collect::<Vec<_>>()
            })),
            Err(error) => Ok(error.to_json()),
        }
    }
}
