//! Authorized provenance adapter over the pinned native knowledge graph.
//!
//! The pinned `adk_memory::GraphMemoryService` owns entities, observations,
//! relations, valid-time stamps, and the SQLite file. This adapter is not a
//! second graph engine: it shares the native connection pool, calls the native
//! graph operations for writes and lexical retrieval, and keeps only an
//! additional provenance table that the native schema does not provide.
//!
//! Provenance is required for every exposed fact. The native graph has no
//! source, document, or allowed-principal columns, so this adapter records them
//! separately and fails closed when a native row has no provenance.
//!
//! Native isolation is the `(app_name, user_id)` pair. The runtime maps
//! `(namespace, principal)` through the same unambiguous `native_user_id`
//! encoding used by sessions and long-term memory, so identical entity names in
//! separate namespaces or principals never share rows.
//!
//! Time semantics are valid-time only. Native `valid_from`/`valid_to` are
//! RFC3339 UTC strings written with `Utc::now()` at the native write or
//! invalidation. There is no native transaction-time column and this adapter
//! does not claim one. The adapter adds `recorded_at` as the ingestion time.

use crate::core::error::{Error, Result};
use crate::execution::runner::{decode_native_user_id, native_user_id};
use adk_memory::{CreateEntityInput, CreateRelationInput, GraphMemoryService, MemoryService};
use chrono::{DateTime, Utc};
use common::{ApiError, KnowledgeFact, Principal};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

/// File name of the canonical native knowledge graph store.
pub const KNOWLEDGE_STORE_FILE: &str = "native-graph.sqlite3";

/// Entity category used for observed subjects and objects.
const ENTITY_TYPE: &str = "entity";

/// Predicate exposed for a native entity observation.
const OBSERVATION_PREDICATE: &str = "observed";

const PROVENANCE_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS kg_provenance (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    kind TEXT NOT NULL, \
    native_id INTEGER NOT NULL, \
    subject TEXT NOT NULL, \
    predicate TEXT NOT NULL, \
    object TEXT NOT NULL, \
    source TEXT NOT NULL, \
    document_id TEXT NOT NULL, \
    allowed_json TEXT NOT NULL, \
    recorded_at TEXT NOT NULL, \
    valid_from TEXT NOT NULL, \
    valid_to TEXT, \
    PRIMARY KEY (app_name, user_id, kind, native_id)\
);\
CREATE INDEX IF NOT EXISTS idx_kg_prov_scope \
    ON kg_provenance(app_name, user_id, subject);";

/// Scoped durable knowledge graph backed by the pinned native service.
pub struct NativeKnowledgeGraph {
    app_name: String,
    path: PathBuf,
    pool: SqlitePool,
    service: GraphMemoryService,
}

impl std::fmt::Debug for NativeKnowledgeGraph {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeKnowledgeGraph")
            .field("app_name", &self.app_name)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
struct ProvenanceRow {
    kind: String,
    native_id: i64,
    subject: String,
    predicate: String,
    object: String,
    source: String,
    document_id: String,
    allowed: Vec<String>,
    recorded_at: String,
    valid_from: String,
    valid_to: Option<String>,
}

impl ProvenanceRow {
    fn key(&self) -> String {
        format!("{}:{}", self.kind, self.native_id)
    }

    fn fact(&self, namespace: &str, principal: &str) -> Result<KnowledgeFact> {
        Ok(KnowledgeFact {
            id: self.key(),
            kind: self.kind.clone(),
            namespace: namespace.to_string(),
            subject: self.subject.clone(),
            predicate: self.predicate.clone(),
            object: self.object.clone(),
            source: self.source.clone(),
            document_id: self.document_id.clone(),
            source_authorized: self.allowed.is_empty()
                || self.allowed.iter().any(|id| id == principal),
            recorded_at: parse_millis(&self.recorded_at)?,
            valid_from: parse_millis(&self.valid_from)?,
            valid_to: self.valid_to.as_deref().map(parse_millis).transpose()?,
        })
    }

