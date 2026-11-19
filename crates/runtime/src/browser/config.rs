//! Operator-facing browser configuration and resolved limits.

use crate::browser::BrowserPolicy;
use common::ApiError;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Tool profile exposed to the model. `Full` still excludes
/// `browser_evaluate_js`; that opt-in is never enabled by the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserProfile {
    /// Navigation, interaction, extraction, waits, and screenshots.
    Minimal,
    /// Same categories as minimal, tuned for form filling.
    FormFilling,
    /// Navigation, extraction, and screenshots without interaction.
    Scraping,
    /// Every supported tool except model-written JavaScript.
    Full,
}

impl BrowserProfile {
    /// Resolve the pinned native profile.
    pub(crate) fn native(self) -> adk_browser::BrowserProfile {
        match self {
            Self::Minimal => adk_browser::BrowserProfile::Minimal,
            Self::FormFilling => adk_browser::BrowserProfile::FormFilling,
            Self::Scraping => adk_browser::BrowserProfile::Scraping,
            Self::Full => adk_browser::BrowserProfile::Full,
        }
    }
}

/// How the harness obtains a WebDriver endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DriverMode {
    /// Use `webdriver_url` exactly as supplied.
    #[default]
    External,
    /// Start one geckodriver process per browser session.
    Managed,
    /// Use `webdriver_url` unless it is a loopback Firefox endpoint and a
    /// geckodriver binary is available, in which case it is managed per session.
    Auto,
}

/// Resolved driver strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverPlan {
    /// Connect to the configured WebDriver endpoint.
    External,
    /// Start a per-session geckodriver process.
    Managed(PathBuf),
}

/// Raw `[browser]` configuration from the operator file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOptions {
    /// Whether this instance is allowed to start browser sessions.
    #[serde(default)]
    pub enabled: bool,
    /// WebDriver server endpoint.
    #[serde(default = "default_webdriver_url")]
    pub webdriver_url: String,
    /// Browser name understood by WebDriver (`firefox` or `chrome`).
    #[serde(default = "default_browser")]
    pub browser: String,
    /// Run the browser without a visible window.
    #[serde(default = "default_true")]
    pub headless: bool,
    /// Explicit destinations the browser may reach, as `host` or `host:port`.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Tools exposed to the model.
    #[serde(default = "default_profile")]
    pub profile: BrowserProfile,
    /// Maximum simultaneous browser sessions for this instance.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    /// Hard wall-clock lifetime of one browser session.
    #[serde(default = "default_session_timeout")]
    pub session_timeout_ms: u64,
    /// Idle lifetime before the runtime releases a browser session.
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_ms: u64,
    /// Maximum bytes of one tool result before it reaches the model or event stream.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Per-session download directory root. Each session gets its own child.
    #[serde(default = "default_downloads_dir")]
    pub downloads_dir: String,
    /// Require HTTPS outside loopback destinations.
    #[serde(default = "default_true")]
    pub require_https: bool,
    /// Driver process strategy.
    #[serde(default)]
    pub driver: DriverMode,
    /// Optional geckodriver path for managed mode.
    #[serde(default)]
    pub driver_binary: Option<String>,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            webdriver_url: default_webdriver_url(),
            browser: default_browser(),
            headless: true,
            allowed_hosts: Vec::new(),
            profile: default_profile(),
            max_sessions: default_max_sessions(),
            session_timeout_ms: default_session_timeout(),
            idle_timeout_ms: default_idle_timeout(),
            max_output_bytes: default_max_output_bytes(),
            downloads_dir: default_downloads_dir(),
            require_https: true,
            driver: DriverMode::External,
            driver_binary: None,
        }
    }
}

/// Validated browser settings owned by one Worker Instance.
#[derive(Debug, Clone)]
pub struct BrowserSettings {
    /// Whether this instance may start sessions.
    pub enabled: bool,
    /// WebDriver endpoint.
    pub webdriver_url: String,
    /// Resolved browser kind.
    pub browser: BrowserKind,
    /// Headless flag.
    pub headless: bool,
    /// Destination policy for navigation and egress.
    pub policy: BrowserPolicy,
    /// Tool profile.
    pub profile: BrowserProfile,
    /// Session count ceiling.
    pub max_sessions: usize,
    /// Hard lifetime.
    pub session_timeout: std::time::Duration,
    /// Idle lifetime.
    pub idle_timeout: std::time::Duration,
    /// Per-result output cap.
    pub max_output_bytes: usize,
    /// Resolved per-session download root.
    pub downloads_dir: PathBuf,
    /// How to obtain WebDriver for each session.
    pub driver: DriverPlan,
}

