//! Worker command parsing and dispatch.

use crate::worker::config::WorkerSettings;
use crate::worker::Worker;
use crate::worker::WorkerResult;
#[cfg(not(feature = "launcher-cli"))]
use adk_rust::server::{create_app, ServerConfig};
use adk_rust::Launcher;
#[cfg(not(feature = "launcher-cli"))]
use adk_rust::SingleAgentLoader;
use common::{ApiError, CommandRequest, Principal, SubmitTaskRequest, TaskState};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
#[cfg(not(feature = "launcher-cli"))]
use std::sync::Arc;

impl Worker {
    /// Host the package's CLI, launcher, and server commands.
    ///
    /// The host owns command parsing. `--config` is required for every command
    /// so package code never guesses an installation or working-directory path.
    pub async fn run(self) -> WorkerResult<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() == 1 {
            match args[0].as_str() {
                "--help" | "-h" | "help" => {
                    self.print_help();
                    return Ok(());
                }
                "--version" | "-V" => {
                    println!("{} {}", self.name, self.version);
                    return Ok(());
                }
                _ => {}
            }
        }
        let mut index = 0;
        let mut positionals: Vec<String> = Vec::new();
        let mut json = false;
        let mut input = None;
        let mut agent = None;
        let mut session = "local-session".to_string();
        let mut addr = None;
        let mut config = None;
        let mut pair_code = None;
        let mut profile_name = None;
        let mut instruction = None;
        let mut model = None;
        let mut expected = None;
        let mut revision_number = None;
        let mut composition = None;
        let mut workflow = None;
        let mut capabilities = Vec::new();
        let mut members = Vec::new();
        let mut grants = Vec::new();
        let mut restrictions = Vec::new();
        let mut skills = Vec::new();
        let mut secret_refs = Vec::new();
        let mut command_name = None;
        let mut command_arguments: BTreeMap<String, String> = BTreeMap::new();
        let mut idempotency_key = None;
        let mut compaction_budget = None;
        let mut compaction_preserve_recent = None;
        let mut compaction_max_retries = None;
        let mut compaction_strategy = None;
        let mut title = None;
        let mut task_id = None;
        let mut checkpoint_id = None;
        let mut restore_mode = None;
        let mut restore_files = Vec::new();
        let mut activate = false;
        let mut probe_only = false;
        while index < args.len() {
            match args[index].as_str() {
                "--json" => json = true,
                "--probe-only" => probe_only = true,
                "--activate" => activate = true,
                "--input"
                | "--agent"
                | "--session"
                | "--addr"
                | "--config"
                | "--code"
                | "--profile"
                | "--instruction"
                | "--model"
                | "--expected"
                | "--revision"
                | "--composition"
                | "--workflow"
                | "--capability"
                | "--member"
                | "--grant"
                | "--restriction"
                | "--skill"
                | "--secret-ref"
                | "--command"
                | "--arg"
                | "--idempotency-key"
                | "--compaction-budget"
                | "--compaction-preserve-recent"
                | "--compaction-max-retries"
                | "--compaction-strategy"
                | "--title"
                | "--task"
                | "--checkpoint"
                | "--mode"
                | "--file" => {
                    let Some(value) = args.get(index + 1) else {
                        return Err(ApiError::new(
                            "cli.value_missing",
                            format!("{} requires a value", args[index]),
                        ));
                    };
                    match args[index].as_str() {
                        "--input" => input = Some(value.clone()),
                        "--agent" => agent = Some(value.clone()),
                        "--session" => session = value.clone(),
                        "--addr" => addr = Some(value.clone()),
                        "--config" => config = Some(value.clone()),
                        "--code" => pair_code = Some(value.clone()),
                        "--profile" => profile_name = Some(value.clone()),
                        "--instruction" => instruction = Some(value.clone()),
                        "--model" => model = Some(value.clone()),
                        "--expected" => {
                            expected = Some(value.parse::<u64>().map_err(|_| {
                                ApiError::new("cli.value_invalid", "--expected must be an integer")
                            })?)
                        }
                        "--revision" => {
                            revision_number = Some(value.parse::<u64>().map_err(|_| {
                                ApiError::new("cli.value_invalid", "--revision must be an integer")
                            })?)
                        }
                        "--composition" => composition = Some(value.clone()),
                        "--workflow" => workflow = Some(value.clone()),
                        "--capability" => capabilities.push(value.clone()),
                        "--member" => members.push(value.clone()),
                        "--grant" => grants.push(value.clone()),
                        "--restriction" => restrictions.push(value.clone()),
                        "--skill" => skills.push(value.clone()),
                        "--secret-ref" => secret_refs.push(value.clone()),
                        "--command" => command_name = Some(value.clone()),
                        "--idempotency-key" => idempotency_key = Some(value.clone()),
                        "--arg" => {
                            let (name, argument) = value.split_once('=').ok_or_else(|| {
                                ApiError::new("cli.value_invalid", "--arg must use name=value")
                            })?;
                            if name.trim().is_empty() {
                                return Err(ApiError::new(
                                    "cli.value_invalid",
                                    "--arg must name an argument",
                                ));
                            }
                            command_arguments.insert(name.to_string(), argument.to_string());
                        }
                        "--compaction-budget" => {
                            compaction_budget = Some(value.parse::<u64>().map_err(|_| {
                                ApiError::new(
                                    "cli.value_invalid",
                                    "--compaction-budget must be an integer",
                                )
                            })?)
                        }
                        "--compaction-preserve-recent" => {
                            compaction_preserve_recent =
                                Some(value.parse::<usize>().map_err(|_| {
                                    ApiError::new(
                                        "cli.value_invalid",
                                        "--compaction-preserve-recent must be an integer",
                                    )
                                })?)
                        }
                        "--compaction-max-retries" => {
                            compaction_max_retries = Some(value.parse::<usize>().map_err(|_| {
                                ApiError::new(
                                    "cli.value_invalid",
                                    "--compaction-max-retries must be an integer",
                                )
                            })?)
                        }
                        "--compaction-strategy" => compaction_strategy = Some(value.clone()),
                        "--title" => title = Some(value.clone()),
                        "--task" => task_id = Some(value.clone()),
                        "--checkpoint" => checkpoint_id = Some(value.clone()),
                        "--mode" => restore_mode = Some(value.clone()),
                        "--file" => restore_files.push(value.clone()),
                        _ => {}
                    }
                    index += 2;
                    continue;
                }
                other if !other.starts_with('-') => positionals.push(other.to_string()),
                other => {
                    return Err(ApiError::new(
                        "cli.option_unknown",
                        format!("unrecognized option '{other}'"),
                    ))
                }
            }
            index += 1;
        }
        let command = positionals
            .first()
            .cloned()
            .ok_or_else(|| ApiError::new("cli.command_required", "a command is required"))?;
        if command == "probe" {
            let manifest = self.manifest();
            let mut registered: Vec<String> = manifest
                .capabilities
                .iter()
                .map(|capability| capability.id.clone())
                .collect();
            registered.sort();
            println!(
                "{}",
                serde_json::json!({
                    "package": manifest.name,
                    "version": manifest.version,
                    "protocol": manifest.protocol,
                    "runtime": manifest.runtime,
                    "adk": manifest.adk,
                    "registered": registered,
                    "capabilities": manifest.capabilities,
                    "required_permissions": manifest.required_permissions,
                    "features": manifest.features,
                    "hooks": manifest.hooks,
                    "launcher_modes": manifest.launcher_modes,
                    "serve_ready": true,
                    // Persisted-data schema this binary understands. Host
                    // upgrade planning compares it with the instance's
                    // existing state before replacing executable code.
                    "data_schema": crate::DATA_SCHEMA,
                })
            );
            return Ok(());
        }
        if command == "serve" && probe_only {
            println!("serve composition ready");
            return Ok(());
        }
        if command == "dev" && probe_only {
            println!("launcher composition ready");
            return Ok(());
        }
        if matches!(
            command.as_str(),
            "up" | "status" | "down" | "pair" | "enroll"
        ) {
            let config = config.ok_or_else(|| {
                ApiError::new(
                    "cli.config_required",
                    "--config is required for worker lifecycle commands",
                )
            })?;
            return lifecycle(
                &self,
                &command,
                &config,
                addr.as_deref(),
                pair_code.as_deref(),
                json,
            )
            .await;
        }
        let config = config.ok_or_else(|| {
            ApiError::new(
                "cli.config_required",
                "--config is required for capabilities, run, dev, and serve",
            )
        })?;
        match command.as_str() {
            "capabilities" => {
                let instance = self.start(&config).await?;
                let manifest = instance.manifest().clone();
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "installed": manifest,
                            "configured": instance.context().catalog().await,
                            "skills": instance
                                .context()
                                .skills()
                                .await
                                .into_iter()
                                .map(|skill| skill.name)
                                .collect::<Vec<_>>(),
                            "commands": instance.command_descriptors().await,
                            "instance": instance.runtime().config().instance_id,
                            "asset_revision": instance.asset_revision(),
                        })
                    );
                } else {
                    println!("package: {} {}", manifest.name, manifest.version);
                    println!("runtime: {}", manifest.runtime);
                    println!("adk: {}", manifest.adk);
                    for capability in &manifest.capabilities {
                        println!("capability: {} ({})", capability.id, capability.kind);
                    }
                    for agent in instance.context().catalog().await {
                        println!("configured: {} ({:?})", agent.id, agent.origin);
                    }
                }
                instance.shutdown().await;
                Ok(())
            }
            "commands" => {
                let instance = self.start(&config).await?;
                let principal = instance
                    .operator_principal()
                    .await
                    .unwrap_or_else(|_| Principal::new("local", "local", &["*"]));
                let subcommand = positionals.get(1).map(String::as_str).unwrap_or("list");
                let name = command_name.clone().or_else(|| positionals.get(2).cloned());
                let result: WorkerResult<()> = async {
                    match subcommand {
                        "list" => {
                            let commands = instance.command_descriptors().await;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&commands)
                                        .unwrap_or_else(|_| "[]".into())
                                );
                            } else {
                                for command in commands {
                                    println!(
                                        "command: {} ({}) - {}",
                                        command.name, command.kind, command.description
                                    );
                                }
                            }
                            Ok(())
                        }
                        "show" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.command_name_required",
                                    "commands show requires --command",
                                )
                            })?;
                            let commands = instance.command_descriptors().await;
                            let command = commands
                                .into_iter()
                                .find(|command| command.name == name)
                                .ok_or_else(|| {
                                    ApiError::new(
                                        "command.not_found",
                                        format!("command '{name}' is not defined"),
                                    )
                                })?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&command)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "run" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.command_name_required",
                                    "commands run requires --command",
                                )
                            })?;
                            let request = CommandRequest {
                                input,
                                arguments: command_arguments.clone(),
                                session_id: session.clone(),
                                idempotency_key: idempotency_key
                                    .clone()
                                    .unwrap_or_else(|| format!("cli-command-{}", uuid_like())),
                                agent: agent.clone(),
                                limits: Default::default(),
                            };
                            let response =
                                instance.execute_command(&principal, &name, request).await?;
                            if let Some(submitted) = &response.task {
                                let runtime = instance.runtime();
                                loop {
                                    let task = runtime
                                        .task_unscoped(&submitted.task_id)
                                        .await
                                        .ok_or_else(|| {
                                            ApiError::new(
                                                "task.not_found",
                                                "task disappeared from durable state",
                                            )
                                        })?;
                                    if task.state.is_terminal() {
                                        if json {
                                            println!(
                                                "{}",
                                                serde_json::json!({
                                                    "command": response.command,
                                                    "kind": response.kind,
                                                    "task": &task,
                                                })
                                            );
                                        } else if let Some(output) = &task.output {
                                            println!("{output}");
                                        } else {
                                            println!("{:?}", task.state);
                                        }
                                        return match task.state {
                                            TaskState::Succeeded => Ok(()),
                                            TaskState::Cancelled => Err(ApiError::new(
                                                "task.cancelled",
                                                "task was cancelled",
                                            )),
                                            _ => Err(task.error.unwrap_or_else(|| {
                                                ApiError::new("task.failed", "task did not succeed")
                                            })),
                                        };
                                    }
                                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                                }
                            }
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&response)
                                        .unwrap_or_else(|_| "{}".into())
                                );
                            } else {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&response.result)
                                        .unwrap_or_else(|_| "{}".into())
                                );
                            }
                            Ok(())
                        }
                        other => Err(ApiError::new(
                            "cli.command_unknown",
                            format!("unknown commands subcommand '{other}'"),
                        )),
                    }
                }
                .await;
                instance.shutdown().await;
                result
            }
            "profile" => {
                let instance = self.start(&config).await?;
                let principal = instance.operator_principal().await?;
                let namespace = principal.namespace.clone();
                let runtime = instance.runtime();
                let subcommand = positionals.get(1).map(String::as_str).unwrap_or("");
                let name = positionals.get(2).map(String::as_str);
                let result: WorkerResult<()> = async {
                    let revision = || -> WorkerResult<common::ProfileRevision> {
                        let instruction = instruction.clone().ok_or_else(|| {
                            ApiError::new(
                                "cli.instruction_required",
                                "profile create/revise requires --instruction",
                            )
                        })?;
                        Ok(common::ProfileRevision {
                            revision: 0,
                            instruction,
                            model: model
                                .clone()
                                .unwrap_or_else(|| runtime.config().default_model.clone()),
                            capabilities: capabilities.clone(),
                            grants: if grants.is_empty() {
                                vec!["task:submit".into()]
                            } else {
                                grants.clone()
                            },
                            restrictions: restrictions.clone(),
                            composition: composition.clone().unwrap_or_else(|| "single".into()),
                            members: members.clone(),
                            skills: skills.clone(),
                            mcp: Vec::new(),
                            workflow: workflow.clone(),
                            limits: common::Limits::default(),
                            compaction: compaction_budget.map(|context_budget| {
                                common::ContextCompaction {
                                    strategy: compaction_strategy
                                        .clone()
                                        .unwrap_or_else(|| "truncation".into()),
                                    context_budget,
                                    preserve_recent: compaction_preserve_recent.unwrap_or(8),
                                    max_retries: compaction_max_retries.unwrap_or(2),
                                }
                            }),
                            secret_refs: secret_refs.clone(),
                            created_at: 0,
                        })
                    };
                    match subcommand {
                        "list" => {
                            let profiles = runtime.profiles(&principal, &namespace).await?;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&profiles)
                                        .unwrap_or_else(|_| "[]".into())
                                );
                            } else {
                                for profile in profiles {
                                    println!(
                                        "profile: {} active={} revisions={}",
                                        profile.name,
                                        profile.active_revision,
                                        profile.revisions.len()
                                    );
                                }
                            }
                            Ok(())
                        }
                        "show" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.profile_name_required",
                                    "profile name is required",
                                )
                            })?;
                            let profile = runtime.profile(&principal, &namespace, name).await?;
                            if let Some(number) = revision_number {
                                let revision = profile.revision(number).ok_or_else(|| {
                                    ApiError::new(
                                        "profile.revision_not_found",
                                        "profile revision does not exist",
                                    )
                                })?;
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(revision)
                                        .unwrap_or_else(|_| "{}".into())
                                );
                            } else {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&profile)
                                        .unwrap_or_else(|_| "{}".into())
                                );
                            }
                            Ok(())
                        }
                        "create" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.profile_name_required",
                                    "profile name is required",
                                )
                            })?;
                            let mut profile = runtime
                                .create_profile(&principal, &namespace, name, revision()?)
                                .await?;
                            if activate {
                                let number = profile
                                    .revisions
                                    .last()
                                    .map(|revision| revision.revision)
                                    .unwrap_or(1);
                                profile = runtime
                                    .activate_profile(&principal, &namespace, name, number)
                                    .await?;
                            }
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&profile)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "revise" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.profile_name_required",
                                    "profile name is required",
                                )
                            })?;
                            let expected = expected.ok_or_else(|| {
                                ApiError::new(
                                    "cli.expected_required",
                                    "profile revise requires --expected",
                                )
                            })?;
                            let mut profile = runtime
                                .revise_profile(&principal, &namespace, name, expected, revision()?)
                                .await?;
                            if activate {
                                let number = profile
                                    .revisions
                                    .last()
                                    .map(|revision| revision.revision)
                                    .unwrap_or(expected + 1);
                                profile = runtime
                                    .activate_profile(&principal, &namespace, name, number)
                                    .await?;
                            }
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&profile)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "activate" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.profile_name_required",
                                    "profile name is required",
                                )
                            })?;
                            let number = revision_number.ok_or_else(|| {
                                ApiError::new(
                                    "cli.revision_required",
                                    "profile activate requires --revision",
                                )
                            })?;
                            let profile = runtime
                                .activate_profile(&principal, &namespace, name, number)
                                .await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&profile)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "deactivate" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.profile_name_required",
                                    "profile name is required",
                                )
                            })?;
                            let profile = runtime
                                .deactivate_profile(&principal, &namespace, name)
                                .await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&profile)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "delete" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.profile_name_required",
                                    "profile name is required",
                                )
                            })?;
                            runtime.delete_profile(&principal, &namespace, name).await?;
                            println!(
                                "{}",
                                serde_json::json!({
                                    "deleted": name,
                                    "instance": instance.runtime().config().instance_id,
                                })
                            );
                            Ok(())
                        }
                        "export" => {
                            let name = name.ok_or_else(|| {
                                ApiError::new(
                                    "cli.profile_name_required",
                                    "profile name is required",
                                )
                            })?;
                            let mut profile = runtime.profile(&principal, &namespace, name).await?;
                            for revision in &mut profile.revisions {
                                revision.instruction = common::redact(&revision.instruction);
                            }
                            let settings: BTreeMap<String, String> = instance
                                .context()
                                .settings()
                                .await
                                .into_iter()
                                .map(|(key, value)| (key, common::redact(&value)))
                                .collect();
                            println!(
                                "{}",
                                serde_json::json!({
                                    "instance": instance.runtime().config().instance_id,
                                    "profile": profile,
                                    "settings": settings,
                                })
                            );
                            Ok(())
                        }
                        other => Err(ApiError::new(
                            "cli.profile_command_unknown",
                            format!("unknown profile command '{other}'"),
                        )),
                    }
                }
                .await;
                instance.shutdown().await;
                result
            }
            "session" => {
                let instance = self.start(&config).await?;
                let principal = instance.operator_principal().await?;
                let runtime = instance.runtime();
                let subcommand = positionals.get(1).map(String::as_str).unwrap_or("");
                let id = positionals.get(2).cloned();
                let result: WorkerResult<()> = async {
                    match subcommand {
                        "list" => {
                            let views = runtime.session_views(&principal).await?;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&views)
                                        .unwrap_or_else(|_| "[]".into())
                                );
                            } else {
                                for view in views {
                                    println!(
                                        "session: {} title={} events={} updated_at={}",
                                        view.session_id,
                                        view.title.as_deref().unwrap_or("-"),
                                        view.event_count,
                                        view.updated_at
                                    );
                                }
                            }
                            Ok(())
                        }
                        "latest" => {
                            let view = runtime.latest_session(&principal).await?;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&view)
                                        .unwrap_or_else(|_| "{}".into())
                                );
                            } else {
                                println!(
                                    "session: {} title={} events={}",
                                    view.session_id,
                                    view.title.as_deref().unwrap_or("-"),
                                    view.event_count
                                );
                            }
                            Ok(())
                        }
                        "show" => {
                            let id = id.ok_or_else(|| {
                                ApiError::new(
                                    "cli.session_required",
                                    "session show requires a session id",
                                )
                            })?;
                            let view = runtime.session_view(&principal, &id).await?;
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&view)
                                        .unwrap_or_else(|_| "{}".into())
                                );
                            } else {
                                for turn in &view.turns {
                                    println!("{}: {}", turn.role, turn.text);
                                }
                            }
                            Ok(())
                        }
                        "compact" => {
                            let id = id.ok_or_else(|| {
                                ApiError::new(
                                    "cli.session_required",
                                    "session compact requires a session id",
                                )
                            })?;
                            let scheduled =
                                runtime.request_session_compaction(&principal, &id).await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&scheduled)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "fork" => {
                            let source = id.ok_or_else(|| {
                                ApiError::new(
                                    "cli.session_required",
                                    "session fork requires a source session id",
                                )
                            })?;
                            let target = positionals.get(3).cloned().ok_or_else(|| {
                                ApiError::new(
                                    "cli.session_required",
                                    "session fork requires a new session id",
                                )
                            })?;
                            let request = common::SessionForkRequest {
                                session_id: target,
                                title: title.clone(),
                            };
                            let view = runtime.fork_session(&principal, &source, &request).await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&view).unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "rename" => {
                            let id = id.ok_or_else(|| {
                                ApiError::new(
                                    "cli.session_required",
                                    "session rename requires a session id",
                                )
                            })?;
                            let title = title.clone().ok_or_else(|| {
                                ApiError::new(
                                    "cli.title_required",
                                    "session rename requires --title",
                                )
                            })?;
                            let view = runtime
                                .rename_session(
                                    &principal,
                                    &id,
                                    &common::SessionRenameRequest { title },
                                )
                                .await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&view).unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "export" => {
                            let id = id.ok_or_else(|| {
                                ApiError::new(
                                    "cli.session_required",
                                    "session export requires a session id",
                                )
                            })?;
                            let transcript = runtime.export_session(&principal, &id).await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&transcript)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "transcript" => {
                            let id = id.ok_or_else(|| {
                                ApiError::new(
                                    "cli.session_required",
                                    "session transcript requires a session id",
                                )
                            })?;
                            let transcript = runtime.session_transcript(&principal, &id).await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&transcript)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        other => Err(ApiError::new(
                            "cli.session_command_unknown",
                            format!("unknown session command '{other}'"),
                        )),
                    }
                }
                .await;
                instance.shutdown().await;
                result
            }
            "checkpoint" => {
                let instance = self.start(&config).await?;
                let principal = instance.operator_principal().await?;
                let runtime = instance.runtime();
                let subcommand = positionals.get(1).map(String::as_str).unwrap_or("");
                let id = positionals.get(2).cloned();
                let result: WorkerResult<()> = async {
                    match subcommand {
                        "list" => {
                            let task = id.or_else(|| task_id.clone()).ok_or_else(|| {
                                ApiError::new(
                                    "cli.task_required",
                                    "checkpoint list requires a task id",
                                )
                            })?;
                            let checkpoints =
                                runtime.checkpoints_for_task(&principal, &task).await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&checkpoints)
                                    .unwrap_or_else(|_| "[]".into())
                            );
                            Ok(())
                        }
                        "diff" => {
                            let checkpoint =
                                id.or_else(|| checkpoint_id.clone()).ok_or_else(|| {
                                    ApiError::new(
                                        "cli.checkpoint_required",
                                        "checkpoint diff requires a checkpoint id",
                                    )
                                })?;
                            let diff = runtime.checkpoint_diff(&principal, &checkpoint).await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&diff).unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        "restore" => {
                            let checkpoint =
                                id.or_else(|| checkpoint_id.clone()).ok_or_else(|| {
                                    ApiError::new(
                                        "cli.checkpoint_required",
                                        "checkpoint restore requires a checkpoint id",
                                    )
                                })?;
                            let request = common::CheckpointRestoreRequest {
                                mode: restore_mode.clone().unwrap_or_else(|| "rewind".into()),
                                files: (!restore_files.is_empty()).then(|| restore_files.clone()),
                            };
                            let outcome = runtime
                                .restore_checkpoint(&principal, &checkpoint, &request)
                                .await?;
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&outcome)
                                    .unwrap_or_else(|_| "{}".into())
                            );
                            Ok(())
                        }
                        other => Err(ApiError::new(
                            "cli.checkpoint_command_unknown",
                            format!("unknown checkpoint command '{other}'"),
                        )),
                    }
                }
                .await;
                instance.shutdown().await;
                result
            }
            "run" => {
                let input = input
                    .ok_or_else(|| ApiError::new("cli.input_required", "run requires --input"))?;
                let instance = self.start(&config).await?;
                let agent = match agent {
                    Some(agent) => agent,
                    None => instance.default_agent().await,
                };
                let principal = instance
                    .operator_principal()
                    .await
                    .unwrap_or_else(|_| Principal::new("local", "local", &["*"]));
                let response = instance
                    .runtime()
                    .submit_task(
                        &principal,
                        SubmitTaskRequest {
                            namespace: Some("local".into()),
                            agent,
                            session_id: session,
                            input,
                            idempotency_key: format!("cli-{}", uuid_like()),
                            profile: profile_name.clone(),
                            worker: None,
                            team: None,
                            limits: Default::default(),
                        },
                    )
                    .await?;
                loop {
                    let task = instance
                        .runtime()
                        .task_unscoped(&response.task_id)
                        .await
                        .ok_or_else(|| {
                            ApiError::new("task.not_found", "task disappeared from durable state")
                        })?;
                    if task.state.is_terminal() {
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "task_id": task.id,
                                    "state": task.state,
                                    "output": task.output,
                                    "evidence": task.evidence,
                                    "error": task.error,
                                    "usage": task.usage,
                                    "asset_revision": task.asset_revision,
                                    "profile_revision": task.profile_revision,
                                    "resolved_model": task.resolved_model,
                                    "native_invocation_id": task.native_invocation_id,
                                })
                            );
                        } else if let Some(output) = &task.output {
                            println!("{output}");
                        } else {
                            println!("{:?}", task.state);
                        }
                        let result = match task.state {
                            TaskState::Succeeded => Ok(()),
                            TaskState::Cancelled => {
                                Err(ApiError::new("task.cancelled", "task was cancelled"))
                            }
                            _ => Err(task.error.unwrap_or_else(|| {
                                ApiError::new("task.failed", "task did not succeed")
                            })),
                        };
                        instance.shutdown().await;
                        return result;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
            "dev" => {
                let instance = self.start(&config).await?;
                let resolved = match &profile_name {
                    Some(profile) => {
                        let principal = instance.operator_principal().await?;
                        let revision = instance
                            .runtime()
                            .active_revision(&principal.namespace, profile)
                            .await?;
                        instance.profile_agent(&revision).await?
                    }
                    None => {
                        let agent = match agent {
                            Some(agent) => agent,
                            None => instance.default_agent().await,
                        };
                        instance.context().agent(&agent).await.ok_or_else(|| {
                            ApiError::new(
                                "capability.not_registered",
                                format!("agent '{agent}' is not resolved by this configuration"),
                            )
                        })?
                    }
                };
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .map_err(|error| {
                            ApiError::new("signal.install_failed", error.to_string())
                        })?;
                // The native Launcher drives a synchronous console stream, so
                // run it on its own thread. The host keeps observing shutdown
                // signals and can drain the Worker even while the console is
                // blocked waiting for input.
                let name = instance.name().to_string();
                let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
                std::thread::spawn(move || {
                    let outcome = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime.block_on(async move {
                            // The package host already parsed the Worker
                            // command line. Use the pinned Launcher's own
                            // console entry point instead of letting it
                            // re-parse arguments that belong to this host.
                            #[cfg(feature = "launcher-cli")]
                            let launched = Launcher::new(resolved)
                                .app_name(name)
                                .run_console_directly()
                                .await;
                            #[cfg(not(feature = "launcher-cli"))]
                            let launched = Launcher::new(resolved).app_name(name).run().await;
                            launched.map_err(|error| {
                                ApiError::new("launcher.failed", error.to_string())
                            })
                        }),
                        Err(error) => Err(ApiError::new(
                            "launcher.failed",
                            format!("launcher runtime could not start: {error}"),
                        )),
                    };
                    let _ = sender.blocking_send(outcome);
                });
                let signalled = tokio::select! {
                    outcome = receiver.recv() => {
                        let result = outcome.unwrap_or_else(|| {
                            Err(ApiError::new(
                                "launcher.failed",
                                "native launcher thread ended without a result",
                            ))
                        });
                        Some(result)
                    }
                    _ = tokio::signal::ctrl_c() => None,
                    _ = terminate.recv() => None,
                };
                let report = instance.shutdown().await;
                match signalled {
                    Some(result) => {
                        result?;
                        shutdown_result(report)
                    }
                    None => {
                        // The launcher thread may still be blocked on console
                        // input. Hooks and owned resources are already closed,
                        // so terminate the development host with the truthful
                        // outcome instead of waiting for that thread.
                        std::process::exit(if report.clean { 0 } else { 1 });
                    }
                }
            }
            "serve" => {
                let instance = self.start(&config).await?;
                let addr = match addr {
                    Some(addr) => addr,
                    None => instance.default_addr().await,
                };
                if !addr.starts_with("127.")
                    && !addr.starts_with("localhost")
                    && !addr.starts_with("[::1]")
                {
                    instance.shutdown().await;
                    return Err(ApiError::new(
                        "listener.remote_refused",
                        "serve refuses a non-loopback address unless remote operation is configured",
                    ));
                }
                let default_agent =
                    agent.unwrap_or_else(|| futures::executor::block_on(instance.default_agent()));
                let resolved = instance
                    .context()
                    .agent(&default_agent)
                    .await
                    .ok_or_else(|| {
                        ApiError::new(
                            "capability.not_registered",
                            format!(
                                "agent '{default_agent}' is not resolved by this configuration"
                            ),
                        )
                    })?;
                let manifest = instance.manifest().clone();
                let mut agents = BTreeMap::new();
                for descriptor in instance.context().catalog().await {
                    if let Some(agent) = instance.context().agent(&descriptor.id).await {
                        agents.insert(descriptor.id.clone(), agent);
                    }
                }
                agents
                    .entry(default_agent.clone())
                    .or_insert_with(|| resolved.clone());
                let runtime = instance.runtime();
                #[cfg(not(feature = "launcher-cli"))]
                let server_config = ServerConfig::new(
                    Arc::new(SingleAgentLoader::new(resolved.clone())),
                    instance.session_service(),
                );
                let listener = tokio::net::TcpListener::bind(&addr)
                    .await
                    .map_err(|error| ApiError::new("listener.bind_failed", error.to_string()))?;
                let bound = listener
                    .local_addr()
                    .map_err(|error| ApiError::new("listener.bind_failed", error.to_string()))?;
                let base = format!("http://{bound}");
                let heartbeat = crate::worker::gateway::spawn(instance.clone(), base.clone()).await;
                // Native REST and UI routes stay upstream. A2A v1 is mounted
                // separately so the legacy in-memory controller is not served
                // alongside a durable task store on the same path.
                //
                // With the full Launcher feature, compose the same native
                // application through `adk_cli::Launcher::build_app()` so the
                // web-server mode uses the pinned full Launcher rather than a
                // second server implementation. The host still binds the
                // listener so the loopback-only listener policy is enforced.
                #[cfg(feature = "launcher-cli")]
                let native = Launcher::new(resolved.clone())
                    .app_name(manifest.name.clone())
                    .with_session_service(instance.session_service())
                    // Host telemetry owns global subscriber setup and stdout
                    // begins with the stable listener readiness announcement.
                    .with_telemetry(adk_cli::launcher::TelemetryConfig::None)
                    .build_app()
                    .map_err(|error| {
                        ApiError::new("server.composition_failed", error.to_string())
                    })?;
                #[cfg(not(feature = "launcher-cli"))]
                let native = create_app(server_config);
                let a2a = crate::serving::A2aServing::open(
                    runtime.clone(),
                    &manifest,
                    agents,
                    default_agent,
                    instance.session_service(),
                    instance.plugins(),
                    &base,
                )
                .await?;
                let state = crate::serving::WorkerState::new(runtime, manifest)
                    .with_durable_a2a(a2a.durable())
                    .with_browser(instance.browser())
                    .with_commands(instance.command_registry().await);
                let app = crate::serving::api_router(state.clone())
                    .merge(native)
                    .merge(a2a.router());
                let app = crate::serving::authenticate(app, state);
                let mut hangup =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                        .map_err(|error| {
                            ApiError::new("signal.install_failed", error.to_string())
                        })?;
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .map_err(|error| {
                            ApiError::new("signal.install_failed", error.to_string())
                        })?;
                let reload_instance = instance.clone();
                let reload = tokio::spawn(async move {
                    while hangup.recv().await.is_some() {
                        match reload_instance.reload().await {
                            Ok(revision) => eprintln!("config: reloaded revision {revision}"),
                            Err(error) => eprintln!(
                                "config: reload failed {}: {}",
                                error.code,
                                common::redact(&error.message)
                            ),
                        }
                    }
                });
                println!("native ADK server listening on {base}");
                let drain_instance = instance.clone();
                let served = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = terminate.recv() => {}
                    }
                    // First signal: stop admission and close streams before
                    // axum waits for connections. A second signal forces the
                    // remaining execution tokens without inventing an outcome.
                    let runtime = drain_instance.runtime();
                    runtime.begin_drain().await;
                    let drain = runtime.await_drain();
                    tokio::pin!(drain);
                    tokio::select! {
                        outcome = &mut drain => {
                            if outcome.timed_out {
                                eprintln!(
                                    "shutdown: bounded drain expired; unfinished work is left for reconciliation"
                                );
                            }
                        }
                        _ = terminate.recv() => {
                            eprintln!("shutdown: second signal; forcing remaining execution stop");
                            runtime.force_stop().await;
                        }
                        _ = tokio::signal::ctrl_c() => {
                            eprintln!("shutdown: second signal; forcing remaining execution stop");
                            runtime.force_stop().await;
                        }
                    }
                })
                .await
                .map_err(|error| ApiError::new("server.failed", error.to_string()));
                reload.abort();
                if let Some(heartbeat) = heartbeat {
                    heartbeat.abort();
                }
                let report = instance.shutdown().await;
                served?;
                shutdown_result(report)
            }
            other => Err(ApiError::new(
                "cli.command_unknown",
                format!("unknown command '{other}'"),
            )),
        }
    }

    fn print_help(&self) {
        println!(
            "Usage: {} <command> [options]\n\n\
             Commands:\n  \
             probe [--json]\n      Machine-readable compiled registration report for installer smoke checks.\n  \
             capabilities --config <path> [--json]\n      Show installed capabilities and configured agents.\n  \
             run --config <path> --input <text> [--agent <id>] [--profile <name>] [--session <id>] [--json]\n      Execute through shared runtime and the native ADK Runner.\n  \
             dev --config <path> [--agent <id>] [--profile <name>]\n      Native ADK Runner launcher console. No Gateway or SDK.\n  \
             serve --config <path> [--addr 127.0.0.1:8080] [--agent <id>]\n      Native ADK web server, REST, and A2A. No Gateway or SDK.\n  \
             commands list|show|run --config <path> [--command <name>] [--input <text>]\n      [--arg name=value] [--session <id>] [--json]\n      Discover and execute neutral runtime and Worker-defined commands.\n  \
             profile list|show|create|revise|activate|deactivate|delete|export --config <path> [options]\n      Worker-native immutable profile management.\n      Compaction options: --compaction-budget <tokens> [--compaction-strategy truncation]\n      [--compaction-preserve-recent <events>] [--compaction-max-retries <attempts>].\n  \
             session list|latest|show|compact|fork|rename|export|transcript --config <path> [options]\n      Continue, resume, compact, fork, rename, and durably export canonical sessions.\n      Compact schedules native compaction for the next model turn.\n      Fork: session fork <source> <new> [--title <title>]. Rename: --title <title>.\n  \
             checkpoint list|diff|restore --config <path> [options]\n      Inspect turn-boundary file checkpoints and rewind/undo/restore a turn or file set.\n      Restore options: --mode rewind|undo|restore [--file <workspace-relative-path> ...].\n  \
             up --config <path> [--addr ADDR] [--json]\n      Start this Worker in the background and wait for readiness.\n  \
             status --config <path> [--json]\n      Report background process and health.\n  \
             down --config <path> [--json]\n      Stop the background Worker with bounded drain.\n  \
             pair --config <path> [--json]\n      Print a single goldenbee:// link for Gateway pairing.\n  \
             enroll --config <path> --code <one-time-code> [--json]\n      Enroll this Worker's X25519 provider key with the Gateway vault.\n\n\
             Relative agent, prompt, skill, and storage paths resolve against\n\
             the configuration file's directory.",
            self.name
        );
    }
}

