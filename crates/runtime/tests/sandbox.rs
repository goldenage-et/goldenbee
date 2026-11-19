//! OS containment checks for native command execution.
//!
//! These tests drive the real `bubblewrap` boundary and inspect observable
//! host effects: files, sockets, credentials, process tables, and resource
//! limits. They do not assert on internal path-string checks.

use runtime::native::sandbox::{Sandbox, SandboxPolicy};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn workspace(root: &Path) -> PathBuf {
    let path = root.join("workspace");
    fs::create_dir_all(&path).expect("workspace exists");
    path
}

fn sandbox(root: &Path) -> Sandbox {
    Sandbox::new(SandboxPolicy::new(workspace(root))).expect("bubblewrap boundary is available")
}

fn processes_containing(marker: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if cmdline
            .split(|byte| *byte == 0)
            .any(|arg| arg == marker.as_bytes())
        {
            found.push(pid);
        }
    }
    found
}

async fn wait_for_no_process(marker: &str) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let found = processes_containing(marker);
        if found.is_empty() || Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn authorized_command_observes_explicit_mounts_environment_and_limits() {
    let root = tempfile::tempdir().unwrap();
    let workspace = workspace(root.path());
    std::env::set_var("GOLDENBEE_T32_HOST_SECRET", "host-credential-canary");
    let policy = SandboxPolicy::new(&workspace)
        .with_timeout(Duration::from_secs(5))
        .with_environment("ALLOWED", "yes");
    let sandbox = Sandbox::new(policy).expect("boundary is available");

    let output = sandbox
        .exec(
            r#"printf 'allowed=%s secret=%s\n' "$ALLOWED" "${GOLDENBEE_T32_HOST_SECRET:-absent}"
pwd
printf 'payload' > artifact.txt
cat artifact.txt
/bin/bash -c 'ulimit -v; ulimit -u; ulimit -f; ulimit -n; ulimit -t'"#,
            None,
        )
        .await
        .expect("command completes inside the boundary");

    std::env::remove_var("GOLDENBEE_T32_HOST_SECRET");
    assert_eq!(output.exit_code, 0, "stderr: {}", output.stderr);
    assert!(!output.timed_out);
    assert!(output.stdout.contains("allowed=yes"), "{}", output.stdout);
    assert!(
        output.stdout.contains("secret=absent"),
        "inherited credential leaked: {}",
        output.stdout
    );
    assert!(output.stdout.contains("/workspace"), "{}", output.stdout);
    assert!(output.stdout.contains("payload"), "{}", output.stdout);
    // 512 MiB, 4096 processes, 16 MiB files, 256 descriptors, 5 CPU seconds.
    for expected in ["524288", "4096", "16384", "256", "5"] {
        assert!(
            output.stdout.contains(expected),
            "missing resource limit {expected}: {}",
            output.stdout
        );
    }
    assert_eq!(
        fs::read_to_string(workspace.join("artifact.txt")).unwrap(),
        "payload"
    );
}

#[tokio::test]
async fn forbidden_paths_and_symlink_escape_are_invisible() {
    let root = tempfile::tempdir().unwrap();
    let workspace = workspace(root.path());
    let host_secret = root.path().join("host-secret.txt");
    fs::write(&host_secret, "HOST-FILE-CANARY").unwrap();
    std::os::unix::fs::symlink(&host_secret, workspace.join("escape")).unwrap();
    std::os::unix::fs::symlink("/etc/shadow", workspace.join("shadow")).unwrap();
    let sandbox = sandbox(root.path());

    let output = sandbox
        .exec(
            "cat escape; cat shadow; cat /etc/shadow; cat ../../etc/shadow; ls /home",
            None,
        )
        .await
        .expect("the sandbox still reports command output");

    assert_ne!(output.exit_code, 0);
    assert!(
        !output.stdout.contains("HOST-FILE-CANARY"),
        "host file escaped through a symlink: {}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("root:"),
        "host shadow file was readable: {}",
        output.stdout
    );
    assert!(
        output.stderr.contains("No such file") || output.stderr.contains("not found"),
        "unexpected stderr: {}",
        output.stderr
    );
    // The host file remains untouched and outside the mounted workspace.
    assert_eq!(
        fs::read_to_string(&host_secret).unwrap(),
        "HOST-FILE-CANARY"
    );
}

