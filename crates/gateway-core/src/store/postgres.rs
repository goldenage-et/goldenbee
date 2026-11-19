//! Transaction-shaped Postgres adapter for shared Gateway state.
//!
//! Every control-plane call runs in its own transaction. The registry document
//! is loaded under a row lock, mutated through the same pure document logic as
//! the standalone store, and committed before the call returns. Dispatch intent,
//! request identity, claims, revisions, and stream mappings live in normalized
//! columns; uniqueness and atomic claims are enforced by the database.

use async_trait::async_trait;
use common::{
    AdminWorkerSummary, ApiError, RegistryAuditEntry, Task, TaskState, WorkerDiscoveryFilter,
    WorkerLease, WorkerLeaseRequest, WorkerRegistrationRequest, WorkerSummary,
};
use common::{Team, TeamSpec};
use serde_json::Value;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgConnection, PgPool, Row};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::registry::{
    Approval, ApprovalPatch, ApprovalRecord, Document, NewApproval, Registry, Verified,
    REGISTRY_SCHEMA,
};

use super::{
    AcceptedMapping, ApprovalStore, AttemptOutcome, AuditStore, Begin, Claim, ClaimToken,
    Connectivity, DispatchState, DispatchStore, LeaseStore, NewIntent, ReplicaId, StreamMapStore,
    StreamMapping, Submission, TeamStore, Transition,
};

/// Supported shared schema version.
pub const STORE_SCHEMA: u32 = 1;

type DbFuture<T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send>>;
type Job = Box<dyn FnOnce(PgPool) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Dedicated bridge that owns the Postgres runtime and blocks sync control-store
/// calls without blocking a caller's async runtime.
struct PgBridge {
    tx: std::sync::mpsc::Sender<Job>,
}

impl PgBridge {
    fn connect(url: String, max_connections: u32) -> Result<Arc<Self>, ApiError> {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), ApiError>>();
        std::thread::Builder::new()
            .name("gateway-postgres".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(storage_error("runtime", &error.to_string())));
                        return;
                    }
                };
                let mut last_error = None;
                let mut pool = None;
                for _ in 0..8 {
                    match runtime.block_on(async {
                        PgPoolOptions::new()
                            .max_connections(max_connections.max(1))
                            .acquire_timeout(Duration::from_secs(15))
                            .connect(&url)
                            .await
                    }) {
                        Ok(connected) => {
                            pool = Some(connected);
                            break;
                        }
                        Err(error) => {
                            last_error = Some(error);
                            std::thread::sleep(Duration::from_millis(250));
                        }
                    }
                }
                let pool = match (pool, last_error) {
                    (Some(pool), _) => pool,
                    (None, Some(error)) => {
                        let _ = ready_tx.send(Err(storage_error(
                            "cannot connect to the shared store",
                            &error.to_string(),
                        )));
                        return;
                    }
                    (None, None) => {
                        let _ = ready_tx.send(Err(storage_error(
                            "cannot connect to the shared store",
                            "no attempt was made",
                        )));
                        return;
                    }
                };
                if let Err(error) = runtime.block_on(init_schema(&pool)) {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
                if ready_tx.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(job) = rx.recv() {
                    runtime.block_on(job(pool.clone()));
                }
            })
            .map_err(|error| storage_error("bridge", &error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Arc::new(Self { tx })),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(storage_error("bridge", "shared store thread stopped")),
        }
    }

    fn call<T, F>(&self, make: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(PgPool) -> DbFuture<T> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel::<Result<T, ApiError>>();
        self.tx
            .send(Box::new(move |pool| {
                Box::pin(async move {
                    let result = make(pool).await;
                    let _ = tx.send(result);
                })
            }))
            .map_err(|_| storage_error("bridge", "shared store is unavailable"))?;
        rx.recv()
            .map_err(|_| storage_error("bridge", "shared store stopped"))?
    }
}

/// Shared transactional adapter. It holds no authoritative process-local state.
pub struct PostgresBackend {
    bridge: Arc<PgBridge>,
    lease_ttl_ms: i64,
}

impl PostgresBackend {
    /// Connect, apply migrations, and verify the schema version.
    pub async fn connect(
        url: &str,
        max_connections: u32,
        lease_ttl_ms: i64,
    ) -> Result<Arc<Self>, ApiError> {
        let bridge = PgBridge::connect(url.to_string(), max_connections)?;
        Ok(Arc::new(Self {
            bridge,
            lease_ttl_ms: lease_ttl_ms.max(1),
        }))
    }

