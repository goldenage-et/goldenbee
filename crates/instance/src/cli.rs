//! `gb-instance` command parsing and output.

use crate::backup::{
    self, BackupRequest, MigrateRequest, RestoreRequest, MANIFEST_FILE, MIGRATION_JOURNAL,
};
use crate::layout::{
    create, inspect, profile, CreateRequest, Inspection, InstanceError, InstanceLimits,
};
use crate::process::{start, stop};
use crate::trust::apply_trust;
use crate::upgrade::{self, UpgradeRequest, WorkPolicy};
use install::TrustPolicy;
use std::path::PathBuf;

pub(crate) fn run(args: &[String]) -> Result<u8, InstanceError> {
    let Some(command) = args.first().map(String::as_str) else {
        help();
        return Ok(0);
    };
    match command {
        "--help" | "-h" | "help" if args.len() == 1 => {
            help();
            Ok(0)
        }
        "--version" | "-V" if args.len() == 1 => {
            println!("gb-instance {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "create" => create_command(&args[1..]),
        "start" => start_command(&args[1..]),
        "inspect" => inspect_command(&args[1..]),
        "stop" => stop_command(&args[1..]),
        "profile" => profile_command(&args[1..]),
        "upgrade" => upgrade_command(&args[1..]),
        "recover" => recover_command(&args[1..]),
        "trust" => trust_command(&args[1..]),
        "backup" => backup_command(&args[1..]),
        "backup-verify" => verify_backup_command(&args[1..]),
        "backup-migrate" => migrate_backup_command(&args[1..]),
        "restore" => restore_command(&args[1..]),
        other if other.starts_with('-') => Err(InstanceError::new(
            "instance.usage",
            format!("unrecognized option '{other}'"),
        )),
        other => Err(InstanceError::new(
            "instance.usage",
            format!("unknown command '{other}'"),
        )),
    }
}

fn create_command(args: &[String]) -> Result<u8, InstanceError> {
    let mut root = None;
    let mut release = None;
    let mut id = None;
    let mut json = false;
    let mut grants = Vec::new();
    let mut secret_refs = Vec::new();
    let mut limits = InstanceLimits::default();
    let mut workspace = None;
    let mut read_only = false;
    let mut config = None;
    let mut default_agent = None;
    let mut token = None;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        match key {
            "--json" => json = true,
            "--read-only" => read_only = true,
            "--root" | "--release" | "--id" | "--grants" | "--workspace" | "--max-active"
            | "--max-queue" | "--per-principal" | "--secret-ref" | "--config"
            | "--default-agent" | "--token" => {
                let value = args.get(index + 1).ok_or_else(|| usage(key))?;
                match key {
                    "--root" => root = Some(PathBuf::from(value)),
                    "--release" => release = Some(PathBuf::from(value)),
                    "--id" => id = Some(value.clone()),
                    "--grants" => grants.extend(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_string),
                    ),
                    "--workspace" => workspace = Some(PathBuf::from(value)),
                    "--max-active" => limits.max_active = number(key, value)?,
                    "--max-queue" => limits.max_queue = number(key, value)?,
                    "--per-principal" => limits.per_principal = number(key, value)?,
                    "--secret-ref" => secret_refs.push(value.clone()),
                    "--config" => config = Some(PathBuf::from(value)),
                    "--default-agent" => default_agent = Some(value.clone()),
                    "--token" => token = Some(value.clone()),
                    _ => {}
                }
                index += 1;
            }
            other => {
                return Err(InstanceError::new(
                    "instance.usage",
                    format!("unrecognized option '{other}'"),
                ));
            }
        }
        index += 1;
    }
    let root = root.ok_or_else(|| usage("--root"))?;
    let release = release.ok_or_else(|| usage("--release"))?;
    let id = id.ok_or_else(|| usage("--id"))?;
    let mut options = CreateRequest::new(root, id, release);
    if !grants.is_empty() {
        options.grants = grants;
    }
    options.workspace = workspace;
    options.read_only = read_only;
    options.limits = limits;
    options.secret_refs = secret_refs;
    options.config = config;
    options.default_agent = default_agent;
    options.token = token;
    let outcome = create(options)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "created": outcome.created,
                "instance": outcome.record.id,
                "package": outcome.record.package,
                "version": outcome.record.version,
                "artifact_digest": outcome.record.artifact_digest,
                "binary_digest": outcome.record.binary_digest,
                "release_dir": outcome.record.release_dir,
                "binary": outcome.record.binary,
                "config": outcome.record.config_path,
                "data_dir": outcome.record.data_dir,
                "workspace_root": outcome.record.workspace_root,
                "workspace_write": outcome.record.workspace_write,
                "grants": outcome.record.grants,
                "limits": outcome.record.limits,
                "secret_refs": outcome.record.secret_refs,
                "seed_assets": outcome.record.seed_assets,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
    } else if outcome.created {
        println!(
            "created instance '{}' from {} {} ({})",
            outcome.record.id,
            outcome.record.package,
            outcome.record.version,
            outcome.record.artifact_digest
        );
        println!("config: {}", outcome.record.config_path.display());
        println!("data: {}", outcome.record.data_dir.display());
    } else {
        println!(
            "instance '{}' already exists for {}",
            outcome.record.id, outcome.record.artifact_digest
        );
    }
    Ok(0)
}