/// Browser kinds the harness supports through the pinned native integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserKind {
    /// Firefox through geckodriver.
    Firefox,
    /// Chrome through chromedriver.
    Chrome,
}

impl BrowserKind {
    pub(crate) fn native(self) -> adk_browser::BrowserType {
        match self {
            Self::Firefox => adk_browser::BrowserType::Firefox,
            Self::Chrome => adk_browser::BrowserType::Chrome,
        }
    }
}

impl BrowserOptions {
    /// Validate and resolve paths against the configuration directory.
    pub fn resolve(&self, config_dir: &Path) -> Result<BrowserSettings, ApiError> {
        if self.webdriver_url.trim().is_empty()
            || (!self.webdriver_url.starts_with("http://")
                && !self.webdriver_url.starts_with("https://"))
        {
            return Err(ApiError::new(
                "browser.config_invalid",
                "browser.webdriver_url must be an HTTP or HTTPS URL",
            ));
        }
        if self.max_sessions == 0 {
            return Err(ApiError::new(
                "browser.config_invalid",
                "browser.max_sessions must be at least 1",
            ));
        }
        if self.session_timeout_ms == 0 || self.idle_timeout_ms == 0 {
            return Err(ApiError::new(
                "browser.config_invalid",
                "browser session timeouts must be positive",
            ));
        }
        if self.max_output_bytes == 0 {
            return Err(ApiError::new(
                "browser.config_invalid",
                "browser.max_output_bytes must be positive",
            ));
        }
        if self.downloads_dir.trim().is_empty() {
            return Err(ApiError::new(
                "browser.config_invalid",
                "browser.downloads_dir must not be empty",
            ));
        }
        let browser = match self.browser.as_str() {
            "firefox" => BrowserKind::Firefox,
            "chrome" => BrowserKind::Chrome,
            other => {
                return Err(ApiError::new(
                    "browser.config_invalid",
                    format!("browser '{other}' is not supported"),
                ))
            }
        };
        let policy = if self.allowed_hosts.is_empty() && !self.enabled {
            BrowserPolicy::none()
        } else {
            BrowserPolicy::new(&self.allowed_hosts)?.require_https(self.require_https)
        };
        let downloads = PathBuf::from(&self.downloads_dir);
        let downloads_dir = if downloads.is_absolute() {
            downloads
        } else {
            config_dir.join(downloads)
        };
        let driver = match self.driver {
            DriverMode::External => DriverPlan::External,
            DriverMode::Managed => DriverPlan::Managed(
                crate::browser::driver::geckodriver_binary(self.driver_binary.as_deref())
                    .ok_or_else(|| {
                        ApiError::new(
                            "browser.unavailable",
                            "browser.driver = \"managed\" but geckodriver was not found",
                        )
                    })?,
            ),
            DriverMode::Auto => {
                let binary =
                    crate::browser::driver::geckodriver_binary(self.driver_binary.as_deref());
                match (browser, binary) {
                    (BrowserKind::Firefox, Some(binary))
                        if is_loopback_url(&self.webdriver_url) =>
                    {
                        DriverPlan::Managed(binary)
                    }
                    _ => DriverPlan::External,
                }
            }
        };
        Ok(BrowserSettings {
            enabled: self.enabled,
            webdriver_url: self.webdriver_url.clone(),
            browser,
            headless: self.headless,
            policy,
            profile: self.profile,
            max_sessions: self.max_sessions,
            session_timeout: std::time::Duration::from_millis(self.session_timeout_ms),
            idle_timeout: std::time::Duration::from_millis(self.idle_timeout_ms),
            max_output_bytes: self.max_output_bytes,
            downloads_dir,
            driver,
        })
    }
}

fn default_webdriver_url() -> String {
    "http://127.0.0.1:4444".into()
}

fn default_browser() -> String {
    "firefox".into()
}

fn default_true() -> bool {
    true
}

fn default_profile() -> BrowserProfile {
    BrowserProfile::Minimal
}

fn default_max_sessions() -> usize {
    2
}

fn default_session_timeout() -> u64 {
    300_000
}

fn default_idle_timeout() -> u64 {
    120_000
}

fn default_max_output_bytes() -> usize {
    262_144
}

fn default_downloads_dir() -> String {
    "browser-downloads".into()
}

fn is_loopback_url(value: &str) -> bool {
    value.starts_with("http://127.")
        || value.starts_with("http://localhost")
        || value.starts_with("http://[::1]")
}
