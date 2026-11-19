//! Offline Worker package installer command.

use install::{InstallOutcome, Installer, TrustPolicy};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gb-install: {}: {}", error.code, error.message);
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> Result<(), install::InstallError> {
    let command = args.first().map(String::as_str).unwrap_or("");
    match command {
        "--help" | "-h" | "help" if args.len() == 1 => {
            help();
            Ok(())
        }
        "--help" | "-h" | "help" => Err(install::InstallError::new(
            "install.usage",
            "help does not accept additional arguments",
        )),
        "" => {
            help();
            Ok(())
        }
        "--version" | "-V" => {
            println!("gb-install {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "inspect" => {
            let options = Options::parse(&args[1..])?;
            let artifact = options.required_artifact()?;
            let installer = options.installer(options.root.clone().unwrap_or_else(dry_root))?;
            let inspection = installer.inspect(&artifact)?;
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "package": inspection.metadata.package.name,
                        "version": inspection.metadata.package.version,
                        "artifact_digest": inspection.artifact_digest,
                        "metadata_digest": inspection.metadata_digest,
                        "source_digest": inspection.metadata.source_digest,
                        "source_revision": inspection.metadata.source_revision,
                        "binary_digest": inspection.metadata.binary_digest,
                        "target": inspection.metadata.target,
                        "signer": inspection.metadata.signer,
                        "runtime": inspection.metadata.package.runtime,
                        "adk": inspection.metadata.package.adk,
                        "protocol": inspection.metadata.package.protocol,
                        "protocol_min": inspection.metadata.protocol_range().0,
                        "protocol_max": inspection.metadata.protocol_range().1,
                        "features": inspection.metadata.package.features,
                        "capabilities": inspection.metadata.package.capabilities,
                        "permissions": inspection.metadata.package.required_permissions,
                        "launcher_modes": inspection.metadata.package.launcher_modes,
                        "hooks": inspection.metadata.package.hooks,
                        "build": inspection.metadata.build,
                        "trust": inspection.trust,
                    }))
                    .unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!(
                    "package: {} {}",
                    inspection.metadata.package.name, inspection.metadata.package.version
                );
                println!("artifact: {}", inspection.artifact_digest);
                println!("source: {}", inspection.metadata.source_digest);
                println!("source revision: {}", inspection.metadata.source_revision);
                println!("binary: {}", inspection.metadata.binary_digest);
                println!("signer: {}", inspection.metadata.signer);
                println!("target: {}", inspection.metadata.target);
                println!("runtime: {}", inspection.metadata.package.runtime);
                println!("adk: {}", inspection.metadata.package.adk);
                println!(
                    "protocol: {}..={}",
                    inspection.metadata.protocol_range().0,
                    inspection.metadata.protocol_range().1
                );
                println!(
                    "trust: {} tier={} advisory={}",
                    inspection.trust.decision,
                    inspection.trust.signer_tier,
                    inspection
                        .trust
                        .advisory_generation
                        .map(|generation| generation.to_string())
                        .unwrap_or_else(|| "not-required".into())
                );
                println!("reason: {}", inspection.trust.reason);
                println!(
                    "build: {} {} ({})",
                    inspection.metadata.build.toolchain,
                    inspection.metadata.build.profile,
                    inspection.metadata.build.evidence_digest
                );
                for capability in &inspection.metadata.package.capabilities {
                    println!("capability: {} ({})", capability.id, capability.kind);
                }
            }
            Ok(())
        }
        "install" => {
            let options = Options::parse(&args[1..])?;
            let artifact = options.required_artifact()?;
            let root = options.root.clone().ok_or_else(|| {
                install::InstallError::new(
                    "install.usage",
                    "install requires --root <verified package cache>",
                )
            })?;
            let installer = options.installer(root)?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    install::InstallError::new("install.storage_unavailable", error.to_string())
                })?;
            let outcome = runtime.block_on(installer.install(&artifact))?;
            let release = outcome.release();
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "outcome": if outcome.installed_now() { "installed" } else { "converged" },
                        "package": release.package,
                        "version": release.version,
                        "artifact_digest": release.artifact_digest,
                        "signer": release.signer,
                        "target": release.target,
                        "advisory_generation": release.advisory_generation,
                        "directory": release.directory.display().to_string(),
                        "binary": release.binary.display().to_string(),
                        "registered": release.registered,
                        "launcher_modes": release.launcher_modes,
                    }))
                    .unwrap_or_else(|_| "{}".into())
                );
            } else if let InstallOutcome::Converged(_) = outcome {
                println!(
                    "{} {} already installed at {}",
                    release.package, release.version, release.artifact_digest
                );
            } else {
                println!(
                    "installed {} {} at {}",
                    release.package, release.version, release.artifact_digest
                );
                println!("binary: {}", release.binary.display());
            }
            Ok(())
        }
        "trust" => {
            let options = Options::parse(&args[1..])?;
            let policy = TrustPolicy::load(&options.required_trust()?)?;
            let snapshot = policy.snapshot();
            let advisory = match &options.advisories {
                Some(path) => {
                    let bytes = std::fs::read(path).map_err(|_| {
                        install::InstallError::new(
                            "install.advisory_unavailable",
                            "advisory file is unreadable",
                        )
                    })?;
                    let prior = match &options.root {
                        Some(root) => {
                            install::Installer::new(root, policy.clone()).advisory_state()?
                        }
                        None => None,
                    };
                    let verified =
                        policy.verify_advisory(&bytes, prior.as_ref(), common::now_ms())?;
                    Some(serde_json::json!({
                        "authority": verified.authority,
                        "generation": verified.payload.generation,
                        "issued_at": verified.payload.issued_at,
                        "expires_at": verified.payload.expires_at,
                        "payload_digest": verified.payload_digest,
                        "revoked_releases": verified.payload.revocations.iter()
                            .filter(|revocation| revocation.kind == "release")
                            .map(|revocation| serde_json::json!({
                                "package": revocation.package,
                                "version": revocation.version,
                                "digest": revocation.digest,
                                "reason": revocation.reason,
                            }))
                            .collect::<Vec<_>>(),
                        "revoked_signers": verified.payload.revocations.iter()
                            .filter(|revocation| revocation.kind == "signer")
                            .map(|revocation| serde_json::json!({
                                "signer": revocation.signer,
                                "reason": revocation.reason,
                            }))
                            .collect::<Vec<_>>(),
                    }))
                }
                None => None,
            };
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "policy": snapshot,
                        "advisory": advisory,
                    }))
                    .unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!("policy: {}", snapshot.policy_digest);
                println!(
                    "advisory freshness: {}s required={}",
                    snapshot.advisory_freshness_secs, snapshot.require_advisory
                );
                for root in &snapshot.signer_roots {
                    println!(
                        "signer: {} tier={} key={}",
                        root.signer, root.tier, root.public_key_fingerprint
                    );
                }
                for key in &snapshot.advisory_keys {
                    println!(
                        "advisory key: {} key={} not_after={:?}",
                        key.id, key.public_key_fingerprint, key.not_after
                    );
                }
                println!(
                    "rotation overlap: {}s",
                    snapshot.rotation.advisory_key_overlap_secs
                );
                println!(
                    "offline: revoked={} missing={}",
                    snapshot.offline.installed_revoked, snapshot.offline.installed_without_advisory
                );
                match advisory {
                    Some(advisory) => println!(
                        "advisory: {} generation {}",
                        advisory["authority"].as_str().unwrap_or(""),
                        advisory["generation"].as_u64().unwrap_or_default()
                    ),
                    None => println!("advisory: not supplied"),
                }
            }
            Ok(())
        }
        "release" => {
            let action = args.get(1).map(String::as_str).unwrap_or("");
            match action {
                "keygen" => {
                    if !args.iter().any(|argument| argument == "--test-only") {
                        return Err(install::InstallError::new("install.usage", "keygen requires --test-only; production keys must be provisioned outside this tool"));
                    }
                    let key_file = flag_value(&args[2..], "--key-file")?;
                    let public_file = optional_flag(&args[2..], "--public-file");
                    let (seed, public) = install::release::keygen();
                    use std::io::Write;
                    use std::os::unix::fs::OpenOptionsExt;
                    let mut key = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&key_file)
                        .map_err(|_| {
                            install::InstallError::new(
                                "release.storage_unavailable",
                                "test key destination must be new and writable",
                            )
                        })?;
                    key.write_all(format!("{seed}\n").as_bytes()).map_err(|_| {
                        install::InstallError::new(
                            "release.storage_unavailable",
                            "signing key could not be written",
                        )
                    })?;
                    if let Some(path) = &public_file {
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o644)
                            .open(path)
                            .and_then(|mut file| file.write_all(format!("{public}\n").as_bytes()))
                            .map_err(|_| {
                                install::InstallError::new(
                                    "release.storage_unavailable",
                                    "public key could not be written",
                                )
                            })?;
                    }
                    if args.iter().any(|argument| argument == "--json") {
                        println!(
                            "{}",
                            serde_json::json!({
                                "key_file": key_file.display().to_string(),
                                "public_file": public_file.map(|path| path.display().to_string()),
                                "public_key": public,
                            })
                        );
                    } else {
                        println!("signing key: {}", key_file.display());
                        println!("public key: {public}");
                    }
                    Ok(())
                }
                "assemble" => {
                    let spec = flag_value(&args[2..], "--spec")?;
                    let output = flag_value(&args[2..], "--output")?;
                    let key = flag_value(&args[2..], "--signing-key")?;
                    let report = install::release::assemble(&spec, &output, &key)?;
                    if args.iter().any(|argument| argument == "--json") {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
                        );
                    } else {
                        println!(
                            "assembled {} ({} packages) at {}",
                            report.release,
                            report.manifest.packages.len(),
                            report.output.display()
                        );
                        for package in &report.manifest.packages {
                            println!(
                                "package: {} {} {}",
                                package.name, package.version, package.artifact_digest
                            );
                        }
                        println!("checksums: {}", report.checksums_digest);
                    }
                    Ok(())
                }
                "verify" => {
                    let release = flag_value(&args[2..], "--release")?;
                    let key = flag_value(&args[2..], "--trusted-key")?;
                    let key = std::fs::read_to_string(key).map_err(|_| {
                        install::InstallError::new(
                            "release.key_unavailable",
                            "trusted public key is unreadable",
                        )
                    })?;
                    let manifest = install::release::verify(&release, &key)?;
                    if args.iter().any(|argument| argument == "--json") {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".into())
                        );
                    } else {
                        println!(
                            "verified {} from {} at {}",
                            manifest.release, manifest.source.revision, manifest.source.tree_digest
                        );
                        for package in &manifest.packages {
                            println!(
                                "package: {} {} {}",
                                package.name, package.version, package.artifact_digest
                            );
                        }
                    }
                    Ok(())
                }
                other => Err(install::InstallError::new(
                    "install.usage",
                    format!(
                        "unknown release command '{other}'; expected keygen, assemble, or verify"
                    ),
                )),
            }
        }
        "publish" => {
            let options = Options::parse(&args[1..])?;
            let artifact = options.required_artifact()?;
            let registry = options.required_registry()?;
            let token = options.required_token()?;
            let publisher = options.required_publisher()?;
            let bundle = install::bundle_from_layout(&artifact)?;
            let client = install::RegistryClient::new(registry, token)?;
            let runtime = runtime()?;
            let receipt = runtime.block_on(client.publish(&publisher, &bundle))?;
            if options.public {
                runtime.block_on(client.set_visibility(
                    &publisher,
                    &receipt.entry.package,
                    true,
                ))?;
            }
            if options.json {
                let mut value =
                    serde_json::to_value(&receipt).unwrap_or_else(|_| serde_json::json!({}));
                value["visibility"] =
                    serde_json::json!(if options.public { "public" } else { "private" });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!(
                    "{} {} {} {}",
                    receipt.outcome,
                    receipt.entry.package,
                    receipt.entry.version,
                    receipt.entry.artifact_digest
                );
                println!(
                    "visibility: {}",
                    if options.public { "public" } else { "private" }
                );
            }
            Ok(())
        }
        "visibility" => {
            let options = Options::parse(&args[1..])?;
            let (publisher, package) = options.required_package()?;
            let public = match (options.public, options.private) {
                (true, false) => true,
                (false, true) => false,
                _ => {
                    return Err(install::InstallError::new(
                        "install.usage",
                        "visibility requires exactly one of --public or --private",
                    ))
                }
            };
            let client = install::RegistryClient::new(
                options.required_registry()?,
                options.required_token()?,
            )?;
            let runtime = runtime()?;
            runtime.block_on(client.set_visibility(&publisher, &package, public))?;
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "publisher": publisher,
                        "package": package,
                        "visibility": if public { "public" } else { "private" },
                    }))
                    .unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!(
                    "{} {} visibility: {}",
                    publisher,
                    package,
                    if public { "public" } else { "private" }
                );
            }
            Ok(())
        }
        "discover" => {
            let options = Options::parse(&args[1..])?;
            let client = install::RegistryClient::public(options.required_registry()?)?;
            let (publisher, package) = match options.package.clone() {
                Some(package) => {
                    let (publisher, name) = package.split_once('/').ok_or_else(|| {
                        install::InstallError::new(
                            "install.usage",
                            "--package must be <publisher>/<name>",
                        )
                    })?;
                    (Some(publisher.to_string()), Some(name.to_string()))
                }
                None => (options.publisher.clone(), None),
            };
            let query = install::PublicQuery {
                publisher,
                package,
                capability: options.capability.clone(),
                target: options.target.clone(),
                runtime: options.runtime.clone(),
                adk: options.adk.clone(),
                protocol: options.protocol.clone(),
                limit: options.limit,
            };
            let runtime = runtime()?;
            let entries = runtime.block_on(client.discover(&query))?;
            let policy = options.optional_trust()?;
            let advisory = match &policy {
                Some(policy) => options.verified_advisory(policy)?,
                None => None,
            };
            let mut results = Vec::with_capacity(entries.len());
            for entry in &entries {
                let mut value =
                    serde_json::to_value(entry).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(policy) = &policy {
                    let status = policy.discovery_status(
                        &entry.entry.package,
                        &entry.entry.version,
                        &entry.entry.artifact_digest,
                        &entry.entry.signer,
                        advisory.as_ref(),
                    );
                    value["local_trust"] =
                        serde_json::to_value(status).unwrap_or_else(|_| serde_json::json!({}));
                }
                results.push(value);
            }
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "packages": results,
                        "local_trust_evaluated": policy.is_some(),
                        "installation_requires_local_trust_policy": true,
                    }))
                    .unwrap_or_else(|_| "{\"packages\":[]}".into())
                );
            } else if entries.is_empty() {
                println!("no public Worker Packages match");
            } else {
                for (entry, value) in entries.iter().zip(&results) {
                    println!(
                        "{} {} {}",
                        entry.entry.package, entry.entry.version, entry.entry.artifact_digest
                    );
                    println!(
                        "  publisher: {} kind: {} trust: {}",
                        entry.entry.publisher, value["kind"], value["publisher_trust"]["status"]
                    );
                    if let Some(local) = value.get("local_trust") {
                        println!(
                            "  local trust: {} ({})",
                            local["status"].as_str().unwrap_or(""),
                            local["reason"].as_str().unwrap_or("")
                        );
                    }
                    println!(
                        "  target: {} runtime: {} adk: {} protocol: {}..={}",
                        entry.entry.target,
                        entry.entry.runtime,
                        entry.entry.adk,
                        entry.entry.protocol_min,
                        entry.entry.protocol_max
                    );
                    println!("  permissions: {}", entry.entry.permissions.join(","));
                }
            }
            Ok(())
        }
        "registry" => {
            let action = args.get(1).map(String::as_str).unwrap_or("");
            match action {
                "list" | "show" => {
                    let options = Options::parse(&args[2..])?;
                    let (publisher, package) = options.required_package()?;
                    let client = install::RegistryClient::new(
                        options.required_registry()?,
                        options.required_token()?,
                    )?;
                    let runtime = runtime()?;
                    let versions = runtime.block_on(client.list(&publisher, &package))?;
                    if action == "list" {
                        if options.json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&versions)
                                    .unwrap_or_else(|_| "[]".into())
                            );
                        } else {
                            for entry in versions {
                                println!(
                                    "{} {} {} {} runtime={} adk={} protocol={}..={} source={}",
                                    entry.package,
                                    entry.version,
                                    entry.artifact_digest,
                                    entry.target,
                                    entry.runtime,
                                    entry.adk,
                                    entry.protocol_min,
                                    entry.protocol_max,
                                    entry.source_revision
                                );
                            }
                        }
                    } else {
                        let version = options.required_version()?;
                        let entry = versions
                            .into_iter()
                            .find(|entry| entry.version == version)
                            .ok_or_else(|| {
                                install::InstallError::new(
                                    "install.registry_not_found",
                                    "release version is not published",
                                )
                            })?;
                        if options.json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&entry)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                        } else {
                            println!(
                                "{} {} {}",
                                entry.package, entry.version, entry.artifact_digest
                            );
                            println!("source revision: {}", entry.source_revision);
                            println!("target: {}", entry.target);
                            println!(
                                "runtime: {} adk: {} protocol: {}..={}",
                                entry.runtime, entry.adk, entry.protocol_min, entry.protocol_max
                            );
                            println!("build: {}", entry.build.evidence_digest);
                        }
                    }
                    Ok(())
                }
                "install" => {
                    let options = Options::parse(&args[2..])?;
                    let (publisher, package) = options.required_package()?;
                    let digest = options.required_digest()?;
                    let root = options.root.clone().ok_or_else(|| {
                        install::InstallError::new(
                            "install.usage",
                            "registry install requires --root <verified package cache>",
                        )
                    })?;
                    let client = match options.optional_token() {
                        Some(token) => {
                            install::RegistryClient::new(options.required_registry()?, token)?
                        }
                        None => install::RegistryClient::public(options.required_registry()?)?,
                    };
                    let installer = options.installer(root)?;
                    let runtime = runtime()?;
                    let outcome = runtime
                        .block_on(client.install(&publisher, &package, &digest, &installer))?;
                    let release = outcome.release();
                    if options.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "outcome": if outcome.installed_now() { "installed" } else { "converged" },
                                "package": release.package,
                                "version": release.version,
                                "artifact_digest": release.artifact_digest,
                                "advisory_generation": release.advisory_generation,
                                "directory": release.directory.display().to_string(),
                                "binary": release.binary.display().to_string(),
                            }))
                            .unwrap_or_else(|_| "{}".into())
                        );
                    } else {
                        println!(
                            "installed {} {} at {}",
                            release.package, release.version, release.artifact_digest
                        );
                        println!("binary: {}", release.binary.display());
                    }
                    Ok(())
                }
                other => Err(install::InstallError::new(
                    "install.usage",
                    format!("unknown registry command '{other}'"),
                )),
            }
        }
        "list" => {
            let options = Options::parse(&args[1..])?;
            let root = options.root.ok_or_else(|| {
                install::InstallError::new("install.usage", "list requires --root")
            })?;
            let installer = Installer::new(root, TrustPolicy::default());
            let installed = installer.installed()?;
            if options.json {
                let entries: Vec<_> = installed
                    .iter()
                    .map(|release| {
                        serde_json::json!({
                            "package": release.package,
                            "version": release.version,
                            "artifact_digest": release.artifact_digest,
                            "signer": release.signer,
                            "target": release.target,
                            "directory": release.directory.display().to_string(),
                            "registered": release.registered,
                            "launcher_modes": release.launcher_modes,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into())
                );
            } else {
                for release in installed {
                    println!(
                        "{} {} {}",
                        release.package, release.version, release.artifact_digest
                    );
                }
            }
            Ok(())
        }
        other => Err(install::InstallError::new(
            "install.usage",
            format!("unknown command '{other}'"),
        )),
    }
}