fn start_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &["--addr"])?;
    let outcome = start(
        &options.root()?,
        &options.instance()?,
        options.value("--addr"),
    )?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "instance": outcome.record.id,
                "pid": outcome.state.pid,
                "incarnation": outcome.state.incarnation,
                "lifecycle": outcome.state.lifecycle,
                "process_health": "running",
                "addr": outcome.addr,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!(
            "started instance '{}' pid {} on {}",
            outcome.record.id,
            outcome.state.pid.unwrap_or_default(),
            outcome.addr
        );
    }
    Ok(0)
}

fn inspect_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &[])?;
    let inspection = inspect(&options.root()?, &options.instance()?)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&inspection).unwrap_or_else(|_| "{}".into())
        );
    } else {
        print_inspection(&inspection);
    }
    Ok(0)
}

fn stop_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &[])?;
    let outcome = stop(&options.root()?, &options.instance()?)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "instance": outcome.record.id,
                "lifecycle": outcome.state.lifecycle,
                "process_health": "exited",
                "pid": outcome.state.pid,
                "stopped_at": outcome.state.stopped_at,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!("stopped instance '{}'", outcome.record.id);
    }
    Ok(0)
}

fn profile_command(args: &[String]) -> Result<u8, InstanceError> {
    let mut root = None;
    let mut instance = None;
    let mut passthrough = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--root" | "--instance" => {
                let value = args.get(index + 1).ok_or_else(|| usage(&args[index]))?;
                if args[index] == "--root" {
                    root = Some(PathBuf::from(value));
                } else {
                    instance = Some(value.clone());
                }
                index += 1;
            }
            _ => passthrough.push(args[index].clone()),
        }
        index += 1;
    }
    let root = root.ok_or_else(|| usage("--root"))?;
    let instance = instance.ok_or_else(|| usage("--instance"))?;
    let outcome = profile(&root, &instance, &passthrough)?;
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    Ok(u8::try_from(outcome.status).unwrap_or(1))
}

