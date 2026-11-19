//! Evidence, checks, digest verification, and bounded command behavior.

use runtime::native::NativeArtifactStore;

mod fixture;

use fixture::{b64, fixed_lib};

fn artifact_version(task: &serde_json::Value, name: &str) -> (i64, String) {
    let event = task["evidence"].as_object().expect("evidence object");
    let reference = event
        .get("diff")
        .filter(|_| name == "coding.diff.patch")
        .or_else(|| {
            event
                .get("evidence")
                .filter(|_| name == "coding.evidence.json")
        })
        .or_else(|| event.get("status").filter(|_| name == "coding.status.txt"))
        .unwrap_or_else(|| panic!("no artifact reference for {name}"));
    (
        reference["version"].as_i64().expect("artifact version"),
        reference["digest"]
            .as_str()
            .expect("artifact digest")
            .to_string(),
    )
}

fn read_artifact(
    fixture: &fixture::Fixture,
    task: &serde_json::Value,
    name: &str,
) -> (Vec<u8>, String) {
    let (version, expected) = artifact_version(task, name);
    let user = runtime::native::native_user_id("local", "local").unwrap();
    let session = task["session_id"].as_str().expect("session id");
    let store = NativeArtifactStore::open(&fixture.data).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let payload = runtime
        .block_on(store.payload("coding", &user, session, name, version))
        .expect("artifact payload");
    (payload.bytes, expected)
}

fn find_version_file(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    fn walk(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(root).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                if path.file_name().and_then(|value| value.to_str()) == Some(name) {
                    let file = path.join("v1.json");
                    if file.exists() {
                        return Some(file);
                    }
                }
                if let Some(found) = walk(&path, name) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(root, name)
        .unwrap_or_else(|| panic!("artifact {name} was not stored below {}", root.display()))
}

#[test]
fn successful_change_produces_verified_evidence() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-success".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let input = format!(
        "coding-edit:src/lib.rs:{}:{}:sh tests/check.sh",
        b64("a - b"),
        b64("a + b")
    );
    let task = server.run_with_evidence("developer", &input, "success");

    assert_eq!(task["state"], "succeeded", "{task}");
    let evidence = &task["evidence"];
    assert_eq!(evidence["accepted"], true, "{task}");
    assert_eq!(evidence["verified"], true, "{task}");
    assert_eq!(evidence["checks"][0]["status"], "passed", "{task}");
    assert_eq!(evidence["checks"][0]["exit_code"], 0, "{task}");
    assert_eq!(evidence["checks"][0]["target_dir"], "target", "{task}");

    let names = evidence["plugin"]["tool_names"]
        .as_array()
        .expect("plugin names");
    let names: Vec<_> = names.iter().filter_map(|value| value.as_str()).collect();
    assert!(names.contains(&"read_file"), "{names:?}");
    assert!(names.contains(&"edit_file"), "{names:?}");
    assert!(names.contains(&"exec_command"), "{names:?}");

    let source = std::fs::read_to_string(fixture.worktree.join("src/lib.rs")).unwrap();
    assert!(source.contains("a + b"), "{source}");
    assert_ne!(fixture.worktree, fixture.repo);

    let (diff, expected) = read_artifact(&fixture, &task, "coding.diff.patch");
    let diff = String::from_utf8(diff).unwrap();
    assert!(diff.contains("+    a + b"), "{diff}");
    assert_eq!(common::bytes_digest(diff.as_bytes()), expected);
}

#[test]
fn failing_check_is_reported_not_hidden() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-failing".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let failing = fixture::broken_lib().replace("still wrong", "changed but still wrong");
    let input = format!("coding-write:src/lib.rs:{}", b64(&failing));
    let task = server.run_with_evidence("developer", &input, "failing");

    let evidence = &task["evidence"];
    assert_eq!(evidence["accepted"], false, "{task}");
    assert_eq!(evidence["checks"][0]["status"], "failed", "{task}");
    assert_eq!(evidence["checks"][0]["exit_code"], 1, "{task}");
    let unverified = evidence["unverified"].to_string();
    assert!(unverified.contains("check did not pass"), "{unverified}");

    let (diff, _) = read_artifact(&fixture, &task, "coding.diff.patch");
    assert!(
        String::from_utf8_lossy(&diff).contains("still wrong"),
        "diff evidence must survive a failed check"
    );
}

#[test]
fn evidence_artifact_digest_is_verified_and_tampering_is_detected() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-digest".into(),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let input = format!(
        "coding-fix:src/lib.rs:{}:sh tests/check.sh",
        b64(fixed_lib())
    );
    let task = server.run_with_evidence("developer", &input, "digest");

    let (original, expected) = read_artifact(&fixture, &task, "coding.diff.patch");
    assert_eq!(common::bytes_digest(&original), expected);

    let path = find_version_file(&fixture.data.join("native-artifacts"), "coding.diff.patch");
    let tampered = std::fs::read_to_string(&path)
        .unwrap()
        .replace("a + b", "a - b");
    std::fs::write(&path, tampered).unwrap();

    let (changed, expected) = read_artifact(&fixture, &task, "coding.diff.patch");
    assert_ne!(
        common::bytes_digest(&changed),
        expected,
        "tampered artifact must not verify against its recorded digest"
    );
}

#[test]
fn bounded_output_is_reported_as_unverified() {
    let _serial = fixture::serial();
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        &root.path().join("config"),
        fixture::Options {
            instance: "coding-output".into(),
            checks: "sh -c 'yes bounded-output | head -c 4096'".into(),
            max_output_bytes: Some(128),
            ..Default::default()
        },
    );
    let cwd = tempfile::tempdir().unwrap();
    let server = fixture::start(&fixture, cwd.path());
    let input = format!("coding-write:src/lib.rs:{}", b64(fixed_lib()));
    let task = server.run_with_evidence("developer", &input, "bounded");
    let unverified = task["evidence"]["unverified"].to_string();
    assert!(
        unverified.contains("check did not complete") || unverified.contains("check did not pass"),
        "{unverified}"
    );
    assert_eq!(task["evidence"]["accepted"], false);
}
