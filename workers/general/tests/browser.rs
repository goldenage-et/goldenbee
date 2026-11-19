//! End-to-end browser isolation, policy, cleanup, and native-agent evidence.
//!
//! Tests that need WebDriver use the installed geckodriver/Firefox when
//! available (or `GOLDENBEE_WEBDRIVER`). Policy, discovery, and degradation
//! checks run without a driver.

use adk_rust::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, ToolContext,
    Toolset,
};
use async_trait::async_trait;
use common::{ControlCommand, ControlCommandRequest, ControlOwner};
use runtime::browser::{
    BrowserController, BrowserOptions, BrowserProfile, BrowserSettings, DriverMode,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod fixture;

// ── Deterministic local site ──────────────────────────────────────────

struct TestSite {
    address: String,
    requests: Arc<AtomicUsize>,
    clicks: Arc<AtomicUsize>,
}

impl TestSite {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test site binds");
        let address = listener.local_addr().expect("site address").to_string();
        let requests = Arc::new(AtomicUsize::new(0));
        let clicks = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let click_counter = clicks.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let counter = counter.clone();
                let click_counter = click_counter.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, &counter, &click_counter);
                });
            }
        });
        Self {
            address,
            requests,
            clicks,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }

    fn port(&self) -> u16 {
        self.address
            .rsplit_once(':')
            .expect("site address has port")
            .1
            .parse()
            .expect("site port")
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn clicks(&self) -> usize {
        self.clicks.load(Ordering::SeqCst)
    }
}

fn serve(
    mut stream: TcpStream,
    requests: &AtomicUsize,
    clicks: &AtomicUsize,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 2048];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 32 * 1024 {
            return Ok(());
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let cookie = lines
        .find_map(|line| line.strip_prefix("Cookie: "))
        .unwrap_or_default()
        .split(';')
        .find_map(|cookie| cookie.trim().strip_prefix("session="))
        .unwrap_or_default()
        .to_string();
    requests.fetch_add(1, Ordering::SeqCst);
    let headers = match path {
        "/" => response(
            200,
            "text/html",
            "",
            "<html><head><title>Goldenbee Browser Fixture</title></head><body><h1>Goldenbee Browser Fixture</h1><form action=\"/login\" method=\"get\"><label for=\"name\">Name</label><input id=\"name\" name=\"user\"><button id=\"go\" type=\"submit\">Sign in</button></form></body></html>",
        ),
        "/login" => {
            let user = query_value(query, "user").unwrap_or_else(|| "anonymous".into());
            response(
                302,
                "text/plain",
                &format!("Set-Cookie: session={user}; Path=/; HttpOnly\r\nLocation: /whoami\r\n"),
                "",
            )
        }
        "/whoami" => {
            let user = if cookie.is_empty() {
                "anonymous".to_string()
            } else {
                cookie
            };
            response(
                200,
                "text/html",
                "",
                &format!("<html><body><h1>Who am I</h1><p id=\"who\">who={user}</p></body></html>"),
            )
        }
        "/downloads" => response(
            200,
            "text/html",
            "",
            "<html><body><h1>Downloads</h1><a id=\"dl\" href=\"/download\" download=\"artifact.txt\">Download artifact</a></body></html>",
        ),
        "/download" => response(
            200,
            "application/octet-stream",
            "Content-Disposition: attachment; filename=\"artifact.txt\"\r\n",
            &format!("download:{}", cookie),
        ),
        "/redirect" => {
            let target = query_value(query, "to").unwrap_or_else(|| "/".into());
            response(
                302,
                "text/plain",
                &format!("Location: {target}\r\n"),
                "",
            )
        }
        "/counter" => response(
            200,
            "text/plain",
            "",
            &format!("counter={}", requests.load(Ordering::SeqCst)),
        ),
        "/mutation" => response(
            200,
            "text/html",
            "",
            &format!(
                "<html><head><title>Mutation fixture</title></head><body><h1>Mutation fixture</h1><a id=\"mutate\" href=\"/mutation/click\">mutate</a><p id=\"count\">count={}</p><input id=\"secret\"></body></html>",
                clicks.load(Ordering::SeqCst)
            ),
        ),
        "/mutation/click" => {
            let count = clicks.fetch_add(1, Ordering::SeqCst) + 1;
            response(
                200,
                "text/html",
                "",
                &format!(
                    "<html><head><title>Mutation fixture</title></head><body><h1>Mutation fixture</h1><p id=\"count\">count={count}</p></body></html>"
                ),
            )
        }
        "/big" => response(200, "text/plain", "", &"x".repeat(512 * 1024)),
        _ => response(404, "text/plain", "", "not found"),
    }?;
    stream.write_all(headers.as_bytes())?;
    stream.flush()
}