fn dry_root() -> PathBuf {
    std::env::temp_dir().join("goldenbee-installer-inspect")
}

fn runtime() -> Result<tokio::runtime::Runtime, install::InstallError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            install::InstallError::new("install.storage_unavailable", error.to_string())
        })
}

fn flag_value(args: &[String], flag: &str) -> Result<PathBuf, install::InstallError> {
    optional_flag(args, flag).ok_or_else(|| {
        install::InstallError::new("install.usage", format!("{flag} <path> is required"))
    })
}

fn optional_flag(args: &[String], flag: &str) -> Option<PathBuf> {
    args.iter()
        .position(|argument| argument == flag)
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
}

struct Options {
    artifact: Option<PathBuf>,
    trust: Option<PathBuf>,
    advisories: Option<PathBuf>,
    root: Option<PathBuf>,
    registry: Option<String>,
    token: Option<String>,
    package: Option<String>,
    version: Option<String>,
    digest: Option<String>,
    publisher: Option<String>,
    capability: Option<String>,
    target: Option<String>,
    runtime: Option<String>,
    adk: Option<String>,
    protocol: Option<String>,
    limit: Option<usize>,
    public: bool,
    private: bool,
    json: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, install::InstallError> {
        let mut options = Self {
            artifact: None,
            trust: None,
            advisories: None,
            root: None,
            registry: None,
            token: None,
            package: None,
            version: None,
            digest: None,
            publisher: None,
            capability: None,
            target: None,
            runtime: None,
            adk: None,
            protocol: None,
            limit: None,
            public: false,
            private: false,
            json: false,
        };
        let mut index = 0;
        while index < args.len() {
            let key = args[index].as_str();
            match key {
                "--json" => options.json = true,
                "--public" => options.public = true,
                "--private" => options.private = true,
                "--artifact" | "--trust" | "--advisories" | "--root" | "--registry" | "--token"
                | "--package" | "--version" | "--digest" | "--publisher" | "--capability"
                | "--target" | "--runtime" | "--adk" | "--protocol" | "--limit" => {
                    let value = args.get(index + 1).ok_or_else(|| {
                        install::InstallError::new(
                            "install.usage",
                            format!("{key} requires a value"),
                        )
                    })?;
                    match key {
                        "--artifact" => options.artifact = Some(PathBuf::from(value)),
                        "--trust" => options.trust = Some(PathBuf::from(value)),
                        "--advisories" => options.advisories = Some(PathBuf::from(value)),
                        "--root" => options.root = Some(PathBuf::from(value)),
                        "--registry" => options.registry = Some(value.clone()),
                        "--token" => options.token = Some(value.clone()),
                        "--package" => options.package = Some(value.clone()),
                        "--version" => options.version = Some(value.clone()),
                        "--digest" => options.digest = Some(value.clone()),
                        "--capability" => options.capability = Some(value.clone()),
                        "--target" => options.target = Some(value.clone()),
                        "--runtime" => options.runtime = Some(value.clone()),
                        "--adk" => options.adk = Some(value.clone()),
                        "--protocol" => options.protocol = Some(value.clone()),
                        "--limit" => {
                            options.limit = Some(value.parse::<usize>().map_err(|_| {
                                install::InstallError::new(
                                    "install.usage",
                                    "--limit must be a positive integer",
                                )
                            })?)
                        }
                        _ => options.publisher = Some(value.clone()),
                    }
                    index += 1;
                }
                other => {
                    return Err(install::InstallError::new(
                        "install.usage",
                        format!("unrecognized option '{other}'"),
                    ));
                }
            }
            index += 1;
        }
        Ok(options)
    }