    fn authorized(&self, principal: &str) -> bool {
        self.allowed.is_empty() || self.allowed.iter().any(|id| id == principal)
    }
}

impl NativeKnowledgeGraph {
    /// Open the canonical native graph below `data_dir`.
    ///
    /// The native schema, the adapter provenance table, and a health check are
    /// all completed before this returns. A corrupt or unavailable path is an
    /// actionable startup failure, never a silent empty graph.
    pub async fn open(
        app_name: impl Into<String>,
        data_dir: impl AsRef<Path>,
    ) -> Result<Arc<Self>> {
        let app_name = app_name.into();
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir).map_err(|error| {
            ApiError::new(
                "knowledge.storage_unavailable",
                format!("cannot create knowledge data directory: {error}"),
            )
        })?;
        let path = data_dir.join(KNOWLEDGE_STORE_FILE);
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(|error| storage_error(&path, error))?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(|error| storage_error(&path, error))?;
        let service = GraphMemoryService::from_pool(pool.clone());
        service
            .migrate()
            .await
            .map_err(|error| storage_error(&path, error))?;
        for statement in PROVENANCE_SCHEMA
            .split(';')
            .filter(|part| !part.trim().is_empty())
        {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .map_err(|error| storage_error(&path, error))?;
        }
        service
            .health_check()
            .await
            .map_err(|error| storage_error(&path, error))?;
        Ok(Arc::new(Self {
            app_name,
            path,
            pool,
            service,
        }))
    }

    /// Path of the canonical native graph store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The native app identity used by this store.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// Verify backend connectivity without reading knowledge content.
    pub async fn health_check(&self) -> Result<()> {
        self.service
            .health_check()
            .await
            .map_err(|error| knowledge_error(error.to_string()))
    }