    fn read_registry<T, F>(&self, make: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&Registry, i64) -> Result<T, ApiError> + Send + 'static,
    {
        let ttl = self.lease_ttl_ms;
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let row = sqlx::query(
                    "SELECT document, (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AS now_ms \
                     FROM gateway_registry WHERE id = 1",
                )
                .fetch_one(&pool)
                .await
                .map_err(db_error)?;
                let document: Value = row.try_get("document").map_err(db_error)?;
                let now: i64 = row.try_get("now_ms").map_err(db_error)?;
                let document: Document = serde_json::from_value(document)
                    .map_err(|error| storage_error("registry document", &error.to_string()))?;
                if document.version > REGISTRY_SCHEMA {
                    return Err(ApiError::new(
                        "registry.schema_unsupported",
                        "shared registry schema is newer than supported",
                    ));
                }
                let registry = Registry::from_document(document, Arc::new(move || now), ttl)?;
                make(&registry, now)
            })
        })
    }

    fn mutate_registry<T, F>(&self, make: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&Registry, i64) -> Result<T, ApiError> + Send + 'static,
    {
        let ttl = self.lease_ttl_ms;
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let mut tx = pool.begin().await.map_err(db_error)?;
                let row = sqlx::query(
                    "SELECT document, (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AS now_ms \
                     FROM gateway_registry WHERE id = 1 FOR UPDATE",
                )
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
                let document: Value = row.try_get("document").map_err(db_error)?;
                let now: i64 = row.try_get("now_ms").map_err(db_error)?;
                let mut document: Document = serde_json::from_value(document)
                    .map_err(|error| storage_error("registry document", &error.to_string()))?;
                if document.version > REGISTRY_SCHEMA {
                    return Err(ApiError::new(
                        "registry.schema_unsupported",
                        "shared registry schema is newer than supported",
                    ));
                }
                document.version = REGISTRY_SCHEMA;
                // The audit table is authoritative; only decisions produced by
                // this transaction are inserted below.
                document.audit.clear();
                let registry = Registry::from_document(document, Arc::new(move || now), ttl)?;
                let outcome = make(&registry, now);
                let snapshot = registry.snapshot();
                insert_audit(&mut tx, &snapshot.audit).await?;
                match outcome {
                    Ok(value) => {
                        let mut persisted = snapshot;
                        persisted.audit.clear();
                        persisted.version = REGISTRY_SCHEMA;
                        let document = serde_json::to_value(&persisted)
                            .map_err(|error| storage_error("registry document", &error.to_string()))?;
                        sqlx::query(
                            "UPDATE gateway_registry SET document = $1, revision = revision + 1 \
                             WHERE id = 1",
                        )
                        .bind(document)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_error)?;
                        tx.commit().await.map_err(db_error)?;
                        Ok(value)
                    }
                    Err(error) => {
                        // A denied decision may have appended audit inside the
                        // temporary registry. Persist only those entries.
                        tx.commit().await.map_err(db_error)?;
                        Err(error)
                    }
                }
            })
        })
    }
}

async fn init_schema(pool: &PgPool) -> Result<(), ApiError> {
    // Serialize concurrent replica startup on one session, so DDL cannot race.
    let mut connection = pool.acquire().await.map_err(db_error)?;
    sqlx::query("SELECT pg_advisory_lock(56056)")
        .execute(&mut *connection)
        .await
        .map_err(db_error)?;
    let migration = include_str!("../../migrations/0001_shared_store.sql");
    let result = async {
        for statement in migration.split(';') {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }
            sqlx::query(statement)
                .execute(&mut *connection)
                .await
                .map_err(db_error)?;
        }
        Ok::<(), ApiError>(())
    }
    .await;
    let _ = sqlx::query("SELECT pg_advisory_unlock(56056)")
        .execute(&mut *connection)
        .await;
    drop(connection);
    result?;
    let version: i32 = sqlx::query_scalar("SELECT version FROM gateway_schema WHERE id = 1")
        .fetch_one(pool)
        .await
        .map_err(db_error)?;
    if version < 0 || version as u32 > STORE_SCHEMA {
        return Err(ApiError::new(
            "storage.schema_unsupported",
            "shared store schema is newer than this Gateway supports",
        )
        .with("supported", STORE_SCHEMA.to_string())
        .with("found", version.to_string()));
    }
    Ok(())
}

async fn insert_audit(
    conn: &mut PgConnection,
    entries: &[RegistryAuditEntry],
) -> Result<(), ApiError> {
    for entry in entries {
        sqlx::query(
            "INSERT INTO gateway_audit (at, action, worker, principal, outcome, detail) \
             VALUES (to_timestamp($1::double precision / 1000.0), $2, $3, $4, $5, $6)",
        )
        .bind(entry.at)
        .bind(&entry.action)
        .bind(&entry.worker)
        .bind(&entry.principal)
        .bind(&entry.outcome)
        .bind(&entry.detail)
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    }
    sqlx::query(
        "DELETE FROM gateway_audit WHERE id <= \
         (SELECT COALESCE(MAX(id), 0) - 2000 FROM gateway_audit)",
    )
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Ok(())
}

