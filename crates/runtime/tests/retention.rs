//! Ticket 45: owner-separated retention and authorized erasure.
//!
//! These tests drive public runtime APIs. They observe each authoritative store
//! through its own retrieval path and never assert a universal deletion table.

use adk_artifact::{ArtifactService, FileArtifactService, SaveRequest};
use adk_rust::session::SessionService;
use common::{Approval, Artifact, ArtifactGrant, Limits, Principal, Task, TaskState, Usage};
use runtime::native::{NativeKnowledgeGraph, NativeMemory};
use std::collections::BTreeMap;
use std::sync::Arc;

fn principal(id: &str, namespace: &str) -> Principal {
    Principal::new(id, namespace, &["*"])
}

fn old_ms() -> i64 {
    common::now_ms() - 3 * 86_400_000
}

async fn open_runtime(dir: &std::path::Path, policy: runtime::RetentionPolicy) -> runtime::Runtime {
    let mut config = runtime::RuntimeConfig::local(dir);
    config.retention = policy;
    config.event_retention = 100;
    runtime::Runtime::open(config).await.expect("runtime opens")
}

fn retention_all_one() -> runtime::RetentionPolicy {
    runtime::RetentionPolicy {
        tasks_days: 1,
        sessions_days: 1,
        checkpoints_days: 1,
        events_days: 1,
        artifacts_days: 1,
        memory_days: 1,
        documents_days: 1,
        audit_days: 1,
        traces_days: 1,
    }
}

async fn make_task(
    rt: &runtime::Runtime,
    owner: &Principal,
    session: &str,
    key: &str,
    state: TaskState,
    updated_at: i64,
) -> String {
    let id = format!("task_{}", uuid::Uuid::new_v4().simple());
    let task = Task {
        id: id.clone(),
        namespace: owner.namespace.clone(),
        caller: owner.id.clone(),
        agent: "echo".into(),
        session_id: session.into(),
        input: "secret task input".into(),
        input_images: Vec::new(),
        idempotency_key: key.into(),
        request_digest: format!("digest-{key}"),
        profile: "default".into(),
        profile_revision: 1,
        asset_revision: 1,
        trace_id: "0123456789abcdef0123456789abcdef".into(),
        traffic: "local".into(),
        resolved_model: None,
        team: None,
        team_version: None,
        team_roster: None,
        scopes: Vec::new(),
        delegation_path: Vec::new(),
        authority_expires_at: None,
        state,
        attempt: 1,
        native_invocation_id: None,
        native_thread_id: None,
        native_a2a_task_id: None,
        output: None,
        evidence: None,
        error: None,
        uncertainty: None,
        limits: Limits::default(),
        usage: Usage::default(),
        created_at: common::now_ms(),
        updated_at,
    };
    rt.store()
        .write(|store| {
            store.tasks.insert(id.clone(), task);
            Ok(())
        })
        .await
        .expect("task projection is inserted");
    id
}

async fn make_artifact(
    rt: &runtime::Runtime,
    owner: &Principal,
    task_id: &str,
    created_at: i64,
) -> (String, runtime::ArtifactScope) {
    let user_id =
        runtime::native::native_user_id(&owner.namespace, &owner.id).expect("native user identity");
    let scope = runtime::ArtifactScope {
        app_name: "general".into(),
        user_id: user_id.clone(),
        session_id: format!("session-{task_id}"),
        file_name: "evidence.txt".into(),
    };
    let service = FileArtifactService::new(rt.config().data_dir.join("native-artifacts"))
        .expect("native artifact store opens");
    let saved = service
        .save(SaveRequest {
            app_name: scope.app_name.clone(),
            user_id: scope.user_id.clone(),
            session_id: scope.session_id.clone(),
            file_name: scope.file_name.clone(),
            part: adk_rust::Part::Text {
                text: "artifact secret".into(),
            },
            version: None,
        })
        .await
        .expect("native artifact saved");
    let id = format!("art_{}", uuid::Uuid::new_v4().simple());
    let artifact = Artifact {
        id: id.clone(),
        task_id: task_id.into(),
        native_invocation_id: format!("inv-{task_id}"),
        attempt: 1,
        trace_id: "0123456789abcdef0123456789abcdef".into(),
        version: saved.version,
        namespace: owner.namespace.clone(),
        owner: owner.id.clone(),
        name: scope.file_name.clone(),
        content_type: "text/plain".into(),
        size: "artifact secret".len() as u64,
        digest: common::bytes_digest(b"artifact secret"),
        finalized: true,
        created_at,
    };
    rt.store()
        .write(|store| {
            store.artifacts.insert(
                id.clone(),
                runtime::ArtifactRecord {
                    artifact,
                    scope: scope.clone(),
                },
            );
            Ok(())
        })
        .await
        .expect("artifact metadata is stored");
    (id, scope)
}

