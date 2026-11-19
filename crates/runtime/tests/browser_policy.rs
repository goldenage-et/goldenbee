//! Navigation policy tests. These do not need a WebDriver server: every
//! rejected destination must be refused before the browser is touched.

use runtime::browser::BrowserPolicy;

fn policy(hosts: &[&str]) -> BrowserPolicy {
    BrowserPolicy::new(hosts).expect("policy accepts valid destinations")
}

#[test]
fn non_web_schemes_are_rejected() {
    let policy = policy(&["127.0.0.1:8080"]);
    for url in [
        "file:///etc/passwd",
        "javascript:alert(document.cookie)",
        "data:text/html,<script>alert(1)</script>",
        "chrome://settings",
        "about:blank",
    ] {
        let error = policy
            .check_url(url)
            .expect_err("a non-web scheme must be refused");
        assert_eq!(error.code, "browser.scheme_denied", "{url}: {error:?}");
    }
}

#[test]
fn destinations_must_be_on_the_allowlist() {
    let policy = policy(&["127.0.0.1:8080"]);
    policy
        .check_url("http://127.0.0.1:8080/index.html")
        .expect("the allowlisted development site is permitted");
    let error = policy
        .check_url("http://127.0.0.1:9090/other")
        .expect_err("another destination must be refused");
    assert_eq!(error.code, "browser.destination_denied");
    let error = policy
        .check_url("https://example.com/")
        .expect_err("an unlisted host must be refused");
    assert_eq!(error.code, "browser.destination_denied");
}

#[test]
fn tls_is_required_outside_loopback() {
    let policy = policy(&["example.com", "127.0.0.1:8080"]);
    let error = policy
        .check_url("http://example.com/")
        .expect_err("plain HTTP to a remote host must be refused");
    assert_eq!(error.code, "browser.tls_required");
    policy
        .check_url("https://example.com/")
        .expect("HTTPS is permitted");
}

#[test]
fn private_addresses_are_explicit_only() {
    // A public hostname that resolves to a private address must not be usable
    // as an SSRF bridge.
    let internal = policy(&["internal.example"]);
    let error = internal
        .check_resolved("internal.example", 443, "https", "10.1.2.3")
        .expect_err("private addresses are refused unless the host is an IP literal");
    assert_eq!(error.code, "browser.private_address_denied");

    // An explicit private IP destination is the operator's decision.
    let private = policy(&["10.1.2.3:8443"]);
    private
        .check_resolved("10.1.2.3", 8443, "https", "10.1.2.3")
        .expect("an explicitly allowlisted private address is permitted");
}

#[test]
fn loopback_is_explicit_only() {
    let policy = policy(&["example.com"]);
    let error = policy
        .check_resolved("localhost", 80, "http", "127.0.0.1")
        .expect_err("loopback needs an explicit destination");
    assert_eq!(error.code, "browser.destination_denied");
}

#[test]
fn an_explicit_localhost_destination_may_resolve_to_loopback() {
    let policy = policy(&["localhost"]);
    policy
        .check_resolved("localhost", 80, "http", "127.0.0.1")
        .expect("an explicitly allowlisted localhost is a development destination");
}

#[test]
fn link_local_and_unspecified_addresses_are_private() {
    let policy = policy(&["metadata.example"]);
    for address in ["169.254.169.254", "0.0.0.0", "::1", "fd00::1"] {
        let error = policy
            .check_resolved("metadata.example", 80, "https", address)
            .expect_err("special-use address must be refused");
        assert_eq!(error.code, "browser.private_address_denied", "{address}");
    }
}

#[test]
fn an_empty_allowlist_cannot_enable_the_browser() {
    let error = BrowserPolicy::new::<&str>(&[]).expect_err("an unrestricted browser is invalid");
    assert_eq!(error.code, "browser.policy_empty");
}

#[test]
fn wildcard_allowlist_is_not_silently_accepted() {
    let error = BrowserPolicy::new(&["*"]).expect_err("a wildcard must be explicit configuration");
    assert_eq!(error.code, "browser.policy_invalid");
}