    /// Record a subject/predicate/object observation for an authenticated
    /// caller and return the native relation fact.
    ///
    /// `allowed` is the verified source-authorization snapshot. An empty list
    /// means the namespace. The adapter never invents an authorization list.
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
        allowed: Vec<String>,
    ) -> Result<KnowledgeFact> {
        principal.require(common::scopes::SESSION_WRITE)?;
        principal.require_namespace(namespace)?;
        let user_id = native_user_id(namespace, &principal.id)?;
        self.write_fact(
            &user_id,
            &principal.id,
            namespace,
            subject,
            predicate,
            object,
            source,
            document_id,
            allowed,
        )
        .await
    }

    /// Write one fact for an already resolved native scope.
    #[allow(clippy::too_many_arguments)]
    async fn write_fact(
        &self,
        user_id: &str,
        principal: &str,
        namespace: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        document_id: &str,
        allowed: Vec<String>,
    ) -> Result<KnowledgeFact> {
        validate_fact(subject, predicate, object, source)?;
        let recorded_at = Utc::now();

        let mut entities = vec![CreateEntityInput {
            name: subject.to_string(),
            entity_type: ENTITY_TYPE.to_string(),
            observations: Vec::new(),
        }];
        if object != subject {
            entities.push(CreateEntityInput {
                name: object.to_string(),
                entity_type: ENTITY_TYPE.to_string(),
                observations: Vec::new(),
            });
        }
        self.service
            .create_entities(&self.app_name, user_id, entities)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        let relation_id = self
            .service
            .create_relations(
                &self.app_name,
                user_id,
                vec![CreateRelationInput {
                    source: subject.to_string(),
                    relation_type: predicate.to_string(),
                    target: object.to_string(),
                }],
            )
            .await
            .map_err(|error| knowledge_error(error.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| knowledge_error("native relation write returned no id"))?;
        let observation_ids = self
            .service
            .add_observations(
                &self.app_name,
                user_id,
                subject,
                vec![format!("{predicate} {object}")],
            )
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        let observation_id = observation_ids
            .into_iter()
            .next()
            .ok_or_else(|| knowledge_error("native observation write returned no id"))?;

        let relation_from = self
            .native_valid_from("kg_relations", user_id, relation_id)
            .await?;
        let observation_from = self
            .native_valid_from("kg_observations", user_id, observation_id)
            .await?;
        let allowed = allowed_snapshot(allowed)?;
        let relation = ProvenanceRow {
            kind: "relation".into(),
            native_id: relation_id,
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            source: source.to_string(),
            document_id: document_id.to_string(),
            allowed: allowed.clone(),
            recorded_at: recorded_at.to_rfc3339(),
            valid_from: relation_from,
            valid_to: None,
        };
        let observation = ProvenanceRow {
            kind: "observation".into(),
            native_id: observation_id,
            subject: subject.to_string(),
            predicate: OBSERVATION_PREDICATE.into(),
            object: format!("{predicate} {object}"),
            source: source.to_string(),
            document_id: document_id.to_string(),
            allowed,
            recorded_at: recorded_at.to_rfc3339(),
            valid_from: observation_from,
            valid_to: None,
        };
        self.insert_provenance(user_id, &relation).await?;
        self.insert_provenance(user_id, &observation).await?;
        relation.fact(namespace, principal)
    }

    /// Retrieve current facts for one authenticated caller.
    ///
    /// Retrieval delegates lexical matching to the native `search_nodes`
    /// operation, then exposes only facts with inspectable provenance that the
    /// caller's source authorization permits. A fact without provenance is
    /// never returned.
    pub async fn search(
        &self,
        principal: &Principal,
        namespace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeFact>> {
        principal.require(common::scopes::SESSION_READ)?;
        principal.require_namespace(namespace)?;
        let user_id = native_user_id(namespace, &principal.id)?;
        self.search_scoped(&user_id, namespace, &principal.id, query, limit)
            .await
    }

    /// Record a relation for an already authenticated native invocation.
    ///
    /// This is the tool boundary: the native user id was produced by the Runner
    /// from the authenticated request, so the namespace and principal are
    /// derived, never accepted as tool arguments. Document-derived provenance is
    /// rejected here because only the runtime ingestion path can verify a
    /// document's allowed-principal list.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe_for_native_user(
        &self,
        user_id: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        source: &str,
        document_id: &str,
    ) -> Result<KnowledgeFact> {
        if !document_id.trim().is_empty() {
            return Err(ApiError::new(
                "knowledge.source_denied",
                "agent tools cannot claim document provenance; ingest the source first",
            ));
        }
        let (namespace, principal) = decode_native_user_id(user_id)?;
        self.write_fact(
            user_id,
            &principal,
            &namespace,
            subject,
            predicate,
            object,
            source,
            "",
            Vec::new(),
        )
        .await
    }

    /// Retrieve current facts for an already authenticated native invocation.
    pub async fn search_for_native_user(
        &self,
        user_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeFact>> {
        let (namespace, principal) = decode_native_user_id(user_id)?;
        self.search_scoped(user_id, &namespace, &principal, query, limit)
            .await
    }

    /// Inspect current and superseded facts for one subject.    ///
    /// The pinned public graph API hides expired rows. This adapter-owned query
    /// over `kg_provenance` is the documented history surface; it returns the
    /// native valid-time values and the original source for each revision.
    pub async fn history(
        &self,
        principal: &Principal,
        namespace: &str,
        subject: &str,
    ) -> Result<Vec<KnowledgeFact>> {
        principal.require(common::scopes::SESSION_READ)?;
        principal.require_namespace(namespace)?;
        let user_id = native_user_id(namespace, &principal.id)?;
        let rows = sqlx::query(
            "SELECT kind, native_id, subject, predicate, object, source, document_id, \
                    allowed_json, recorded_at, valid_from, valid_to \
             FROM kg_provenance WHERE app_name=?1 AND user_id=?2 AND (subject=?3 OR object=?3) \
             ORDER BY valid_from, kind, native_id",
        )
        .bind(&self.app_name)
        .bind(&user_id)
        .bind(subject)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        let mut facts = Vec::new();
        for row in rows {
            let provenance = provenance_from_row(&row)?;
            if provenance.authorized(&principal.id) {
                facts.push(provenance.fact(namespace, &principal.id)?);
            }
        }
        Ok(facts)
    }

    /// Invalidate one current fact after verifying adapter ownership.
    ///
    /// The native invalidate methods are not scoped by app or user, so this
    /// adapter first proves the fact belongs to the caller's native scope and
    /// then stamps both native and provenance `valid_to` in one transaction.
    pub async fn invalidate(
        &self,
        principal: &Principal,
        namespace: &str,
        kind: &str,
        fact_id: &str,
    ) -> Result<KnowledgeFact> {
        principal.require(common::scopes::SESSION_WRITE)?;
        principal.require_namespace(namespace)?;
        let user_id = native_user_id(namespace, &principal.id)?;
        let native_id = parse_fact_id(kind, fact_id)?;
        let table = native_table(kind)?;
        let now = Utc::now().to_rfc3339();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        let proven = sqlx::query(
            "SELECT allowed_json FROM kg_provenance \
             WHERE app_name=?1 AND user_id=?2 AND kind=?3 AND native_id=?4 \
             AND valid_to IS NULL",
        )
        .bind(&self.app_name)
        .bind(&user_id)
        .bind(kind)
        .bind(native_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        let Some(proven) = proven else {
            return Err(ApiError::new(
                "knowledge.not_found",
                "knowledge fact is not visible to this caller",
            ));
        };
        let allowed_json: String = proven
            .try_get("allowed_json")
            .map_err(|error| knowledge_error(error.to_string()))?;
        let allowed: Vec<String> = serde_json::from_str(&allowed_json).map_err(|error| {
            ApiError::new(
                "knowledge.storage_corrupt",
                format!("provenance authorization is unreadable: {error}"),
            )
        })?;
        if !allowed.is_empty() && !allowed.iter().any(|id| id == &principal.id) {
            return Err(ApiError::new(
                "knowledge.not_found",
                "knowledge fact is not visible to this caller",
            ));
        }
        sqlx::query(&format!(
            "UPDATE {table} SET valid_to=?1 \
             WHERE app_name=?2 AND user_id=?3 AND id=?4 AND valid_to IS NULL"
        ))
        .bind(&now)
        .bind(&self.app_name)
        .bind(&user_id)
        .bind(native_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        sqlx::query(
            "UPDATE kg_provenance SET valid_to=?1 \
             WHERE app_name=?2 AND user_id=?3 AND kind=?4 AND native_id=?5 \
             AND valid_to IS NULL",
        )
        .bind(&now)
        .bind(&self.app_name)
        .bind(&user_id)
        .bind(kind)
        .bind(native_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        transaction
            .commit()
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;

        let row = sqlx::query(
            "SELECT kind, native_id, subject, predicate, object, source, document_id, \
                    allowed_json, recorded_at, valid_from, valid_to \
             FROM kg_provenance WHERE app_name=?1 AND user_id=?2 AND kind=?3 AND native_id=?4",
        )
        .bind(&self.app_name)
        .bind(&user_id)
        .bind(kind)
        .bind(native_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        provenance_from_row(&row)?.fact(namespace, &principal.id)
    }

    /// Remove one entity and every fact touching it for the caller's scope.
    pub async fn delete_entity(
        &self,
        principal: &Principal,
        namespace: &str,
        name: &str,
    ) -> Result<usize> {
        principal.require(common::scopes::SESSION_WRITE)?;
        principal.require_namespace(namespace)?;
        if name.trim().is_empty() {
            return Err(ApiError::new(
                "knowledge.invalid_fact",
                "entity name is required",
            ));
        }
        let user_id = native_user_id(namespace, &principal.id)?;
        self.service
            .delete_entities(&self.app_name, &user_id, vec![name.to_string()])
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        let deleted = sqlx::query(
            "DELETE FROM kg_provenance \
             WHERE app_name=?1 AND user_id=?2 AND (subject=?3 OR object=?3)",
        )
        .bind(&self.app_name)
        .bind(&user_id)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?
        .rows_affected() as usize;
        Ok(deleted)
    }

    /// Hard-delete every graph fact and native entity for one principal.
    ///
    /// Authorization belongs to the caller; this is the owner-level erasure
    /// operation used by the runtime after it has checked the administrative
    /// scope.
    pub async fn erase_principal(&self, namespace: &str, principal: &str) -> Result<()> {
        let user_id = native_user_id(namespace, principal)?;
        self.service
            .delete_user(&self.app_name, &user_id)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        sqlx::query("DELETE FROM kg_provenance WHERE app_name=?1 AND user_id=?2")
            .bind(&self.app_name)
            .bind(&user_id)
            .execute(&self.pool)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        Ok(())
    }

    /// Remove facts and entity rows older than the configured retention cutoff.
    ///
    /// The native service has no age policy. This owner-level transaction
    /// deletes native rows and their adapter provenance together so retrieval
    /// cannot expose a fact whose provenance was removed.
    pub async fn prune_before(&self, cutoff: DateTime<Utc>) -> Result<usize> {
        let cutoff = cutoff.to_rfc3339();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        let mut removed = 0usize;
        for table in ["kg_observations", "kg_relations"] {
            let result = sqlx::query(&format!(
                "DELETE FROM {table} WHERE app_name=?1 AND valid_from<?2"
            ))
            .bind(&self.app_name)
            .bind(&cutoff)
            .execute(&mut *transaction)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
            removed += result.rows_affected() as usize;
        }
        let provenance =
            sqlx::query("DELETE FROM kg_provenance WHERE app_name=?1 AND recorded_at<?2")
                .bind(&self.app_name)
                .bind(&cutoff)
                .execute(&mut *transaction)
                .await
                .map_err(|error| knowledge_error(error.to_string()))?;
        removed += provenance.rows_affected() as usize;
        let entities = sqlx::query(
            "DELETE FROM kg_entities WHERE app_name=?1 AND updated_at<?2 \
             AND NOT EXISTS (SELECT 1 FROM kg_observations \
                             WHERE kg_observations.app_name=kg_entities.app_name \
                               AND kg_observations.user_id=kg_entities.user_id \
                               AND kg_observations.entity_name=kg_entities.name) \
             AND NOT EXISTS (SELECT 1 FROM kg_relations \
                             WHERE kg_relations.app_name=kg_entities.app_name \
                               AND kg_relations.user_id=kg_entities.user_id \
                               AND (kg_relations.source=kg_entities.name \
                                    OR kg_relations.target=kg_entities.name))",
        )
        .bind(&self.app_name)
        .bind(&cutoff)
        .execute(&mut *transaction)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        removed += entities.rows_affected() as usize;
        transaction
            .commit()
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        Ok(removed)
    }

    /// Remove every fact derived from one ingested document identity.
    ///
    /// Native graph identity is `(app_name, user_id)`, so the adapter deletes
    /// the exact native relation/observation rows named by provenance and then
    /// removes any entity left without a native fact. Other documents' facts
    /// remain untouched.
    pub async fn erase_document(&self, document_id: &str) -> Result<usize> {
        let users = sqlx::query(
            "SELECT DISTINCT user_id FROM kg_provenance \
             WHERE app_name=?1 AND document_id=?2",
        )
        .bind(&self.app_name)
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        for table in ["kg_observations", "kg_relations"] {
            sqlx::query(&format!(
                "DELETE FROM {table} WHERE app_name=?1 AND id IN (\
                 SELECT native_id FROM kg_provenance \
                 WHERE app_name=?1 AND document_id=?2 AND kind=?3)"
            ))
            .bind(&self.app_name)
            .bind(document_id)
            .bind(if table == "kg_relations" {
                "relation"
            } else {
                "observation"
            })
            .execute(&mut *transaction)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        }
        let removed = sqlx::query("DELETE FROM kg_provenance WHERE app_name=?1 AND document_id=?2")
            .bind(&self.app_name)
            .bind(document_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?
            .rows_affected() as usize;
        for row in users {
            let user_id: String = row
                .try_get("user_id")
                .map_err(|error| knowledge_error(error.to_string()))?;
            sqlx::query(
                "DELETE FROM kg_entities WHERE app_name=?1 AND user_id=?2 \
                 AND NOT EXISTS (SELECT 1 FROM kg_observations \
                                 WHERE kg_observations.app_name=kg_entities.app_name \
                                   AND kg_observations.user_id=kg_entities.user_id \
                                   AND kg_observations.entity_name=kg_entities.name) \
                 AND NOT EXISTS (SELECT 1 FROM kg_relations \
                                 WHERE kg_relations.app_name=kg_entities.app_name \
                                   AND kg_relations.user_id=kg_entities.user_id \
                                   AND (kg_relations.source=kg_entities.name \
                                        OR kg_relations.target=kg_entities.name))",
            )
            .bind(&self.app_name)
            .bind(&user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        Ok(removed)
    }

    /// Hard-delete every graph scope recorded in one namespace.
    ///
    /// Native graph identity is `(app_name, user_id)` and there is no native
    /// namespace column. The adapter enumerates every user id present in the
    /// native tables or provenance, decodes it, and erases each matching
    /// namespace through the native `delete_user` operation. This reaches native
    /// rows even if they predate the provenance adapter, so no graph fact
    /// survives a namespace erasure merely because its provenance is missing.
    pub async fn erase_namespace(&self, namespace: &str) -> Result<usize> {
        let rows = sqlx::query(
            "SELECT user_id FROM kg_entities WHERE app_name=?1 \
             UNION SELECT user_id FROM kg_observations WHERE app_name=?1 \
             UNION SELECT user_id FROM kg_relations WHERE app_name=?1 \
             UNION SELECT user_id FROM kg_episodic WHERE app_name=?1 \
             UNION SELECT user_id FROM kg_provenance WHERE app_name=?1",
        )
        .bind(&self.app_name)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        let mut erased = 0usize;
        for row in rows {
            let user_id: String = row
                .try_get("user_id")
                .map_err(|error| knowledge_error(error.to_string()))?;
            let Ok((owner_namespace, principal)) = decode_native_user_id(&user_id) else {
                continue;
            };
            if owner_namespace == namespace {
                self.erase_principal(namespace, &principal).await?;
                erased += 1;
            }
        }
        Ok(erased)
    }

    async fn search_scoped(
        &self,
        user_id: &str,
        namespace: &str,
        principal: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeFact>> {
        let limit = limit.clamp(1, 100);
        let results = self
            .service
            .search_nodes(&self.app_name, user_id, query, limit)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?;
        let current = self.current_provenance(user_id).await?;
        let mut facts: BTreeMap<String, KnowledgeFact> = BTreeMap::new();
        for result in &results {
            for relation in &result.relations {
                self.collect_fact(
                    &current,
                    "relation",
                    relation.id,
                    namespace,
                    principal,
                    &mut facts,
                )?;
            }
            for observation in &result.entity.observations {
                self.collect_fact(
                    &current,
                    "observation",
                    observation.id,
                    namespace,
                    principal,
                    &mut facts,
                )?;
            }
        }
        Ok(facts.into_values().collect())
    }

    fn collect_fact(
        &self,
        current: &BTreeMap<String, ProvenanceRow>,
        kind: &str,
        native_id: i64,
        namespace: &str,
        principal: &str,
        facts: &mut BTreeMap<String, KnowledgeFact>,
    ) -> Result<()> {
        let key = format!("{kind}:{native_id}");
        let Some(row) = current.get(&key) else {
            // Fail closed: a native row without provenance is not exposed.
            return Ok(());
        };
        if row.authorized(principal) {
            facts.insert(key, row.fact(namespace, principal)?);
        }
        Ok(())
    }

    async fn insert_provenance(&self, user_id: &str, row: &ProvenanceRow) -> Result<()> {
        sqlx::query(
            "INSERT INTO kg_provenance \
             (app_name, user_id, kind, native_id, subject, predicate, object, source, \
              document_id, allowed_json, recorded_at, valid_from, valid_to) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(&self.app_name)
        .bind(user_id)
        .bind(&row.kind)
        .bind(row.native_id)
        .bind(&row.subject)
        .bind(&row.predicate)
        .bind(&row.object)
        .bind(&row.source)
        .bind(&row.document_id)
        .bind(
            serde_json::to_string(&row.allowed).map_err(|error| {
                ApiError::new("knowledge.storage_unavailable", error.to_string())
            })?,
        )
        .bind(&row.recorded_at)
        .bind(&row.valid_from)
        .bind(&row.valid_to)
        .execute(&self.pool)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        Ok(())
    }

    async fn current_provenance(&self, user_id: &str) -> Result<BTreeMap<String, ProvenanceRow>> {
        let rows = sqlx::query(
            "SELECT kind, native_id, subject, predicate, object, source, document_id, \
                    allowed_json, recorded_at, valid_from, valid_to \
             FROM kg_provenance \
             WHERE app_name=?1 AND user_id=?2 AND valid_to IS NULL",
        )
        .bind(&self.app_name)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| knowledge_error(error.to_string()))?;
        let mut current = BTreeMap::new();
        for row in rows {
            let provenance = provenance_from_row(&row)?;
            current.insert(provenance.key(), provenance);
        }
        Ok(current)
    }

    async fn native_valid_from(
        &self,
        table: &str,
        user_id: &str,
        native_id: i64,
    ) -> Result<String> {
        let statement =
            format!("SELECT valid_from FROM {table} WHERE app_name=?1 AND user_id=?2 AND id=?3");
        let row = sqlx::query(&statement)
            .bind(&self.app_name)
            .bind(user_id)
            .bind(native_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| knowledge_error(error.to_string()))?
            .ok_or_else(|| knowledge_error("native graph write is not readable after insert"))?;
        row.try_get::<String, _>("valid_from")
            .map_err(|error| knowledge_error(error.to_string()))
    }
}

fn provenance_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ProvenanceRow> {
    let allowed_json: String = row
        .try_get("allowed_json")
        .map_err(|error| knowledge_error(error.to_string()))?;
    let allowed: Vec<String> = serde_json::from_str(&allowed_json).map_err(|error| {
        ApiError::new(
            "knowledge.storage_corrupt",
            format!("provenance authorization is unreadable: {error}"),
        )
    })?;
    Ok(ProvenanceRow {
        kind: row
            .try_get("kind")
            .map_err(|error| knowledge_error(error.to_string()))?,
        native_id: row
            .try_get("native_id")
            .map_err(|error| knowledge_error(error.to_string()))?,
        subject: row
            .try_get("subject")
            .map_err(|error| knowledge_error(error.to_string()))?,
        predicate: row
            .try_get("predicate")
            .map_err(|error| knowledge_error(error.to_string()))?,
        object: row
            .try_get("object")
            .map_err(|error| knowledge_error(error.to_string()))?,
        source: row
            .try_get("source")
            .map_err(|error| knowledge_error(error.to_string()))?,
        document_id: row
            .try_get("document_id")
            .map_err(|error| knowledge_error(error.to_string()))?,
        allowed,
        recorded_at: row
            .try_get("recorded_at")
            .map_err(|error| knowledge_error(error.to_string()))?,
        valid_from: row
            .try_get("valid_from")
            .map_err(|error| knowledge_error(error.to_string()))?,
        valid_to: row
            .try_get("valid_to")
            .map_err(|error| knowledge_error(error.to_string()))?,
    })
}

fn validate_fact(subject: &str, predicate: &str, object: &str, source: &str) -> Result<()> {
    if subject.trim().is_empty() || predicate.trim().is_empty() || object.trim().is_empty() {
        return Err(ApiError::new(
            "knowledge.invalid_fact",
            "subject, predicate, and object are required",
        ));
    }
    if source.trim().is_empty() {
        return Err(ApiError::new(
            "knowledge.source_required",
            "knowledge source is required",
        ));
    }
    Ok(())
}

fn allowed_snapshot(allowed: Vec<String>) -> Result<Vec<String>> {
    let mut allowed: Vec<String> = allowed
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect();
    allowed.sort();
    allowed.dedup();
    Ok(allowed)
}

fn native_table(kind: &str) -> Result<&'static str> {
    match kind {
        "relation" => Ok("kg_relations"),
        "observation" => Ok("kg_observations"),
        other => Err(ApiError::new(
            "knowledge.invalid_fact",
            format!("unknown knowledge fact kind '{other}'"),
        )),
    }
}

fn parse_fact_id(kind: &str, value: &str) -> Result<i64> {
    let prefix = format!("{kind}:");
    let raw = value.strip_prefix(&prefix).unwrap_or(value);
    raw.parse::<i64>().map_err(|_| {
        ApiError::new(
            "knowledge.invalid_fact",
            format!("'{value}' is not a native {kind} identity"),
        )
    })
}

fn parse_millis(value: &str) -> Result<i64> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.timestamp_millis())
        .map_err(|error| {
            ApiError::new(
                "knowledge.storage_corrupt",
                format!("native graph timestamp '{value}' is invalid: {error}"),
            )
        })
}

fn storage_error(path: &Path, error: impl std::fmt::Display) -> ApiError {
    let message = error.to_string();
    let code = if message.contains("not a database") || message.contains("malformed") {
        "knowledge.storage_corrupt"
    } else {
        "knowledge.storage_unavailable"
    };
    ApiError::new(
        code,
        format!("native knowledge graph '{}': {message}", path.display()),
    )
}

fn knowledge_error(message: impl Into<String>) -> Error {
    ApiError::new("knowledge.storage_unavailable", message)
}