fn db_error(error: sqlx::Error) -> ApiError {
    ApiError::retryable(
        "storage.unavailable",
        format!("shared Gateway store operation failed: {error}"),
    )
}

fn storage_error(detail: &str, message: &str) -> ApiError {
    ApiError::new(
        "storage.unavailable",
        format!("shared Gateway store is unavailable: {detail}"),
    )
    .with("detail", message.to_string())
}

fn parse_state(value: &str) -> DispatchState {
    match value {
        "dispatching" => DispatchState::Dispatching,
        "accepted" => DispatchState::Accepted,
        "unknown" => DispatchState::Unknown,
        _ => DispatchState::Intent,
    }
}

fn parse_connectivity(value: &str) -> Connectivity {
    match value {
        "online" => Connectivity::Online,
        "unreachable" => Connectivity::Unreachable,
        _ => Connectivity::Unknown,
    }
}

fn row_to_submission(row: &PgRow) -> Result<Submission, ApiError> {
    let accepted: Option<Value> = row.try_get("accepted").map_err(db_error)?;
    let projection: Option<Value> = row.try_get("projection").map_err(db_error)?;
    let transitions: Value = row.try_get("transitions").map_err(db_error)?;
    let state: String = row.try_get("state").map_err(db_error)?;
    let connectivity: String = row.try_get("connectivity").map_err(db_error)?;
    Ok(Submission {
        execution_id: row.try_get("execution_id").map_err(db_error)?,
        namespace: row.try_get("namespace").map_err(db_error)?,
        caller: row.try_get("caller").map_err(db_error)?,
        idempotency_key: row.try_get("idempotency_key").map_err(db_error)?,
        request_digest: row.try_get("request_digest").map_err(db_error)?,
        worker: row.try_get("worker").map_err(db_error)?,
        worker_instance: row.try_get("worker_instance").map_err(db_error)?,
        worker_incarnation: row.try_get("worker_incarnation").map_err(db_error)?,
        agent: row.try_get("agent").map_err(db_error)?,
        dispatch: parse_state(&state),
        worker_task_id: row.try_get("worker_task_id").map_err(db_error)?,
        accepted: accepted
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| storage_error("dispatch accepted", &error.to_string()))?,
        projection: projection
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| storage_error("dispatch projection", &error.to_string()))?,
        connectivity: parse_connectivity(&connectivity),
        last_error: row.try_get("last_error").map_err(db_error)?,
        revision: row.try_get::<i64, _>("revision").map_err(db_error)? as u64,
        claim_id: row.try_get("claim_id").map_err(db_error)?,
        claimed_by: row.try_get("claimed_by").map_err(db_error)?,
        claim_expires_at: row.try_get("claim_expires_at_ms").map_err(db_error)?,
        created_at: row.try_get("created_at_ms").map_err(db_error)?,
        updated_at: row.try_get("updated_at_ms").map_err(db_error)?,
        transitions: serde_json::from_value(transitions)
            .map_err(|error| storage_error("dispatch transitions", &error.to_string()))?,
    })
}

const DISPATCH_COLUMNS: &str = "execution_id, namespace, caller, idempotency_key, request_digest, \
 worker, agent, state, worker_instance, worker_incarnation, worker_task_id, accepted, projection, \
 connectivity, last_error, revision, claim_id, claimed_by, \
 (EXTRACT(EPOCH FROM claim_expires_at)*1000)::bigint AS claim_expires_at_ms, \
 (EXTRACT(EPOCH FROM created_at)*1000)::bigint AS created_at_ms, \
 (EXTRACT(EPOCH FROM updated_at)*1000)::bigint AS updated_at_ms, transitions";

fn transition_json(outcome: &str, status: &str, detail: &str) -> Value {
    serde_json::to_value(vec![Transition {
        at: common::now_ms(),
        status: status.into(),
        outcome: outcome.into(),
        detail: detail.into(),
    }])
    .unwrap_or_else(|_| Value::Array(Vec::new()))
}

async fn load_dispatch(pool: &PgPool, execution_id: &str) -> Result<Option<Submission>, ApiError> {
    let sql = format!("SELECT {DISPATCH_COLUMNS} FROM gateway_dispatch WHERE execution_id = $1");
    let row = sqlx::query(&sql)
        .bind(execution_id)
        .fetch_optional(pool)
        .await
        .map_err(db_error)?;
    row.as_ref().map(row_to_submission).transpose()
}

