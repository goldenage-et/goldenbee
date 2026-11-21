//! Session continue, resume, fork, rename, and transcript export through the
//! real Worker HTTP seam.
//!
//! These tests observe external behavior only: canonical event history,
//! identity scoping, durable metadata across a process restart, and scope
//! enforcement. They do not reach into the session store's private schema.

use adk_rust::session::{AppendEventRequest, CreateRequest, Event, SessionService};
use adk_rust::{AdkIdentity, AppName, Content, Part, SessionId, UserId};
use common::{PackageManifest, Principal};
use runtime::native::FileSessionService;
use runtime::serving::{api_router, authenticate, WorkerState};
use runtime::{ExecutionOutcome, Executor, Runtime, RuntimeConfig};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const OWNER: &str = "owner-token";
const OTHER: &str = "other-principal-token";
const OTHER_NS: &str = "other-namespace-token";
const NO_SCOPE: &str = "no-scope-token";
const APP: &str = "goldenbee";

struct Noop;

#[async_trait::async_trait]
impl Executor for Noop {
    async fn execute(
        &self,
        _runtime: Runtime,
        _task: common::Task,
        _cancel: CancellationToken,
    ) -> ExecutionOutcome {
        ExecutionOutcome::success("ok")
    }

    async fn cancel(&self, _task_id: &str) -> bool {
        false
    }
}

fn config(directory: &Path) -> RuntimeConfig {
    let mut config = RuntimeConfig::local(directory);
    config.tokens.clear();
    config.tokens.insert(
        OWNER.into(),
        Principal::new("alice", "local", &["session:read", "session:write"]),
    );
    config.tokens.insert(
        OTHER.into(),
        Principal::new("bob", "local", &["session:read", "session:write"]),
    );
    config.tokens.insert(
        OTHER_NS.into(),
        Principal::new("alice", "other", &["session:read", "session:write"]),
    );
    config.tokens.insert(
        NO_SCOPE.into(),
        Principal::new("carol", "local", &["task:submit"]),
    );
    config
}

fn manifest() -> PackageManifest {
    PackageManifest {
        name: APP.into(),
        version: "0.1.0".into(),
        protocol: common::PROTOCOL_VERSION.into(),
        runtime: "0.1.0".into(),
        adk: "2.3.0".into(),
        capabilities: vec![common::CapabilityDescriptor {
            id: "general".into(),
            kind: "agent".into(),
            description: "session management test".into(),
            permissions: Vec::new(),
        }],
        required_permissions: Vec::new(),
        features: Vec::new(),
        launcher_modes: Vec::new(),
        hooks: Vec::new(),
    }
}

async fn open(directory: &Path) -> Runtime {
    let runtime = Runtime::open(config(directory))
        .await
        .expect("runtime opens");
    runtime.set_executor(Arc::new(Noop)).await;
    let sessions = Arc::new(
        FileSessionService::open(directory.join(runtime::native::SESSION_STORE_FILE))
            .expect("session store opens"),
    );
    runtime.set_sessions(sessions).await;
    runtime
}

async fn serve(runtime: Runtime) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let state = WorkerState::new(runtime, manifest());
    let app = authenticate(api_router(state.clone()), state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), handle)
}

fn event(text: &str, author: &str) -> Event {
    let mut event = Event::new("inv-1");
    event.author = author.into();
    let mut content = Content::new(author);
    content.parts.push(Part::Text { text: text.into() });
    event.llm_response.content = Some(content);
    event
}

async fn create(runtime: &Runtime, namespace: &str, principal: &str, session: &str, text: &str) {
    let sessions = runtime.sessions().await.expect("session store");
    let user = runtime::native::native_user_id(namespace, principal).expect("native user");
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: user.clone(),
            session_id: Some(session.into()),
            state: HashMap::new(),
        })
        .await
        .expect("session created");
    sessions
        .append_event_for_identity(AppendEventRequest {
            identity: AdkIdentity::new(
                AppName::try_from(APP).expect("app"),
                UserId::try_from(user).expect("user"),
                SessionId::try_from(session).expect("session"),
            ),
            event: event(text, "user"),
        })
        .await
        .expect("event appended");
}

