//! Goldenbee Gateway executable.
//!
//! `gb-gateway serve --config <path>` loads approved Workers and caller token
//! bindings from an operator file. No agent loop or Worker runtime is linked.

use std::process::{Command, ExitCode, Stdio};

fn help() {
    println!(
        "Usage: gb-gateway <command> [options]\n\n\
         Commands:\n  \
         serve --config <path> [--addr <address>]\n      \
         Route authenticated clients to approved Worker Instances.\n  \
         backup --config <path> --output <directory>\n      \
         Snapshot the standalone dispatch and registry stores with a portable manifest.\n  \
         backup-verify --backup <directory> [--json]\n      \
         Validate Gateway backup content, schema, identity, and execution mappings.\n  \
         restore --backup <directory> --config <path> [--not-within <worker-root>]\n      \
                 [--revoked-worker <id>] [--revoked-approval <id>] [--json]\n      \
         Restore the Gateway projection into its own clean store and validate access.\n  \
         up --config <path> [--addr <address>] [--json]\n      Start the Gateway in the background and wait for readiness.\n  \
         status --config <path> [--json]\n      Report background process and health.\n  \
         down --config <path> [--json]\n      Stop the background Gateway.\n  \
         pair --config <path> [--json]\n      Print a single goldenbee:// client pairing link.\n  \
         provider list|show --id <id> [--url URL] [--token TOKEN] [--json]\n      \
         Inspect the Gateway provider catalog without secrets.\n  \
         provider add --id <id> --adapter <kind> [--model <id>] [--endpoint URL] [--modality text]\n      \
                 [--allowed-model <id>]... [--auto-default] [--tool-calling] [--context-window N]\n      \
                 [--local] [--residency LABEL] [--worker <id>]... [--url URL] [--token TOKEN] [--json]\n      Create provider metadata and run discovery where supported.\n  \
         provider update --id <id> --revision N [metadata flags] [--enable|--disable] [--json]\n      Update provider metadata with optimistic concurrency; endpoint and filter changes rediscover.\n  \
         provider discover --id <id> [--url URL] [--token TOKEN] [--json]\n      Refresh discovered models and advance the provider revision.\n  \
         provider credential set --id <id> [--url URL] [--token TOKEN] [--json]\n      Read the provider API key from stdin only. The key is never accepted on argv.\n  \
         provider credential revoke --id <id> [--url URL] [--token TOKEN] [--json]\n      Revoke the active credential and stop new envelopes.\n  \
         provider pair --worker <id> --instance <name> [--url URL] [--token TOKEN] [--json]\n      Mint a one-time Worker enrollment code. The code contains no provider key.\n  \
         --version\n\n\
         Environment: GOLDENBEE_DELEGATION_SECRET (shared Worker signing secret).\n\
         Provider admin environment: GOLDENBEE_GATEWAY_URL, GOLDENBEE_ADMIN_TOKEN."
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    if args.len() == 1 && matches!(command, "--help" | "-h" | "help") {
        help();
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 && matches!(command, "--version" | "-V") {
        println!(concat!("gb-gateway ", env!("CARGO_PKG_VERSION")));
        return ExitCode::SUCCESS;
    }
    match command {
        "serve" => serve(&args[1..]),
        "backup" => backup(&args[1..]),
        "backup-verify" => backup_verify(&args[1..]),
        "restore" => restore(&args[1..]),
        "up" => gateway_lifecycle(&args[1..], "up"),
        "status" => gateway_lifecycle(&args[1..], "status"),
        "down" => gateway_lifecycle(&args[1..], "down"),
        "pair" => gateway_pair(&args[1..]),
        "provider" => provider(&args[1..]),
        _ => {
            eprintln!("gb-gateway: unknown command");
            ExitCode::from(2)
        }
    }
}

fn serve(args: &[String]) -> ExitCode {
    let mut config_path = None;
    let mut addr = None;
    let mut rest = args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--config" => match rest.next() {
                Some(value) => config_path = Some(value.clone()),
                None => {
                    eprintln!("gb-gateway: --config requires a value");
                    return ExitCode::from(2);
                }
            },
            "--addr" => match rest.next() {
                Some(value) => addr = Some(value.clone()),
                None => {
                    eprintln!("gb-gateway: --addr requires a value");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("gb-gateway: unrecognized option");
                return ExitCode::from(2);
            }
        }
    }
    let Some(config_path) = config_path else {
        eprintln!("gb-gateway: serve requires --config <path>");
        return ExitCode::from(2);
    };
    let config = match goldenbee_gateway::load_config(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            return ExitCode::from(1);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("gb-gateway: async runtime could not start");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(goldenbee_gateway::serve(config, addr)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            ExitCode::from(1)
        }
    }
}

