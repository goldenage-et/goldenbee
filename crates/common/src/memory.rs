//! Persistent memory items and knowledge facts.

use serde::{Deserialize, Serialize};

/// A persistent memory item with lifecycle metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    /// Memory identity.
    pub id: String,
    /// Namespace.
    pub namespace: String,
    /// Source descriptor.
    pub source: String,
    /// Text.
    pub text: String,
    /// Valid-from time.
    pub valid_from: i64,
    /// Valid-to time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<i64>,
    /// Erasure marker.
    pub deleted: bool,
}

/// One inspectable knowledge-graph fact with provenance.
///
/// The native graph models an entity observation or a typed relation. This wire
/// shape maps those forms explicitly:
///
/// - a relation has `kind = "relation"`, `subject = source entity`,
///   `predicate = relation_type`, and `object = target entity`;
/// - an entity observation has `kind = "observation"`, `subject = entity`,
///   `predicate = "observed"`, and `object = the observation text`.
///
/// `valid_from` and `valid_to` are the pinned native valid-time values.
/// `recorded_at` is the adapter ingestion time. There is no native
/// transaction-time value, and this shape does not claim one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeFact {
    /// Stable fact identity, for example `relation:12` or `observation:7`.
    pub id: String,
    /// `relation` or `observation`.
    pub kind: String,
    /// Namespace that owns the fact.
    pub namespace: String,
    /// Subject entity (or the entity for an observation).
    pub subject: String,
    /// Relation type, or `observed` for an entity observation.
    pub predicate: String,
    /// Target entity, or the observation text.
    pub object: String,
    /// Source descriptor recorded when the fact was ingested.
    pub source: String,
    /// Ingested document identity, empty when the source is not a document.
    pub document_id: String,
    /// True when the caller's source authorization permits exposure.
    pub source_authorized: bool,
    /// Adapter ingestion time in Unix milliseconds.
    pub recorded_at: i64,
    /// Native valid-from time in Unix milliseconds.
    pub valid_from: i64,
    /// Native valid-to time in Unix milliseconds for superseded facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<i64>,
}
