//! Registry unit tests.

use std::sync::Arc;

use common::{
    CapabilityDescriptor, Capacity, WorkerDiscoveryFilter, WorkerLeaseRequest,
    WorkerRegistrationRequest, LOCALITY_LOCAL,
};

use super::model::{Approval, ApprovalPatch, Document, Record, Verified};
use super::persistence::hash_token;
use super::Registry;

fn verified(incarnation: &str) -> Verified {
    Verified {
        instance: "worker-a".into(),
        incarnation: incarnation.into(),
        package: "general".into(),
        version: "0.1.0".into(),
        protocol: common::PROTOCOL_VERSION.into(),
        capabilities: vec![CapabilityDescriptor {
            id: "general".into(),
            kind: "agent".into(),
            description: "general".into(),
            permissions: Vec::new(),
        }],
    }
}

fn approval() -> Approval {
    Approval {
        id: "worker-a".into(),
        endpoint: "http://127.0.0.1:1".into(),
        instance: Some("worker-a".into()),
        package: Some("general".into()),
        capabilities: None,
        control_token: Some("control-a".into()),
    }
}

fn registration(endpoint: &str, incarnation: &str) -> WorkerRegistrationRequest {
    WorkerRegistrationRequest {
        endpoint: endpoint.into(),
        incarnation: incarnation.into(),
        capacity: Capacity::default(),
    }
}

#[test]
fn reopen_recomputes_expired_leases_and_keeps_approval_state() {
    use std::sync::atomic::{AtomicI64, Ordering};
    const TTL: i64 = 100;
    let directory = tempfile::tempdir().expect("registry directory");
    let path = directory.path().join("registry.json");
    let clock = Arc::new(AtomicI64::new(1_000));
    let read = clock.clone();
    {
        let registry = Registry::open(
            Some(path.clone()),
            Arc::new(move || read.load(Ordering::SeqCst)),
            TTL,
            vec![approval()],
        )
        .expect("registry opens");
        registry
            .register(
                "worker-a",
                &registration("http://127.0.0.1:1", "inc-1"),
                &verified("inc-1"),
                registry.now(),
            )
            .expect("registration accepted");
        assert_eq!(registry.health(), (1, 1));
    }

    clock.store(1_000 + TTL, Ordering::SeqCst);
    let reopened = Registry::open(
        Some(path),
        Arc::new(move || clock.load(Ordering::SeqCst)),
        TTL,
        vec![approval()],
    )
    .expect("registry reopens");
    let summary = reopened.worker("worker-a").expect("record survives reopen");
    assert_eq!(summary.status, "unreachable");
    assert!(summary
        .incarnations
        .iter()
        .any(|entry| entry.incarnation == "inc-1"));
    let approval = reopened
        .admin_worker("worker-a")
        .expect("approval survives reopen");
    assert!(approval.enabled);
    assert_eq!(approval.status, "unreachable");
    assert!(reopened
        .audit_entries(Some("worker-a"))
        .iter()
        .any(|entry| entry.action == "worker.lease_expired"));
}

#[test]
fn controlled_clock_expiry_marks_unreachable_and_preserves_history() {
    use std::sync::atomic::{AtomicI64, Ordering};
    const TTL: i64 = 100;
    let clock = Arc::new(AtomicI64::new(1_000));
    let read = clock.clone();
    let registry = Registry::open(
        None,
        Arc::new(move || read.load(Ordering::SeqCst)),
        TTL,
        vec![approval()],
    )
    .expect("registry opens");
    registry
        .register(
            "worker-a",
            &registration("http://127.0.0.1:1/", "inc-1"),
            &verified("inc-1"),
            registry.now(),
        )
        .expect("registration accepted");
    assert_eq!(registry.health(), (1, 1));

    clock.store(1_000 + TTL, Ordering::SeqCst);
    assert_eq!(registry.expire(registry.now()), 1);
    assert_eq!(registry.health(), (1, 0));
    let summary = registry.worker("worker-a").expect("record stays visible");
    assert_eq!(summary.status, "unreachable");
    assert!(summary
        .incarnations
        .iter()
        .any(|entry| entry.incarnation == "inc-1"));

    let renewal = WorkerLeaseRequest {
        incarnation: "inc-2".into(),
        capacity: Capacity::default(),
    };
    let lease = registry
        .renew("worker-a", &renewal, &verified("inc-2"), registry.now())
        .expect("reconnection reconciles");
    assert_eq!(lease.incarnation, "inc-2");
    assert!(lease
        .incarnations
        .iter()
        .any(|entry| entry.incarnation == "inc-1" && entry.superseded_at.is_some()));
}

