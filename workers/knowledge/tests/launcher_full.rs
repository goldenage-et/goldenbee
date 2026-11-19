//! Full pinned native Launcher performs ingestion and retrieval in both modes.

mod fixture;

#[test]
fn full_launcher_ingests_and_retrieves_real_source() {
    let data = tempfile::tempdir().unwrap();
    let fixture = fixture::write(data.path(), fixture::Options::default());
    let source = fixture.sources.join("launcher.md");
    std::fs::write(&source, "full native Launcher source CANARY50.\n").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let input = format!("knowledge-ingest:{}", source.display());
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/launcher.py"
        ))
        .args(["--binary", env!("CARGO_BIN_EXE_knowledge"), "--config"])
        .arg(&fixture.config)
        .args([
            "--agent",
            "knowledge",
            "--input",
            &input,
            "--expect",
            "indexed",
            "--input",
            "knowledge-ask:CANARY50",
            "--expect",
            "\"results\":1",
        ])
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("ADK-Rust  v2.3.0"), "{stdout}");
    assert!(stdout.contains("citations"), "{stdout}");
}
