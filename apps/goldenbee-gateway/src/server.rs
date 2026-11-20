//! Listener lifecycle for the standalone Gateway.
//!
//! Preparing the control plane, binding the listener, and graceful shutdown
//! live here. Route ownership lives in the route modules.

use crate::config::Config;
use crate::host::HostManager;
use crate::router;
use crate::socket::SocketLimits;
use crate::state::{AppState, SharedState, TicketStore};
use axum::Router;
use common::{ApiError, GatewayHealth};
use gateway_core::{Caller, Gateway as CoreGateway, LocalHost};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Prepared standalone Gateway: control plane plus caller authentication.
pub struct Gateway {
    state: SharedState,
}

impl Gateway {
    /// Authenticate operator configuration, then prepare the control plane.
    pub async fn prepare(config: Config) -> Result<Self, ApiError> {
        let callers: BTreeMap<String, Caller> = config
            .callers
            .iter()
            .map(|caller| (caller.token.clone(), caller.caller()))
            .collect();
        let socket = SocketLimits::from(config.socket);
        let base_dir = config.core.base_dir.clone();
        let local_host = if config.local.enabled() {
            Some(Arc::new(HostManager::new(
                config.local.clone(),
                base_dir.as_deref(),
            )?))
        } else {
            None
        };
        let local_host_id = local_host.as_ref().map(|host| host.host_id().to_string());
        let local: Option<Arc<dyn LocalHost>> = local_host
            .as_ref()
            .map(|host| host.clone() as Arc<dyn LocalHost>);
        let core = CoreGateway::prepare_with_local_host(config.core, local).await?;
        Ok(Self {
            state: Arc::new(AppState {
                core,
                local_host,
                local_host_id,
                callers,
                tickets: std::sync::Mutex::new(TicketStore::new(common::SOCKET_MAX_TICKETS)),
                socket,
            }),
        })
    }

    /// Build the authenticated Gateway router.
    pub fn router(&self) -> Router {
        router::router(self.state.clone())
    }

    /// Current Gateway liveness and lease-derived reachability.
    pub fn health(&self) -> GatewayHealth {
        self.state.core.health()
    }

    /// Startup summary for operator output. Contains no credentials.
    pub fn worker_summary(&self) -> (usize, usize) {
        self.state.core.worker_summary()
    }

    /// Durable dispatch store path, for operator diagnostics.
    pub fn dispatch_path(&self) -> std::path::PathBuf {
        self.state.core.dispatch_path()
    }
}

/// Run the Gateway against an operator configuration until shutdown.
pub async fn serve(config: Config, addr: Option<String>) -> Result<(), ApiError> {
    let listen = addr.unwrap_or_else(|| config.listen.clone());
    let gateway = Gateway::prepare(config).await?;
    let (workers, reachable) = gateway.worker_summary();
    let listener = tokio::net::TcpListener::bind(&listen).await.map_err(|_| {
        ApiError::new("gateway.listen", "Gateway listener could not bind")
            .with("listen", listen.clone())
    })?;
    let bound = listener
        .local_addr()
        .map_err(|_| ApiError::new("gateway.listen", "Gateway address is unavailable"))?;
    println!("gateway listening on http://{bound}");
    println!("gateway approved {workers} Worker(s), {reachable} reachable");
    axum::serve(listener, gateway.router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|_| ApiError::new("gateway.serve", "Gateway server stopped unexpectedly"))
}

/// Stop on Ctrl-C or SIGTERM. The systemd units send SIGTERM so in-flight
/// requests can finish before `TimeoutStopSec` expires.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