async fn create_session(rt: &runtime::Runtime, owner: &Principal, session_id: &str) {
    let sessions = rt.sessions().await.expect("sessions are composed");
    sessions
        .create(adk_rust::session::CreateRequest {
            app_name: "general".into(),
            user_id: runtime::native::native_user_id(&owner.namespace, &owner.id)
                .expect("native user identity"),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await
        .expect("native session created");
}

async fn age_session(rt: &runtime::Runtime, session_id: &str, at: i64) {
    let sessions = rt.sessions().await.expect("sessions are composed");
    let mut event = adk_rust::Event::new("aging-invocation");
    event.timestamp =
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at).expect("valid timestamp");
    sessions
        .append_event(session_id, event)
        .await
        .expect("session history is aged");
}

#[tokio::test]
async fn retention_uses_separate_windows_and_preserves_unfinished_recovery() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let rt = open_runtime(directory.path(), retention_all_one()).await;
    let sessions = Arc::new(
        runtime::native::FileSessionService::open(directory.path().join("native-sessions.json"))
            .expect("session store opens"),
    );
    rt.set_sessions(sessions.clone()).await;
    let memory = NativeMemory::open("general", directory.path())
        .await
        .expect("memory opens");
    rt.set_memory(Some(memory.clone())).await;
    let alice = principal("alice", "local");
    let old = old_ms();

    // Conversations: one old terminal-task session and one unfinished session.
    create_session(&rt, &alice, "session-old").await;
    create_session(&rt, &alice, "session-unfinished").await;
    age_session(&rt, "session-old", old).await;

    let old_task = make_task(
        &rt,
        &alice,
        "session-old",
        "ret-old",
        TaskState::Succeeded,
        old,
    )
    .await;
    let recent = make_task(
        &rt,
        &alice,
        "session-recent",
        "ret-recent",
        TaskState::Succeeded,
        common::now_ms(),
    )
    .await;
    let unfinished = make_task(
        &rt,
        &alice,
        "session-unfinished",
        "ret-live",
        TaskState::Running,
        old,
    )
    .await;
    rt.append_event(
        &old_task,
        "task.running",
        serde_json::json!({"secret": "old-task-content"}),
        true,
    )
    .await
    .expect("old event");
    for index in 0..3 {
        rt.append_event(
            &recent,
            "task.progress",
            serde_json::json!({"secret": format!("recent-{index}")}),
            true,
        )
        .await
        .expect("recent event");
    }
    rt.append_event(
        &unfinished,
        "task.running",
        serde_json::json!({"secret": "live-task-content"}),
        true,
    )
    .await
    .expect("live event");
    rt.store()
        .write(|store| {
            for event in store.events.get_mut(&old_task).into_iter().flatten() {
                event.at = old;
            }
            if let Some(events) = store.events.get_mut(&recent) {
                if let Some(first) = events.first_mut() {
                    first.at = old;
                }
                if let Some(second) = events.get_mut(1) {
                    second.at = old;
                }
            }
            for event in store.events.get_mut(&unfinished).into_iter().flatten() {
                event.at = old;
            }
            Ok(())
        })
        .await
        .expect("events are aged");

    // An old artifact without a grant is deleted; one with a live grant is
    // retained. An artifact referenced by unfinished evidence is retained.
    let (deleted_artifact, deleted_scope) = make_artifact(&rt, &alice, &old_task, old).await;
    let (shared_artifact, _) = make_artifact(&rt, &alice, &old_task, old).await;
    let (evidence_artifact, _) = make_artifact(&rt, &alice, &unfinished, old).await;
    rt.store()
        .write(|store| {
            store.grants.insert(
                "grant-live".into(),
                ArtifactGrant {
                    artifact_id: shared_artifact.clone(),
                    grant: "grant-live-secret".into(),
                    expires_at: common::now_ms() + 3_600_000,
                },
            );
            let task = store.tasks.get_mut(&unfinished).expect("task exists");
            task.evidence = Some(serde_json::json!({
                "artifact_ids": [evidence_artifact.clone()]
            }));
            Ok(())
        })
        .await
        .expect("artifact share and evidence configured");

    // Ingested source and derived memory/graph fact.
    let document = rt
        .ingest_document(
            &alice,
            "local",
            "retention source",
            "source://retention",
            "alpha document secret\n\nsecond paragraph",
            Vec::new(),
        )
        .await
        .expect("document ingested");
    rt.store()
        .write(|store| {
            store
                .documents
                .get_mut(&document.id)
                .expect("document exists")
                .created_at = old;
            Ok(())
        })
        .await
        .expect("document is aged");
    let graph = NativeKnowledgeGraph::open("general", directory.path())
        .await
        .expect("graph opens");
    rt.set_graph(Some(graph.clone())).await;
    rt.observe(
        &alice,
        "local",
        "Ada",
        "works_at",
        "Acme",
        "source://retention",
        &document.id,
    )
    .await
    .expect("derived graph fact");
    // Age native memory and graph rows through their canonical stores.
    let memory_pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", memory.path().display()))
        .await
        .expect("memory pool");
    sqlx::query("UPDATE memory_entries SET timestamp=?1")
        .bind(
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(old)
                .unwrap()
                .to_rfc3339(),
        )
        .execute(&memory_pool)
        .await
        .expect("memory aged");
    let graph_pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", graph.path().display()))
        .await
        .expect("graph pool");
    let old_stamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(old)
        .unwrap()
        .to_rfc3339();
    for table in ["kg_observations", "kg_relations"] {
        sqlx::query(&format!("UPDATE {table} SET valid_from=?1"))
            .bind(&old_stamp)
            .execute(&graph_pool)
            .await
            .expect("graph fact aged");
    }
    sqlx::query("UPDATE kg_provenance SET recorded_at=?1")
        .bind(&old_stamp)
        .execute(&graph_pool)
        .await
        .expect("provenance aged");
    sqlx::query("UPDATE kg_entities SET updated_at=?1")
        .bind(&old_stamp)
        .execute(&graph_pool)
        .await
        .expect("entities aged");

    // Audit and telemetry.
    rt.audit("alice", "task.submit", &old_task, "accepted", "old audit")
        .await
        .expect("audit");
    rt.trace("trace-old", &old_task, "run", 1, BTreeMap::new())
        .await
        .expect("trace");
    rt.store()
        .write(|store| {
            if let Some(record) = store
                .audit
                .iter_mut()
                .find(|record| record.detail == "old audit")
            {
                record.at = old;
            }
            if let Some(trace) = store
                .traces
                .iter_mut()
                .find(|trace| trace.trace_id == "trace-old")
            {
                trace.at = old;
            }
            Ok(())
        })
        .await
        .expect("audit and trace aged");

    let report = rt
        .apply_retention(&principal("admin", "local"))
        .await
        .expect("retention applies");
    assert!(report.completed, "blocked: {:?}", report.blocked);
    assert!(!report.retained.is_empty());

    // Separate owners observed independently.
    assert!(rt
        .sessions()
        .await
        .unwrap()
        .list(adk_rust::session::ListRequest {
            app_name: "general".into(),
            user_id: runtime::native::native_user_id("local", "alice").unwrap(),
            limit: None,
            offset: None,
        })
        .await
        .unwrap()
        .iter()
        .all(|session| session.id() != "session-old"));
    let store = rt.store().read(|state| state.clone()).await;
    assert!(!store.tasks.contains_key(&old_task));
    assert!(store.tasks.contains_key(&unfinished));
    assert_eq!(
        store.tasks.get(&unfinished).unwrap().state,
        TaskState::Running
    );
    assert!(!store.events.contains_key(&old_task));
    assert!(store.events.contains_key(&unfinished));
    assert!(store.events.contains_key(&recent));
    let page = rt
        .events_since(&recent, 1, 10)
        .await
        .expect("retained event page");
    assert!(
        page.cursor_expired,
        "trimmed replay history must report cursor expiry: {page:?}"
    );
    assert!(report.cursor_expiry > 0);
    assert!(!store.artifacts.contains_key(&deleted_artifact));
    assert!(store.artifacts.contains_key(&shared_artifact));
    assert!(store.artifacts.contains_key(&evidence_artifact));
    assert!(!store.documents.contains_key(&document.id));
    assert!(store
        .audit
        .iter()
        .all(|record| record.detail != "old audit"));
    assert!(!store
        .traces
        .iter()
        .any(|trace| trace.trace_id == "trace-old"));
    assert!(report.retained.iter().any(|obligation| {
        obligation.id == shared_artifact && obligation.reason == "outstanding-grant"
    }));
    assert!(report.retained.iter().any(|obligation| {
        obligation.id == evidence_artifact && obligation.reason == "unfinished-task-evidence"
    }));
    assert!(report.retained.iter().any(|obligation| {
        obligation.id == unfinished && obligation.reason == "unfinished-task"
    }));

    // Native artifact bytes and derivative stores observe the same decisions.
    assert!(rt
        .artifact_store()
        .payload(
            &deleted_scope.app_name,
            &deleted_scope.user_id,
            &deleted_scope.session_id,
            &deleted_scope.file_name,
            1,
        )
        .await
        .is_err());
    assert!(rt
        .search_memory(&alice, "local", "alpha", 10)
        .await
        .expect("memory retrieves")
        .is_empty());
    assert!(rt
        .search_documents(&alice, "local", "alpha")
        .await
        .expect("documents retrieve")
        .is_empty());
    assert!(rt
        .search_knowledge(&alice, "local", "Ada", 10)
        .await
        .expect("knowledge retrieves")
        .is_empty());
}

