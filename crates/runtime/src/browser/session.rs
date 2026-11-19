//! Instance-owned browser sessions keyed by authenticated native session.
//!
//! A WebDriver session is the isolation unit: it has its own browser profile,
//! cookie jar, downloads directory, egress proxy, and WebDriver facade. Sessions
//! are bounded by count, hard lifetime, and idle lifetime. Worker shutdown,
//! release, expiry, and failed creation all quit the browser and stop the
//! adapters.

use crate::browser::control::ControlRegistry;
use crate::browser::driver::ManagedDriver;
use crate::browser::facade::{DriverSession, WebDriverFacade};
use crate::browser::proxy::EgressProxy;
use crate::browser::{BrowserKind, BrowserSettings, DriverPlan};
use adk_rust::ReadonlyContext;
use common::{
    ApiError, ControlAuditEntry, ControlCommand, ControlCommandRequest, ControlCommandResponse,
    ControlLease, ControlPage, ControlStatus,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// One isolated browser session.
pub struct SessionEntry {
    /// Stable session key, for diagnostics.
    pub key: SessionKey,
    /// Native pinned browser session.
    pub session: Arc<adk_browser::BrowserSession>,
    /// Per-session WebDriver facade.
    pub facade: WebDriverFacade,
    /// Per-session egress proxy.
    pub proxy: EgressProxy,
    /// Per-session download directory.
    pub downloads: PathBuf,
    /// Per-session geckodriver process, when the harness manages it.
    pub driver: Option<ManagedDriver>,
    created: Instant,
    last_used: std::sync::Mutex<Instant>,
}

impl SessionEntry {
    fn last_used(&self) -> Instant {
        *self.last_used.lock().expect("last-used lock")
    }

    fn touch(&self) {
        *self.last_used.lock().expect("last-used lock") = Instant::now();
    }

    /// Driver-reported profile and process identity.
    pub async fn driver_session(&self) -> DriverSession {
        self.facade.driver_session().await
    }
}

impl std::fmt::Debug for SessionEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionEntry")
            .field("key", &self.key)
            .field("downloads", &self.downloads)
            .finish()
    }
}

/// Authenticated session identity used as the isolation key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    /// Authenticated principal.
    pub user: String,
    /// Native session id.
    pub session: String,
}

/// One Worker Instance's browser capability.
pub struct BrowserController {
    id: String,
    settings: RwLock<BrowserSettings>,
    sessions: RwLock<HashMap<SessionKey, Arc<SessionEntry>>>,
    creating: Mutex<()>,
    control: ControlRegistry,
    cancel: CancellationToken,
    reaper: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl BrowserController {
    /// Create the instance-owned controller. Sessions start lazily.
    pub async fn start(id: String, settings: BrowserSettings) -> Arc<Self> {
        let controller = Arc::new(Self {
            id,
            settings: RwLock::new(settings),
            sessions: RwLock::new(HashMap::new()),
            creating: Mutex::new(()),
            control: ControlRegistry::new(),
            cancel: CancellationToken::new(),
            reaper: Mutex::new(None),
        });
        controller.spawn_reaper().await;
        controller
    }

    /// Registered toolset id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Current settings.
    pub async fn settings(&self) -> BrowserSettings {
        self.settings.read().await.clone()
    }

    /// Whether this instance may open browser sessions.
    pub async fn enabled(&self) -> bool {
        self.settings.read().await.enabled
    }

    /// Number of live entries (including a session whose browser has just died
    /// and has not been reaped yet).
    pub async fn active_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Subsystem status for readiness projections.
    pub async fn readiness(&self) -> &'static str {
        let settings = self.settings.read().await;
        if !settings.enabled {
            return "disabled";
        }
        match &settings.driver {
            DriverPlan::Managed(binary) => {
                if binary.exists() {
                    "ready"
                } else {
                    "unavailable"
                }
            }
            DriverPlan::External => {
                if probe(&settings.webdriver_url).await {
                    "ready"
                } else {
                    "unavailable"
                }
            }
        }
    }

    /// Replace settings after an operator reload. Existing sessions are closed
    /// when anything that determines their isolation or policy changed.
    pub async fn reconfigure(self: &Arc<Self>, settings: BrowserSettings) {
        let previous = self.settings.read().await.clone();
        let changed = previous.enabled != settings.enabled
            || previous.webdriver_url != settings.webdriver_url
            || previous.browser != settings.browser
            || previous.downloads_dir != settings.downloads_dir
            || previous.max_sessions != settings.max_sessions
            || previous.policy.destinations() != settings.policy.destinations();
        if changed || !settings.enabled {
            self.release_all().await;
        }
        let now_enabled = settings.enabled;
        *self.settings.write().await = settings;
        if now_enabled && self.reaper.lock().await.is_none() {
            self.spawn_reaper().await;
        }
    }

