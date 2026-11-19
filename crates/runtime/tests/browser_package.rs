//! Package boundary: the browser capability is Worker-owned and must be
//! declared by package code. A Runtime Profile or another Client cannot add it.

use runtime::worker::Worker;

#[test]
fn a_package_without_a_browser_declaration_has_no_browser_capability() {
    let manifest = Worker::new("boundary-test", "0.1.0").manifest();
    assert!(
        !manifest
            .capabilities
            .iter()
            .any(|capability| capability.id == "browser"),
        "{:?}",
        manifest.capabilities
    );
}

#[test]
fn package_code_declares_the_browser_toolset_explicitly() {
    let manifest = Worker::new("boundary-test", "0.1.0")
        .browser("browser")
        .manifest();
    assert!(manifest
        .capabilities
        .iter()
        .any(|capability| { capability.id == "browser" && capability.kind == "toolset" }));
}

#[test]
fn common_and_client_sdk_do_not_link_native_browser_execution() {
    for manifest in [
        "../../crates/common/Cargo.toml",
        "../../crates/sdk/Cargo.toml",
    ] {
        let text = std::fs::read_to_string(manifest).expect("workspace manifest");
        assert!(
            !text.contains("adk-browser") && !text.contains("adk-rust"),
            "{manifest} must not link native browser execution"
        );
    }
    let runtime = std::fs::read_to_string("../../crates/runtime/Cargo.toml").expect("runtime");
    assert!(runtime.contains("adk-browser"));
}

#[test]
fn configuration_cannot_enable_a_browser_the_package_did_not_install() {
    let data = tempfile::tempdir().expect("data");
    let config = data.path().join("worker.toml");
    std::fs::write(
        &config,
        "instance = \"boundary\"\n[browser]\nenabled = true\nallowed_hosts = [\"example.com\"]\n",
    )
    .expect("config");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let error = match runtime.block_on(Worker::new("boundary-test", "0.1.0").start(&config)) {
        Ok(_) => panic!("a missing package capability cannot be enabled by configuration"),
        Err(error) => error,
    };
    assert_eq!(error.code, "browser.not_installed");
}