fn backup(args: &[String]) -> ExitCode {
    let mut config_path = None;
    let mut output = None;
    let mut rest = args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--config" => config_path = rest.next().cloned(),
            "--output" => output = rest.next().cloned(),
            _ => {
                eprintln!("gb-gateway: unrecognized option");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(config_path), Some(output)) = (config_path, output) else {
        eprintln!("gb-gateway: backup requires --config <path> and --output <directory>");
        return ExitCode::from(2);
    };
    let config = match goldenbee_gateway::load_config(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            return ExitCode::from(1);
        }
    };
    if config.core.store().kind != "file" {
        eprintln!(
            "gb-gateway: backup.store_unsupported: shared {} stores require their native dump procedure; keep that backup separate from Worker state",
            config.core.store().kind
        );
        return ExitCode::from(1);
    }
    let request = gateway_core::backup::BackupRequest {
        data_dir: config.core.data_dir(),
        registry: config.core.registry_path(),
        output: std::path::PathBuf::from(output),
    };
    match gateway_core::backup::create(&request) {
        Ok(outcome) => {
            println!(
                "created Gateway backup '{}' at {} ({} files)",
                outcome.manifest.id,
                outcome.path.display(),
                outcome.manifest.files.len()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            ExitCode::from(1)
        }
    }
}

fn backup_verify(args: &[String]) -> ExitCode {
    let Some(path) = args
        .iter()
        .position(|argument| argument == "--backup")
        .and_then(|index| args.get(index + 1))
    else {
        eprintln!("gb-gateway: backup-verify requires --backup <directory>");
        return ExitCode::from(2);
    };
    let json = args.iter().any(|argument| argument == "--json");
    match gateway_core::backup::verify(std::path::Path::new(path)) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!("status: {}", if report.ready { "ready" } else { "blocked" });
                for check in &report.checks {
                    println!("{}: {}: {}", check.status, check.name, check.detail);
                }
            }
            if report.ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            ExitCode::from(1)
        }
    }
}

fn restore(args: &[String]) -> ExitCode {
    let mut backup = None;
    let mut config_path = None;
    let mut not_within = None;
    let mut revoked_workers = Vec::new();
    let mut revoked_approvals = Vec::new();
    let mut json = false;
    let mut rest = args.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--json" => json = true,
            "--backup" => backup = rest.next().cloned(),
            "--config" => config_path = rest.next().cloned(),
            "--not-within" => not_within = rest.next().cloned(),
            "--revoked-worker" => {
                if let Some(value) = rest.next() {
                    revoked_workers.push(value.clone());
                }
            }
            "--revoked-approval" => {
                if let Some(value) = rest.next() {
                    revoked_approvals.push(value.clone());
                }
            }
            _ => {
                eprintln!("gb-gateway: unrecognized option");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(backup), Some(config_path)) = (backup, config_path) else {
        eprintln!("gb-gateway: restore requires --backup <directory> and --config <path>");
        return ExitCode::from(2);
    };
    let config = match goldenbee_gateway::load_config(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            return ExitCode::from(1);
        }
    };
    if config.core.store().kind != "file" {
        eprintln!(
            "gb-gateway: restore.store_unsupported: shared {} stores restore through their native procedure; do not merge them into a Worker database",
            config.core.store().kind
        );
        return ExitCode::from(1);
    }
    let request = gateway_core::backup::RestoreRequest {
        backup: std::path::PathBuf::from(backup),
        data_dir: config.core.data_dir(),
        registry: config.core.registry_path(),
        not_within: not_within.map(std::path::PathBuf::from),
        caller_config: Some(std::path::PathBuf::from(config_path)),
        revoked_workers,
        revoked_approvals,
    };
    match gateway_core::backup::restore(&request) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!("status: {}", report.status);
                println!("gb-gateway: {}", report.gateway_id);
                println!("access: {}: {}", report.access.status, report.access.detail);
                for check in &report.checks {
                    if check.status != "ok" {
                        println!("{}: {}: {}", check.status, check.name, check.detail);
                    }
                }
            }
            if report.ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            ExitCode::from(1)
        }
    }
}