#[test]
fn forbidden_endpoint_changes_are_rejected_and_audited() {
    let registry =
        Registry::open(None, Arc::new(|| 5_000), 100, vec![approval()]).expect("registry opens");
    let error = registry
        .register(
            "worker-a",
            &registration("http://127.0.0.1:2", "inc-1"),
            &verified("inc-1"),
            registry.now(),
        )
        .expect_err("endpoint change fails");
    assert_eq!(error.code, "worker.endpoint_forbidden");
    assert!(registry
        .audit_entries(Some("worker-a"))
        .iter()
        .any(|entry| entry.outcome == "denied"));
}

#[test]
fn verified_registration_recovers_a_failed_preflight_record() {
    let registry =
        Registry::open(None, Arc::new(|| 7_000), 100, vec![approval()]).expect("registry opens");
    registry
        .mark_unreachable("worker-a", 7_000, "startup verification failed")
        .expect("placeholder record");
    let summary = registry.worker("worker-a").expect("record");
    assert_eq!(summary.status, "unreachable");
    assert_eq!(summary.instance, "worker-a");

    let lease = registry
        .register(
            "worker-a",
            &registration("http://127.0.0.1:1", "inc-1"),
            &verified("inc-1"),
            registry.now(),
        )
        .expect("verified registration replaces the placeholder");
    assert_eq!(lease.instance, "worker-a");
    assert_eq!(lease.status, "registered");
    assert!(lease
        .incarnations
        .iter()
        .any(|entry| entry.incarnation == "inc-1"));
}

#[test]
fn control_tokens_are_hashed_and_lookup_never_retains_plaintext() {
    let directory = tempfile::tempdir().expect("registry directory");
    let path = directory.path().join("registry.json");
    let registry = Registry::open(
        Some(path.clone()),
        Arc::new(|| 9_000),
        100,
        vec![approval()],
    )
    .expect("registry opens");
    assert_eq!(
        registry.control_worker("control-a").as_deref(),
        Some("worker-a")
    );
    assert_eq!(registry.control_worker("other"), None);
    let bytes = std::fs::read_to_string(&path).expect("registry file");
    assert!(!bytes.contains("control-a"), "plain token is not persisted");
    assert!(bytes.contains(&hash_token("control-a")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "registry file is private");
    }
}

#[test]
fn config_seed_conflicts_fail_startup() {
    let mut second = approval();
    second.id = "worker-b".into();
    let error = match Registry::open(None, Arc::new(|| 1), 100, vec![approval(), second]) {
        Ok(_) => panic!("duplicate control token must fail startup"),
        Err(error) => error,
    };
    assert_eq!(error.code, "config.invalid");
}

#[test]
fn runtime_edits_are_not_overwritten_by_config_seed() {
    let directory = tempfile::tempdir().expect("registry directory");
    let path = directory.path().join("registry.json");
    {
        let registry = Registry::open(
            Some(path.clone()),
            Arc::new(|| 10_000),
            100,
            vec![approval()],
        )
        .expect("registry opens");
        let summary = registry.admin_worker("worker-a").expect("seeded approval");
        registry
            .update(
                "worker-a",
                ApprovalPatch {
                    revision: summary.revision,
                    endpoint: Some("http://127.0.0.1:9".into()),
                    ..ApprovalPatch::default()
                },
                "admin",
                11_000,
            )
            .expect("runtime edit");
    }
    let registry = Registry::open(Some(path), Arc::new(|| 12_000), 100, vec![approval()])
        .expect("registry reopens");
    let summary = registry.admin_worker("worker-a").expect("approval");
    assert_eq!(summary.endpoint, "http://127.0.0.1:9");
    assert_eq!(summary.source, "runtime");
}

