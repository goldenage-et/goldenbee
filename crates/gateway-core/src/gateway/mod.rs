//! Gateway control plane: registry routing, dispatch, projections, and streams.
//!
//! The control plane takes an already-authenticated [`crate::Caller`]. It never parses
//! an `Authorization` header, registers HTTP routes, starts a listener, or owns
//! business user, payment, or tenant policy. A deployment adapter turns its own
//! authenticated identity into a [`Caller`] and maps the returned types onto a
//! transport.

mod admin;
mod approval;
mod artifact;
mod cancel;
mod control;
mod diagnostics;
mod discovery;
mod lifecycle;
mod local;
mod profiles;
mod providers;
mod stream;
mod submit;
mod support;
mod task;
mod teams;
#[cfg(test)]
mod tests;
mod worker_client;
mod workers;

pub use self::local::LocalHost;
use crate::config::{CoreConfig, HostConfig, StoreConfig, WorkerConfig};
use crate::lifecycle::Lifecycle;
use crate::registry::{Approval, Clock, Registry};
use crate::store::{Control, DispatchStore, FileDispatchStore, ReplicaId, StreamMapStore};
use common::{ApiError, GatewayHealth};
use std::sync::Arc;
use std::time::Duration;

pub use self::artifact::{ArtifactContent, ArtifactStream};

use self::worker_client::probe;

struct State {
    secret: Vec<u8>,
    registry: Control,
    dispatch: Arc<dyn DispatchStore>,
    streams: Arc<dyn StreamMapStore>,
    replica: ReplicaId,
    claim_ttl_ms: i64,
    store_description: String,
    client: reqwest::Client,
    stream_client: reqwest::Client,
    lifecycle_client: reqwest::Client,
    stream_slots: Arc<tokio::sync::Semaphore>,
    hosts: Vec<HostConfig>,
    local_host: Option<Arc<dyn LocalHost>>,
    lifecycle: Lifecycle,
    vault: Option<Arc<crate::vault::Vault>>,
}

/// Prepared Gateway control plane with approved Worker preflight results.
pub struct Gateway {
    state: Arc<State>,
}

impl Gateway {
    /// Resolve configuration, then verify each approved Worker that has no
    /// persisted registry record.
    pub async fn prepare(config: CoreConfig) -> Result<Self, ApiError> {
        Self::prepare_with_local_host(config, None).await
    }

    /// Prepare with an optional in-process local host adapter.
    pub async fn prepare_with_local_host(
        config: CoreConfig,
        local_host: Option<Arc<dyn LocalHost>>,
    ) -> Result<Self, ApiError> {
        Self::prepare_with_clock_and_local_host(config, Arc::new(common::now_ms), local_host).await
    }

    /// Prepare with an injected Gateway-side clock for deterministic tests.
    pub async fn prepare_with_clock(config: CoreConfig, clock: Clock) -> Result<Self, ApiError> {
        Self::prepare_with_clock_and_local_host(config, clock, None).await
    }