async fn get(base: &str, token: &str, path: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request responds");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&body).unwrap_or_default())
}

async fn post(base: &str, token: &str, path: &str, body: &Value) -> (reqwest::StatusCode, Value) {
    let response = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .expect("request responds");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&body).unwrap_or_default())
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_one_session_store_upgrades_without_losing_history() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join(runtime::native::SESSION_STORE_FILE);
    {
        let sessions = FileSessionService::open(&path).expect("store opens");
        let user = runtime::native::native_user_id("local", "alice").expect("native user");
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: user.clone(),
                session_id: Some("legacy".into()),
                state: HashMap::new(),
            })
            .await
            .expect("session created");
        sessions
            .append_event_for_identity(AppendEventRequest {
                identity: AdkIdentity::new(
                    AppName::try_from(APP).expect("app"),
                    UserId::try_from(user).expect("user"),
                    SessionId::try_from("legacy").expect("session"),
                ),
                event: event("legacy-turn", "user"),
            })
            .await
            .expect("event appended");
    }
    // Downgrade the file to the schema-1 shape: no management metadata fields.
    let bytes = std::fs::read(&path).expect("store bytes");
    let mut value: Value = serde_json::from_slice(&bytes).expect("store JSON");
    value["schema"] = serde_json::json!(1);
    for session in value["sessions"]
        .as_object_mut()
        .expect("sessions")
        .values_mut()
    {
        let object = session.as_object_mut().expect("session object");
        object.remove("title");
        object.remove("created_at");
        object.remove("forked_from");
        object.remove("exports");
    }
    std::fs::write(&path, serde_json::to_vec(&value).expect("legacy JSON")).expect("legacy write");

    let sessions = FileSessionService::open(&path).expect("legacy store opens");
    let user = runtime::native::native_user_id("local", "alice").expect("native user");
    let details = sessions
        .details(APP, &user, "legacy")
        .await
        .expect("legacy history");
    assert_eq!(details.summary.event_count, 1);
    assert_eq!(details.summary.title, None);
    assert!(details.summary.created_at > 0);

    // The reopened store rewrites the current schema and keeps the event.
    let bytes = std::fs::read(&path).expect("upgraded bytes");
    let value: Value = serde_json::from_slice(&bytes).expect("upgraded JSON");
    assert_eq!(
        value["schema"].as_u64(),
        Some(u64::from(runtime::native::session::SESSION_STORE_SCHEMA))
    );
    assert_eq!(
        value["sessions"]
            .as_object()
            .expect("sessions")
            .values()
            .next()
            .expect("session")["events"]
            .as_array()
            .expect("events")
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_and_resume_preserve_history_and_enforce_identity() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(directory.path()).await;
    create(&runtime, "local", "alice", "owner-session", "alpha-secret").await;
    create(&runtime, "local", "alice", "older-session", "older-secret").await;
    let (base, server) = serve(runtime).await;

    let (status, latest) = get(&base, OWNER, "/v1/sessions/latest").await;
    assert!(status.is_success(), "{latest}");
    assert_eq!(latest["session_id"], "older-session");
    assert_eq!(latest["namespace"], "local");
    assert_eq!(latest["principal"], "alice");
    assert_eq!(latest["event_count"], 1);
    assert_eq!(latest["turns"][0]["text"], "older-secret");

    let (status, resumed) = get(&base, OWNER, "/v1/sessions/owner-session").await;
    assert!(status.is_success(), "{resumed}");
    assert_eq!(resumed["turns"][0]["text"], "alpha-secret");

    let (status, list) = get(&base, OWNER, "/v1/sessions").await;
    assert!(status.is_success(), "{list}");
    assert_eq!(list.as_array().expect("list").len(), 2);

    // A different principal and a different namespace never see this history.
    let (status, _) = get(&base, OTHER, "/v1/sessions/owner-session").await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    let (status, _) = get(&base, OTHER_NS, "/v1/sessions/owner-session").await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    let (status, list) = get(&base, OTHER, "/v1/sessions").await;
    assert!(status.is_success());
    assert!(list.as_array().expect("list").is_empty());
    let (status, _) = get(&base, OTHER, "/v1/sessions/latest").await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    // Reading and mutating sessions requires the declared scope.
    let (status, _) = get(&base, NO_SCOPE, "/v1/sessions/owner-session").await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    let (status, _) = post(
        &base,
        NO_SCOPE,
        "/v1/sessions/owner-session/fork",
        &serde_json::json!({"session_id": "no-scope-fork"}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_is_independent_and_rename_and_export_survive_restart() {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(directory.path()).await;
    create(&runtime, "local", "alice", "source", "original-turn").await;
    let (base, server) = serve(runtime.clone()).await;

    let (status, fork) = post(
        &base,
        OWNER,
        "/v1/sessions/source/fork",
        &serde_json::json!({"session_id": "branch", "title": "branch title"}),
    )
    .await;
    assert!(status.is_success(), "{fork}");
    assert_eq!(fork["session_id"], "branch");
    assert_eq!(fork["forked_from"], "source");
    assert_eq!(fork["title"], "branch title");
    assert_eq!(fork["event_count"], 1);
    assert_eq!(fork["turns"][0]["text"], "original-turn");

    // Appending to the fork must not change the source history.
    {
        let sessions = runtime.sessions().await.expect("session store");
        let user = runtime::native::native_user_id("local", "alice").expect("native user");
        sessions
            .append_event_for_identity(AppendEventRequest {
                identity: AdkIdentity::new(
                    AppName::try_from(APP).expect("app"),
                    UserId::try_from(user).expect("user"),
                    SessionId::try_from("branch").expect("session"),
                ),
                event: event("branch-only", "agent"),
            })
            .await
            .expect("fork event");
    }
    let (status, source) = get(&base, OWNER, "/v1/sessions/source").await;
    assert!(status.is_success(), "{source}");
    assert_eq!(source["event_count"], 1);
    assert_eq!(source["turns"][0]["text"], "original-turn");
    let (status, branch) = get(&base, OWNER, "/v1/sessions/branch").await;
    assert!(status.is_success(), "{branch}");
    assert_eq!(branch["event_count"], 2);

    let (status, renamed) = post(
        &base,
        OWNER,
        "/v1/sessions/branch/rename",
        &serde_json::json!({"title": "renamed branch"}),
    )
    .await;
    assert!(status.is_success(), "{renamed}");
    assert_eq!(renamed["title"], "renamed branch");

    let (status, exported) = post(
        &base,
        OWNER,
        "/v1/sessions/branch/transcript",
        &serde_json::json!({}),
    )
    .await;
    assert!(status.is_success(), "{exported}");
    let digest = exported["digest"].as_str().expect("digest").to_string();
    assert_eq!(exported["turn_count"], 2);
    assert_eq!(exported["turns"][1]["text"], "branch-only");

    // Restart: the same data directory must preserve title and export.
    server.abort();
    drop(runtime);
    let runtime = open(directory.path()).await;
    let (base, server) = serve(runtime).await;
    let (status, resumed) = get(&base, OWNER, "/v1/sessions/branch").await;
    assert!(status.is_success(), "{resumed}");
    assert_eq!(resumed["title"], "renamed branch");
    assert_eq!(resumed["forked_from"], "source");
    let (status, transcript) = get(&base, OWNER, "/v1/sessions/branch/transcript").await;
    assert!(status.is_success(), "{transcript}");
    assert_eq!(transcript["digest"], digest);
    assert_eq!(transcript["turns"][1]["text"], "branch-only");

    server.abort();
}