fn upgrade_command(args: &[String]) -> Result<u8, InstanceError> {
    let mut root = None;
    let mut instance = None;
    let mut release = None;
    let mut authorized = Vec::new();
    let mut work_policy = WorkPolicy::Drain;
    let mut verify_input = None;
    let mut addr = None;
    let mut json = false;
    let mut apply = false;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        match key {
            "--json" => json = true,
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--root" | "--instance" | "--release" | "--authorize" | "--work-policy"
            | "--verify-input" | "--addr" => {
                let value = args.get(index + 1).ok_or_else(|| usage(key))?;
                match key {
                    "--root" => root = Some(PathBuf::from(value)),
                    "--instance" => instance = Some(value.clone()),
                    "--release" => release = Some(PathBuf::from(value)),
                    "--authorize" => authorized.extend(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|entry| !entry.is_empty())
                            .map(str::to_string),
                    ),
                    "--work-policy" => {
                        work_policy = match value.as_str() {
                            "drain" => WorkPolicy::Drain,
                            "pin" => WorkPolicy::Pin,
                            "migrate" => WorkPolicy::Migrate,
                            other => {
                                return Err(InstanceError::new(
                                    "instance.usage",
                                    format!(
                                    "--work-policy must be drain, pin, or migrate, not '{other}'"
                                ),
                                ))
                            }
                        };
                    }
                    "--verify-input" => verify_input = Some(value.clone()),
                    "--addr" => addr = Some(value.clone()),
                    _ => {}
                }
                index += 1;
            }
            other => {
                return Err(InstanceError::new(
                    "instance.usage",
                    format!("unrecognized option '{other}'"),
                ));
            }
        }
        index += 1;
    }
    let root = root.ok_or_else(|| usage("--root"))?;
    let instance = instance.ok_or_else(|| usage("--instance"))?;
    let release = release.ok_or_else(|| usage("--release"))?;
    let mut options = UpgradeRequest::new(root, instance, release);
    options.authorized_permissions = authorized;
    options.work_policy = work_policy;
    options.verify_input = verify_input;
    options.addr = addr;
    if apply {
        let outcome = upgrade::apply(&options)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".into())
            );
        } else {
            print_upgrade_report(&outcome.report);
            println!("upgrade: complete");
            println!(
                "readiness: {} capability_operation: {}",
                outcome.verification.runtime_phase, outcome.verification.capability_operation
            );
            println!(
                "identity_preserved={} profiles_preserved={} history_preserved={}",
                outcome.verification.identity_preserved,
                outcome.verification.profiles_preserved,
                outcome.verification.history_preserved
            );
        }
        return Ok(0);
    }

    let report = upgrade::plan(&options)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
        );
    } else {
        print_upgrade_report(&report);
        if report.blocked.is_empty() {
            println!("upgrade: compatible (dry run; pass --apply to activate)");
        } else {
            println!("upgrade: blocked");
        }
    }
    Ok(u8::from(!report.blocked.is_empty()))
}

fn recover_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &[])?;
    let outcome = upgrade::recover(&options.root()?, &options.instance()?)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!(
            "recovered instance '{}': binary_restored={} data_restored={}",
            outcome.journal.instance, outcome.binary_restored, outcome.data_restored
        );
    }
    Ok(0)
}

fn trust_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &["--trust", "--advisories"])?;
    let root = options.root()?;
    let instance = options.instance()?;
    let trust_path = options.value("--trust").ok_or_else(|| usage("--trust"))?;
    let policy = TrustPolicy::load(std::path::Path::new(trust_path))?;
    let advisory = match options.value("--advisories") {
        Some(path) => Some(std::fs::read(path).map_err(|_| {
            InstanceError::new(
                "install.advisory_unavailable",
                "advisory file is unreadable",
            )
        })?),
        None => None,
    };
    let outcome = apply_trust(&root, &instance, &policy, advisory.as_deref())?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "instance": outcome.record.id,
                "decision": outcome.decision,
                "reason": outcome.reason,
                "advisory_generation": outcome.advisory_generation,
                "lifecycle": outcome.state.lifecycle,
                "pid": outcome.state.pid,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!(
            "instance '{}' trust: {}",
            outcome.record.id, outcome.decision
        );
        println!("reason: {}", outcome.reason);
        if let Some(generation) = outcome.advisory_generation {
            println!("advisory generation: {generation}");
        }
    }
    Ok(0)
}