#[tokio::test]
async fn network_destinations_are_unreachable_and_host_socket_stays_idle() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("host listener");
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let sandbox = sandbox(root.path());

    let output = sandbox
        .exec(
            &format!("/bin/bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'"),
            None,
        )
        .await
        .expect("network attempt returns an observable failure");

    assert_ne!(output.exit_code, 0, "network unexpectedly succeeded");
    assert!(
        listener.accept().is_err(),
        "host listener received a sandbox connection"
    );
}

#[tokio::test]
async fn privilege_escalation_is_blocked_by_the_os_boundary() {
    let root = tempfile::tempdir().unwrap();
    let sandbox = sandbox(root.path());

    let output = sandbox
        .exec(
            "id -u; grep CapEff /proc/self/status; mkdir -p /tmp/m; mount -t tmpfs tmpfs /tmp/m; echo mount=$?; chroot / /bin/true; echo chroot=$?; unshare -Ur true; echo unshare=$?",
            None,
        )
        .await
        .expect("privilege probes return observable outcomes");

    assert!(output.stdout.contains("65534"), "{}", output.stdout);
    assert!(
        output.stdout.contains("CapEff:\t0000000000000000"),
        "capabilities were not dropped: {}",
        output.stdout
    );
    assert!(
        output.stdout.contains("mount=32") || output.stdout.contains("mount=1"),
        "mount did not fail: {}",
        output.stdout
    );
    assert!(
        output.stdout.contains("chroot=1")
            || output.stdout.contains("chroot=126")
            || output.stdout.contains("chroot=32"),
        "chroot did not fail: {}",
        output.stdout
    );
    assert!(
        !output.stdout.contains("unshare=0"),
        "nested user namespace succeeded: {}",
        output.stdout
    );
}

#[tokio::test]
async fn timeout_kills_the_process_group_and_descendants() {
    let root = tempfile::tempdir().unwrap();
    let marker = format!("t32-timeout-{}", std::process::id());
    let policy =
        SandboxPolicy::new(workspace(root.path())).with_timeout(Duration::from_millis(400));
    let sandbox = Sandbox::new(policy).unwrap();

    let started = Instant::now();
    let output = sandbox
        .exec(
            &format!("trap '' TERM; /bin/bash -c 'exec -a {marker} sleep 300' & wait"),
            None,
        )
        .await
        .expect("timeout is reported, not hidden");

    assert!(output.timed_out, "command was not marked timed out");
    assert_eq!(output.exit_code, -1);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout took too long: {:?}",
        started.elapsed()
    );
    let remaining = wait_for_no_process(&marker).await;
    assert!(
        remaining.is_empty(),
        "timed-out descendants survived: {remaining:?}"
    );
}