fn response(status: u16, content_type: &str, extra: &str, body: &str) -> std::io::Result<String> {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        _ => "Error",
    };
    Ok(format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
}

fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

// ── Driver ────────────────────────────────────────────────────────────

fn driver_available() -> bool {
    std::env::var("GOLDENBEE_WEBDRIVER")
        .map(|url| !url.trim().is_empty())
        .unwrap_or(false)
        || geckodriver_path().is_some()
}

fn geckodriver_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GOLDENBEE_GECKODRIVER") {
        return Some(PathBuf::from(path));
    }
    for candidate in [
        "/snap/bin/geckodriver",
        "/usr/bin/geckodriver",
        "/usr/local/bin/geckodriver",
    ] {
        if Path::new(candidate).exists() {
            return Some(PathBuf::from(candidate));
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join("geckodriver"))
            .find(|path| path.exists())
    })
}

const WEBDRIVER_PLACEHOLDER: &str = "http://127.0.0.1:4444";

/// Browser tests start real Firefox and geckodriver processes. Running many at
/// once exhausts the test runner's thread stacks and process budget, so the
/// WebDriver tests serialize while policy/unit tests stay parallel.
static BROWSER_LOCK: Mutex<()> = Mutex::new(());

fn browser_guard() -> std::sync::MutexGuard<'static, ()> {
    BROWSER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn browser_tempdir() -> tempfile::TempDir {
    // Snap-packaged Firefox can only write inside the real home directory.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    tempfile::Builder::new()
        .prefix("goldenbee-browser-")
        .tempdir_in(home)
        .expect("browser temporary directory")
}

// ── Controller and tool calls ─────────────────────────────────────────

fn settings(site: &TestSite, downloads: &Path, profile: BrowserProfile) -> BrowserSettings {
    BrowserOptions {
        enabled: true,
        webdriver_url: WEBDRIVER_PLACEHOLDER.into(),
        browser: "firefox".into(),
        headless: true,
        allowed_hosts: vec![format!("127.0.0.1:{}", site.port())],
        profile,
        max_sessions: 2,
        session_timeout_ms: 120_000,
        idle_timeout_ms: 60_000,
        max_output_bytes: 262_144,
        downloads_dir: downloads.display().to_string(),
        require_https: true,
        driver: DriverMode::Managed,
        driver_binary: None,
    }
    .resolve(Path::new("/"))
    .expect("browser settings resolve")
}

struct Ctx {
    user: String,
    session: String,
    content: Content,
    actions: Mutex<EventActions>,
    cancelled: AtomicBool,
}

impl Ctx {
    fn new(user: &str, session: &str) -> Arc<Self> {
        Arc::new(Self {
            user: user.into(),
            session: session.into(),
            content: Content::new("user").with_text("browser test"),
            actions: Mutex::new(EventActions::default()),
            cancelled: AtomicBool::new(false),
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        "invocation"
    }
    fn agent_name(&self) -> &str {
        "browser"
    }
    fn user_id(&self) -> &str {
        &self.user
    }
    fn app_name(&self) -> &str {
        "general"
    }
    fn session_id(&self) -> &str {
        &self.session
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_rust::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for Ctx {
    fn function_call_id(&self) -> &str {
        "call"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().expect("actions").clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().expect("actions") = actions;
    }
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

async fn call(toolset: Arc<dyn Toolset>, ctx: Arc<Ctx>, name: &str, args: Value) -> Result<Value> {
    let tools = toolset.tools(ctx.clone()).await?;
    let tool = tools
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("{name} is exposed"));
    tool.execute(ctx, args).await
}

fn text_of(value: &Value) -> String {
    value["text"].as_str().unwrap_or_default().to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[test]
fn denied_navigation_is_refused_before_a_browser_session_starts() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        let settings = BrowserOptions {
            enabled: true,
            webdriver_url: "http://127.0.0.1:1".into(),
            allowed_hosts: vec!["127.0.0.1:8080".into()],
            ..BrowserOptions::default()
        }
        .resolve(Path::new("/tmp"))
        .expect("settings");
        BrowserController::start("browser".into(), settings).await
    });
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let ctx = Ctx::new("alice", "session-a");
    for url in ["file:///etc/passwd", "http://127.0.0.1:9090/denied"] {
        let error = runtime
            .block_on(call(
                toolset.clone(),
                ctx.clone(),
                "browser_navigate",
                json!({ "url": url }),
            ))
            .expect_err("navigation is denied");
        assert!(error.to_string().contains("browser."), "{url}: {error}");
    }
    assert_eq!(runtime.block_on(controller.active_count()), 0);
    runtime.block_on(controller.shutdown());
}

#[test]
fn browser_is_degraded_when_webdriver_is_unreachable() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        let settings = BrowserOptions {
            enabled: true,
            webdriver_url: "http://127.0.0.1:1".into(),
            allowed_hosts: vec!["127.0.0.1:8080".into()],
            ..BrowserOptions::default()
        }
        .resolve(Path::new("/tmp"))
        .expect("settings");
        BrowserController::start("browser".into(), settings).await
    });
    assert_eq!(runtime.block_on(controller.readiness()), "unavailable");
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let ctx = Ctx::new("alice", "session-a");
    let error = runtime
        .block_on(call(
            toolset,
            ctx,
            "browser_navigate",
            json!({ "url": "http://127.0.0.1:8080/" }),
        ))
        .expect_err("an unavailable driver is explicit");
    assert!(error.to_string().contains("browser.unavailable"), "{error}");
    runtime.block_on(controller.shutdown());
}