#[async_trait]
impl DispatchStore for PostgresBackend {
    async fn begin(&self, intent: NewIntent) -> Result<Begin, ApiError> {
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let transitions = transition_json(
                    "intent_recorded",
                    "intent",
                    "caller-scoped identity and dispatch intent are durable",
                );
                let sql = format!(
                    "INSERT INTO gateway_dispatch \
                     (execution_id, namespace, caller, idempotency_key, request_digest, worker, agent, \
                      state, revision, created_at, updated_at, transitions) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'intent', 1, \
                             clock_timestamp(), clock_timestamp(), $8) \
                     ON CONFLICT (namespace, caller, idempotency_key) DO NOTHING \
                     RETURNING {DISPATCH_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&intent.execution_id)
                    .bind(&intent.namespace)
                    .bind(&intent.caller)
                    .bind(&intent.idempotency_key)
                    .bind(&intent.request_digest)
                    .bind(&intent.worker)
                    .bind(&intent.agent)
                    .bind(transitions)
                    .fetch_optional(&pool)
                    .await
                    .map_err(db_error)?;
                if let Some(row) = row {
                    return Ok(Begin::Created(row_to_submission(&row)?));
                }
                let sql = format!(
                    "SELECT {DISPATCH_COLUMNS} FROM gateway_dispatch \
                     WHERE namespace = $1 AND caller = $2 AND idempotency_key = $3"
                );
                let row = sqlx::query(&sql)
                    .bind(&intent.namespace)
                    .bind(&intent.caller)
                    .bind(&intent.idempotency_key)
                    .fetch_one(&pool)
                    .await
                    .map_err(db_error)?;
                let existing = row_to_submission(&row)?;
                if existing.request_digest == intent.request_digest {
                    Ok(Begin::Existing(existing))
                } else {
                    Ok(Begin::Conflict(existing))
                }
            })
        })
    }

    async fn claim(
        &self,
        execution_id: &str,
        replica: &ReplicaId,
        claim_ttl_ms: i64,
    ) -> Result<Claim, ApiError> {
        let execution_id = execution_id.to_string();
        let replica = replica.clone();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let claim_id = uuid::Uuid::new_v4().to_string();
                let transitions = transition_json(
                    "dispatch_attempted",
                    "dispatching",
                    "remote submission may have reached the Worker",
                );
                let sql = format!(
                    "UPDATE gateway_dispatch SET \
                       state = 'dispatching', claim_id = $2, claimed_by = $3, \
                       claim_expires_at = clock_timestamp() + \
                         make_interval(secs => $4::double precision / 1000.0), \
                       connectivity = 'unknown', revision = revision + 1, \
                       updated_at = clock_timestamp(), \
                       transitions = transitions || $5::jsonb \
                     WHERE execution_id = $1 AND \
                       (state IN ('intent','unknown') OR \
                        (state = 'dispatching' AND \
                         (claim_expires_at IS NULL OR claim_expires_at <= clock_timestamp()))) \
                     RETURNING {DISPATCH_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&execution_id)
                    .bind(&claim_id)
                    .bind(&replica.0)
                    .bind(claim_ttl_ms.max(1))
                    .bind(transitions)
                    .fetch_optional(&pool)
                    .await
                    .map_err(db_error)?;
                if let Some(row) = row {
                    let submission = row_to_submission(&row)?;
                    return Ok(Claim::Claimed(
                        ClaimToken {
                            execution_id: execution_id.clone(),
                            claim_id,
                            replica: replica.0.clone(),
                        },
                        submission,
                    ));
                }
                let Some(submission) = load_dispatch(&pool, &execution_id).await? else {
                    return Ok(Claim::Missing);
                };
                if submission.dispatch == DispatchState::Accepted {
                    return Ok(Claim::Accepted(submission));
                }
                Ok(Claim::Busy {
                    until_ms: submission.claim_expires_at.unwrap_or_else(common::now_ms),
                })
            })
        })
    }

    async fn commit(
        &self,
        claim: &ClaimToken,
        mapping: AcceptedMapping,
    ) -> Result<Submission, ApiError> {
        let claim = claim.clone();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let transitions = transition_json(
                    "accepted",
                    "accepted",
                    "Worker returned a durable task handle for the original key",
                );
                let accepted = serde_json::to_value(&mapping.accepted)
                    .map_err(|error| storage_error("acceptance", &error.to_string()))?;
                let sql = format!(
                    "UPDATE gateway_dispatch SET \
                       state = 'accepted', worker_task_id = $3, worker_instance = $4, \
                       worker_incarnation = $5, accepted = $6, connectivity = 'online', \
                       last_error = NULL, claim_id = NULL, claim_expires_at = NULL, \
                       revision = revision + 1, updated_at = clock_timestamp(), \
                       transitions = transitions || $7::jsonb \
                     WHERE execution_id = $1 AND claim_id = $2 AND state = 'dispatching' \
                     RETURNING {DISPATCH_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&claim.execution_id)
                    .bind(&claim.claim_id)
                    .bind(&mapping.worker_task_id)
                    .bind(&mapping.worker_instance)
                    .bind(&mapping.worker_incarnation)
                    .bind(accepted)
                    .bind(transitions)
                    .fetch_optional(&pool)
                    .await
                    .map_err(db_error)?;
                if let Some(row) = row {
                    return row_to_submission(&row);
                }
                let Some(submission) = load_dispatch(&pool, &claim.execution_id).await? else {
                    return Err(ApiError::new(
                        "storage.corrupt",
                        "the dispatch row disappeared before the mapping could commit",
                    ));
                };
                if submission.dispatch == DispatchState::Accepted {
                    return if submission.worker_task_id.as_deref()
                        == Some(&mapping.worker_task_id)
                    {
                        Ok(submission)
                    } else {
                        Err(ApiError::new(
                            "task.dispatch_conflict",
                            "the Worker acceptance was already committed with a different task handle",
                        )
                        .with("execution_id", claim.execution_id))
                    };
                }
                Err(ApiError::retryable(
                    "task.claim_lost",
                    "the dispatch claim was lost before the mapping could commit",
                )
                .with("execution_id", claim.execution_id))
            })
        })
    }

    async fn release(&self, claim: &ClaimToken, outcome: AttemptOutcome) -> Result<(), ApiError> {
        let claim = claim.clone();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let state = match outcome.state {
                    DispatchState::Intent => "intent",
                    DispatchState::Dispatching => "dispatching",
                    DispatchState::Accepted => "accepted",
                    DispatchState::Unknown => "unknown",
                };
                let connectivity = match outcome.connectivity {
                    Connectivity::Unknown => "unknown",
                    Connectivity::Online => "online",
                    Connectivity::Unreachable => "unreachable",
                };
                let transitions = transition_json(&outcome.outcome, state, &outcome.detail);
                let result = sqlx::query(
                    "UPDATE gateway_dispatch SET state = $3, connectivity = $4, last_error = $5, \
                     claim_id = NULL, claim_expires_at = NULL, revision = revision + 1, \
                     updated_at = clock_timestamp(), transitions = transitions || $6::jsonb \
                     WHERE execution_id = $1 AND claim_id = $2",
                )
                .bind(&claim.execution_id)
                .bind(&claim.claim_id)
                .bind(state)
                .bind(connectivity)
                .bind(&outcome.last_error)
                .bind(transitions)
                .execute(&pool)
                .await
                .map_err(db_error)?;
                if result.rows_affected() == 0 {
                    return Err(ApiError::new(
                        "task.claim_lost",
                        "the dispatch claim was already released",
                    )
                    .with("execution_id", claim.execution_id));
                }
                Ok(())
            })
        })
    }

    async fn record_projection(
        &self,
        execution_id: &str,
        projection: Option<Task>,
        state: Option<TaskState>,
        connectivity: Connectivity,
    ) -> Result<(), ApiError> {
        let execution_id = execution_id.to_string();
        let projection = projection
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| storage_error("projection", &error.to_string()))?;
        let state = state.and_then(|state| {
            serde_json::to_value(state)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
        });
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let connectivity = match connectivity {
                    Connectivity::Unknown => "unknown",
                    Connectivity::Online => "online",
                    Connectivity::Unreachable => "unreachable",
                };
                let transitions = transition_json(
                    "projection_refreshed",
                    "accepted",
                    "Worker projection observed",
                );
                sqlx::query(
                    "UPDATE gateway_dispatch SET projection = COALESCE($2, projection), \
                     accepted = CASE WHEN $3::text IS NULL THEN accepted \
                       ELSE jsonb_set(accepted, '{state}', to_jsonb($3::text)) END, \
                     connectivity = $4, revision = revision + 1, updated_at = clock_timestamp(), \
                     transitions = transitions || $5::jsonb \
                     WHERE execution_id = $1",
                )
                .bind(&execution_id)
                .bind(projection)
                .bind(state)
                .bind(connectivity)
                .bind(transitions)
                .execute(&pool)
                .await
                .map_err(db_error)?;
                Ok(())
            })
        })
    }

    async fn mark_connectivity(
        &self,
        execution_id: &str,
        connectivity: Connectivity,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError> {
        let execution_id = execution_id.to_string();
        let outcome = outcome.to_string();
        let detail = detail.to_string();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let connectivity = match connectivity {
                    Connectivity::Unknown => "unknown",
                    Connectivity::Online => "online",
                    Connectivity::Unreachable => "unreachable",
                };
                let transitions = transition_json(&outcome, "accepted", &detail);
                sqlx::query(
                    "UPDATE gateway_dispatch SET connectivity = $2, revision = revision + 1, \
                     updated_at = clock_timestamp(), transitions = transitions || $3::jsonb \
                     WHERE execution_id = $1",
                )
                .bind(&execution_id)
                .bind(connectivity)
                .bind(transitions)
                .execute(&pool)
                .await
                .map_err(db_error)?;
                Ok(())
            })
        })
    }

    async fn clear_projection(
        &self,
        execution_id: &str,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError> {
        let execution_id = execution_id.to_string();
        let outcome = outcome.to_string();
        let detail = detail.to_string();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let transitions = transition_json(&outcome, "accepted", &detail);
                sqlx::query(
                    "UPDATE gateway_dispatch SET projection = NULL, last_error = $2, \
                     revision = revision + 1, updated_at = clock_timestamp(), \
                     transitions = transitions || $3::jsonb \
                     WHERE execution_id = $1",
                )
                .bind(&execution_id)
                .bind(&outcome)
                .bind(transitions)
                .execute(&pool)
                .await
                .map_err(db_error)?;
                Ok(())
            })
        })
    }

    async fn transition(
        &self,
        execution_id: &str,
        outcome: &str,
        detail: &str,
    ) -> Result<(), ApiError> {
        let execution_id = execution_id.to_string();
        let outcome = outcome.to_string();
        let detail = detail.to_string();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let transitions = transition_json(&outcome, "accepted", &detail);
                sqlx::query(
                    "UPDATE gateway_dispatch SET revision = revision + 1, \
                     updated_at = clock_timestamp(), transitions = transitions || $2::jsonb \
                     WHERE execution_id = $1",
                )
                .bind(&execution_id)
                .bind(transitions)
                .execute(&pool)
                .await
                .map_err(db_error)?;
                Ok(())
            })
        })
    }

    async fn submission(&self, execution_id: &str) -> Result<Option<Submission>, ApiError> {
        let execution_id = execution_id.to_string();
        self.bridge
            .call(move |pool| Box::pin(async move { load_dispatch(&pool, &execution_id).await }))
    }

    async fn submission_by_key(
        &self,
        namespace: &str,
        caller: &str,
        key: &str,
    ) -> Result<Option<Submission>, ApiError> {
        let namespace = namespace.to_string();
        let caller = caller.to_string();
        let key = key.to_string();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let sql = format!(
                    "SELECT {DISPATCH_COLUMNS} FROM gateway_dispatch \
                     WHERE namespace = $1 AND caller = $2 AND idempotency_key = $3"
                );
                let row = sqlx::query(&sql)
                    .bind(namespace)
                    .bind(caller)
                    .bind(key)
                    .fetch_optional(&pool)
                    .await
                    .map_err(db_error)?;
                row.as_ref().map(row_to_submission).transpose()
            })
        })
    }

    fn description(&self) -> String {
        "postgres".into()
    }

    fn path(&self) -> Option<std::path::PathBuf> {
        None
    }

    async fn ready(&self) -> Result<(), ApiError> {
        self.bridge.call(|pool| {
            Box::pin(async move {
                let version: i32 =
                    sqlx::query_scalar("SELECT version FROM gateway_schema WHERE id = 1")
                        .fetch_one(&pool)
                        .await
                        .map_err(db_error)?;
                if version < 0 || version as u32 > STORE_SCHEMA {
                    return Err(ApiError::new(
                        "storage.schema_unsupported",
                        "shared store schema is newer than this Gateway supports",
                    ));
                }
                Ok(())
            })
        })
    }
}