    fn installer(&self, root: PathBuf) -> Result<Installer, install::InstallError> {
        let policy = TrustPolicy::load(&self.required_trust()?)?;
        let installer = Installer::new(root, policy);
        match &self.advisories {
            Some(path) => installer.with_advisory_file(path),
            None => Ok(installer),
        }
    }

    fn required_artifact(&self) -> Result<PathBuf, install::InstallError> {
        self.artifact.clone().ok_or_else(|| {
            install::InstallError::new("install.usage", "an artifact path is required")
        })
    }

    fn required_trust(&self) -> Result<PathBuf, install::InstallError> {
        self.trust.clone().ok_or_else(|| {
            install::InstallError::new("install.usage", "a trust policy path is required")
        })
    }

    fn required_registry(&self) -> Result<String, install::InstallError> {
        self.registry.clone().ok_or_else(|| {
            install::InstallError::new("install.usage", "--registry <url> is required")
        })
    }

    fn required_token(&self) -> Result<String, install::InstallError> {
        self.token
            .clone()
            .or_else(|| std::env::var("GOLDENBEE_REGISTRY_TOKEN").ok())
            .ok_or_else(|| {
                install::InstallError::new(
                    "install.usage",
                    "--token or GOLDENBEE_REGISTRY_TOKEN is required",
                )
            })
    }