#[tokio::test]
async fn cancellation_kills_the_process_group_and_descendants() {
    let root = tempfile::tempdir().unwrap();
    let marker = format!("t32-cancel-{}", std::process::id());
    let sandbox = sandbox(root.path());
    let token = CancellationToken::new();
    let cancelled = token.clone();

    let command = format!("trap '' TERM; /bin/bash -c 'exec -a {marker} sleep 300' & wait");
    let task = tokio::spawn(async move {
        sandbox
            .exec_with_cancel(&command, None, cancelled.cancelled())
            .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    token.cancel();
    let output = task.await.unwrap().expect("cancellation is observable");

    assert!(output.cancelled, "command was not marked cancelled");
    let remaining = wait_for_no_process(&marker).await;
    assert!(
        remaining.is_empty(),
        "cancelled descendants survived: {remaining:?}"
    );
}

#[tokio::test]
async fn dropping_the_execution_future_kills_descendants() {
    let root = tempfile::tempdir().unwrap();
    let marker = format!("t32-shutdown-{}", std::process::id());
    let sandbox = sandbox(root.path());
    let command = format!("trap '' TERM; /bin/bash -c 'exec -a {marker} sleep 300' & wait");
    let task = tokio::spawn(async move { sandbox.exec(command.as_str(), None).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    task.abort();
    let _ = task.await;

    let remaining = wait_for_no_process(&marker).await;
    assert!(
        remaining.is_empty(),
        "aborted execution left descendants: {remaining:?}"
    );
}

#[tokio::test]
async fn output_limit_terminates_the_command_and_its_descendants() {
    let root = tempfile::tempdir().unwrap();
    let marker = format!("t32-output-{}", std::process::id());
    let policy = SandboxPolicy::new(workspace(root.path())).with_output_limit(1024);
    let sandbox = Sandbox::new(policy).unwrap();

    let error = sandbox
        .exec(
            &format!("/bin/bash -c 'exec -a {marker} yes t32-output'"),
            None,
        )
        .await
        .expect_err("unbounded output must fail explicitly");

    assert_eq!(error.code, "sandbox.output_limit");
    let remaining = wait_for_no_process(&marker).await;
    assert!(
        remaining.is_empty(),
        "output-limited descendants survived: {remaining:?}"
    );
}

#[tokio::test]
async fn separate_boundaries_use_independent_identities_and_resource_policies() {
    let root = tempfile::tempdir().unwrap();
    let first = SandboxPolicy::new(root.path().join("first"))
        .with_identity(65533, 65533)
        .with_limits(runtime::native::sandbox::SandboxLimits {
            // Keep this above the host user's existing thread count. On a busy
            // developer machine, RLIMIT_NPROC is evaluated for the real user,
            // not only for processes inside the new namespace.
            max_processes: 8192,
            ..Default::default()
        });
    let second = SandboxPolicy::new(root.path().join("second"))
        .with_identity(65534, 65534)
        .with_limits(runtime::native::sandbox::SandboxLimits {
            max_processes: 16384,
            ..Default::default()
        });
    let first = Sandbox::new(first).unwrap();
    let second = Sandbox::new(second).unwrap();

    let first_output = first
        .exec("id -u; /bin/bash -c 'ulimit -u'", None)
        .await
        .unwrap();
    let second_output = second
        .exec("id -u; /bin/bash -c 'ulimit -u'", None)
        .await
        .unwrap();

    assert!(
        first_output.stdout.contains("65533"),
        "stdout={:?} stderr={:?} exit={}",
        first_output.stdout,
        first_output.stderr,
        first_output.exit_code
    );
    assert!(
        first_output.stdout.contains("8192"),
        "stdout={:?} stderr={:?} exit={}",
        first_output.stdout,
        first_output.stderr,
        first_output.exit_code
    );
    assert!(
        second_output.stdout.contains("65534"),
        "stdout={:?} stderr={:?} exit={}",
        second_output.stdout,
        second_output.stderr,
        second_output.exit_code
    );
    assert!(
        second_output.stdout.contains("16384"),
        "stdout={:?} stderr={:?} exit={}",
        second_output.stdout,
        second_output.stderr,
        second_output.exit_code
    );
}

#[tokio::test]
async fn unavailable_required_mechanism_fails_closed_without_host_fallback() {
    let root = tempfile::tempdir().unwrap();
    let workspace = workspace(root.path());
    let canary = root.path().join("host-fallback-canary");
    let policy = SandboxPolicy::new(&workspace).with_bubblewrap("/nonexistent/t32-bwrap");
    let result = Sandbox::new(policy);

    let error = match result {
        Ok(sandbox) => sandbox
            .exec(&format!("touch {}", canary.display()), None)
            .await
            .expect_err("a missing mechanism cannot execute commands"),
        Err(error) => error,
    };
    assert_eq!(error.code, "sandbox.unavailable");
    assert!(
        !canary.exists(),
        "command fell back to unrestricted host execution"
    );

    let no_limiter = SandboxPolicy::new(&workspace).with_prlimit("/nonexistent/t32-prlimit");
    assert_eq!(
        Sandbox::new(no_limiter).unwrap_err().code,
        "sandbox.unavailable"
    );
}