#[async_trait]
impl StreamMapStore for PostgresBackend {
    async fn stream_mapping(&self, execution_id: &str) -> Result<Option<StreamMapping>, ApiError> {
        let execution_id = execution_id.to_string();
        self.bridge.call(move |pool| {
            Box::pin(async move {
                let row = sqlx::query(
                    "SELECT execution_id, worker, worker_instance, worker_incarnation, \
                            worker_task_id, agent, namespace, caller \
                     FROM gateway_dispatch WHERE execution_id = $1 \
                       AND worker_task_id IS NOT NULL",
                )
                .bind(execution_id)
                .fetch_optional(&pool)
                .await
                .map_err(db_error)?;
                let Some(row) = row else {
                    return Ok(None);
                };
                Ok(Some(StreamMapping {
                    execution_id: row.try_get("execution_id").map_err(db_error)?,
                    worker: row.try_get("worker").map_err(db_error)?,
                    worker_instance: row.try_get("worker_instance").map_err(db_error)?,
                    worker_incarnation: row.try_get("worker_incarnation").map_err(db_error)?,
                    worker_task_id: row.try_get("worker_task_id").map_err(db_error)?,
                    agent: row.try_get("agent").map_err(db_error)?,
                    namespace: row.try_get("namespace").map_err(db_error)?,
                    caller: row.try_get("caller").map_err(db_error)?,
                }))
            })
        })
    }
}