    /// Resolve or create the isolated session for this invocation context.
    pub async fn get_or_create(
        self: &Arc<Self>,
        ctx: &dyn ReadonlyContext,
    ) -> Result<Arc<SessionEntry>, ApiError> {
        let key = SessionKey {
            user: ctx.user_id().to_string(),
            session: ctx.session_id().to_string(),
        };
        self.get_or_create_key(key).await
    }

    /// Resolve or create a session for an explicit key.
    pub async fn get_or_create_key(
        self: &Arc<Self>,
        key: SessionKey,
    ) -> Result<Arc<SessionEntry>, ApiError> {
        let _creating = self.creating.lock().await;
        self.prune().await;
        {
            let sessions = self.sessions.read().await;
            if let Some(entry) = sessions.get(&key) {
                if !self.expired(entry) && entry.session.is_active().await {
                    entry.touch();
                    return Ok(entry.clone());
                }
            }
        }
        if let Some(entry) = self.sessions.write().await.remove(&key) {
            stop_entry(entry).await;
        }
        let settings = self.settings.read().await.clone();
        if !settings.enabled {
            return Err(ApiError::new(
                "browser.disabled",
                "browser capability is disabled for this Worker Instance",
            ));
        }
        if self.sessions.read().await.len() >= settings.max_sessions {
            return Err(ApiError::new(
                "browser.capacity",
                format!(
                    "browser session limit {} reached; release a session or raise browser.max_sessions",
                    settings.max_sessions
                ),
            ));
        }
        let entry = create_entry(&settings, key).await?;
        self.sessions
            .write()
            .await
            .insert(entry.key.clone(), entry.clone());
        Ok(entry)
    }

    /// Release one session and quit its browser.
    pub async fn release(&self, user: &str, session: &str) {
        let key = SessionKey {
            user: user.to_string(),
            session: session.to_string(),
        };
        let gate = self.control_gate(&key);
        let _guard = gate.lock().await;
        if let Some(entry) = self.sessions.write().await.remove(&key) {
            stop_entry(entry).await;
        }
        self.control.forget(&key);
    }

    /// Release every session.
    pub async fn release_all(&self) {
        let entries: Vec<_> = self
            .sessions
            .write()
            .await
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        for entry in &entries {
            self.control.forget(&entry.key);
        }
        futures::future::join_all(entries.into_iter().map(stop_entry)).await;
    }

    /// Per-session gate shared by agent commands, takeover, and human commands.
    pub fn control_gate(&self, key: &SessionKey) -> Arc<Mutex<()>> {
        self.control.gate(key)
    }

    /// Take exclusive human control of the exact authenticated session.
    pub async fn acquire_control(
        &self,
        key: SessionKey,
        principal: String,
        ttl_ms: Option<u64>,
    ) -> Result<ControlLease, ApiError> {
        if !self.enabled().await {
            return Err(ApiError::new(
                "browser.disabled",
                "browser control is disabled for this Worker Instance",
            ));
        }
        let gate = self.control_gate(&key);
        let _guard = gate.lock().await;
        self.control.acquire(&key, &principal, ttl_ms)
    }

    /// Explicitly return control to the Worker agent.
    pub async fn hand_back_control(
        &self,
        key: &SessionKey,
        principal: &str,
        epoch: &str,
        generation: u64,
    ) -> Result<ControlLease, ApiError> {
        let gate = self.control_gate(key);
        let _guard = gate.lock().await;
        self.control.hand_back(key, principal, epoch, generation)
    }

    /// Validate that a human command still owns the exact session.
    pub async fn verify_human_lease(
        &self,
        key: &SessionKey,
        principal: &str,
        epoch: &str,
        generation: u64,
    ) -> Result<(), ApiError> {
        self.control.verify(key, principal, epoch, generation)
    }

    /// Refuse an agent browser command while a human controller owns the session.
    ///
    /// The caller must hold [`control_gate`](Self::control_gate) across this
    /// check and the actual browser operation.
    pub fn begin_agent_command(&self, key: &SessionKey) -> Result<u64, ApiError> {
        self.control.begin_agent(key)
    }

