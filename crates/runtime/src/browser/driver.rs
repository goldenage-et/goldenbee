//! Per-session WebDriver process for single-session drivers.
//!
//! geckodriver serves one browser session at a time. Isolation between two
//! native sessions therefore needs one driver process per session (or an
//! external multi-session Selenium server). This module owns the local
//! geckodriver child so browser process count and cleanup stay bounded.

use common::ApiError;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// A geckodriver child owned by one browser session.
pub struct ManagedDriver {
    child: Mutex<Option<Child>>,
    url: String,
}

impl ManagedDriver {
    /// Start geckodriver on a loopback ephemeral port.
    pub async fn start(binary: &Path, session_dir: &Path) -> Result<Self, ApiError> {
        let port = free_port()?;
        let child = Command::new(binary)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .current_dir(session_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                ApiError::new(
                    "browser.unavailable",
                    format!("geckodriver could not start: {error}"),
                )
            })?;
        let driver = Self {
            child: Mutex::new(Some(child)),
            url: format!("http://127.0.0.1:{port}"),
        };
        if driver.wait_ready(Duration::from_secs(15)).await {
            Ok(driver)
        } else {
            Err(ApiError::new(
                "browser.unavailable",
                "geckodriver did not become ready",
            ))
        }
    }

    /// WebDriver endpoint for the native browser session.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Process id, for cleanup evidence.
    pub fn pid(&self) -> Option<u32> {
        self.child
            .lock()
            .ok()
            .and_then(|child| child.as_ref().map(Child::id))
    }

    /// Stop the driver process.
    pub fn stop(&self) {
        let Ok(mut child) = self.child.lock() else {
            return;
        };
        if let Some(mut child) = child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    async fn wait_ready(&self, timeout: Duration) -> bool {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
        {
            Ok(client) => client,
            Err(_) => return false,
        };
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if let Ok(response) = client.get(format!("{}/status", self.url)).send().await {
                if response.status().is_success() {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }
}

impl Drop for ManagedDriver {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for ManagedDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedDriver")
            .field("url", &self.url)
            .finish()
    }
}

fn free_port() -> Result<u16, ApiError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| {
        ApiError::new(
            "browser.unavailable",
            format!("a loopback driver port is unavailable: {error}"),
        )
    })?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| {
            ApiError::new(
                "browser.unavailable",
                format!("a loopback driver port is unavailable: {error}"),
            )
        })
}

/// Locate geckodriver without guessing across the whole filesystem.
pub fn geckodriver_binary(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = configured {
        let path = PathBuf::from(path);
        return path.exists().then_some(path);
    }
    if let Some(path) = std::env::var_os("GOLDENBEE_GECKODRIVER") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    for candidate in [
        "/snap/bin/geckodriver",
        "/usr/bin/geckodriver",
        "/usr/local/bin/geckodriver",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join("geckodriver"))
            .find(|path| path.exists())
    })
}