impl ApprovalStore for PostgresBackend {
    fn now(&self) -> i64 {
        self.bridge
            .call(|pool| {
                Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
                    )
                    .fetch_one(&pool)
                    .await
                    .map_err(db_error)
                })
            })
            .unwrap_or_else(|_| common::now_ms())
    }

    fn control_identity(&self, token: &str) -> Option<String> {
        let token = token.to_string();
        self.read_registry(move |registry, _| Ok(registry.control_worker(&token)))
            .ok()
            .flatten()
    }

    fn approval(&self, id: &str) -> Option<ApprovalRecord> {
        let id = id.to_string();
        self.read_registry(move |registry, _| Ok(registry.approval_record(&id)))
            .ok()
            .flatten()
    }

    fn seed_config(&self, approvals: Vec<Approval>, _now: i64) -> Result<(), ApiError> {
        self.mutate_registry(move |registry, db_now| registry.seed_config(approvals, db_now))
    }

    fn add(
        &self,
        input: NewApproval,
        principal: &str,
        _now: i64,
    ) -> Result<(AdminWorkerSummary, bool), ApiError> {
        let principal = principal.to_string();
        self.mutate_registry(move |registry, db_now| registry.add(input, &principal, db_now))
    }

    fn update(
        &self,
        id: &str,
        patch: ApprovalPatch,
        principal: &str,
        _now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        let id = id.to_string();
        let principal = principal.to_string();
        self.mutate_registry(move |registry, db_now| {
            registry.update(&id, patch, &principal, db_now)
        })
    }

    fn rollback_add(&self, id: &str, principal: &str, _now: i64) -> Result<(), ApiError> {
        let id = id.to_string();
        let principal = principal.to_string();
        self.mutate_registry(move |registry, db_now| registry.rollback_add(&id, &principal, db_now))
    }

    fn disable(
        &self,
        id: &str,
        principal: &str,
        _now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        let id = id.to_string();
        let principal = principal.to_string();
        self.mutate_registry(move |registry, db_now| registry.disable(&id, &principal, db_now))
    }

    fn enable(&self, id: &str, principal: &str, _now: i64) -> Result<AdminWorkerSummary, ApiError> {
        let id = id.to_string();
        let principal = principal.to_string();
        self.mutate_registry(move |registry, db_now| registry.enable(&id, &principal, db_now))
    }

    fn decommission(
        &self,
        id: &str,
        principal: &str,
        _now: i64,
    ) -> Result<AdminWorkerSummary, ApiError> {
        let id = id.to_string();
        let principal = principal.to_string();
        self.mutate_registry(move |registry, db_now| registry.decommission(&id, &principal, db_now))
    }

    fn admin_workers(&self) -> Vec<AdminWorkerSummary> {
        self.read_registry(|registry, _| Ok(registry.admin_workers()))
            .unwrap_or_default()
    }

    fn admin_worker(&self, id: &str) -> Option<AdminWorkerSummary> {
        let id = id.to_string();
        self.read_registry(move |registry, _| Ok(registry.admin_worker(&id)))
            .ok()
            .flatten()
    }
}

