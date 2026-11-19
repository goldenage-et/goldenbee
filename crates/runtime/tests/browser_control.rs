//! Fenced human takeover of a Worker-owned browser session.
//!
//! These tests exercise the public control-lease seam without starting a
//! browser. Page effects are covered by the General browser suite.

use common::ControlOwner;
use runtime::browser::{
    BrowserController, BrowserOptions, BrowserSettings, DriverMode, SessionKey,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn settings() -> BrowserSettings {
    BrowserOptions {
        enabled: true,
        webdriver_url: "http://127.0.0.1:1".into(),
        browser: "firefox".into(),
        headless: true,
        allowed_hosts: vec!["127.0.0.1:8080".into()],
        max_sessions: 2,
        driver: DriverMode::External,
        require_https: true,
        downloads_dir: "/tmp/goldenbee-control-test".into(),
        ..BrowserOptions::default()
    }
    .resolve(Path::new("/"))
    .expect("browser settings resolve")
}

fn key(user: &str) -> SessionKey {
    SessionKey {
        user: user.into(),
        session: "session-a".into(),
    }
}

async fn controller() -> Arc<BrowserController> {
    BrowserController::start("browser".into(), settings()).await
}

#[tokio::test]
async fn takeover_rotates_the_generation_and_fences_agent_commands() {
    let controller = controller().await;
    let key = key("alice");

    let lease = controller
        .acquire_control(key.clone(), "alice".into(), Some(60_000))
        .await
        .expect("human acquires the exact session");
    assert_eq!(lease.owner, ControlOwner::Human);
    assert_eq!(lease.session_id, "session-a");
    assert_eq!(lease.generation, 1);
    assert_eq!(lease.principal.as_deref(), Some("alice"));
    assert!(!lease.epoch.is_empty());

    let error = controller
        .begin_agent_command(&key)
        .expect_err("the agent cannot act during human control");
    assert_eq!(error.code, "control.fenced");

    let agent = controller
        .hand_back_control(&key, "alice", &lease.epoch, lease.generation)
        .await
        .expect("explicit hand-back");
    assert_eq!(agent.owner, ControlOwner::Agent);
    assert_eq!(agent.generation, lease.generation + 1);
    assert!(controller.begin_agent_command(&key).is_ok());

    let stale = controller
        .verify_human_lease(&key, "alice", &lease.epoch, lease.generation)
        .await
        .expect_err("the old generation is fenced");
    assert_eq!(stale.code, "control.fenced");

    controller.shutdown().await;
}

#[tokio::test]
async fn a_second_takeover_rotates_ownership_and_fences_the_first_client() {
    let controller = controller().await;
    let key = key("alice");

    let first = controller
        .acquire_control(key.clone(), "alice".into(), Some(60_000))
        .await
        .expect("first client acquires");
    let second = controller
        .acquire_control(key.clone(), "alice".into(), Some(60_000))
        .await
        .expect("a new takeover is allowed");
    assert!(second.generation > first.generation);

    let error = controller
        .verify_human_lease(&key, "alice", &first.epoch, first.generation)
        .await
        .expect_err("the first client is stale");
    assert_eq!(error.code, "control.fenced");
    controller
        .verify_human_lease(&key, "alice", &second.epoch, second.generation)
        .await
        .expect("the newest client owns the session");

    controller.shutdown().await;
}

#[tokio::test]
async fn concurrent_takeovers_leave_exactly_one_controller() {
    let controller = controller().await;
    let key = key("alice");

    let mut tasks = Vec::new();
    for _ in 0..16 {
        let controller = controller.clone();
        let key = key.clone();
        tasks.push(tokio::spawn(async move {
            controller
                .acquire_control(key, "alice".into(), Some(60_000))
                .await
        }));
    }
    let mut leases = Vec::new();
    for task in tasks {
        leases.push(task.await.expect("takeover task").expect("acquire"));
    }
    let mut generations: Vec<u64> = leases.iter().map(|lease| lease.generation).collect();
    generations.sort_unstable();
    generations.dedup();
    assert_eq!(
        generations.len(),
        leases.len(),
        "every takeover allocates a new generation"
    );

    let mut valid = 0;
    for lease in leases {
        if controller
            .verify_human_lease(&key, "alice", &lease.epoch, lease.generation)
            .await
            .is_ok()
        {
            valid += 1;
        }
    }
    assert_eq!(valid, 1, "only the newest takeover may control the session");
    controller.shutdown().await;
}

#[tokio::test]
async fn expiry_returns_control_to_the_agent_and_fences_the_human_client() {
    let controller = controller().await;
    let key = key("alice");

    let lease = controller
        .acquire_control(key.clone(), "alice".into(), Some(25))
        .await
        .expect("acquire");
    tokio::time::sleep(Duration::from_millis(80)).await;

    let error = controller
        .verify_human_lease(&key, "alice", &lease.epoch, lease.generation)
        .await
        .expect_err("the expired lease is fenced");
    assert_eq!(error.code, "control.lease_expired");
    assert!(controller.begin_agent_command(&key).is_ok());

    let status = controller
        .control_status(&key)
        .await
        .expect("status is available");
    assert_eq!(status.owner, ControlOwner::Agent);
    assert!(status.generation > lease.generation);
    controller.shutdown().await;
}

#[tokio::test]
async fn a_foreign_principal_cannot_inspect_or_command_the_session() {
    let controller = controller().await;
    let alice = key("alice");
    let bob = key("bob");

    let lease = controller
        .acquire_control(alice.clone(), "alice".into(), Some(60_000))
        .await
        .expect("alice acquires");
    let denied = controller
        .verify_human_lease(&alice, "bob", &lease.epoch, lease.generation)
        .await
        .expect_err("another principal cannot command alice's session");
    assert_eq!(denied.code, "control.principal_denied");

    let missing = controller
        .control_status(&bob)
        .await
        .expect_err("bob has no lease for alice's session");
    assert_eq!(missing.code, "control.session_not_found");
    assert!(controller.control_audit(&bob).await.is_empty());

    let audit = controller.control_audit(&alice).await;
    assert!(audit.iter().any(|entry| entry.action == "acquire"));
    controller.shutdown().await;
}

#[tokio::test]
async fn process_restart_fences_every_previous_lease() {
    let first = controller().await;
    let key = key("alice");
    let lease = first
        .acquire_control(key.clone(), "alice".into(), Some(60_000))
        .await
        .expect("acquire before restart");
    first.shutdown().await;

    let restarted = controller().await;
    let error = restarted
        .verify_human_lease(&key, "alice", &lease.epoch, lease.generation)
        .await
        .expect_err("a lease from a previous process cannot control this one");
    assert_eq!(error.code, "control.epoch_mismatch");
    assert!(restarted.begin_agent_command(&key).is_ok());
    restarted.shutdown().await;
}