fn lifecycle_args(args: &[String]) -> Result<(std::path::PathBuf, Option<String>, bool), ()> {
    let mut config = None;
    let mut addr = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "--config" | "--addr" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("gb-gateway: {} requires a value", args[index]);
                    return Err(());
                };
                if args[index] == "--config" {
                    config = Some(std::path::PathBuf::from(value));
                } else {
                    addr = Some(value.clone());
                }
                index += 2;
                continue;
            }
            _ => {
                eprintln!("gb-gateway: unrecognized option");
                return Err(());
            }
        }
        index += 1;
    }
    let Some(config) = config else {
        eprintln!("gb-gateway: this command requires --config <path>");
        return Err(());
    };
    Ok((config, addr, json))
}

fn gateway_health(addr: &str) -> Option<serde_json::Value> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?
        .get(format!("http://{addr}/v1/health"))
        .send()
        .ok()?
        .json()
        .ok()
}

fn gateway_lifecycle(args: &[String], command: &str) -> ExitCode {
    let (config_path, addr, json) = match lifecycle_args(args) {
        Ok(values) => values,
        Err(()) => return ExitCode::from(2),
    };
    let config = match goldenbee_gateway::load_config(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            return ExitCode::from(1);
        }
    };
    let pid_path = config.core.data_dir().join("gateway.pid");
    let address = addr.unwrap_or_else(|| config.listen.clone());
    match command {
        "up" => {
            if let Some(pid) = gateway_pid(&pid_path) {
                if gateway_pid_alive(pid) {
                    println!("gb-gateway up: pid={pid} addr={address} (already running)");
                    return ExitCode::SUCCESS;
                }
            }
            if let Err(error) = std::fs::create_dir_all(config.core.data_dir()) {
                eprintln!("gb-gateway: storage.unavailable: {error}");
                return ExitCode::from(1);
            }
            let log_path = config.core.data_dir().join("gateway.log");
            let log = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                Ok(log) => log,
                Err(error) => {
                    eprintln!("gb-gateway: storage.unavailable: {error}");
                    return ExitCode::from(1);
                }
            };
            let log_err = match log.try_clone() {
                Ok(log) => log,
                Err(error) => {
                    eprintln!("gb-gateway: storage.unavailable: {error}");
                    return ExitCode::from(1);
                }
            };
            let exe = match std::env::current_exe() {
                Ok(exe) => exe,
                Err(error) => {
                    eprintln!("gb-gateway: executable: {error}");
                    return ExitCode::from(1);
                }
            };
            let child = match Command::new(exe)
                .args([
                    "serve",
                    "--config",
                    &config_path.display().to_string(),
                    "--addr",
                    &address,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::from(log))
                .stderr(Stdio::from(log_err))
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    eprintln!("gb-gateway: start: {error}");
                    return ExitCode::from(1);
                }
            };
            if let Err(error) = std::fs::write(&pid_path, child.id().to_string()) {
                eprintln!("gb-gateway: pid: {error}");
                return ExitCode::from(1);
            }
            for _ in 0..150 {
                if gateway_health(&address).is_some() {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({"status":"running","pid":child.id(),"addr":address,"log":log_path})
                        );
                    } else {
                        println!(
                            "gb-gateway up: pid={} addr={} log={}",
                            child.id(),
                            address,
                            log_path.display()
                        );
                    }
                    return ExitCode::SUCCESS;
                }
                if !gateway_pid_alive(child.id()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = Command::new("kill")
                .args(["-KILL", &child.id().to_string()])
                .status();
            let _ = std::fs::remove_file(&pid_path);
            eprintln!("gb-gateway: start_failed: see {}", log_path.display());
            ExitCode::from(1)
        }
        "status" => {
            let pid = gateway_pid(&pid_path);
            let health = if pid.is_some_and(gateway_pid_alive) {
                gateway_health(&address)
            } else {
                None
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": if health.is_some() { "running" } else { "stopped" },
                        "pid": pid,
                        "listen": address,
                        "health": health,
                    })
                );
            } else {
                println!(
                    "gb-gateway: {}",
                    if health.is_some() {
                        "running"
                    } else {
                        "stopped"
                    }
                );
                println!("listen: {address}");
                if let Some(pid) = pid {
                    println!("pid: {pid}");
                }
                if let Some(health) = health {
                    println!("health: {}", health);
                }
            }
            ExitCode::SUCCESS
        }
        "down" => {
            let Some(pid) = gateway_pid(&pid_path) else {
                println!("gb-gateway down: not running");
                return ExitCode::SUCCESS;
            };
            if gateway_pid_alive(pid) {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
                for _ in 0..150 {
                    if !gateway_pid_alive(pid) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if gateway_pid_alive(pid) {
                    let _ = Command::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .status();
                }
            }
            let _ = std::fs::remove_file(&pid_path);
            println!("gb-gateway down: pid={pid} addr={address}");
            ExitCode::SUCCESS
        }
        _ => unreachable!(),
    }
}

