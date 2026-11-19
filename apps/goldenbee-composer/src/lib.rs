//! Authorized host-local deployment composer for Goldenbee Worker instances.
//!
//! `goldenbee-composer` is the only process in this slice that performs privileged
//! host actions. The Gateway selects a configured host, a verified package
//! digest, an instance identity, and a profile; the composer resolves the
//! package inside its own cache, validates every host-local path and grant
//! against host policy, and uses the shared `instance` crate for provisioning,
//! upgrade, and rollback. The Gateway never reads Worker asset directories.

// Axum handlers return `Response` as the rejected variant, matching the
// Gateway composer and keeping the handler contract direct.
#![allow(clippy::result_large_err)]

mod composer;
mod config;
mod server;

pub use composer::Composer;
pub use config::{Config, HostCaller};

use common::ApiError;

/// Run the composer HTTP listener until the process is stopped.
pub async fn serve(config: Config, addr: Option<String>) -> Result<(), ApiError> {
    let composer = std::sync::Arc::new(Composer::new(config)?);
    let supported = composer.descriptor().supported.join(",");
    let listen = addr.unwrap_or_else(|| "127.0.0.1:9090".to_string());
    let listener = tokio::net::TcpListener::bind(&listen).await.map_err(|_| {
        ApiError::new("host.listen", "host composer listener could not bind")
            .with("listen", listen.clone())
    })?;
    let bound = listener
        .local_addr()
        .map_err(|_| ApiError::new("host.listen", "host composer address is unavailable"))?;
    println!("gb-composer listening on http://{bound}");
    println!("gb-composer supports {supported}");
    axum::serve(listener, server::router(composer))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|_| ApiError::new("host.serve", "host composer listener stopped unexpectedly"))
}

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
