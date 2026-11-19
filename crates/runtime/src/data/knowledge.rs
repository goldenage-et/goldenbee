//! Runtime policy for the native knowledge graph.
//!
//! The graph adapter owns storage and native calls. This module owns the
//! runtime decisions around it: authenticated caller scope, document source
//! authorization, audit, and the absence of a second writable graph owner.

use crate::{ApiError, Result, Runtime};
use common::{KnowledgeFact, Principal};

impl Runtime {
    /// Record one graph relation for an authenticated caller.
    ///
    /// A document identity is verified against the harness document record
    /// before any fact is written. The caller must belong to the document's
    /// allowed-principal list; the resulting fact carries that same snapshot so
    /// later retrieval cannot widen it.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe(
        &self,
        principal: &Principal,
        namespace: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        document_id: &str,
    ) -> Result<KnowledgeFact> {
        let graph = self.graph().await.ok_or_else(knowledge_unavailable)?;
        let allowed = self
            .authorized_source(principal, namespace, document_id)
            .await?;
        let fact = graph
            .observe(
                principal,
                namespace,
                subject,
                predicate,
                object,
                source,
                document_id,
                allowed,
            )
            .await?;
        self.audit(
            &principal.id,
            "knowledge.observe",
            &fact.id,
            "accepted",
            &fact.source,
        )
        .await?;
        Ok(fact)
    }

    /// Retrieve current graph facts for one authenticated caller.
    ///
    /// Retrieval is lexical and delegates to the native graph scoring. Source
    /// authorization and namespace filtering happen before exposure.
    pub async fn search_knowledge(
        &self,
        principal: &Principal,
        namespace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeFact>> {
        let graph = self.graph().await.ok_or_else(knowledge_unavailable)?;
        graph.search(principal, namespace, query, limit).await
    }

    /// Inspect current and superseded graph facts for one subject.
    pub async fn knowledge_history(
        &self,
        principal: &Principal,
        namespace: &str,
        subject: &str,
    ) -> Result<Vec<KnowledgeFact>> {
        let graph = self.graph().await.ok_or_else(knowledge_unavailable)?;
        graph.history(principal, namespace, subject).await
    }

    /// Supersede one graph fact, preserving its native valid-time history.
    pub async fn invalidate_knowledge(
        &self,
        principal: &Principal,
        namespace: &str,
        kind: &str,
        fact_id: &str,
    ) -> Result<KnowledgeFact> {
        let graph = self.graph().await.ok_or_else(knowledge_unavailable)?;
        let fact = graph
            .invalidate(principal, namespace, kind, fact_id)
            .await?;
        self.audit(
            &principal.id,
            "knowledge.invalidate",
            &fact.id,
            "accepted",
            &fact.source,
        )
        .await?;
        Ok(fact)
    }

    /// Delete one entity and every graph fact touching it.
    pub async fn delete_knowledge_entity(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
    ) -> Result<usize> {
        let graph = self.graph().await.ok_or_else(knowledge_unavailable)?;
        let removed = graph.delete_entity(principal, namespace, name).await?;
        self.audit(
            &principal.id,
            "knowledge.delete_entity",
            name,
            "deleted",
            "",
        )
        .await?;
        Ok(removed)
    }

    async fn authorized_source(
        &self,
        principal: &Principal,
        namespace: &str,
        document_id: &str,
    ) -> Result<Vec<String>> {
        principal.require(common::scopes::SESSION_WRITE)?;
        principal.require_namespace(namespace)?;
        if document_id.trim().is_empty() {
            return Ok(Vec::new());
        }
        let document = self
            .store()
            .read(|state| state.documents.get(document_id).cloned())
            .await
            .ok_or_else(|| {
                ApiError::new(
                    "knowledge.source_denied",
                    "knowledge source document is not available to this caller",
                )
            })?;
        if document.namespace != namespace {
            return Err(ApiError::new(
                "knowledge.source_denied",
                "knowledge source document belongs to another namespace",
            ));
        }
        if !document.allowed.is_empty() && !document.allowed.iter().any(|id| id == &principal.id) {
            return Err(ApiError::new(
                "knowledge.source_denied",
                "knowledge source document is not authorized for this caller",
            ));
        }
        Ok(document.allowed)
    }
}

fn knowledge_unavailable() -> ApiError {
    ApiError::new(
        "knowledge.unavailable",
        "this Worker Instance has no native knowledge graph configured",
    )
}