async fn lifecycle(
    worker: &Worker,
    command: &str,
    config: &str,
    addr: Option<&str>,
    code: Option<&str>,
    json: bool,
) -> WorkerResult<()> {
    let settings = WorkerSettings::load(std::path::Path::new(config))?;
    let pid_path = settings.data_dir.join("worker.pid");
    let address = addr.unwrap_or(&settings.file.addr).to_string();
    match command {
        "up" => {
            if let Some(pid) = read_pid(&pid_path) {
                if pid_alive(pid) {
                    println!("worker up: pid={pid} addr={address} (already running)");
                    return Ok(());
                }
            }
            std::fs::create_dir_all(&settings.data_dir).map_err(|error| {
                ApiError::new(
                    "worker.data_dir",
                    format!("cannot create Worker data directory: {error}"),
                )
            })?;
            let log_path = settings.data_dir.join("worker.log");
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .map_err(|error| {
                    ApiError::new("worker.log", format!("cannot open Worker log: {error}"))
                })?;
            let log_err = log.try_clone().map_err(|error| {
                ApiError::new(
                    "worker.log",
                    format!("cannot duplicate Worker log: {error}"),
                )
            })?;
            let exe = std::env::current_exe().map_err(|error| {
                ApiError::new(
                    "worker.executable",
                    format!("cannot resolve executable: {error}"),
                )
            })?;
            let child = Command::new(exe)
                .args(["serve", "--config", config, "--addr", &address])
                .stdin(Stdio::null())
                .stdout(Stdio::from(log))
                .stderr(Stdio::from(log_err))
                .spawn()
                .map_err(|error| {
                    ApiError::new("worker.start", format!("cannot start Worker: {error}"))
                })?;
            std::fs::write(&pid_path, child.id().to_string()).map_err(|error| {
                ApiError::new("worker.pid", format!("cannot write Worker pid: {error}"))
            })?;
            for _ in 0..150 {
                if health(&address).await.is_some() {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({"status":"running","pid":child.id(),"addr":address,"log":log_path})
                        );
                    } else {
                        println!(
                            "worker up: pid={} addr={} log={}",
                            child.id(),
                            address,
                            log_path.display()
                        );
                    }
                    return Ok(());
                }
                if !pid_alive(child.id()) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let _ = Command::new("kill")
                .args(["-KILL", &child.id().to_string()])
                .status();
            let _ = std::fs::remove_file(&pid_path);
            Err(ApiError::new(
                "worker.start_failed",
                format!("Worker did not become healthy; see {}", log_path.display()),
            ))
        }
        "status" => {
            let pid = read_pid(&pid_path);
            let alive = pid.is_some_and(pid_alive);
            let health = if alive { health(&address).await } else { None };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": if alive { "running" } else { "stopped" },
                        "pid": pid,
                        "addr": address,
                        "health": health,
                    })
                );
            } else {
                println!("worker: {}", if alive { "running" } else { "stopped" });
                println!("instance: {}", settings.file.instance);
                println!("addr: {address}");
                if let Some(pid) = pid {
                    println!("pid: {pid}");
                }
                if let Some(health) = health {
                    println!("health: {}", health);
                }
            }
            Ok(())
        }
        "down" => {
            let Some(pid) = read_pid(&pid_path) else {
                println!("worker down: not running");
                return Ok(());
            };
            if pid_alive(pid) {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
                for _ in 0..150 {
                    if !pid_alive(pid) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                if pid_alive(pid) {
                    let _ = Command::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .status();
                }
            }
            let _ = std::fs::remove_file(&pid_path);
            println!("worker down: pid={pid} addr={address}");
            Ok(())
        }
        "enroll" => {
            let code = code.ok_or_else(|| {
                ApiError::new(
                    "cli.code_required",
                    "enroll requires --code from the Gateway operator",
                )
            })?;
            let response = crate::worker::models::enroll(&settings, code).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "worker": response.worker_id,
                        "instance": response.instance,
                        "enrolled_at": response.enrolled_at,
                    })
                );
            } else {
                println!(
                    "worker enrolled: instance={} at={}",
                    response.instance, response.enrolled_at
                );
            }
            Ok(())
        }
        "pair" => {
            let gateway = settings.file.gateway.as_ref().ok_or_else(|| {
                ApiError::new(
                    "worker.pair_unconfigured",
                    "Worker configuration has no [gateway] section",
                )
            })?;
            let endpoint = gateway
                .advertise_url
                .clone()
                .unwrap_or_else(|| format!("http://{address}"));
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query
                .append_pair("gateway", &gateway.url)
                .append_pair("worker", &settings.file.instance)
                .append_pair("endpoint", &endpoint)
                .append_pair("package", &worker.name)
                .append_pair("control_token", &gateway.control_token);
            let link = format!("goldenbee://pair/worker?{}", query.finish());
            if json {
                println!(
                    "{}",
                    serde_json::json!({"link":link,"worker":settings.file.instance,"gateway":gateway.url})
                );
            } else {
                println!("{link}");
                eprintln!("warning: pair link contains the Worker control token; share it only with the intended Gateway operator");
            }
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn read_pid(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn health(address: &str) -> Option<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    client
        .get(format!("http://{address}/v1/health"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

fn uuid_like() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )
}

fn shutdown_result(report: crate::worker::ShutdownReport) -> WorkerResult<()> {
    if report.clean {
        return Ok(());
    }
    Err(ApiError::new(
        "worker.shutdown_degraded",
        format!(
            "shutdown ended in phase '{}' (timed_out={}, forced={}, unfinished={})",
            report.phase, report.timed_out, report.forced, report.unfinished
        ),
    )
    .with(
        "hook_error",
        report.hook_error.unwrap_or_else(|| "none".into()),
    ))
}