    /// Prepare with an injected clock and an optional local host.
    pub async fn prepare_with_clock_and_local_host(
        config: CoreConfig,
        clock: Clock,
        local_host: Option<Arc<dyn LocalHost>>,
    ) -> Result<Self, ApiError> {
        config.validate()?;
        let secret = config.resolve_secret()?;
        let lease_ttl_ms = config.lease_ttl_ms();
        let approvals: Vec<Approval> = config.workers.iter().map(approval_of).collect();
        let store = config.store();
        let (registry, dispatch, streams, store_description) =
            select_store(&config, &store, lease_ttl_ms, &approvals, clock.clone()).await?;
        let replica = ReplicaId::resolve(config.replica_id.as_deref());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| ApiError::new("gateway.client", "HTTP client could not be built"))?;
        let stream_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| {
                ApiError::new("gateway.client", "HTTP stream client could not be built")
            })?;
        // Host lifecycle operations may legitimately run a verified upgrade
        // smoke task; only the connect attempt is bounded here.
        let lifecycle_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| {
                ApiError::new("gateway.client", "host composer client could not be built")
            })?;
        let lifecycle = Lifecycle::open(&config.data_dir())?;
        let vault = crate::vault::Vault::open(
            config.vault.as_ref(),
            config.base_dir.as_deref(),
            clock.clone(),
        )?
        .map(Arc::new);
        let known: Vec<String> = approvals
            .iter()
            .map(|approval| approval.id.clone())
            .collect();
        for id in known {
            if registry.worker(&id).is_some() {
                continue;
            }
            if config
                .workers
                .iter()
                .find(|worker| worker.id == id)
                .and_then(|worker| worker.preflight)
                == Some(false)
            {
                continue;
            }
            let Some(approval) = registry.approval_record(&id) else {
                continue;
            };
            let endpoint = approval.endpoint.clone();
            let expected_instance = approval.instance.clone();
            let expected_package = approval.package.clone();
            let now = registry.now();
            match probe(&client, &secret, &endpoint).await {
                Ok(verified)
                    if expected_instance
                        .as_deref()
                        .is_none_or(|expected| expected == verified.instance)
                        && expected_package
                            .as_deref()
                            .is_none_or(|expected| expected == verified.package) =>
                {
                    registry.bootstrap(&id, &verified, now)?;
                }
                Ok(_) => {
                    registry.mark_unreachable(
                        &id,
                        now,
                        "startup endpoint identity does not match the operator configuration",
                    )?;
                }
                Err(_) => {
                    registry.mark_unreachable(
                        &id,
                        now,
                        "startup endpoint verification did not succeed",
                    )?;
                }
            }
        }
        Ok(Self {
            state: Arc::new(State {
                secret,
                registry,
                dispatch,
                streams,
                replica,
                claim_ttl_ms: config.claim_ttl_ms(),
                store_description,
                client,
                stream_client,
                lifecycle_client,
                stream_slots: Arc::new(tokio::sync::Semaphore::new(config.max_streams.max(1))),
                hosts: config.hosts.clone(),
                local_host,
                lifecycle,
                vault,
            }),
        })
    }

    /// Current control-plane liveness and lease-derived reachability.
    pub fn health(&self) -> GatewayHealth {
        self.state.registry.expire(self.state.registry.now());
        let (workers, reachable) = self.state.registry.health();
        GatewayHealth {
            status: "live".into(),
            workers,
            reachable,
        }
    }

    /// Approved and reachable Worker counts for operator output.
    pub fn worker_summary(&self) -> (usize, usize) {
        let (approved, reachable) = self.state.registry.health();
        (approved as usize, reachable as usize)
    }

    /// Durable dispatch store path, for operator diagnostics. Empty when the
    /// selected store has no file path.
    pub fn dispatch_path(&self) -> std::path::PathBuf {
        self.state.dispatch.path().unwrap_or_default()
    }

    /// Redacted durable-store description for operator output.
    pub fn store_description(&self) -> String {
        self.state.store_description.clone()
    }

    /// Verify that the selected store is reachable and its schema is supported.
    pub async fn ready(&self) -> Result<(), ApiError> {
        self.state.dispatch.ready().await
    }
}

async fn select_store(
    config: &CoreConfig,
    store: &StoreConfig,
    lease_ttl_ms: i64,
    approvals: &[Approval],
    clock: Clock,
) -> Result<
    (
        Control,
        Arc<dyn DispatchStore>,
        Arc<dyn StreamMapStore>,
        String,
    ),
    ApiError,
> {
    if store.is_postgres() {
        #[cfg(feature = "postgres")]
        {
            let url = store.resolve_url()?;
            let backend =
                crate::store::PostgresBackend::connect(&url, store.max_connections, lease_ttl_ms)
                    .await?;
            let registry = Control::Shared(backend.clone());
            registry.seed_config(approvals.to_vec(), registry.now())?;
            registry.expire(registry.now());
            let dispatch: Arc<dyn DispatchStore> = backend.clone();
            let streams: Arc<dyn StreamMapStore> = backend;
            return Ok((registry, dispatch, streams, "postgres".into()));
        }
        #[cfg(not(feature = "postgres"))]
        {
            let _ = (config, lease_ttl_ms, approvals, clock);
            return Err(ApiError::new(
                "config.invalid",
                "this gateway-core build does not include the postgres store feature",
            ));
        }
    }
    let dispatch_store = Arc::new(FileDispatchStore::open(config.data_dir()).await?);
    let registry = Registry::open(
        config.registry_path(),
        clock,
        lease_ttl_ms,
        approvals.to_vec(),
    )?;
    let description = dispatch_store.description();
    let dispatch: Arc<dyn DispatchStore> = dispatch_store.clone();
    let streams: Arc<dyn StreamMapStore> = dispatch_store;
    Ok((Control::File(registry), dispatch, streams, description))
}

fn approval_of(worker: &WorkerConfig) -> Approval {
    Approval {
        id: worker.id.clone(),
        endpoint: worker.endpoint.trim_end_matches('/').to_string(),
        instance: worker.instance.clone(),
        package: worker.package.clone(),
        capabilities: worker.capabilities.clone(),
        control_token: worker.control_token.clone(),
    }
}
