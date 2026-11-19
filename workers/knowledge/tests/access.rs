//! Namespace/source authorization, hooks, callbacks, and derived memory.

mod fixture;

use fixture::{knowledge_payload, payload_from_text};
use runtime::native::NativeMemory;

fn principals() -> Vec<(String, String, String)> {
    vec![
        ("alice-token".into(), "local".into(), "alice".into()),
        ("bob-token".into(), "local".into(), "bob".into()),
    ]
}

#[test]
fn denied_source_never_leaks_chunks_or_memory() {
    let temp = tempfile::tempdir().unwrap();
    let options = fixture::Options {
        principals: principals(),
        ..Default::default()
    };
    let fixture = fixture::write(temp.path(), options);
    let source = fixture.sources.join("denied.md");
    std::fs::write(
        &source,
        b"denied canary KNOWLEDGE-DENIED-31 only alice may read this.\n",
    )
    .unwrap();

    {
        let worker = fixture::spawn(&fixture.config, &fixture.root, "dev-token");
        let ingest = payload_from_text(
            worker.run_as(
                "alice-token",
                &format!("knowledge-ingest:{}|allowed=alice", source.display()),
                "denied-ingest",
            )["output"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(ingest["status"], "indexed", "{ingest}");
        assert_eq!(ingest["allowed"], serde_json::json!(["alice"]));

        let bob = payload_from_text(
            worker.run_as(
                "bob-token",
                "knowledge-ask:KNOWLEDGE-DENIED-31",
                "denied-bob",
            )["output"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(bob["results"], 0, "bob must not see denied chunks: {bob}");
        assert_eq!(bob["citations"], serde_json::json!([]));
        assert!(
            !bob.to_string().contains("only alice may read"),
            "denied source text leaked: {bob}"
        );

        let alice = payload_from_text(
            worker.run_as(
                "alice-token",
                "knowledge-ask:KNOWLEDGE-DENIED-31",
                "denied-alice",
            )["output"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(alice["results"], 1, "alice must see her chunk: {alice}");
        assert!(alice["citations"][0]["text"]
            .as_str()
            .unwrap()
            .contains("KNOWLEDGE-DENIED-31"));
    }

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let memory = NativeMemory::open("knowledge", &fixture.data_dir)
            .await
            .expect("native memory opens");
        let alice = common::Principal::new("alice", "local", &["*"]);
        let bob = common::Principal::new("bob", "local", &["*"]);
        let alice_items = memory
            .search(&alice, "local", "KNOWLEDGE-DENIED-31", 10)
            .await
            .expect("alice memory search");
        assert!(
            alice_items
                .iter()
                .any(|item| item.text.contains("KNOWLEDGE-DENIED-31")),
            "derived memory must contain alice's permitted chunk: {alice_items:?}"
        );
        let bob_items = memory
            .search(&bob, "local", "KNOWLEDGE-DENIED-31", 10)
            .await
            .expect("bob memory search");
        assert!(
            bob_items.is_empty(),
            "derived memory leaked to bob: {bob_items:?}"
        );
    });
}

#[test]
fn principal_erasure_removes_package_source_index_and_derived_memory() {
    let temp = tempfile::tempdir().unwrap();
    let options = fixture::Options {
        principals: principals(),
        ..Default::default()
    };
    let fixture = fixture::write(temp.path(), options);
    let source = fixture.sources.join("shared.md");
    std::fs::write(
        &source,
        b"shared canary KNOWLEDGE-ERASE-45 is visible before erasure.\n",
    )
    .unwrap();

    let worker = fixture::spawn(&fixture.config, &fixture.root, "dev-token");
    let ingest = payload_from_text(
        worker.run_as(
            "alice-token",
            &format!("knowledge-ingest:{}", source.display()),
            "erase-ingest",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(ingest["status"], "indexed", "{ingest}");

    // Bob may read the namespace-shared source before the owner erases it.
    let before = payload_from_text(
        worker.run_as(
            "bob-token",
            "knowledge-ask:KNOWLEDGE-ERASE-45",
            "erase-before",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert!(
        !before["citations"].as_array().unwrap().is_empty(),
        "{before}"
    );

    let response = reqwest::blocking::Client::new()
        .post(format!("{}/v1/principals/alice/erase", worker.base))
        .bearer_auth("dev-token")
        .send()
        .expect("erasure responds");
    let status = response.status();
    let report: serde_json::Value = response.json().expect("erasure report");
    assert!(status.is_success(), "{status} {report}");
    assert_eq!(report["completed"], true, "{report}");

    // The package-owned job catalogue and source bytes are gone, and the
    // remaining reader can no longer retrieve the erased content.
    let jobs: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.data_dir.join("knowledge").join("jobs.json"))
            .expect("jobs file remains readable"),
    )
    .unwrap();
    assert!(
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|job| !job["source_uri"]
                .as_str()
                .unwrap_or("")
                .ends_with("shared.md")),
        "{jobs}"
    );
    let after = payload_from_text(
        worker.run_as(
            "bob-token",
            "knowledge-ask:KNOWLEDGE-ERASE-45",
            "erase-after",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert!(after["citations"].as_array().unwrap().is_empty(), "{after}");
}

#[test]
fn restart_keeps_denied_source_denied() {
    let temp = tempfile::tempdir().unwrap();
    let options = fixture::Options {
        principals: principals(),
        ..Default::default()
    };
    let fixture = fixture::write(temp.path(), options);
    let source = fixture.sources.join("durable-denied.md");
    std::fs::write(
        &source,
        b"durable denied canary KNOWLEDGE-DURABLE-DENIED-31\n",
    )
    .unwrap();

    {
        let worker = fixture::spawn(&fixture.config, &fixture.root, "dev-token");
        let ingest = payload_from_text(
            worker.run_as(
                "alice-token",
                &format!("knowledge-ingest:{}|allowed=alice", source.display()),
                "durable-ingest",
            )["output"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(ingest["status"], "indexed", "{ingest}");
    }

    let restarted = fixture::spawn(&fixture.config, &fixture.root, "dev-token");
    let bob = payload_from_text(
        restarted.run_as(
            "bob-token",
            "knowledge-ask:KNOWLEDGE-DURABLE-DENIED-31",
            "durable-bob",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(bob["results"], 0, "{bob}");

    let alice = payload_from_text(
        restarted.run_as(
            "alice-token",
            "knowledge-ask:KNOWLEDGE-DURABLE-DENIED-31",
            "durable-alice",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(alice["results"], 1, "{alice}");
}

#[test]
fn hook_callback_and_plugin_keep_denial_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = fixture::Options::default();
    options
        .settings
        .push(("deny_source_contains".into(), "blocked.md".into()));
    let fixture = fixture::write(temp.path(), options);
    let source = fixture.sources.join("blocked.md");
    std::fs::write(&source, b"blocked canary KNOWLEDGE-BLOCKED-31\n").unwrap();

    let denied = knowledge_payload(&fixture::run(
        &fixture,
        &format!("knowledge-ingest:{}", source.display()),
    ));
    assert_eq!(denied["error"], "knowledge.source_denied", "{denied}");

    let plugin = fixture::run(&fixture, "knowledge-plugin-deny");
    let stdout = String::from_utf8_lossy(&plugin.stdout);
    assert!(
        stdout.contains("knowledge.access_denied"),
        "plugin did not attach: {stdout} stderr={}",
        String::from_utf8_lossy(&plugin.stderr)
    );
}
