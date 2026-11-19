//! Native launcher modes and operator assets outside the installation.

mod fixture;

use std::io::Write;
use std::process::{Command, Stdio};

fn dev(config: &std::path::Path, cwd: &std::path::Path, lines: &[String]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_knowledge"))
        .args([
            "dev",
            "--config",
            config.to_str().expect("config path"),
            "--agent",
            "knowledge",
        ])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("knowledge dev starts");
    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        for line in lines {
            writeln!(stdin, "{line}").expect("write console line");
        }
        writeln!(stdin, "exit").expect("write exit");
    }
    child.wait_with_output().expect("dev exits")
}

#[test]
fn native_launcher_modes_execute_real_ingestion_and_retrieval() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture::write(temp.path(), fixture::Options::default());
    let source = fixture.sources.join("launcher.md");
    std::fs::write(
        &source,
        b"launcher canary KNOWLEDGE-LAUNCHER-31 through the native console.\n",
    )
    .unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    let ingest = dev(
        &fixture.config,
        elsewhere.path(),
        &[format!("knowledge-ingest:{}", source.display())],
    );
    let ingest_stdout = String::from_utf8_lossy(&ingest.stdout);
    assert!(
        ingest.status.success(),
        "dev ingest failed: {}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let ingested = fixture::knowledge_payload_from_stdout(&ingest_stdout);
    assert_eq!(ingested["status"], "indexed", "{ingest_stdout}");

    let ask = dev(
        &fixture.config,
        elsewhere.path(),
        &["knowledge-ask:KNOWLEDGE-LAUNCHER-31".into()],
    );
    let ask_stdout = String::from_utf8_lossy(&ask.stdout);
    assert!(
        ask.status.success(),
        "dev ask failed: {}",
        String::from_utf8_lossy(&ask.stderr)
    );
    let payload = fixture::knowledge_payload_from_stdout(&ask_stdout);
    assert_eq!(payload["results"], 1, "{ask_stdout}");
    assert!(payload["citations"][0]["text"]
        .as_str()
        .unwrap()
        .contains("KNOWLEDGE-LAUNCHER-31"));

    // The web-server mode drives the same package composition over HTTP,
    // without the SDK or Gateway.
    let worker = fixture::spawn(&fixture.config, elsewhere.path(), "dev-token");
    assert!(worker.wait_port());
    let served = fixture::payload_from_text(
        worker.run_as(
            "dev-token",
            "knowledge-ask:KNOWLEDGE-LAUNCHER-31",
            "launcher-serve",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(served["results"], 1, "{served}");
}

#[test]
fn packaged_executable_loads_operator_assets_from_an_unrelated_directory() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture::write(temp.path(), fixture::Options::default());
    let source = fixture.sources.join("operator.md");
    std::fs::write(&source, b"operator asset canary KNOWLEDGE-OPERATOR-31.\n").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    let ingest = fixture::run_in(
        &fixture.config,
        elsewhere.path(),
        &format!("knowledge-ingest:{}", source.display()),
    );
    assert!(
        ingest.status.success(),
        "{}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let asked = fixture::run_in(
        &fixture.config,
        elsewhere.path(),
        "knowledge-ask:KNOWLEDGE-OPERATOR-31",
    );
    let payload = fixture::knowledge_payload(&asked);
    assert_eq!(payload["results"], 1, "{payload}");

    // Changing operator assets requires no rebuild: the prompt and skill live
    // outside the executable and are loaded by the shared runtime.
    std::fs::write(
        fixture.prompts.join("knowledge.md"),
        "Operator-edited prompt with no rebuild.",
    )
    .unwrap();
    let capabilities = Command::new(env!("CARGO_BIN_EXE_knowledge"))
        .args([
            "capabilities",
            "--config",
            fixture.config.to_str().unwrap(),
            "--json",
        ])
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    assert!(capabilities.status.success());
    let report = fixture::output_json(&capabilities);
    assert_eq!(report["configured"][0]["id"], "knowledge");
    assert_eq!(report["installed"]["name"], "knowledge");
}