#[derive(Default)]
struct ProviderOptions {
    url: Option<String>,
    token: Option<String>,
    json: bool,
    id: Option<String>,
    adapter: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    modalities: Vec<String>,
    allowed_models: Vec<String>,
    auto_default: bool,
    tool_calling: bool,
    context_window: Option<u64>,
    local: bool,
    residency: Option<String>,
    workers: Vec<String>,
    revision: Option<u64>,
    active: Option<bool>,
    worker: Option<String>,
    instance: Option<String>,
}

fn parse_provider_options(args: &[String]) -> Result<ProviderOptions, ()> {
    let mut options = ProviderOptions::default();
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        match argument {
            "--json" => options.json = true,
            "--tool-calling" => options.tool_calling = true,
            "--local" => options.local = true,
            "--auto-default" => options.auto_default = true,
            "--enable" => options.active = Some(true),
            "--disable" => options.active = Some(false),
            "--url" | "--token" | "--id" | "--adapter" | "--model" | "--endpoint"
            | "--modality" | "--allowed-model" | "--context-window" | "--residency"
            | "--worker" | "--instance" | "--revision" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("gb-gateway: provider: {argument} requires a value");
                    return Err(());
                };
                match argument {
                    "--url" => options.url = Some(value.clone()),
                    "--token" => options.token = Some(value.clone()),
                    "--id" => options.id = Some(value.clone()),
                    "--adapter" => options.adapter = Some(value.clone()),
                    "--model" => options.model = Some(value.clone()),
                    "--endpoint" => options.endpoint = Some(value.clone()),
                    "--modality" => options.modalities.push(value.clone()),
                    "--allowed-model" => options.allowed_models.push(value.clone()),
                    "--context-window" => {
                        options.context_window = Some(value.parse().map_err(|_| {
                            eprintln!("gb-gateway: provider: --context-window must be an integer");
                        })?)
                    }
                    "--residency" => options.residency = Some(value.clone()),
                    "--worker" => {
                        options.workers.push(value.clone());
                        if options.worker.is_none() {
                            options.worker = Some(value.clone());
                        }
                    }
                    "--instance" => options.instance = Some(value.clone()),
                    "--revision" => {
                        options.revision = Some(value.parse().map_err(|_| {
                            eprintln!("gb-gateway: provider: --revision must be an integer");
                        })?)
                    }
                    _ => {}
                }
                index += 2;
                continue;
            }
            other => {
                eprintln!("gb-gateway: provider: unrecognized option '{other}'");
                return Err(());
            }
        }
        index += 1;
    }
    Ok(options)
}

