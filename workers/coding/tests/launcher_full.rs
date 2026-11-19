//! Full pinned native Launcher drives actual CodingAgent tools in both modes.

mod fixture;

#[test]
fn full_launcher_executes_native_coding_tools() {
    let data = tempfile::tempdir().unwrap();
    let fixture = fixture::write(data.path(), fixture::Options::default());
    let elsewhere = tempfile::tempdir().unwrap();
    let input = format!(
        "coding-fix:src/lib.rs:{}:sh tests/check.sh",
        fixture::b64(fixture::fixed_lib())
    );
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/launcher.py"
        ))
        .args(["--binary", env!("CARGO_BIN_EXE_coding"), "--config"])
        .arg(&fixture.config)
        .args([
            "--agent",
            "developer",
            "--input",
            &input,
            "--expect",
            "coding result",
        ])
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("ADK-Rust  v2.3.0"), "{stdout}");
    assert!(stdout.contains("write_file"), "{stdout}");
    assert!(stdout.contains("exec_command"), "{stdout}");
    assert!(stdout.contains("\"exit_code\":0"), "{stdout}");
    assert!(
        !fixture.worktree.exists(),
        "shutdown must remove owned worktree"
    );
}