impl LeaseStore for PostgresBackend {
    fn register(
        &self,
        id: &str,
        request: &WorkerRegistrationRequest,
        verified: &Verified,
        _now: i64,
    ) -> Result<WorkerLease, ApiError> {
        let id = id.to_string();
        let request = request.clone();
        let verified = verified.clone();
        self.mutate_registry(move |registry, db_now| {
            registry.register(&id, &request, &verified, db_now)
        })
    }

    fn renew(
        &self,
        id: &str,
        request: &WorkerLeaseRequest,
        verified: &Verified,
        _now: i64,
    ) -> Result<WorkerLease, ApiError> {
        let id = id.to_string();
        let request = request.clone();
        let verified = verified.clone();
        self.mutate_registry(move |registry, db_now| {
            registry.renew(&id, &request, &verified, db_now)
        })
    }

    fn bootstrap(&self, id: &str, verified: &Verified, _now: i64) -> Result<WorkerLease, ApiError> {
        let id = id.to_string();
        let verified = verified.clone();
        self.mutate_registry(move |registry, db_now| registry.bootstrap(&id, &verified, db_now))
    }

    fn mark_unreachable(&self, id: &str, _now: i64, detail: &str) -> Result<(), ApiError> {
        let id = id.to_string();
        let detail = detail.to_string();
        self.mutate_registry(move |registry, db_now| {
            registry.mark_unreachable(&id, db_now, &detail)
        })
    }

    fn expire(&self, _now: i64) -> usize {
        self.mutate_registry(move |registry, db_now| Ok(registry.expire(db_now)))
            .unwrap_or(0)
    }

    fn worker(&self, id: &str) -> Option<WorkerSummary> {
        let id = id.to_string();
        self.read_registry(move |registry, _| Ok(registry.worker(&id)))
            .ok()
            .flatten()
    }