fn provider(args: &[String]) -> ExitCode {
    let Some(subcommand) = args.first().map(String::as_str) else {
        eprintln!("gb-gateway: provider requires list, show, add, update, credential, or pair");
        return ExitCode::from(2);
    };
    let option_start = if subcommand == "credential" { 2 } else { 1 };
    let options = match args.get(option_start..) {
        Some(rest) => match parse_provider_options(rest) {
            Ok(options) => options,
            Err(()) => return ExitCode::from(2),
        },
        None => ProviderOptions::default(),
    };
    let (base, token) = match provider_endpoint(&options) {
        Ok(values) => values,
        Err(()) => return ExitCode::from(2),
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            eprintln!("gb-gateway: provider: HTTP client could not be built");
            return ExitCode::from(2);
        }
    };
    match subcommand {
        "list" => provider_send(
            client
                .get(format!("{base}/v1/admin/model-providers"))
                .bearer_auth(token),
            options.json,
        ),
        "show" => {
            let Some(id) = options.id else {
                eprintln!("gb-gateway: provider show requires --id");
                return ExitCode::from(2);
            };
            provider_send(
                client
                    .get(format!("{base}/v1/admin/model-providers/{id}"))
                    .bearer_auth(token),
                options.json,
            )
        }
        "add" => {
            let (Some(id), Some(adapter), Some(residency)) =
                (options.id, options.adapter, options.residency)
            else {
                eprintln!("gb-gateway: provider add requires --id, --adapter, and --residency");
                return ExitCode::from(2);
            };
            let modalities = if options.modalities.is_empty() {
                vec!["text".to_string()]
            } else {
                options.modalities
            };
            let body = serde_json::json!({
                "id": id,
                "adapter": adapter,
                "model": options.model.unwrap_or_default(),
                "endpoint": options.endpoint,
                "modalities": modalities,
                "tool_calling": options.tool_calling,
                "context_window": options.context_window,
                "local": options.local,
                "residency": residency,
                "allowed_workers": options.workers,
                "allowed_models": (!options.allowed_models.is_empty()).then_some(options.allowed_models),
                "allow_auto_default": options.auto_default,
            });
            provider_send(
                client
                    .post(format!("{base}/v1/admin/model-providers"))
                    .bearer_auth(token)
                    .json(&body),
                options.json,
            )
        }
        "update" => {
            let (Some(id), Some(revision)) = (options.id, options.revision) else {
                eprintln!("gb-gateway: provider update requires --id and --revision");
                return ExitCode::from(2);
            };
            let mut body = serde_json::json!({"revision": revision});
            let object = body.as_object_mut().expect("object");
            if let Some(adapter) = options.adapter {
                object.insert("adapter".into(), adapter.into());
            }
            if let Some(model) = options.model {
                object.insert("model".into(), model.into());
            }
            if let Some(endpoint) = options.endpoint {
                object.insert("endpoint".into(), endpoint.into());
            }
            if !options.modalities.is_empty() {
                object.insert("modalities".into(), options.modalities.into());
            }
            if options.tool_calling {
                object.insert("tool_calling".into(), true.into());
            }
            if !options.allowed_models.is_empty() {
                object.insert("allowed_models".into(), options.allowed_models.into());
            }
            if options.auto_default {
                object.insert("allow_auto_default".into(), true.into());
            }
            if let Some(window) = options.context_window {
                object.insert("context_window".into(), window.into());
            }
            if options.local {
                object.insert("local".into(), true.into());
            }
            if let Some(residency) = options.residency {
                object.insert("residency".into(), residency.into());
            }
            if !options.workers.is_empty() {
                object.insert("allowed_workers".into(), options.workers.into());
            }
            if let Some(active) = options.active {
                object.insert("active".into(), active.into());
            }
            provider_send(
                client
                    .patch(format!("{base}/v1/admin/model-providers/{id}"))
                    .bearer_auth(token)
                    .json(&body),
                options.json,
            )
        }
        "credential" => {
            let Some(action) = args.get(1).map(String::as_str) else {
                eprintln!("gb-gateway: provider credential requires set or revoke");
                return ExitCode::from(2);
            };
            let Some(id) = options.id else {
                eprintln!("gb-gateway: provider credential requires --id");
                return ExitCode::from(2);
            };
            match action {
                "set" => {
                    use std::io::Read;
                    let mut key = String::new();
                    if std::io::stdin().read_to_string(&mut key).is_err() {
                        eprintln!("gb-gateway: provider credential set: cannot read stdin");
                        return ExitCode::from(1);
                    }
                    let key = key.trim().to_string();
                    if key.is_empty() {
                        eprintln!(
                            "gb-gateway: provider credential set: stdin did not contain a key"
                        );
                        return ExitCode::from(2);
                    }
                    provider_send(
                        client
                            .put(format!("{base}/v1/admin/model-providers/{id}/credential"))
                            .bearer_auth(token)
                            .json(&serde_json::json!({"key": key})),
                        options.json,
                    )
                }
                "revoke" => provider_send(
                    client
                        .delete(format!("{base}/v1/admin/model-providers/{id}/credential"))
                        .bearer_auth(token),
                    options.json,
                ),
                other => {
                    eprintln!("gb-gateway: provider credential: unknown action '{other}'");
                    ExitCode::from(2)
                }
            }
        }
        "discover" => {
            let Some(id) = options.id else {
                eprintln!("gb-gateway: provider discover requires --id");
                return ExitCode::from(2);
            };
            provider_send(
                client
                    .post(format!("{base}/v1/admin/model-providers/{id}/discover"))
                    .bearer_auth(token),
                options.json,
            )
        }
        "pair" => {
            let (Some(worker), Some(instance)) = (options.worker, options.instance) else {
                eprintln!("gb-gateway: provider pair requires --worker and --instance");
                return ExitCode::from(2);
            };
            provider_send(
                client
                    .post(format!("{base}/v1/admin/model-providers/pair-code"))
                    .bearer_auth(token)
                    .json(&serde_json::json!({"worker_id": worker, "instance": instance})),
                options.json,
            )
        }
        other => {
            eprintln!("gb-gateway: provider: unknown subcommand '{other}'");
            ExitCode::from(2)
        }
    }
}