    /// Current ownership projection.
    pub async fn control_status(&self, key: &SessionKey) -> Result<ControlStatus, ApiError> {
        match self.control.status(key) {
            Ok(status) => Ok(status),
            Err(error)
                if error.code == "control.session_not_found"
                    && self.sessions.read().await.contains_key(key) =>
            {
                Ok(ControlStatus {
                    session_id: key.session.clone(),
                    generation: 0,
                    owner: common::ControlOwner::Agent,
                    principal: None,
                    epoch: self.control.epoch().to_string(),
                    expires_at_ms: 0,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Redacted control decisions for one exact session.
    pub async fn control_audit(&self, key: &SessionKey) -> Vec<ControlAuditEntry> {
        self.control.audit(key)
    }

    /// Execute one fenced human command against the exact session.
    pub async fn control_command(
        self: &Arc<Self>,
        key: SessionKey,
        principal: &str,
        request: ControlCommandRequest,
    ) -> Result<ControlCommandResponse, ApiError> {
        let gate = self.control_gate(&key);
        let _guard = gate.lock().await;
        if let Err(error) = self
            .control
            .verify(&key, principal, &request.epoch, request.generation)
        {
            self.control.record_reject(&key, principal, &error);
            return Err(error);
        }
        let result = self
            .execute_control(&key, request.generation, &request.command)
            .await;
        match &result {
            Ok(response) => self.control.record(
                &key,
                principal,
                "command",
                request.generation,
                "accepted",
                &response.command,
            ),
            Err(error) => self.control.record_reject(&key, principal, error),
        }
        result
    }

    async fn execute_control(
        self: &Arc<Self>,
        key: &SessionKey,
        generation: u64,
        command: &ControlCommand,
    ) -> Result<ControlCommandResponse, ApiError> {
        let entry = self.get_or_create_key(key.clone()).await?;
        let settings = self.settings.read().await.clone();
        let mut output = String::new();
        match command {
            ControlCommand::Navigate { url } => {
                crate::browser::tools::check_navigation(&settings.policy, url)
                    .await
                    .map_err(|error| {
                        ApiError::new("browser.destination_denied", error.to_string())
                    })?;
                entry.session.navigate(url).await.map_err(command_error)?;
                let current = entry.session.current_url().await.unwrap_or_default();
                settings.policy.check_url(&current)?;
            }
            ControlCommand::Click { selector } => {
                entry.session.click(selector).await.map_err(command_error)?;
            }
            ControlCommand::Type { selector, text } => {
                entry
                    .session
                    .type_text(selector, text)
                    .await
                    .map_err(command_error)?;
            }
            ControlCommand::ExtractText { selector } => {
                output = entry
                    .session
                    .get_text(selector)
                    .await
                    .map_err(command_error)?;
            }
        }
        let context = entry.session.page_context().await.map_err(command_error)?;
        let page = ControlPage {
            url: context
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: context
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            text: truncate(
                context
                    .get("page_text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
                settings.max_output_bytes,
            ),
        };
        Ok(ControlCommandResponse {
            session_id: key.session.clone(),
            generation,
            command: command.kind().to_string(),
            output: truncate(&output, settings.max_output_bytes),
            page,
        })
    }

    /// Stop the reaper and every browser session.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(reaper) = self.reaper.lock().await.take() {
            reaper.abort();
        }
        self.release_all().await;
    }

    async fn spawn_reaper(self: &Arc<Self>) {
        let enabled = self.settings.read().await.enabled;
        if !enabled {
            return;
        }
        let weak = Arc::downgrade(self);
        let cancel = self.cancel.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {}
                }
                let Some(controller) = weak.upgrade() else {
                    break;
                };
                controller.prune().await;
            }
        });
        *self.reaper.lock().await = Some(handle);
    }

    async fn prune(&self) {
        let expired: Vec<SessionKey> = {
            let sessions = self.sessions.read().await;
            sessions
                .iter()
                .filter(|(_, entry)| self.expired(entry))
                .map(|(key, _)| key.clone())
                .collect()
        };
        let mut removed = Vec::new();
        {
            let mut sessions = self.sessions.write().await;
            for key in expired {
                if let Some(entry) = sessions.remove(&key) {
                    removed.push(entry);
                }
            }
        }
        futures::future::join_all(removed.into_iter().map(stop_entry)).await;
    }

    fn expired(&self, entry: &SessionEntry) -> bool {
        // Read the settings without awaiting: the check is cheap and the
        // reaper serialises expiry with stop. The lock is a std RwLock guard in
        // `settings`; use a cached snapshot instead to stay in async context.
        let (session_timeout, idle_timeout) = self.limits();
        let now = Instant::now();
        now.duration_since(entry.created) >= session_timeout
            || now.duration_since(entry.last_used()) >= idle_timeout
    }

    fn limits(&self) -> (Duration, Duration) {
        // `try_read` is enough: settings are replaced wholesale and a reload
        // that changes limits also releases existing sessions.
        let settings = self
            .settings
            .try_read()
            .map(|settings| (settings.session_timeout, settings.idle_timeout))
            .unwrap_or((Duration::from_secs(300), Duration::from_secs(120)));
        settings
    }
}