fn backup_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &["--output"])?;
    let request = BackupRequest {
        root: options.root()?,
        instance: options.instance()?,
        output: PathBuf::from(options.value("--output").ok_or_else(|| usage("--output"))?),
    };
    let outcome = backup::create(&request)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "backup": outcome.path,
                "id": outcome.manifest.id,
                "digest": outcome.digest,
                "files": outcome.manifest.files.len(),
                "manifest": outcome.path.join(MANIFEST_FILE),
            }))
            .unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!(
            "created backup '{}' at {} ({} files)",
            outcome.manifest.id,
            outcome.path.display(),
            outcome.manifest.files.len()
        );
    }
    Ok(0)
}

fn verify_backup_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &["--backup"])?;
    let path = PathBuf::from(options.value("--backup").ok_or_else(|| usage("--backup"))?);
    let report = backup::verify(&path)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!("backup: {}", path.display());
        println!("status: {}", if report.ready { "ready" } else { "blocked" });
        for check in &report.checks {
            println!("{}: {}: {}", check.status, check.name, check.detail);
        }
    }
    Ok(if report.ready { 0 } else { 1 })
}

fn migrate_backup_command(args: &[String]) -> Result<u8, InstanceError> {
    let options = Simple::parse(args, &["--backup", "--output", "--supported-schema"])?;
    let mut request = MigrateRequest::new(
        PathBuf::from(options.value("--backup").ok_or_else(|| usage("--backup"))?),
        PathBuf::from(options.value("--output").ok_or_else(|| usage("--output"))?),
    );
    if let Some(value) = options.value("--supported-schema") {
        request.supported_data_schema = value.parse::<u32>().map_err(|_| {
            InstanceError::new(
                "instance.usage",
                "--supported-schema must be a non-negative integer",
            )
        })?;
    }
    let outcome = backup::migrate(&request)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "target": outcome.path,
                "resumed": outcome.resumed,
                "status": outcome.journal.status,
                "completed": outcome.journal.completed,
                "irreversible": outcome.irreversible,
                "ready": outcome.verification.ready,
                "journal": outcome.path.join(MIGRATION_JOURNAL),
            }))
            .unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!(
            "migrated backup copy at {} (resumed={} status={})",
            outcome.path.display(),
            outcome.resumed,
            outcome.journal.status
        );
        println!("steps: {}", outcome.journal.completed.join(","));
    }
    Ok(0)
}