#[test]
fn discovery_does_not_start_a_browser() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        let settings = BrowserOptions {
            enabled: true,
            webdriver_url: "http://127.0.0.1:1".into(),
            allowed_hosts: vec!["127.0.0.1:8080".into()],
            ..BrowserOptions::default()
        }
        .resolve(Path::new("/tmp"))
        .expect("settings");
        BrowserController::start("browser".into(), settings).await
    });
    let toolset = runtime::browser::BrowserCapabilityToolset::new(controller.clone());
    let ctx = Ctx::new("alice", "session-a");
    let tools = runtime
        .block_on(Toolset::tools(&toolset, ctx))
        .expect("discovery works without a driver");
    assert!(tools.iter().any(|tool| tool.name() == "browser_navigate"));
    assert!(!tools
        .iter()
        .any(|tool| tool.name() == "browser_evaluate_js"));
    assert_eq!(runtime.block_on(controller.active_count()), 0);
    runtime.block_on(controller.shutdown());
}

#[test]
fn sessions_isolate_cookies_and_page_effects() {
    if !driver_available() {
        eprintln!("SKIP browser cookies: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let downloads = browser_tempdir();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        BrowserController::start(
            "browser".into(),
            settings(&site, downloads.path(), BrowserProfile::Full),
        )
        .await
    });
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let alice = Ctx::new("alice", "session-a");
    let bob = Ctx::new("bob", "session-b");

    for (ctx, user) in [(&alice, "alice"), (&bob, "bob")] {
        runtime
            .block_on(call(
                toolset.clone(),
                ctx.clone(),
                "browser_navigate",
                json!({ "url": site.url(&format!("/login?user={user}")) }),
            ))
            .expect("login navigation");
        let page = runtime
            .block_on(call(
                toolset.clone(),
                ctx.clone(),
                "browser_extract_text",
                json!({ "selector": "#who" }),
            ))
            .expect("whoami text");
        assert!(text_of(&page).contains(&format!("who={user}")), "{page}");
        let cookies = runtime
            .block_on(call(
                toolset.clone(),
                ctx.clone(),
                "browser_get_cookies",
                json!({}),
            ))
            .expect("cookies");
        let names: Vec<_> = cookies["cookies"]
            .as_array()
            .expect("cookies array")
            .iter()
            .map(|cookie| cookie["value"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, vec![user.to_string()], "{user}: {cookies}");
    }

    let alice_entry = runtime
        .block_on(controller.get_or_create_key(runtime::browser::SessionKey {
            user: "alice".into(),
            session: "session-a".into(),
        }))
        .expect("alice entry");
    let bob_entry = runtime
        .block_on(controller.get_or_create_key(runtime::browser::SessionKey {
            user: "bob".into(),
            session: "session-b".into(),
        }))
        .expect("bob entry");
    let alice_driver = runtime.block_on(alice_entry.driver_session());
    let bob_driver = runtime.block_on(bob_entry.driver_session());
    assert!(
        alice_driver.profile.is_some() && bob_driver.profile.is_some(),
        "the driver reports browser profiles"
    );
    assert_ne!(alice_driver.profile, bob_driver.profile);
    assert_ne!(alice_entry.downloads, bob_entry.downloads);
    let page = runtime
        .block_on(call(
            toolset.clone(),
            alice.clone(),
            "browser_navigate",
            json!({ "url": site.url("/whoami") }),
        ))
        .expect("whoami");
    let text = page.to_string();
    assert!(
        !text.contains("dev-token"),
        "worker credential leaked into a page"
    );
    assert!(
        !text.contains("secret://"),
        "a credential reference leaked into a page"
    );

    // Closing alice's session must not expose or change bob's cookie.
    runtime.block_on(controller.release("alice", "session-a"));
    let bob_page = runtime
        .block_on(call(
            toolset,
            bob.clone(),
            "browser_extract_text",
            json!({ "selector": "#who" }),
        ))
        .expect("bob remains live");
    assert!(text_of(&bob_page).contains("who=bob"), "{bob_page}");
    runtime.block_on(controller.shutdown());
}

