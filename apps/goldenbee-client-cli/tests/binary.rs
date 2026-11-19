//! Ensures the CLI executable exists for cross-process Client journeys.

use std::process::Command;

#[test]
fn cli_executable_reports_its_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .arg("--version")
        .output()
        .expect("cli executable runs");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("gb-client {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_chat_provider_and_artifact_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_gb-client"))
        .arg("--help")
        .output()
        .expect("cli executable runs");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("chat [--worker <id>]"));
    assert!(help.contains("provider list"));
    assert!(help.contains("provider credential set"));
    assert!(help.contains("artifact download"));
    assert!(help.contains("manage"));
    assert!(help.contains("model list"));
}