fn restore_command(args: &[String]) -> Result<u8, InstanceError> {
    let mut options = RestoreOptions::default();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        match key {
            "--json" => options.json = true,
            "--no-launcher-check" => options.verify_launcher = false,
            "--map" | "--secret-available" | "--revoked-secret" => {
                let value = args.get(index + 1).ok_or_else(|| usage(key))?;
                match key {
                    "--map" => {
                        let (source, target) = value.split_once('=').ok_or_else(|| {
                            InstanceError::new("instance.usage", "--map requires SOURCE=TARGET")
                        })?;
                        if source.is_empty() || target.is_empty() {
                            return Err(InstanceError::new(
                                "instance.usage",
                                "--map requires a non-empty SOURCE and TARGET",
                            ));
                        }
                        options
                            .path_map
                            .insert(source.to_string(), PathBuf::from(target));
                    }
                    "--secret-available" => options.available_secrets.push(value.clone()),
                    "--revoked-secret" => options.revoked_secrets.push(value.clone()),
                    _ => {}
                }
                index += 1;
            }
            "--backup" | "--root" | "--instance" | "--release" | "--config" | "--data"
            | "--workspace" | "--gateway-url" | "--gateway-token" => {
                let value = args.get(index + 1).ok_or_else(|| usage(key))?;
                match key {
                    "--backup" => options.backup = Some(PathBuf::from(value)),
                    "--root" => options.root = Some(PathBuf::from(value)),
                    "--instance" => options.instance = Some(value.clone()),
                    "--release" => options.release = Some(PathBuf::from(value)),
                    "--config" => options.config = Some(PathBuf::from(value)),
                    "--data" => options.data = Some(PathBuf::from(value)),
                    "--workspace" => options.workspace = Some(PathBuf::from(value)),
                    "--gateway-url" => options.gateway_url = Some(value.clone()),
                    "--gateway-token" => options.gateway_token = Some(value.clone()),
                    _ => {}
                }
                index += 1;
            }
            other => {
                return Err(InstanceError::new(
                    "instance.usage",
                    format!("unrecognized option '{other}'"),
                ))
            }
        }
        index += 1;
    }
    let mut request = RestoreRequest::new(
        options.backup.ok_or_else(|| usage("--backup"))?,
        options.root.ok_or_else(|| usage("--root"))?,
    );
    request.instance = options.instance;
    request.release = options.release;
    request.config = options.config;
    request.data = options.data;
    request.workspace = options.workspace;
    request.path_map = options.path_map;
    request.available_secrets = options.available_secrets;
    request.revoked_secrets = options.revoked_secrets;
    request.verify_launcher = options.verify_launcher;
    request.gateway_url = options.gateway_url;
    request.gateway_token = options.gateway_token;
    let report = backup::restore(&request)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!("instance: {}", report.instance);
        println!("status: {}", report.status);
        println!(
            "launcher: {}: {}",
            report.launcher.status, report.launcher.detail
        );
        println!(
            "gateway: {}: {}",
            report.gateway.status, report.gateway.detail
        );
        for check in &report.checks {
            if check.status != "ok" {
                println!("{}: {}: {}", check.status, check.name, check.detail);
            }
        }
        for task in &report.blocked_tasks {
            println!("blocked task: {}", task.task_id);
            for reason in &task.reasons {
                println!("  {reason}");
            }
        }
    }
    Ok(if report.ready { 0 } else { 1 })
}

struct RestoreOptions {
    backup: Option<PathBuf>,
    root: Option<PathBuf>,
    instance: Option<String>,
    release: Option<PathBuf>,
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    workspace: Option<PathBuf>,
    path_map: std::collections::BTreeMap<String, PathBuf>,
    available_secrets: Vec<String>,
    revoked_secrets: Vec<String>,
    verify_launcher: bool,
    gateway_url: Option<String>,
    gateway_token: Option<String>,
    json: bool,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            backup: None,
            root: None,
            instance: None,
            release: None,
            config: None,
            data: None,
            workspace: None,
            path_map: std::collections::BTreeMap::new(),
            available_secrets: Vec::new(),
            revoked_secrets: Vec::new(),
            verify_launcher: true,
            gateway_url: None,
            gateway_token: None,
            json: false,
        }
    }
}