#[test]
fn legacy_decommissioned_record_is_not_resurrected_by_config_seed() {
    let directory = tempfile::tempdir().expect("registry directory");
    let path = directory.path().join("registry.json");
    let mut document = Document {
        version: 1,
        ..Document::default()
    };
    document.workers.insert(
        "worker-a".into(),
        Record {
            id: "worker-a".into(),
            instance: "worker-a".into(),
            endpoint: "http://127.0.0.1:1".into(),
            package: "general".into(),
            version: "0.1.0".into(),
            protocol: common::PROTOCOL_VERSION.into(),
            capabilities: Vec::new(),
            declared_capabilities: Vec::new(),
            approved_capabilities: Vec::new(),
            locality: LOCALITY_LOCAL.into(),
            incarnation: "inc-1".into(),
            incarnations: Vec::new(),
            capacity: Capacity::default(),
            lease_expires_at: 0,
            renewed_at: 0,
            status: "decommissioned".into(),
        },
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&document).expect("legacy registry JSON"),
    )
    .expect("legacy registry file");

    let registry = Registry::open(Some(path), Arc::new(|| 30_000), 100, vec![approval()])
        .expect("legacy registry opens");
    let summary = registry.admin_worker("worker-a").expect("approval");
    assert_eq!(summary.status, "decommissioned");
    assert!(!summary.enabled);
    assert_eq!(
        registry
            .enable("worker-a", "admin", 30_000)
            .expect_err("terminal identity cannot be enabled")
            .code,
        "worker.decommissioned"
    );
}

#[test]
fn disable_enable_and_decommission_are_terminal_and_audited() {
    let registry =
        Registry::open(None, Arc::new(|| 20_000), 100, vec![approval()]).expect("registry opens");
    registry
        .register(
            "worker-a",
            &registration("http://127.0.0.1:1", "inc-1"),
            &verified("inc-1"),
            registry.now(),
        )
        .expect("registration");

    let disabled = registry
        .disable("worker-a", "admin", 21_000)
        .expect("disable");
    assert_eq!(disabled.status, "disabled");
    assert!(!disabled.reachable);
    assert_eq!(
        registry
            .register(
                "worker-a",
                &registration("http://127.0.0.1:1", "inc-1"),
                &verified("inc-1"),
                21_000,
            )
            .expect_err("disabled registration")
            .code,
        "worker.disabled"
    );
    assert!(registry
        .discover(|_| true, &WorkerDiscoveryFilter::default())
        .is_empty());

    let enabled = registry
        .enable("worker-a", "admin", 22_000)
        .expect("enable");
    assert_eq!(enabled.status, "unreachable");
    assert!(registry
        .register(
            "worker-a",
            &registration("http://127.0.0.1:1", "inc-2"),
            &verified("inc-2"),
            22_000,
        )
        .is_ok());

    let decommissioned = registry
        .decommission("worker-a", "admin", 23_000)
        .expect("decommission");
    assert_eq!(decommissioned.status, "decommissioned");
    assert!(!registry.admin_worker("worker-a").expect("approval").enabled);
    assert!(registry
        .register(
            "worker-a",
            &registration("http://127.0.0.1:1", "inc-3"),
            &verified("inc-3"),
            24_000,
        )
        .is_err());
    assert!(registry
        .audit_entries(Some("worker-a"))
        .iter()
        .any(|entry| entry.action == "worker.disable"));
}
