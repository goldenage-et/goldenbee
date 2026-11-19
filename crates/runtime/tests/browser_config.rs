//! Browser operator configuration. Unknown keys must fail and an enabled
//! browser must carry an explicit destination allowlist.

use runtime::browser::{BrowserOptions, BrowserProfile};
use runtime::worker::FileConfig;

fn parse(text: &str) -> FileConfig {
    toml::from_str(text).expect("configuration parses")
}

#[test]
fn browser_is_disabled_unless_configured() {
    let file = parse("instance = \"test\"\n");
    assert!(file.browser.is_none());
}

#[test]
fn an_enabled_browser_requires_destinations() {
    let file = parse("[browser]\nenabled = true\n");
    let browser = file.browser.expect("browser section");
    let error = browser
        .resolve(std::path::Path::new("."))
        .expect_err("an enabled browser without destinations is invalid");
    assert_eq!(error.code, "browser.policy_empty");
}

#[test]
fn browser_settings_are_resolved_and_validated() {
    let file = parse(
        r#"
[browser]
enabled = true
webdriver_url = "http://127.0.0.1:4444"
browser = "firefox"
profile = "form_filling"
allowed_hosts = ["127.0.0.1:8080"]
max_sessions = 2
session_timeout_ms = 60000
idle_timeout_ms = 30000
max_output_bytes = 4096
downloads_dir = "downloads"
"#,
    );
    let browser = file.browser.expect("browser section");
    let resolved = browser
        .resolve(std::path::Path::new("/tmp/worker"))
        .expect("settings resolve");
    assert!(resolved.enabled);
    assert_eq!(resolved.profile, BrowserProfile::FormFilling);
    assert_eq!(resolved.max_sessions, 2);
    assert_eq!(
        resolved.downloads_dir,
        std::path::Path::new("/tmp/worker/downloads")
    );
    assert_eq!(resolved.max_output_bytes, 4096);
}

#[test]
fn unknown_browser_keys_are_rejected() {
    let error = toml::from_str::<FileConfig>("[browser]\nenabled = false\nevaluate_js = true\n")
        .expect_err("unknown browser keys must not silently enable behavior");
    assert!(error.to_string().contains("unknown field"), "{error}");
}

#[test]
fn an_unknown_browser_name_is_rejected() {
    let file = parse(
        "[browser]\nenabled = true\nallowed_hosts = [\"example.com\"]\nbrowser = \"netscape\"\n",
    );
    let error = file
        .browser
        .expect("browser section")
        .resolve(std::path::Path::new("."))
        .expect_err("unsupported browser");
    assert_eq!(error.code, "browser.config_invalid");
}

#[test]
fn browser_options_default_to_headless_firefox_with_bounded_lifetime() {
    let options = BrowserOptions::default();
    assert!(!options.enabled);
    assert!(options.headless);
    assert_eq!(options.browser, "firefox");
    assert_eq!(options.max_sessions, 2);
    assert_eq!(options.session_timeout_ms, 300_000);
    assert_eq!(options.idle_timeout_ms, 120_000);
    assert_eq!(options.max_output_bytes, 262_144);
    assert_eq!(options.profile, BrowserProfile::Minimal);
}

#[test]
fn browser_configuration_has_no_credential_value_channel() {
    let error = toml::from_str::<FileConfig>(
        "[browser]\nenabled = true\nallowed_hosts = [\"example.com\"]\ncredential = \"sk-canary\"\n",
    )
    .expect_err("a credential value cannot be placed in browser configuration");
    assert!(error.to_string().contains("unknown field"), "{error}");
}
