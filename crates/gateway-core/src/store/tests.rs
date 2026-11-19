//! File adapter contract tests.

use std::sync::Arc;

use common::TaskState;

use super::{
    Accepted, AcceptedMapping, AttemptOutcome, Begin, Claim, Connectivity, DispatchData,
    DispatchState, DispatchStore, FileDispatchStore, NewIntent, ReplicaId, StreamMapStore,
    DISPATCH_SCHEMA,
};

fn intent(execution_id: &str, key: &str) -> NewIntent {
    NewIntent {
        execution_id: execution_id.into(),
        namespace: "local".into(),
        caller: "caller".into(),
        idempotency_key: key.into(),
        request_digest: "sha256:digest".into(),
        worker: "worker-a".into(),
        agent: "general".into(),
    }
}

fn mapping(worker_task_id: &str) -> AcceptedMapping {
    AcceptedMapping {
        worker_task_id: worker_task_id.into(),
        worker_instance: "instance-a".into(),
        worker_incarnation: "inc-1".into(),
        accepted: Accepted {
            agent: "general".into(),
            profile: "default@1".into(),
            state: TaskState::Queued,
            team_version: None,
        },
    }
}

#[tokio::test]
async fn committed_intent_survives_reopen() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    {
        let store = FileDispatchStore::open(directory.path())
            .await
            .expect("dispatch store opens");
        assert!(matches!(
            store.begin(intent("exec-1", "key")).await.unwrap(),
            Begin::Created(_)
        ));
    }
    let reopened = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store reopens");
    let submission = reopened
        .submission_by_key("local", "caller", "key")
        .await
        .unwrap()
        .expect("identity survives reopen");
    assert_eq!(submission.execution_id, "exec-1");
    assert_eq!(submission.dispatch, DispatchState::Intent);
}

#[tokio::test]
async fn concurrent_same_key_creates_one_intent() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = Arc::new(
        FileDispatchStore::open(directory.path())
            .await
            .expect("dispatch store opens"),
    );
    let mut tasks = Vec::new();
    for index in 0..16 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .begin(intent(&format!("exec-{index}"), "same-key"))
                .await
                .expect("begin succeeds")
        }));
    }
    let mut created = 0;
    let mut existing = Vec::new();
    for task in tasks {
        match task.await.expect("task") {
            Begin::Created(submission) => {
                created += 1;
                existing.push(submission.execution_id);
            }
            Begin::Existing(submission) => existing.push(submission.execution_id),
            Begin::Conflict(_) => panic!("same digest cannot conflict"),
        }
    }
    assert_eq!(created, 1, "exactly one intent is created");
    assert!(
        existing.windows(2).all(|pair| pair[0] == pair[1]),
        "all callers observe one execution id"
    );
}

#[tokio::test]
async fn different_digest_conflicts_without_inserting() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store opens");
    store.begin(intent("exec-1", "key")).await.unwrap();
    let mut changed = intent("exec-2", "key");
    changed.request_digest = "sha256:other".into();
    let begin = store.begin(changed).await.unwrap();
    match begin {
        Begin::Conflict(submission) => assert_eq!(submission.execution_id, "exec-1"),
        _ => panic!("different digest must conflict"),
    }
    assert_eq!(
        store
            .submission_by_key("local", "caller", "key")
            .await
            .unwrap()
            .unwrap()
            .execution_id,
        "exec-1"
    );
}

#[tokio::test]
async fn claim_twice_is_busy_then_available_after_expiry() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store opens");
    store.begin(intent("exec-1", "key")).await.unwrap();
    let a = ReplicaId("a".into());
    let b = ReplicaId("b".into());
    assert!(matches!(
        store.claim("exec-1", &a, 5_000).await.unwrap(),
        Claim::Claimed(_, _)
    ));
    assert!(matches!(
        store.claim("exec-1", &b, 5_000).await.unwrap(),
        Claim::Busy { .. }
    ));
    tokio::time::sleep(std::time::Duration::from_millis(5_200)).await;
    assert!(matches!(
        store.claim("exec-1", &b, 1_000).await.unwrap(),
        Claim::Claimed(_, _)
    ));
}

#[tokio::test]
async fn stale_claim_cannot_commit() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store opens");
    store.begin(intent("exec-1", "key")).await.unwrap();
    let a = ReplicaId("a".into());
    let b = ReplicaId("b".into());
    let Claim::Claimed(stale, _) = store.claim("exec-1", &a, 1).await.unwrap() else {
        panic!("first claim");
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let Claim::Claimed(fresh, _) = store.claim("exec-1", &b, 1_000).await.unwrap() else {
        panic!("second claim");
    };
    let error = store.commit(&stale, mapping("task-a")).await.unwrap_err();
    assert_eq!(error.code, "task.claim_lost");
    let committed = store.commit(&fresh, mapping("task-b")).await.unwrap();
    assert_eq!(committed.worker_task_id.as_deref(), Some("task-b"));
}