async fn create_entry(
    settings: &BrowserSettings,
    key: SessionKey,
) -> Result<Arc<SessionEntry>, ApiError> {
    let downloads = settings.downloads_dir.join(format!(
        "{}-{}",
        safe_component(&key.user),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&downloads).map_err(|error| {
        ApiError::new(
            "browser.sandbox_failed",
            format!("browser download directory could not be created: {error}"),
        )
    })?;
    let driver = match &settings.driver {
        DriverPlan::External => None,
        DriverPlan::Managed(binary) => match ManagedDriver::start(binary, &downloads).await {
            Ok(driver) => Some(driver),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&downloads);
                return Err(error);
            }
        },
    };
    let proxy = match EgressProxy::start(settings.policy.clone()).await {
        Ok(proxy) => proxy,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&downloads);
            return Err(error);
        }
    };
    let capabilities = session_capabilities(settings.browser, proxy.port(), &downloads);
    let upstream = driver
        .as_ref()
        .map(|driver| driver.url().to_string())
        .unwrap_or_else(|| settings.webdriver_url.clone());
    let facade = match WebDriverFacade::start(upstream, capabilities).await {
        Ok(facade) => facade,
        Err(error) => {
            proxy.shutdown();
            if let Some(driver) = driver {
                driver.stop();
            }
            let _ = std::fs::remove_dir_all(&downloads);
            return Err(error);
        }
    };
    let config = adk_browser::BrowserConfig::new()
        .webdriver_url(facade.url())
        .browser(settings.browser.native())
        .headless(settings.headless)
        .viewport(1280, 720)
        .page_load_timeout(30)
        .require_explicit_start(true);
    let session = Arc::new(adk_browser::BrowserSession::new(config));
    match tokio::time::timeout(Duration::from_secs(120), session.start()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            facade.shutdown();
            proxy.shutdown();
            if let Some(driver) = driver {
                driver.stop();
            }
            let _ = std::fs::remove_dir_all(&downloads);
            return Err(ApiError::new(
                "browser.unavailable",
                format!("browser session could not start: {error}"),
            ));
        }
        Err(error) => {
            facade.shutdown();
            proxy.shutdown();
            if let Some(driver) = driver {
                driver.stop();
            }
            let _ = std::fs::remove_dir_all(&downloads);
            return Err(ApiError::new(
                "browser.unavailable",
                format!("browser session did not start within the deadline: {error}"),
            ));
        }
    }
    Ok(Arc::new(SessionEntry {
        key,
        session,
        facade,
        proxy,
        downloads,
        driver,
        created: Instant::now(),
        last_used: std::sync::Mutex::new(Instant::now()),
    }))
}

async fn stop_entry(entry: Arc<SessionEntry>) {
    let _ = tokio::time::timeout(Duration::from_secs(10), entry.session.stop()).await;
    entry.facade.shutdown();
    entry.proxy.shutdown();
    if let Some(driver) = &entry.driver {
        driver.stop();
    }
    let _ = std::fs::remove_dir_all(&entry.downloads);
}

fn session_capabilities(
    browser: BrowserKind,
    proxy_port: u16,
    downloads: &Path,
) -> serde_json::Value {
    let download_dir = downloads.to_string_lossy().to_string();
    match browser {
        BrowserKind::Firefox => serde_json::json!({
            "moz:firefoxOptions": {
                "prefs": {
                    "network.proxy.type": 1,
                    "network.proxy.http": "127.0.0.1",
                    "network.proxy.http_port": proxy_port,
                    "network.proxy.ssl": "127.0.0.1",
                    "network.proxy.ssl_port": proxy_port,
                    "network.proxy.no_proxies_on": "",
                    "network.proxy.allow_hijacking_localhost": true,
                    "network.proxy.failover_direct": false,
                    "network.proxy.share_proxy_settings": true,
                    "browser.download.dir": download_dir,
                    "browser.download.folderList": 2,
                    "browser.download.useDownloadDir": true,
                    "browser.download.always_ask_before_handling_new_types": false,
                    "browser.helperApps.neverAsk.saveToDisk": "application/octet-stream,text/plain,text/csv",
                    "security.fileuri.strict_origin_policy": true
                }
            }
        }),
        BrowserKind::Chrome => serde_json::json!({
            "goog:chromeOptions": {
                "prefs": {
                    "download.default_directory": download_dir,
                    "download.prompt_for_download": false
                },
                "args": [
                    format!("--proxy-server=http://127.0.0.1:{proxy_port}"),
                    format!("--user-data-dir={}", downloads.join("profile").display())
                ]
            }
        }),
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

async fn probe(webdriver_url: &str) -> bool {
    let url = format!("{}/status", webdriver_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    matches!(client.get(url).send().await, Ok(response) if response.status().is_success())
}

fn command_error(error: adk_rust::AdkError) -> ApiError {
    ApiError::new(
        "browser.control_failed",
        format!("the controlled browser command failed: {error}"),
    )
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.saturating_sub(32).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &value[..end])
}