    fn discover(&self, filter: &WorkerDiscoveryFilter) -> Vec<WorkerSummary> {
        let filter = filter.clone();
        self.read_registry(move |registry, _| Ok(registry.discover(|_| true, &filter)))
            .unwrap_or_default()
    }

    fn health(&self) -> (u32, u32) {
        self.read_registry(|registry, _| Ok(registry.health()))
            .unwrap_or((0, 0))
    }
}

impl TeamStore for PostgresBackend {
    fn create_team(
        &self,
        namespace: &str,
        name: &str,
        spec: TeamSpec,
        _now: i64,
    ) -> Result<Team, ApiError> {
        let namespace = namespace.to_string();
        let name = name.to_string();
        self.mutate_registry(move |registry, db_now| {
            registry.create_team(&namespace, &name, spec, db_now)
        })
    }

    fn update_team(
        &self,
        namespace: &str,
        name: &str,
        expected_version: u64,
        spec: TeamSpec,
        _now: i64,
    ) -> Result<Team, ApiError> {
        let namespace = namespace.to_string();
        let name = name.to_string();
        self.mutate_registry(move |registry, db_now| {
            registry.update_team(&namespace, &name, expected_version, spec, db_now)
        })
    }

    fn validate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        _now: i64,
    ) -> Result<Team, ApiError> {
        let namespace = namespace.to_string();
        let name = name.to_string();
        self.mutate_registry(move |registry, db_now| {
            registry.validate_team(&namespace, &name, version, db_now)
        })
    }

    fn activate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        _now: i64,
    ) -> Result<Team, ApiError> {
        let namespace = namespace.to_string();
        let name = name.to_string();
        self.mutate_registry(move |registry, db_now| {
            registry.activate_team(&namespace, &name, version, db_now)
        })
    }

    fn deactivate_team(
        &self,
        namespace: &str,
        name: &str,
        version: Option<u64>,
        _now: i64,
    ) -> Result<Team, ApiError> {
        let namespace = namespace.to_string();
        let name = name.to_string();
        self.mutate_registry(move |registry, db_now| {
            registry.deactivate_team(&namespace, &name, version, db_now)
        })
    }

    fn team(&self, namespace: &str, name: &str) -> Option<Team> {
        let namespace = namespace.to_string();
        let name = name.to_string();
        self.read_registry(move |registry, _| Ok(registry.team(&namespace, &name)))
            .ok()
            .flatten()
    }

    fn teams(&self, namespace: &str) -> Vec<Team> {
        let namespace = namespace.to_string();
        self.read_registry(move |registry, _| Ok(registry.teams(&namespace)))
            .unwrap_or_default()
    }
}

impl AuditStore for PostgresBackend {
    fn audit(
        &self,
        _now: i64,
        action: &str,
        worker: &str,
        principal: &str,
        outcome: &str,
        detail: &str,
    ) {
        let action = action.to_string();
        let worker = worker.to_string();
        let principal = principal.to_string();
        let outcome = outcome.to_string();
        let detail = detail.to_string();
        let _ = self.mutate_registry(move |registry, db_now| {
            registry.audit(db_now, &action, &worker, &principal, &outcome, &detail);
            Ok(())
        });
    }

    fn audit_entries(&self, worker: Option<&str>) -> Vec<RegistryAuditEntry> {
        let worker = worker.map(str::to_string);
        self.bridge
            .call(move |pool| {
                Box::pin(async move {
                    let rows = if let Some(worker) = worker {
                        sqlx::query(
                            "SELECT (EXTRACT(EPOCH FROM at)*1000)::bigint AS at_ms, \
                                    action, worker, principal, outcome, detail \
                             FROM gateway_audit WHERE worker = $1 ORDER BY id",
                        )
                        .bind(worker)
                        .fetch_all(&pool)
                        .await
                    } else {
                        sqlx::query(
                            "SELECT (EXTRACT(EPOCH FROM at)*1000)::bigint AS at_ms, \
                                    action, worker, principal, outcome, detail \
                             FROM gateway_audit ORDER BY id",
                        )
                        .fetch_all(&pool)
                        .await
                    }
                    .map_err(db_error)?;
                    rows.into_iter()
                        .map(|row| {
                            Ok(RegistryAuditEntry {
                                at: row.try_get("at_ms").map_err(db_error)?,
                                action: row.try_get("action").map_err(db_error)?,
                                worker: row.try_get("worker").map_err(db_error)?,
                                principal: row.try_get("principal").map_err(db_error)?,
                                outcome: row.try_get("outcome").map_err(db_error)?,
                                detail: row.try_get("detail").map_err(db_error)?,
                                source: "gateway".into(),
                                policy_revision: 0,
                                correlation: String::new(),
                            })
                        })
                        .collect::<Result<Vec<_>, ApiError>>()
                })
            })
            .unwrap_or_default()
    }
}