#[tokio::test]
async fn commit_twice_keeps_first_mapping() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store opens");
    store.begin(intent("exec-1", "key")).await.unwrap();
    let Claim::Claimed(claim, _) = store
        .claim("exec-1", &ReplicaId("a".into()), 1_000)
        .await
        .unwrap()
    else {
        panic!("claim");
    };
    store.commit(&claim, mapping("task-a")).await.unwrap();
    let again = store.commit(&claim, mapping("task-a")).await.unwrap();
    assert_eq!(again.worker_task_id.as_deref(), Some("task-a"));
    let conflict = store.commit(&claim, mapping("task-b")).await.unwrap_err();
    assert_eq!(conflict.code, "task.dispatch_conflict");
}

#[tokio::test]
async fn release_records_unknown_and_allows_reclaim() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store opens");
    store.begin(intent("exec-1", "key")).await.unwrap();
    let Claim::Claimed(claim, _) = store
        .claim("exec-1", &ReplicaId("a".into()), 1_000)
        .await
        .unwrap()
    else {
        panic!("claim");
    };
    store
        .release(&claim, AttemptOutcome::ambiguous("gateway.transport"))
        .await
        .unwrap();
    let submission = store.submission("exec-1").await.unwrap().unwrap();
    assert_eq!(submission.dispatch, DispatchState::Unknown);
    assert_eq!(submission.connectivity, Connectivity::Unreachable);
    assert!(matches!(
        store
            .claim("exec-1", &ReplicaId("b".into()), 1_000)
            .await
            .unwrap(),
        Claim::Claimed(_, _)
    ));
}

#[tokio::test]
async fn projection_does_not_change_execution_or_task_identity() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store opens");
    store.begin(intent("exec-1", "key")).await.unwrap();
    let Claim::Claimed(claim, _) = store
        .claim("exec-1", &ReplicaId("a".into()), 1_000)
        .await
        .unwrap()
    else {
        panic!("claim");
    };
    store.commit(&claim, mapping("task-a")).await.unwrap();
    store
        .record_projection(
            "exec-1",
            None,
            Some(TaskState::Succeeded),
            Connectivity::Online,
        )
        .await
        .unwrap();
    let submission = store.submission("exec-1").await.unwrap().unwrap();
    assert_eq!(submission.execution_id, "exec-1");
    assert_eq!(submission.worker_task_id.as_deref(), Some("task-a"));
    assert_eq!(
        submission.accepted.as_ref().unwrap().state,
        TaskState::Succeeded
    );
}

#[tokio::test]
async fn stream_mapping_is_available_without_submission_body() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let store = FileDispatchStore::open(directory.path())
        .await
        .expect("dispatch store opens");
    store.begin(intent("exec-1", "key")).await.unwrap();
    let Claim::Claimed(claim, _) = store
        .claim("exec-1", &ReplicaId("a".into()), 1_000)
        .await
        .unwrap()
    else {
        panic!("claim");
    };
    store.commit(&claim, mapping("task-a")).await.unwrap();
    let mapping = store.stream_mapping("exec-1").await.unwrap().unwrap();
    assert_eq!(mapping.worker_task_id, "task-a");
    assert_eq!(mapping.namespace, "local");
    assert_eq!(mapping.caller, "caller");
}

#[tokio::test]
async fn second_open_fails_single_writer() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let _first = FileDispatchStore::open(directory.path())
        .await
        .expect("first writer opens");
    let error = FileDispatchStore::open(directory.path())
        .await
        .expect_err("second writer is refused");
    assert_eq!(error.code, "storage.single_writer");
}

#[tokio::test]
async fn newer_dispatch_schema_is_refused() {
    let directory = tempfile::tempdir().expect("temporary dispatch directory");
    let path = directory.path().join("dispatch.json");
    let data = DispatchData {
        schema: DISPATCH_SCHEMA + 1,
        ..DispatchData::default()
    };
    std::fs::write(
        &path,
        serde_json::to_vec(&data).expect("newer dispatch JSON"),
    )
    .expect("newer dispatch file");
    let error = match FileDispatchStore::open(directory.path()).await {
        Ok(_) => panic!("newer schema is refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, "storage.schema_unsupported");
}