fn provider_endpoint(options: &ProviderOptions) -> Result<(String, String), ()> {
    let base = options
        .url
        .clone()
        .or_else(|| std::env::var("GOLDENBEE_GATEWAY_URL").ok())
        .ok_or_else(|| {
            eprintln!("gb-gateway: provider: --url or GOLDENBEE_GATEWAY_URL is required");
        })?;
    let token = options
        .token
        .clone()
        .or_else(|| std::env::var("GOLDENBEE_ADMIN_TOKEN").ok())
        .ok_or_else(|| {
            eprintln!("gb-gateway: provider: --token or GOLDENBEE_ADMIN_TOKEN is required");
        })?;
    Ok((base.trim_end_matches('/').to_string(), token))
}

fn provider_send(request: reqwest::blocking::RequestBuilder, compact: bool) -> ExitCode {
    let response = match request.send() {
        Ok(response) => response,
        Err(_) => {
            eprintln!("gb-gateway: provider: Gateway request did not complete");
            return ExitCode::from(5);
        }
    };
    let status = response.status();
    let body: serde_json::Value = match response.json() {
        Ok(body) => body,
        Err(_) => {
            eprintln!("gb-gateway: provider: Gateway returned a non-JSON response");
            return ExitCode::from(1);
        }
    };
    if !status.is_success() {
        let code = body["code"].as_str().unwrap_or("provider.request_failed");
        let message = body["message"]
            .as_str()
            .unwrap_or("Gateway rejected the provider request");
        eprintln!("gb-gateway: {code}: {message}");
        return ExitCode::from(1);
    }
    if compact {
        println!(
            "{}",
            serde_json::to_string(&body).unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())
        );
    }
    ExitCode::SUCCESS
}

fn gateway_pid(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn gateway_pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn gateway_pair(args: &[String]) -> ExitCode {
    let (config_path, _, json) = match lifecycle_args(args) {
        Ok(values) => values,
        Err(()) => return ExitCode::from(2),
    };
    let config = match goldenbee_gateway::load_config(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("gb-gateway: {}: {}", error.code, error.message);
            return ExitCode::from(1);
        }
    };
    let Some(caller) = config.callers.first() else {
        eprintln!("gb-gateway: pair: configuration has no caller token");
        return ExitCode::from(1);
    };
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("gateway", &format!("http://{}", config.listen))
        .append_pair("caller", &caller.id)
        .append_pair("namespace", &caller.namespace)
        .append_pair("token", &caller.token);
    let link = format!("goldenbee://pair/client?{}", query.finish());
    if json {
        println!(
            "{}",
            serde_json::json!({"link":link,"gateway":format!("http://{}", config.listen),"caller":caller.id})
        );
    } else {
        println!("{link}");
        eprintln!("warning: pair link contains a caller token; share it only with the intended Client operator");
    }
    ExitCode::SUCCESS
}