#[test]
fn downloads_are_written_to_the_session_directory() {
    if !driver_available() {
        eprintln!("SKIP browser downloads: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let downloads = browser_tempdir();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        BrowserController::start(
            "browser".into(),
            settings(&site, downloads.path(), BrowserProfile::Full),
        )
        .await
    });
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let alice = Ctx::new("alice", "session-a");
    runtime
        .block_on(call(
            toolset.clone(),
            alice.clone(),
            "browser_navigate",
            json!({ "url": site.url("/downloads") }),
        ))
        .expect("download page navigation");
    runtime
        .block_on(call(
            toolset,
            alice,
            "browser_click",
            json!({ "selector": "#dl" }),
        ))
        .expect("download link click");
    let entry_dir = wait_for_download(downloads.path());
    let file = first_file(&entry_dir);
    assert_eq!(
        std::fs::read_to_string(&file).expect("download content"),
        "download:"
    );
    runtime.block_on(controller.shutdown());
    assert!(
        !entry_dir.exists(),
        "session download directory is removed on shutdown"
    );
}

fn wait_for_download(root: &Path) -> PathBuf {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        for entry in std::fs::read_dir(root).expect("download root") {
            let entry = entry.expect("download entry");
            if entry.path().is_dir() {
                for file in std::fs::read_dir(entry.path()).expect("session downloads") {
                    let file = file.expect("download file");
                    if file.metadata().map(|meta| meta.len() > 0).unwrap_or(false) {
                        return entry.path();
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!("download root: {root:?}");
    for entry in std::fs::read_dir(root).expect("download root") {
        let entry = entry.expect("download entry");
        eprintln!("  {:?}", entry.path());
        if entry.path().is_dir() {
            for file in std::fs::read_dir(entry.path()).expect("session downloads") {
                let file = file.expect("download file");
                eprintln!(
                    "    {:?} {:?}",
                    file.path(),
                    file.metadata().map(|m| m.len())
                );
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    eprintln!(
        "home downloads: {}",
        Path::new(&home).join("Downloads").display()
    );
    panic!("download did not appear in {}", root.display());
}

fn first_file(directory: &Path) -> PathBuf {
    std::fs::read_dir(directory)
        .expect("directory")
        .map(|entry| entry.expect("entry").path())
        .find(|path| path.is_file())
        .expect("downloaded file")
}

#[test]
fn redirect_to_an_unlisted_destination_is_blocked_by_the_egress_proxy() {
    if !driver_available() {
        eprintln!("SKIP browser redirect: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let blocked = TestSite::start();
    let downloads = browser_tempdir();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        BrowserController::start(
            "browser".into(),
            settings(&site, downloads.path(), BrowserProfile::Full),
        )
        .await
    });
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let ctx = Ctx::new("alice", "session-a");
    let target = format!("http://127.0.0.1:{}/counter", blocked.port());
    let error = runtime
        .block_on(call(
            toolset,
            ctx,
            "browser_navigate",
            json!({ "url": site.url(&format!("/redirect?to={target}")) }),
        ))
        .expect_err("redirect is denied");
    assert!(
        error.to_string().contains("browser.redirect_denied"),
        "{error}"
    );
    assert_eq!(blocked.requests(), 0, "the denied host received a request");
    runtime.block_on(controller.shutdown());
}

#[test]
fn human_takeover_fences_delayed_agent_commands_and_records_page_effects() {
    if !driver_available() {
        eprintln!("SKIP browser takeover: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let downloads = browser_tempdir();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        BrowserController::start(
            "browser".into(),
            settings(&site, downloads.path(), BrowserProfile::Full),
        )
        .await
    });
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let alice = Ctx::new("alice", "session-a");

    runtime.block_on(async {
        call(
            toolset.clone(),
            alice.clone(),
            "browser_navigate",
            json!({ "url": site.url("/mutation") }),
        )
        .await
        .expect("agent opens the mutation page");
        let key = runtime::browser::SessionKey {
            user: "alice".into(),
            session: "session-a".into(),
        };

        // Queue a human takeover and a delayed agent click behind the same
        // gate. The takeover is admitted first, so the queued agent command
        // must be rejected after the new owner is announced.
        let gate = controller.control_gate(&key);
        let held = gate.lock().await;
        let takeover = {
            let controller = controller.clone();
            let key = key.clone();
            tokio::spawn(async move {
                controller
                    .acquire_control(key, "alice".into(), Some(60_000))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        let delayed = {
            let toolset = toolset.clone();
            let ctx = alice.clone();
            tokio::spawn(async move {
                call(
                    toolset,
                    ctx,
                    "browser_click",
                    json!({ "selector": "#mutate" }),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(held);
        let lease = takeover
            .await
            .expect("takeover task")
            .expect("human takes control");
        assert_eq!(lease.owner, ControlOwner::Human);
        let error = delayed
            .await
            .expect("delayed task")
            .expect_err("the stale agent click is rejected");
        assert!(
            error.to_string().contains("control.fenced"),
            "stale agent command: {error}"
        );
        assert_eq!(site.clicks(), 0, "the stale click never reached the page");

        let secret = "super-secret-input-value";
        controller
            .control_command(
                key.clone(),
                "alice",
                ControlCommandRequest {
                    session_id: "session-a".into(),
                    generation: lease.generation,
                    epoch: lease.epoch.clone(),
                    command: ControlCommand::Type {
                        selector: "#secret".into(),
                        text: secret.into(),
                    },
                },
            )
            .await
            .expect("human types into the exact owned page");
        let response = controller
            .control_command(
                key.clone(),
                "alice",
                ControlCommandRequest {
                    session_id: "session-a".into(),
                    generation: lease.generation,
                    epoch: lease.epoch.clone(),
                    command: ControlCommand::Click {
                        selector: "#mutate".into(),
                    },
                },
            )
            .await
            .expect("human clicks the page");
        assert_eq!(response.command, "click");
        assert_eq!(response.generation, lease.generation);
        assert_eq!(site.clicks(), 1, "only the human click mutates the page");
        assert!(
            response.page.text.contains("count=1"),
            "the actual page records the human mutation: {}",
            response.page.text
        );

        let hand_back = controller
            .hand_back_control(&key, "alice", &lease.epoch, lease.generation)
            .await
            .expect("explicit hand-back");
        assert_eq!(hand_back.owner, ControlOwner::Agent);
        let stale = controller
            .control_command(
                key.clone(),
                "alice",
                ControlCommandRequest {
                    session_id: "session-a".into(),
                    generation: lease.generation,
                    epoch: lease.epoch.clone(),
                    command: ControlCommand::Click {
                        selector: "#mutate".into(),
                    },
                },
            )
            .await
            .expect_err("the previous human generation is fenced");
        assert_eq!(stale.code, "control.fenced");
        assert_eq!(site.clicks(), 1, "no stale mutation after hand-back");

        let audit =
            serde_json::to_string(&controller.control_audit(&key).await).expect("audit serializes");
        assert!(!audit.contains(secret), "secret input leaked into audit");
        assert!(!audit.contains("#secret"), "selector leaked into audit");
        assert!(
            !audit.contains("?token="),
            "navigation URL leaked into audit"
        );

        // The agent resumes only after explicit hand-back.
        call(
            toolset,
            alice,
            "browser_extract_text",
            json!({ "selector": "#count" }),
        )
        .await
        .expect("the agent owner resumes after hand-back");
        controller.shutdown().await;
    });
}

#[test]
fn output_is_bounded_by_configuration() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        let mut settings = BrowserOptions {
            enabled: true,
            webdriver_url: "http://127.0.0.1:1".into(),
            allowed_hosts: vec!["127.0.0.1:8080".into()],
            ..BrowserOptions::default()
        }
        .resolve(Path::new("/tmp"))
        .expect("settings");
        settings.max_output_bytes = 128;
        BrowserController::start("browser".into(), settings).await
    });
    let toolset = runtime::browser::BrowserCapabilityToolset::new(controller.clone());
    let ctx = Ctx::new("alice", "session-a");
    // Discovery alone proves the guard is constructed with the configured cap;
    // the cap itself is exercised through the public tool result below.
    let tools = runtime
        .block_on(Toolset::tools(&toolset, ctx))
        .expect("tools");
    assert!(tools
        .iter()
        .any(|tool| tool.name() == "browser_extract_text"));
    runtime.block_on(controller.shutdown());
}

#[test]
fn shutdown_quits_the_browser_and_removes_session_state() {
    if !driver_available() {
        eprintln!("SKIP browser cleanup: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let downloads = browser_tempdir();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        BrowserController::start(
            "browser".into(),
            settings(&site, downloads.path(), BrowserProfile::Minimal),
        )
        .await
    });
    let entry = runtime
        .block_on(controller.get_or_create_key(runtime::browser::SessionKey {
            user: "alice".into(),
            session: "session-a".into(),
        }))
        .expect("session starts");
    let session = runtime.block_on(entry.driver_session());
    let profile = session.profile.clone().expect("driver reports a profile");
    let download_dir = entry.downloads.clone();
    runtime
        .block_on(call(
            Arc::new(runtime::browser::BrowserCapabilityToolset::new(
                controller.clone(),
            )) as Arc<dyn Toolset>,
            Ctx::new("alice", "session-a"),
            "browser_navigate",
            json!({ "url": site.url("/") }),
        ))
        .expect("navigate");
    runtime.block_on(controller.shutdown());
    assert_eq!(runtime.block_on(controller.active_count()), 0);
    assert!(!download_dir.exists(), "session download directory removed");
    assert!(
        !process_mentions(&profile),
        "no browser process still references {profile}"
    );
    if let Some(pid) = session.process_id {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "browser pid {pid} exited"
        );
    }
}

#[test]
fn large_tool_output_is_truncated_before_it_reaches_the_model() {
    if !driver_available() {
        eprintln!("SKIP browser output bound: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let downloads = browser_tempdir();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        let mut settings = settings(&site, downloads.path(), BrowserProfile::Minimal);
        settings.max_output_bytes = 4096;
        BrowserController::start("browser".into(), settings).await
    });
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let ctx = Ctx::new("alice", "session-a");
    runtime
        .block_on(call(
            toolset.clone(),
            ctx.clone(),
            "browser_navigate",
            json!({ "url": site.url("/big") }),
        ))
        .expect("navigate");
    let result = runtime
        .block_on(call(
            toolset,
            ctx,
            "browser_extract_text",
            json!({ "selector": "body" }),
        ))
        .expect("extract");
    let encoded = serde_json::to_string(&result).expect("JSON");
    assert!(
        encoded.len() <= 4096 + 1024,
        "tool output exceeded its bound: {} bytes",
        encoded.len()
    );
    assert_eq!(result["truncated"], true);
    runtime.block_on(controller.shutdown());
}

fn process_mentions(needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        if name.to_string_lossy().parse::<u32>().is_err() {
            return false;
        }
        std::fs::read(entry.path().join("cmdline"))
            .map(|cmdline| String::from_utf8_lossy(&cmdline).contains(needle))
            .unwrap_or(false)
    })
}

#[test]
fn capacity_and_lifetime_bounds_are_enforced() {
    if !driver_available() {
        eprintln!("SKIP browser capacity: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let downloads = browser_tempdir();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        let mut settings = settings(&site, downloads.path(), BrowserProfile::Minimal);
        settings.max_sessions = 1;
        settings.idle_timeout = Duration::from_millis(400);
        settings.session_timeout = Duration::from_secs(30);
        BrowserController::start("browser".into(), settings).await
    });
    let first = runtime
        .block_on(controller.get_or_create_key(runtime::browser::SessionKey {
            user: "alice".into(),
            session: "session-a".into(),
        }))
        .expect("first session");
    let profile = runtime
        .block_on(first.driver_session())
        .profile
        .expect("driver profile");
    let error = runtime
        .block_on(controller.get_or_create_key(runtime::browser::SessionKey {
            user: "bob".into(),
            session: "session-b".into(),
        }))
        .expect_err("session count is bounded");
    assert_eq!(error.code, "browser.capacity");

    std::thread::sleep(Duration::from_millis(1400));
    assert_eq!(runtime.block_on(controller.active_count()), 0);
    assert!(!process_mentions(&profile), "the idle browser was quit");
    runtime.block_on(controller.shutdown());
}

#[test]
fn a_cancelled_task_does_not_start_a_browser() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let controller = runtime.block_on(async {
        let settings = BrowserOptions {
            enabled: true,
            webdriver_url: "http://127.0.0.1:1".into(),
            allowed_hosts: vec!["127.0.0.1:8080".into()],
            ..BrowserOptions::default()
        }
        .resolve(Path::new("/tmp"))
        .expect("settings");
        BrowserController::start("browser".into(), settings).await
    });
    let toolset = Arc::new(runtime::browser::BrowserCapabilityToolset::new(
        controller.clone(),
    )) as Arc<dyn Toolset>;
    let ctx = Ctx::new("alice", "session-a");
    ctx.cancel();
    let error = runtime
        .block_on(call(
            toolset,
            ctx,
            "browser_navigate",
            json!({ "url": "http://127.0.0.1:8080/" }),
        ))
        .expect_err("cancelled call is refused");
    assert!(error.to_string().contains("browser.cancelled"), "{error}");
    assert_eq!(runtime.block_on(controller.active_count()), 0);
    runtime.block_on(controller.shutdown());
}

// ── Worker process and native agent ───────────────────────────────────

struct WorkerProcess {
    child: Child,
    base: String,
    token: String,
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_worker(fixture: &fixture::Fixture) -> WorkerProcess {
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "serve",
            "--config",
            fixture.config.to_str().expect("config path"),
            "--addr",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker serves");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("listener address is announced");
    let base = line
        .split("listening on ")
        .nth(1)
        .expect("address announcement")
        .trim()
        .to_string();
    WorkerProcess {
        child,
        base,
        token: "dev-token".into(),
    }
}

impl WorkerProcess {
    fn submit(&self, input: &str, key: &str) -> Value {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/v1/tasks", self.base))
            .bearer_auth(&self.token)
            .json(&json!({
                "agent": "general",
                "session_id": format!("session-{key}"),
                "input": input,
                "idempotency_key": key,
            }))
            .send()
            .expect("submit responds");
        let status = response.status();
        let body = response.text().expect("submit body");
        assert!(status.is_success(), "submit failed: {status} {body}");
        serde_json::from_str(&body).expect("submit JSON")
    }

    fn wait(&self, task_id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let task: Value = reqwest::blocking::Client::new()
                .get(format!("{}/v1/tasks/{task_id}", self.base))
                .bearer_auth(&self.token)
                .send()
                .expect("task responds")
                .json()
                .expect("task JSON");
            if matches!(
                task["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            ) {
                return task;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("task did not finish");
    }

    fn run(&self, input: &str, key: &str) -> Value {
        let submitted = self.submit(input, key);
        self.wait(submitted["task_id"].as_str().expect("task id"))
    }

    fn capabilities(&self) -> Value {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/capabilities", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("capabilities respond")
            .json()
            .expect("capabilities JSON")
    }

    fn status(&self) -> Value {
        reqwest::blocking::Client::new()
            .get(format!("{}/v1/status", self.base))
            .bearer_auth(&self.token)
            .send()
            .expect("status responds")
            .json()
            .expect("status JSON")
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

fn enable_browser(fixture: &fixture::Fixture, site: &TestSite) {
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config.push_str(&format!(
        "\n[browser]\nenabled = true\nwebdriver_url = \"{}\"\nbrowser = \"firefox\"\ndriver = \"managed\"\nprofile = \"minimal\"\nallowed_hosts = [\"127.0.0.1:{}\"]\nmax_sessions = 2\nsession_timeout_ms = 120000\nidle_timeout_ms = 60000\nmax_output_bytes = 262144\ndownloads_dir = \"browser-downloads\"\n",
        WEBDRIVER_PLACEHOLDER,
        site.port()
    ));
    std::fs::write(&fixture.config, config).expect("browser config");
    for agent in ["general", "echo"] {
        let path = fixture.agents.join(format!("{agent}.toml"));
        let mut definition = std::fs::read_to_string(&path).expect("agent definition");
        definition.push_str("\ntoolsets = [\"browser\"]\n");
        std::fs::write(path, definition).expect("agent toolset");
    }
}

#[test]
fn native_agent_navigates_and_interacts_with_the_local_site() {
    if !driver_available() {
        eprintln!("SKIP browser agent: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-browser".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    enable_browser(&fixture, &site);
    let worker = spawn_worker(&fixture);
    let input = format!(
        "browser-flow:{}",
        json!({
            "url": site.url("/"),
            "selector": "#name",
            "text": "alice",
            "button": "#go",
            "extract": "body"
        })
    );
    let task = worker.run(&input, "browser-agent");
    assert_eq!(task["state"], "succeeded", "{task}");
    let output = task["output"].as_str().unwrap_or_default();
    assert!(output.contains("who=alice"), "{output}");
}

#[test]
fn control_routes_require_the_control_scope_and_an_installed_browser() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-control-scope".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: vec![("viewer-token".into(), "local".into(), "viewer".into())],
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let worker = spawn_worker(&fixture);
    let client = reqwest::blocking::Client::new();
    let revoked = client
        .post(format!("{}/v1/sessions/session-a/control", worker.base))
        .bearer_auth("viewer-token")
        .json(&json!({ "session_id": "session-a" }))
        .send()
        .expect("revoked request responds");
    assert_eq!(revoked.status(), reqwest::StatusCode::FORBIDDEN);
    let body: Value = revoked.json().expect("typed error");
    assert_eq!(body["code"], "scope.denied");

    let installed = client
        .post(format!("{}/v1/sessions/session-a/control", worker.base))
        .bearer_auth("dev-token")
        .json(&json!({ "session_id": "session-a" }))
        .send()
        .expect("authorized request responds");
    assert_eq!(installed.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = installed.json().expect("typed error");
    assert_eq!(body["code"], "browser.disabled");
}

#[test]
fn capabilities_advertise_browser_only_when_enabled_and_declared() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-browser-caps".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let worker = spawn_worker(&fixture);
    let capabilities = worker.capabilities();
    assert!(
        capabilities["browser"].is_null(),
        "a disabled browser is not advertised: {capabilities}"
    );
    assert!(capabilities["installed_capabilities"]
        .as_array()
        .expect("installed")
        .iter()
        .any(|entry| entry["id"] == "browser"));
    let status = worker.status();
    assert_eq!(status["subsystems"]["browser"], "disabled");
    drop(worker);

    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-browser-unavailable".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config.push_str("\n[browser]\nenabled = true\nwebdriver_url = \"http://127.0.0.1:1\"\ndriver = \"external\"\nallowed_hosts = [\"example.com\"]\n");
    std::fs::write(&fixture.config, config).expect("config");
    let worker = spawn_worker(&fixture);
    let capabilities = worker.capabilities();
    assert_eq!(capabilities["browser"]["enabled"], true);
    assert_eq!(capabilities["browser"]["ready"], "unavailable");
    assert_eq!(capabilities["browser"]["streaming"], false);
    assert_eq!(capabilities["browser"]["takeover"], true);
    assert_eq!(
        capabilities["browser"]["control_protocol"],
        "fenced-lease-v1"
    );
    assert_eq!(capabilities["browser"]["terminal_control"], false);
    assert_eq!(capabilities["browser"]["evaluate_js"], false);
    let status = worker.status();
    assert_eq!(status["subsystems"]["browser"], "unavailable");
}

#[test]
fn an_unavailable_browser_does_not_block_a_non_browser_task() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-browser-degraded".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config.push_str("\n[browser]\nenabled = true\nwebdriver_url = \"http://127.0.0.1:1\"\ndriver = \"external\"\nallowed_hosts = [\"example.com\"]\n");
    std::fs::write(&fixture.config, config).expect("config");
    let definition = std::fs::read_to_string(fixture.agents.join("general.toml")).expect("agent");
    std::fs::write(
        fixture.agents.join("general.toml"),
        format!("{definition}\ntoolsets = [\"browser\"]\n"),
    )
    .expect("agent");
    let worker = spawn_worker(&fixture);
    assert_eq!(worker.status()["subsystems"]["browser"], "unavailable");
    let task = worker.run("hello without a browser", "non-browser");
    assert_eq!(task["state"], "succeeded", "{task}");
    assert!(task["output"]
        .as_str()
        .unwrap_or_default()
        .contains("hello without a browser"));
}

#[test]
fn a_profile_cannot_enable_a_browser_capability_by_name() {
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-browser-profile".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let output = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "profile",
            "create",
            "browser-profile",
            "--config",
            fixture.config.to_str().expect("config"),
            "--instruction",
            "use a browser",
            "--capability",
            "browser",
        ])
        .output()
        .expect("profile command runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("profile.capability_unavailable"),
        "{stderr}"
    );
}

#[test]
fn worker_shutdown_releases_browser_processes_and_session_state() {
    if !driver_available() {
        eprintln!("SKIP browser shutdown: geckodriver is not installed");
        return;
    }
    let _browser = browser_guard();
    let site = TestSite::start();
    let data = tempfile::tempdir().expect("data");
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-browser-shutdown".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    enable_browser(&fixture, &site);
    let worker = spawn_worker(&fixture);
    let input = format!(
        "browser-flow:{}",
        json!({
            "url": site.url("/"),
            "selector": "#name",
            "text": "alice",
            "button": "#go",
            "extract": "body"
        })
    );
    let task = worker.run(&input, "browser-shutdown");
    assert_eq!(task["state"], "succeeded", "{task}");
    let downloads = data.path().join("browser-downloads");
    assert!(
        std::fs::read_dir(&downloads)
            .expect("browser downloads")
            .next()
            .is_some(),
        "a session directory exists before shutdown"
    );
    let pid = worker.pid();
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("signal worker");
    assert!(status.success());
    let mut worker = worker;
    for _ in 0..200 {
        if let Ok(Some(_)) = worker.child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    worker.child.wait().ok();
    let remaining: Vec<_> = std::fs::read_dir(&downloads)
        .expect("browser downloads")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert!(remaining.is_empty(), "{remaining:?}");
}
