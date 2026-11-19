//! Exact pinned full native ADK Launcher acceptance for the released package.
//!
//! `dev` drives `adk_cli::Launcher::run_console_directly`; `serve` composes its
//! native REST/UI application through `adk_cli::Launcher::build_app` while the
//! host binds a loopback listener. Both use the package's resolved agents.

use std::process::Command;

mod fixture;

#[test]
fn full_launcher_cli_and_web_server_run_real_composition_and_hooks() {
    let data = tempfile::tempdir().unwrap();
    let fixture = fixture::write(
        data.path(),
        fixture::Options {
            instance: "general-launcher-cli".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    let elsewhere = tempfile::tempdir().unwrap();
    // The driver owns process groups, drains both pipes concurrently, enforces
    // readiness/interaction/HTTP deadlines, and kills AND reaps on every error.
    // Native rustyline supports pipe input: no PTY or terminal query emulator.
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/launcher.py"
        ))
        .args(["--binary", env!("CARGO_BIN_EXE_general"), "--config"])
        .arg(&fixture.config)
        .args(["--agent", "general"])
        .current_dir(elsewhere.path())
        .output()
        .expect("bounded native Launcher driver starts");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("ADK-Rust  v2.3.0"), "{stdout}");
    assert!(stdout.contains("echo: full launcher canary"), "{stdout}");
    assert!(stdout.contains("hook:startup"), "{stdout}");
    assert!(stdout.contains("hook:shutdown"), "{stdout}");

    assert!(
        stdout.contains("PASS full native Launcher CLI+web: general"),
        "{stdout}"
    );
}