#[tokio::test]
async fn authorized_erasure_denies_foreign_namespace_and_reports_obligations() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let rt = open_runtime(directory.path(), runtime::RetentionPolicy::default()).await;
    let sessions = Arc::new(
        runtime::native::FileSessionService::open(directory.path().join("native-sessions.json"))
            .expect("session store opens"),
    );
    rt.set_sessions(sessions.clone()).await;
    let memory = NativeMemory::open("general", directory.path())
        .await
        .expect("memory opens");
    rt.set_memory(Some(memory.clone())).await;
    let graph = NativeKnowledgeGraph::open("general", directory.path())
        .await
        .expect("graph opens");
    rt.set_graph(Some(graph.clone())).await;

    let alice = principal("alice", "local");
    let bob = principal("bob", "local");
    let admin = principal("admin", "local");
    let foreign_admin = principal("foreign-admin", "other");
    create_session(&rt, &alice, "alice-session").await;
    create_session(&rt, &bob, "bob-session").await;

    let alice_task = make_task(
        &rt,
        &alice,
        "alice-session",
        "erase-done",
        TaskState::Succeeded,
        common::now_ms(),
    )
    .await;
    let unfinished = make_task(
        &rt,
        &alice,
        "alice-session",
        "erase-live",
        TaskState::Running,
        common::now_ms(),
    )
    .await;
    rt.append_event(
        &unfinished,
        "task.running",
        serde_json::json!({"secret": "alice live content"}),
        true,
    )
    .await
    .expect("event");
    let bob_task = make_task(
        &rt,
        &bob,
        "bob-session",
        "bob-done",
        TaskState::Succeeded,
        common::now_ms(),
    )
    .await;

    let (alice_artifact, alice_scope) =
        make_artifact(&rt, &alice, &alice_task, common::now_ms()).await;
    let (bob_artifact, _) = make_artifact(&rt, &bob, &bob_task, common::now_ms()).await;
    rt.store()
        .write(|store| {
            store.grants.insert(
                "alice-grant".into(),
                ArtifactGrant {
                    artifact_id: alice_artifact.clone(),
                    grant: "alice-grant-value".into(),
                    expires_at: common::now_ms() + 60_000,
                },
            );
            let task = store.tasks.get_mut(&unfinished).expect("task exists");
            task.evidence = Some(serde_json::json!({"digest": "evidence-kept"}));
            store.approvals.insert(
                "approval-alice".into(),
                Approval {
                    id: "approval-alice".into(),
                    task_id: unfinished.clone(),
                    namespace: "local".into(),
                    operation: "exec_command".into(),
                    target: "alice target".into(),
                    digest: "alice-digest".into(),
                    risk: "alice risk".into(),
                    requested_by: "alice".into(),
                    expires_at: common::now_ms() + 60_000,
                    status: "pending".into(),
                    action: None,
                    mode: None,
                    rule: None,
                    decided_by: None,
                    decided_at: None,
                    receipt: Some("alice receipt".into()),
                    answer: None,
                    created_at: common::now_ms(),
                },
            );
            store.approvals.insert(
                "approval-bob".into(),
                Approval {
                    id: "approval-bob".into(),
                    task_id: bob_task.clone(),
                    namespace: "local".into(),
                    operation: "exec_command".into(),
                    target: "bob target".into(),
                    digest: "bob-digest".into(),
                    risk: "bob risk".into(),
                    requested_by: "bob".into(),
                    expires_at: common::now_ms() + 60_000,
                    status: "pending".into(),
                    action: None,
                    mode: None,
                    rule: None,
                    decided_by: None,
                    decided_at: None,
                    receipt: None,
                    answer: None,
                    created_at: common::now_ms(),
                },
            );
            Ok(())
        })
        .await
        .expect("grant and evidence");

    rt.ingest_document(
        &alice,
        "local",
        "alice source",
        "source://alice",
        "alice private document",
        Vec::new(),
    )
    .await
    .expect("alice document");
    let alice_doc = rt
        .store()
        .read(|state| {
            state
                .documents
                .values()
                .find(|document| document.owner == "alice")
                .map(|document| document.id.clone())
        })
        .await
        .expect("alice document id");
    rt.observe(
        &alice,
        "local",
        "Alice",
        "owns",
        "Secret",
        "source://alice",
        &alice_doc,
    )
    .await
    .expect("alice graph fact");
    rt.ingest_document(
        &bob,
        "local",
        "bob source",
        "source://bob",
        "bob document",
        Vec::new(),
    )
    .await
    .expect("bob document");
    rt.audit(
        "alice",
        "task.submit",
        &alice_task,
        "accepted",
        "alice audit secret",
    )
    .await
    .expect("audit");

    // A principal in another namespace cannot address or inspect local alice.
    let denied = rt
        .search_memory(&foreign_admin, "local", "alice", 10)
        .await
        .expect_err("foreign namespace cannot inspect local data");
    assert_eq!(denied.code, "scope.namespace_denied");
    let unauthorized = Principal::new("eve", "local", &["task:read"]);
    let rejected = rt
        .erase_principal(&unauthorized, "alice")
        .await
        .expect_err("a caller without admin cannot erase another principal");
    assert_eq!(rejected.code, "scope.denied");
    let foreign = rt
        .erase_principal(&foreign_admin, "alice")
        .await
        .expect("foreign namespace erase only reaches its own namespace");
    assert!(foreign.completed, "{:?}", foreign.blocked);
    assert_eq!(
        rt.search_memory(&alice, "local", "alice", 10)
            .await
            .expect("warm memory")
            .len(),
        1,
        "foreign erase removed local alice memory"
    );
    assert_eq!(
        rt.search_documents(&alice, "local", "private")
            .await
            .expect("warm docs")
            .len(),
        1,
        "foreign erase removed local alice documents"
    );

    let report = rt
        .erase_principal(&admin, "alice")
        .await
        .expect("authorized erasure");
    assert!(report.completed, "blocked: {:?}", report.blocked);
    assert!(report
        .retained
        .iter()
        .any(|obligation| obligation.class == "task"
            && obligation.id == unfinished
            && obligation.reason == "unfinished-task"));

    assert!(rt
        .search_memory(&alice, "local", "private", 10)
        .await
        .expect("memory retrieves")
        .is_empty());
    assert!(rt
        .search_memory(&alice, "local", "bob", 10)
        .await
        .expect("memory retrieves")
        .is_empty());
    assert!(rt
        .search_documents(&alice, "local", "private")
        .await
        .expect("documents retrieve")
        .is_empty());
    assert!(rt
        .search_knowledge(&alice, "local", "Alice", 10)
        .await
        .expect("knowledge retrieves")
        .is_empty());
    assert!(rt.artifact(&alice, &alice_artifact).await.is_err());
    assert!(rt
        .sessions()
        .await
        .unwrap()
        .get(adk_rust::session::GetRequest {
            app_name: "general".into(),
            user_id: runtime::native::native_user_id("local", "alice").unwrap(),
            session_id: "alice-session".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .is_err());
    assert!(rt
        .artifact_store()
        .payload(
            &alice_scope.app_name,
            &alice_scope.user_id,
            &alice_scope.session_id,
            &alice_scope.file_name,
            1,
        )
        .await
        .is_err());
    let store = rt.store().read(|state| state.clone()).await;
    assert!(!store.tasks.contains_key(&alice_task));
    let live = store
        .tasks
        .get(&unfinished)
        .expect("unfinished task retained");
    assert_eq!(live.input, "[erased]");
    assert_eq!(
        live.evidence,
        Some(serde_json::json!({"digest": "evidence-kept"}))
    );
    assert!(store
        .events
        .get(&unfinished)
        .unwrap()
        .iter()
        .all(|event| event.payload == serde_json::json!({"erased": true})));
    assert!(store.artifacts.contains_key(&bob_artifact));
    assert!(store
        .documents
        .values()
        .any(|document| document.owner == "bob"));
    assert!(store.approvals.contains_key("approval-bob"));
    let alice_approval = store
        .approvals
        .get("approval-alice")
        .expect("unfinished approval retained");
    assert_eq!(alice_approval.target, "[erased]");
    assert_eq!(alice_approval.receipt, None);
    assert!(!store.grants.contains_key("alice-grant"));
    assert!(store
        .audit
        .iter()
        .any(|record| record.action == "principal.erase" && record.target == "alice"));
    assert!(!store
        .audit
        .iter()
        .any(|record| record.detail.contains("alice audit secret")));
    assert!(
        rt.search_memory(&bob, "local", "bob", 10)
            .await
            .expect("bob memory")
            .len()
            == 1
    );

    // Repeating the same erasure is idempotent and does not fail on absent
    // native rows.
    let second = rt
        .erase_principal(&admin, "alice")
        .await
        .expect("idempotent erasure");
    assert!(second.completed, "blocked: {:?}", second.blocked);
}

#[cfg(feature = "graph")]
#[tokio::test]
async fn retention_prunes_terminal_checkpoints_but_keeps_unfinished_recovery() {
    use adk_rust::graph::{Checkpointer, MemoryCheckpointer};

    let directory = tempfile::tempdir().expect("temporary directory");
    let policy = runtime::RetentionPolicy {
        tasks_days: 0,
        sessions_days: 0,
        checkpoints_days: 1,
        events_days: 0,
        artifacts_days: 0,
        memory_days: 0,
        documents_days: 0,
        audit_days: 0,
        traces_days: 0,
    };
    let rt = open_runtime(directory.path(), policy).await;
    let alice = principal("alice", "local");
    let native = Arc::new(MemoryCheckpointer::new());
    let checkpointer = Arc::new(runtime::native::DefinitionCheckpointer::new(native.clone()));
    rt.set_graph_checkpointers(
        [("graph".to_string(), checkpointer.clone())]
            .into_iter()
            .collect(),
    )
    .await;

    let terminal = make_task(
        &rt,
        &alice,
        "session-terminal",
        "checkpoint-terminal",
        TaskState::Succeeded,
        old_ms(),
    )
    .await;
    let unfinished = make_task(
        &rt,
        &alice,
        "session-live",
        "checkpoint-live",
        TaskState::Running,
        old_ms(),
    )
    .await;
    rt.store()
        .write(|store| {
            store.tasks.get_mut(&terminal).unwrap().native_thread_id =
                Some("thread-terminal".into());
            store.tasks.get_mut(&unfinished).unwrap().native_thread_id = Some("thread-live".into());
            Ok(())
        })
        .await
        .expect("thread ids");
    for thread in ["thread-terminal", "thread-live"] {
        native
            .save(&adk_rust::graph::Checkpoint::new(
                thread,
                adk_rust::graph::State::new(),
                1,
                vec!["pending".into()],
            ))
            .await
            .expect("checkpoint saved");
    }

    let report = rt
        .apply_retention(&principal("admin", "local"))
        .await
        .expect("retention applies");
    assert!(report.completed, "{:?}", report.blocked);
    assert!(
        checkpointer
            .latest("thread-live")
            .await
            .expect("live checkpoint read")
            .is_some(),
        "unfinished recovery checkpoint was deleted"
    );
    assert!(
        checkpointer
            .latest("thread-terminal")
            .await
            .expect("terminal checkpoint read")
            .is_none(),
        "terminal checkpoint was retained"
    );
    assert!(report.retained.iter().any(|obligation| {
        obligation.class == "checkpoint"
            && obligation.id == "thread-live"
            && obligation.reason == "unfinished-task"
    }));
}

#[tokio::test]
async fn interrupted_cleanup_resumes_from_durable_progress() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut config = runtime::RuntimeConfig::local(directory.path());
    config.retention = runtime::RetentionPolicy::default();
    let rt = runtime::Runtime::open(config).await.expect("runtime opens");
    let memory = NativeMemory::open("general", directory.path())
        .await
        .expect("memory opens");
    rt.set_memory(Some(memory)).await;
    let graph = NativeKnowledgeGraph::open("general", directory.path())
        .await
        .expect("graph opens");
    rt.set_graph(Some(graph)).await;
    let alice = principal("alice", "local");
    rt.remember(&alice, "local", "source://resume", "resume secret")
        .await
        .expect("memory");
    let admin = principal("admin", "local");

    // The session owner is not composed yet: erasure reports that store as
    // blocked and leaves a durable job behind rather than claiming success.
    let blocked = rt
        .erase_principal(&admin, "alice")
        .await
        .expect("partial cleanup reports");
    assert!(!blocked.completed);
    assert!(blocked
        .blocked
        .iter()
        .any(|step| step.store == "sessions" && step.code == "erasure.store_not_composed"));
    drop(rt);

    // A restarted process attaches the session owner and resumes the same job.
    let rt = runtime::Runtime::open(runtime::RuntimeConfig::local(directory.path()))
        .await
        .expect("runtime reopens");
    let memory = NativeMemory::open("general", directory.path())
        .await
        .expect("memory reopens");
    rt.set_memory(Some(memory)).await;
    let graph = NativeKnowledgeGraph::open("general", directory.path())
        .await
        .expect("graph reopens");
    rt.set_graph(Some(graph)).await;
    rt.set_sessions(Arc::new(
        runtime::native::FileSessionService::open(directory.path().join("native-sessions.json"))
            .expect("sessions open"),
    ))
    .await;
    let resumed = rt
        .erase_principal(&admin, "alice")
        .await
        .expect("cleanup resumes");
    assert!(resumed.resumed, "a durable partial job was not resumed");
    assert!(resumed.completed, "blocked: {:?}", resumed.blocked);
    assert!(rt
        .search_memory(&alice, "local", "resume", 10)
        .await
        .expect("memory retrieves")
        .is_empty());
}