    fn optional_token(&self) -> Option<String> {
        self.token
            .clone()
            .or_else(|| std::env::var("GOLDENBEE_REGISTRY_TOKEN").ok())
    }

    fn optional_trust(&self) -> Result<Option<TrustPolicy>, install::InstallError> {
        match &self.trust {
            Some(path) => Ok(Some(TrustPolicy::load(path)?)),
            None => Ok(None),
        }
    }

    fn verified_advisory(
        &self,
        policy: &TrustPolicy,
    ) -> Result<Option<install::VerifiedAdvisory>, install::InstallError> {
        let Some(path) = &self.advisories else {
            return Ok(None);
        };
        let bytes = std::fs::read(path).map_err(|_| {
            install::InstallError::new(
                "install.advisory_unavailable",
                "advisory file is unreadable",
            )
        })?;
        let prior = match &self.root {
            Some(root) => install::Installer::new(root.clone(), policy.clone()).advisory_state()?,
            None => None,
        };
        Ok(Some(policy.verify_advisory(
            &bytes,
            prior.as_ref(),
            common::now_ms(),
        )?))
    }

    fn required_publisher(&self) -> Result<String, install::InstallError> {
        self.publisher
            .clone()
            .ok_or_else(|| install::InstallError::new("install.usage", "--publisher is required"))
    }