fn print_upgrade_report(report: &upgrade::UpgradeReport) {
    println!("instance: {}", report.instance);
    println!(
        "current: {} {} ({})",
        report.current.package, report.current.version, report.current.artifact_digest
    );
    println!(
        "candidate: {} {} ({})",
        report.candidate.package, report.candidate.version, report.candidate.artifact_digest
    );
    println!(
        "platform: current={} candidate={} changed={}",
        report.platform.current.join(","),
        report.platform.candidate.join(","),
        report.platform.changed
    );
    println!(
        "signature: current={} candidate={} verified={}",
        report.signature.current, report.signature.candidate, report.signature.verified
    );
    println!(
        "protocol: {} -> {}-{}",
        report.protocol.candidate, report.protocol.minimum, report.protocol.maximum
    );
    println!(
        "runtime: {} -> {}",
        report.runtime.current.join(","),
        report.runtime.candidate.join(",")
    );
    println!(
        "capabilities added: {}",
        report.capabilities.added.join(",")
    );
    println!(
        "capabilities removed: {}",
        report.capabilities.removed.join(",")
    );
    println!("permissions added: {}", report.permissions.added.join(","));
    println!(
        "authorization: required={} satisfied={}",
        report.authorization.required.join(","),
        report.authorization.satisfied
    );
    println!(
        "schema: {} -> {} ({})",
        report.schema_change.current, report.schema_change.candidate, report.schema_change.status
    );
    println!(
        "profiles: {} revisions, teams: {}",
        report.profiles.len(),
        report.teams.len()
    );
    println!(
        "work: active={} queued={} policy={} action={}",
        report.work.active, report.work.queued, report.work.policy, report.work.action
    );
    println!(
        "config: {} (generated={} preflight={})",
        report.config.path.display(),
        report.config.generated,
        report.config.preflight_ok
    );
    for reference in &report.config.references {
        println!(
            "  {}: {} exists={}",
            reference.kind,
            reference.path.display(),
            reference.exists
        );
    }
    println!(
        "launcher: candidate={} verified={}",
        report.launcher.candidate.join(","),
        report.launcher.verified.join(",")
    );
    for finding in &report.blocked {
        println!("blocked: {}: {}", finding.code, finding.message);
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn print_inspection(inspection: &Inspection) {
    println!("instance: {}", inspection.instance);
    println!(
        "package: {} {} ({})",
        inspection.package, inspection.version, inspection.artifact_digest
    );
    println!("signer: {}", inspection.signer);
    println!("binary: {}", inspection.binary.display());
    println!("config: {}", inspection.config_path.display());
    println!("data: {}", inspection.data_dir.display());
    println!("workspace: {}", inspection.workspace_root.display());
    println!("process_health: {}", inspection.process_health);
    println!("lifecycle: {}", inspection.lifecycle);
    println!("task_activity: {}", inspection.task_activity);
    if let Some(phase) = &inspection.runtime_phase {
        println!("runtime_phase: {phase}");
    }
    if let Some(profile) = &inspection.active_profile {
        println!("active_profile: {profile}");
    }
    if let Some(pid) = inspection.pid {
        println!("pid: {pid}");
    }
    if let Some(incarnation) = &inspection.incarnation {
        println!("incarnation: {incarnation}");
    }
    if let Some(capacity) = &inspection.capacity {
        println!(
            "capacity: active={} limit={} queued={} queue_limit={}",
            capacity.active, capacity.limit, capacity.queued, capacity.queue_limit
        );
    }
    println!("grants: {}", inspection.grants.join(","));
    println!("workspace_write: {}", inspection.workspace_write);
    if let Some(error) = &inspection.last_error {
        println!("last_error: {error}");
    }
    match &inspection.trust {
        Some(trust) => {
            println!("trust: {}", trust.decision);
            println!("trust_reason: {}", trust.reason);
            if let Some(generation) = trust.advisory_generation {
                println!("trust_advisory_generation: {generation}");
            }
        }
        None => println!("trust: not-evaluated"),
    }
}

struct Simple {
    root: Option<PathBuf>,
    instance: Option<String>,
    json: bool,
    values: std::collections::BTreeMap<String, String>,
}

impl Simple {
    fn parse(args: &[String], value_flags: &[&str]) -> Result<Self, InstanceError> {
        let mut options = Self {
            root: None,
            instance: None,
            json: false,
            values: std::collections::BTreeMap::new(),
        };
        let mut index = 0;
        while index < args.len() {
            let key = args[index].as_str();
            match key {
                "--json" => options.json = true,
                "--root" | "--instance" => {
                    let value = args.get(index + 1).ok_or_else(|| usage(key))?;
                    if key == "--root" {
                        options.root = Some(PathBuf::from(value));
                    } else {
                        options.instance = Some(value.clone());
                    }
                    index += 1;
                }
                flag if value_flags.contains(&flag) => {
                    let value = args.get(index + 1).ok_or_else(|| usage(flag))?;
                    options.values.insert(flag.to_string(), value.clone());
                    index += 1;
                }
                other => {
                    return Err(InstanceError::new(
                        "instance.usage",
                        format!("unrecognized option '{other}'"),
                    ));
                }
            }
            index += 1;
        }
        Ok(options)
    }

    fn root(&self) -> Result<PathBuf, InstanceError> {
        self.root.clone().ok_or_else(|| usage("--root"))
    }

    fn instance(&self) -> Result<String, InstanceError> {
        self.instance.clone().ok_or_else(|| usage("--instance"))
    }

    fn value(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }
}

fn number(flag: &str, value: &str) -> Result<usize, InstanceError> {
    value.parse::<usize>().map_err(|_| {
        InstanceError::new(
            "instance.usage",
            format!("{flag} must be a non-negative integer"),
        )
    })
}

fn usage(option: &str) -> InstanceError {
    InstanceError::new(
        "instance.usage",
        format!("{option} is required or missing its value"),
    )
}

fn help() {
    println!(
        "Usage: gb-instance <command> [options]\n\n\
         Commands:\n  \
         create --root <instances-root> --release <verified-release> --id <id> [options]\n      \
             Create one instance from a verified release directory under packages/sha256/<digest>.\n      \
             Options: --grants task:submit,task:read --workspace <path> --read-only\n      \
                      --max-active <n> --max-queue <n> --per-principal <n>\n      \
                      --secret-ref <reference> --config <worker.toml> --default-agent <id>\n  \
         start --root <instances-root> --instance <id> [--addr 127.0.0.1:port] [--json]\n      \
             Start the recorded verified executable and wait for real readiness.\n  \
         inspect --root <instances-root> --instance <id> [--json]\n      \
             Show process health, lifecycle, and task activity as distinct fields.\n  \
         stop --root <instances-root> --instance <id> [--json]\n      \
             Send SIGTERM to the recorded process only.\n  \
         trust --root <instances-root> --instance <id> --trust <policy.json> [--advisories <signed-advisory.json>] [--json]\n      \
             Apply an explicit continue, restrict, or quarantine decision using local evidence only.\n  \
         profile --root <instances-root> --instance <id> <profile arguments...>\n      \
             Proxy to the installed binary's native profile subcommands.\n  \
         upgrade --root <instances-root> --instance <id> --release <verified-release> [options]\n      \
             Dry-run compatibility and authority differences without modifying the instance.\n      \
             Add --apply to drain work, snapshot data, activate, and verify the candidate.\n      \
             Options: --apply --authorize task:submit,exec:command\n      \
                      --work-policy drain|pin|migrate --verify-input <input> --addr <addr>\n  \
         recover --root <instances-root> --instance <id> [--json]\n      \
             Restore the previous binary and data snapshot after an interrupted upgrade.\n  \
         backup --root <instances-root> --instance <id> --output <directory> [--json]\n      \
             Cold-copy a stopped instance with per-file digests and a portable manifest.\n  \
         backup-verify --backup <directory> [--json]\n      \
             Validate manifest schema, file digests, package references, and owner mappings.\n  \
         backup-migrate --backup <directory> --output <new-copy> [--supported-schema <n>] [--json]\n      \
             Exercise a resumable migration on a copy; newer state is never rewritten.\n  \
         restore --backup <directory> --root <instances-root> [options]\n      \
             Validate content, package, assets, secrets, identity, and access before readiness.\n      \
             Options: --instance <id> --release <verified-release> --config <path>\n      \
                      --data <path> --workspace <path> --map SOURCE=TARGET\n      \
                      --secret-available <ref> --revoked-secret <ref>\n      --gateway-url <url> --gateway-token <token> --no-launcher-check\n  \
         --version\n\n\
         Instance lifecycle is local operator tooling. Production restart remains\n\
         the host supervisor's responsibility."
    );
}