    fn required_package(&self) -> Result<(String, String), install::InstallError> {
        let package = self.package.clone().ok_or_else(|| {
            install::InstallError::new("install.usage", "--package <publisher>/<name> is required")
        })?;
        let (publisher, name) = package.split_once('/').ok_or_else(|| {
            install::InstallError::new("install.usage", "--package must be <publisher>/<name>")
        })?;
        Ok((publisher.to_string(), name.to_string()))
    }

    fn required_version(&self) -> Result<String, install::InstallError> {
        self.version
            .clone()
            .ok_or_else(|| install::InstallError::new("install.usage", "--version is required"))
    }

    fn required_digest(&self) -> Result<String, install::InstallError> {
        self.digest.clone().ok_or_else(|| {
            install::InstallError::new("install.usage", "--digest sha256:<hex> is required")
        })
    }
}

fn help() {
    println!(
        "Usage: gb-install <command> [options]\n\n\
         Commands:\n  \
         inspect --artifact <oci-layout> --trust <policy.json> [--advisories <signed-advisory.json>] [--json]\n      Verify provenance, digests, compatibility, signer tier, advisory, and permissions without executing the binary.\n  \
         install --artifact <oci-layout> --trust <policy.json> [--advisories <signed-advisory.json>] --root <cache> [--json]\n      Verify, smoke-test inside the OS sandbox, and install immutably by digest.\n  \
         trust --trust <policy.json> [--advisories <signed-advisory.json>] [--root <cache>] [--json]\n      Show recorded roots, tiers, freshness, rotation, offline policy, and the effective advisory.\n  \
         list --root <cache> [--json]\n      List verified releases in the cache.\n  \
         publish --artifact <oci-layout> --registry <url> --token <token> --publisher <namespace> [--public] [--json]\n      Upload one signed release without executing it; --public opts into anonymous discovery.\n  \
         visibility --registry <url> --token <token> --package <publisher>/<name> (--public|--private) [--json]\n      Change explicit public visibility without changing release identity or trust.\n  \
         discover --registry <url> [--package <publisher>/<name>] [--capability <id>] [--target <triple>] [--runtime <version>] [--adk <version>] [--protocol <version>] [--limit <n>] [--trust <policy.json>] [--advisories <signed-advisory.json>] [--json]\n      Search explicitly public Worker Packages anonymously; optional local trust annotation uses verified evidence.\n  \
         registry list --registry <url> --token <token> --package <publisher>/<name> [--json]\n      List immutable versions and compatibility metadata.\n  \
         registry show --registry <url> --token <token> --package <publisher>/<name> --version <version> [--json]\n      Show one version without executing it.\n  \
         registry install --registry <url> [--token <token>] --package <publisher>/<name> --digest <sha256:hex> --trust <policy.json> [--advisories <signed-advisory.json>] --root <cache> [--json]\n      Download a chosen digest and run the existing installer verification; omit --token for public download.\n  \
         release keygen --test-only --key-file <path> [--public-file <path>] [--json]\n      Generate an explicitly non-production ed25519 drill identity.\n  \
         release assemble --spec <release.json> --output <dir> --signing-key <path> [--json]\n      Probe each built package, write signed OCI layouts, release manifest, provenance, checksums, and a signed checksum file.\n  \
         release verify --release <dir> --trusted-key <external-public-key> [--json]\n      Authenticate checksums against an independently trusted key, then verify complete inventory and bundled Git source. Never execute a candidate binary.\n  \
         --version"
    );
}
