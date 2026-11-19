//! Typed command-line parsing.
//!
//! Global connection flags are peeled first, then each command domain parses
//! the flags it owns. Unknown flags and malformed values fail before any
//! network or database work starts.

use crate::commands::approval::ApprovalCommand;
use crate::commands::artifact::ArtifactCommand;
use crate::commands::chat::ChatOptions;
use crate::commands::control::ControlCommand;
use crate::commands::diagnose::DiagnoseCommand;
use crate::commands::fleet::{
    FleetCommand, FleetProvisionOptions, FleetUpgradeOptions, WorkPolicy,
};
use crate::commands::gateway::GatewayCommand;
use crate::commands::model::ModelCommand;
use crate::commands::profile::{ProfileAction, ProfileCommand, ProfileRevisionOptions};
use crate::commands::provider::{
    PairOptions, ProviderAddOptions, ProviderCommand, ProviderUpdateOptions,
};
use crate::commands::task::{SubmitOptions, TaskCommand};
use crate::commands::team::{TeamCommand, TeamOptions};
use crate::commands::worker::{AddWorkerOptions, UpdateWorkerOptions, WorkerCommand, WorkersQuery};
use std::collections::{BTreeMap, BTreeSet};

/// Connection flags shared by every Gateway command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionOptions {
    /// Explicit Gateway URL.
    pub gateway: Option<String>,
    /// Explicit bearer token.
    pub token: Option<String>,
}

/// One parsed invocation.
#[derive(Debug, Clone)]
pub struct Cli {
    /// Explicit connection overrides.
    pub connection: ConnectionOptions,
    /// Stable wire JSON output.
    pub json: bool,
    /// The command and its owned flags.
    pub command: Command,
}

/// Every command domain the CLI understands.
#[derive(Debug, Clone)]
pub enum Command {
    /// Interactive chat.
    Chat(ChatOptions),
    /// Local Gateway profile management.
    Gateway(GatewayCommand),
    /// Model-provider administration.
    Provider(ProviderCommand),
    /// Approved Worker administration.
    Worker(WorkerCommand),
    /// Caller-scoped Worker discovery.
    Workers(WorkersQuery),
    /// Provider-owned model management.
    Model(ModelCommand),
    /// Team rosters.
    Team(TeamCommand),
    /// Runtime profiles.
    Profile(ProfileCommand),
    /// Task submission, inspection, events, cancellation, and watch.
    Task(TaskCommand),
    /// Task artifacts.
    Artifact(ArtifactCommand),
    /// Host fleet operations.
    Fleet(FleetCommand),
    /// Approval decisions.
    Approval(ApprovalCommand),
    /// Human control leases.
    Control(ControlCommand),
    /// Worker diagnostics.
    Diagnose(DiagnoseCommand),
    /// Gateway liveness.
    Health,
    /// Full-screen management session.
    Manage,
}

/// Parse one invocation.
pub fn parse(args: Vec<String>) -> Result<Cli, String> {
    let command_index = find_command(&args);
    let (connection, json, rest) = peel_globals(&args, command_index)?;
    let name = command_index
        .map(|index| args[index].as_str())
        .unwrap_or("");
    let command = match name {
        "" | "chat" => Command::Chat(parse_chat(rest)?),
        "gateway" | "gw" => Command::Gateway(parse_gateway(rest)?),
        "provider" => Command::Provider(parse_provider(rest)?),
        "worker" => Command::Worker(parse_worker(rest)?),
        "workers" => Command::Workers(parse_workers(rest)?),
        "model" | "models" => Command::Model(parse_model(rest)?),
        "team" => Command::Team(parse_team(rest)?),
        "profile" => Command::Profile(parse_profile(rest)?),
        "submit" | "task" | "cancel" | "events" | "watch" => Command::Task(parse_task(name, rest)?),
        "artifact" | "artifacts" => Command::Artifact(parse_artifact(rest)?),
        "fleet" => Command::Fleet(parse_fleet(rest)?),
        "approvals" | "approve" | "reject" | "revoke" => {
            Command::Approval(parse_approval(name, rest)?)
        }
        "control" => Command::Control(parse_control(rest)?),
        "diagnose" => Command::Diagnose(parse_diagnose(rest)?),
        "health" => {
            split(rest, &[], &[])?;
            Command::Health
        }
        "manage" => {
            split(rest, &[], &[])?;
            Command::Manage
        }
        _ => return Err("gb-client: unknown command".into()),
    };
    Ok(Cli {
        connection,
        json,
        command,
    })
}

fn parse_chat(rest: Vec<String>) -> Result<ChatOptions, String> {
    let raw = split(rest, &["worker", "agent"], &[])?;
    if !raw.positionals.is_empty() {
        return Err("gb-client: chat accepts options, not positional arguments".into());
    }
    Ok(ChatOptions {
        worker: raw.get("worker"),
        agent: raw.get("agent"),
    })
}

fn parse_gateway(rest: Vec<String>) -> Result<GatewayCommand, String> {
    let raw = split(rest, &["worker", "agent"], &[])?;
    let action = raw.positional(0).unwrap_or("");
    Ok(match action {
        "add" => GatewayCommand::Add {
            name: raw.require_positional(1, "gb-client: gw add requires <name> <url> <token>")?,
            url: raw.require_positional(2, "gb-client: gw add requires <name> <url> <token>")?,
            token: raw.require_positional(3, "gb-client: gw add requires <name> <url> <token>")?,
        },
        "switch" => GatewayCommand::Switch {
            name: raw.require_positional(1, "gb-client: gw switch requires <name>")?,
        },
        "open" => GatewayCommand::Open {
            name: raw.require_positional(1, "gb-client: gw open requires <name>")?,
            worker: raw.get("worker"),
            agent: raw.get("agent"),
        },
        "list" => GatewayCommand::List,
        "detail" => GatewayCommand::Detail {
            name: raw.require_positional(1, "gb-client: gw detail requires <name>")?,
        },
        _ => return Err("gb-client: gw expects add, switch, open, list, or detail".to_string()),
    })
}

fn parse_provider(rest: Vec<String>) -> Result<ProviderCommand, String> {
    let raw = split(
        rest,
        &[
            "adapter",
            "model",
            "endpoint",
            "modality",
            "allowed-model",
            "residency",
            "context-window",
            "worker",
            "revision",
            "instance",
        ],
        &[
            "auto-default",
            "tool-calling",
            "local",
            "all-workers",
            "enable",
            "disable",
            "no-auto-default",
            "no-tool-calling",
            "remote",
            "clear-allowed-models",
            "clear-workers",
        ],
    )?;
    let action = raw.positional(0).unwrap_or("");
    Ok(match action {
        "list" => ProviderCommand::List,
        "show" => ProviderCommand::Show {
            id: raw.require_positional(1, "gb-client: provider show requires <id>")?,
        },
        "add" | "create" => {
            let id = raw.require_positional(1, "gb-client: provider add requires <id>")?;
            let adapter = raw.require(
                "adapter",
                "gb-client: provider add requires --adapter <kind>",
            )?;
            let workers = if raw.has("all-workers") {
                vec!["*".to_string()]
            } else {
                raw.all("worker")
            };
            if workers.is_empty() {
                return Err(
                    "gb-client: provider add requires --worker <id> or --all-workers".into(),
                );
            }
            ProviderCommand::Add {
                id,
                options: ProviderAddOptions {
                    adapter,
                    model: raw.get("model").unwrap_or_default(),
                    endpoint: raw.get("endpoint"),
                    allowed_models: if raw.has("clear-allowed-models") {
                        Some(Vec::new())
                    } else if raw.all("allowed-model").is_empty() {
                        None
                    } else {
                        Some(raw.all("allowed-model"))
                    },
                    auto_default: raw.has("auto-default"),
                    modalities: if raw.all("modality").is_empty() {
                        vec!["text".to_string()]
                    } else {
                        raw.all("modality")
                    },
                    tool_calling: raw.has("tool-calling"),
                    context_window: raw.uint("context-window", "--context-window")?,
                    local: raw.has("local"),
                    residency: raw.get("residency"),
                    allowed_workers: workers,
                },
            }
        }
        "update" => {
            let id = raw.require_positional(1, "gb-client: provider update requires <id>")?;
            let revision = raw
                .uint("revision", "--revision")?
                .ok_or_else(|| "gb-client: provider update requires --revision <n>".to_string())?;
            ProviderCommand::Update {
                id,
                options: ProviderUpdateOptions {
                    revision,
                    adapter: raw.get("adapter"),
                    model: raw.get("model"),
                    endpoint: raw.get("endpoint"),
                    allowed_models: if raw.has("clear-allowed-models") {
                        Some(Vec::new())
                    } else if raw.all("allowed-model").is_empty() {
                        None
                    } else {
                        Some(raw.all("allowed-model"))
                    },
                    allow_auto_default: if raw.has("no-auto-default") {
                        Some(false)
                    } else if raw.has("auto-default") {
                        Some(true)
                    } else {
                        None
                    },
                    modalities: if raw.all("modality").is_empty() {
                        None
                    } else {
                        Some(raw.all("modality"))
                    },
                    tool_calling: if raw.has("no-tool-calling") {
                        Some(false)
                    } else if raw.has("tool-calling") {
                        Some(true)
                    } else {
                        None
                    },
                    context_window: raw.uint("context-window", "--context-window")?,
                    local: if raw.has("remote") {
                        Some(false)
                    } else if raw.has("local") {
                        Some(true)
                    } else {
                        None
                    },
                    residency: raw.get("residency"),
                    allowed_workers: if raw.has("clear-workers") {
                        Some(Vec::new())
                    } else if raw.has("all-workers") {
                        Some(vec!["*".to_string()])
                    } else if raw.all("worker").is_empty() {
                        None
                    } else {
                        Some(raw.all("worker"))
                    },
                    active: if raw.has("enable") {
                        Some(true)
                    } else if raw.has("disable") {
                        Some(false)
                    } else {
                        None
                    },
                },
            }
        }
        "discover" => ProviderCommand::Discover {
            id: raw.require_positional(1, "gb-client: provider discover requires <id>")?,
        },
        "credential" => {
            let verb = raw.positional(1).unwrap_or("");
            let id = raw.require_positional(
                2,
                "gb-client: provider credential requires <set|revoke> <id>",
            )?;
            match verb {
                "set" => ProviderCommand::CredentialSet { id },
                "revoke" => ProviderCommand::CredentialRevoke { id },
                _ => return Err("gb-client: provider credential expects set or revoke".into()),
            }
        }
        "pair" => ProviderCommand::Pair(PairOptions {
            worker: raw.require(
                "worker",
                "gb-client: provider pair requires --worker <id> and --instance <name>",
            )?,
            instance: raw.require(
                "instance",
                "gb-client: provider pair requires --worker <id> and --instance <name>",
            )?,
        }),
        _ => return Err(
            "gb-client: provider expects list, show, add, update, discover, credential, or pair"
                .to_string(),
        ),
    })
}

fn parse_worker(rest: Vec<String>) -> Result<WorkerCommand, String> {
    let raw = split(
        rest,
        &[
            "instance",
            "package",
            "capability",
            "key",
            "revision",
            "endpoint",
            "worker",
        ],
        &["control-token-stdin", "no-preflight", "all"],
    )?;
    let action = raw.positional(0).unwrap_or("");
    Ok(match action {
        "list" => WorkerCommand::List,
        "show" => WorkerCommand::Show {
            id: raw.require_positional(1, "gb-client: worker show requires <id>")?,
        },
        "add" => {
            let id = raw.require_positional(1, "gb-client: worker add requires <id> <endpoint>")?;
            let endpoint = raw.require_positional(
                2,
                "gb-client: worker add requires <id> <endpoint>",
            )?;
            WorkerCommand::Add(AddWorkerOptions {
                id,
                endpoint,
                instance: raw.get("instance"),
                package: raw.get("package"),
                capabilities: if raw.all("capability").is_empty() {
                    None
                } else {
                    Some(raw.all("capability"))
                },
                control_token_stdin: raw.has("control-token-stdin"),
                preflight: !raw.has("no-preflight"),
                key: raw.get("key"),
            })
        }
        "update" => {
            let id = raw
                .require_positional(1, "gb-client: worker update requires <id> --revision <n>")?;
            let revision = raw
                .uint("revision", "--revision")?
                .ok_or_else(|| {
                    "gb-client: worker update requires --revision <n>".to_string()
                })?;
            WorkerCommand::Update {
                id,
                options: UpdateWorkerOptions {
                    revision,
                    endpoint: raw.get("endpoint"),
                    instance: raw.get("instance"),
                    package: raw.get("package"),
                    capabilities: if raw.all("capability").is_empty() {
                        None
                    } else {
                        Some(raw.all("capability"))
                    },
                    key: raw.get("key"),
                },
            }
        }
        "disable" => WorkerCommand::Disable {
            id: raw.require_positional(1, "gb-client: worker disable requires <id>")?,
        },
        "enable" => WorkerCommand::Enable {
            id: raw.require_positional(1, "gb-client: worker enable requires <id>")?,
        },
        "probe" => WorkerCommand::Probe {
            id: raw.require_positional(1, "gb-client: worker probe requires <id>")?,
        },
        "remove" => WorkerCommand::Remove {
            id: raw.require_positional(1, "gb-client: worker remove requires <id>")?,
        },
        "audit" => WorkerCommand::Audit {
            worker: raw.get("worker"),
        },
        _ => {
            return Err(
                "gb-client: worker expects list, show, add, update, disable, enable, probe, remove, or audit"
                    .to_string(),
            )
        }
    })
}

fn parse_workers(rest: Vec<String>) -> Result<WorkersQuery, String> {
    let raw = split(rest, &["capability", "protocol", "locality", "health"], &[])?;
    Ok(WorkersQuery {
        capability: raw.get("capability"),
        protocol: raw.get("protocol"),
        locality: raw.get("locality"),
        health: raw.get("health"),
    })
}

fn parse_model(rest: Vec<String>) -> Result<ModelCommand, String> {
    let raw = split(
        rest,
        &["provider", "model", "allowed-model"],
        &["clear-allowed-models"],
    )?;
    let action = raw.positional(0).unwrap_or("");
    Ok(match action {
        "list" => ModelCommand::List {
            provider: raw.get("provider"),
        },
        "allow" | "allowed" => {
            let provider =
                raw.require_positional(1, "gb-client: model allow requires <provider>")?;
            let mut models = raw.all("model");
            models.extend(raw.all("allowed-model"));
            models.sort();
            models.dedup();
            if models.is_empty() {
                return Err("gb-client: model allow requires --model <id>".into());
            }
            ModelCommand::Allow { provider, models }
        }
        "clear" => ModelCommand::Clear {
            provider: raw.require_positional(1, "gb-client: model clear requires <provider>")?,
        },
        "preferred" | "default" | "set" => {
            let provider =
                raw.require_positional(1, "gb-client: model preferred requires <provider>")?;
            let model = raw.require("model", "gb-client: model preferred requires --model <id>")?;
            ModelCommand::Preferred { provider, model }
        }
        "discover" => ModelCommand::Discover {
            provider: raw.require_positional(1, "gb-client: model discover requires <provider>")?,
        },
        _ => {
            return Err(
                "gb-client: model expects list, allow, clear, preferred, or discover".to_string(),
            )
        }
    })
}

fn parse_team(rest: Vec<String>) -> Result<TeamCommand, String> {
    let raw = split(
        rest,
        &[
            "master",
            "member",
            "max-depth",
            "max-fanout",
            "locality",
            "version",
        ],
        &[],
    )?;
    let action = raw.positional(0).unwrap_or("");
    let options = || -> Result<TeamOptions, String> {
        Ok(TeamOptions {
            master: raw.get("master"),
            members: raw.all("member"),
            max_depth: raw.u32("max-depth", "--max-depth")?,
            max_fanout: raw.u32("max-fanout", "--max-fanout")?,
            locality: raw.get("locality"),
        })
    };
    Ok(match action {
        "list" => TeamCommand::List,
        "show" | "detail" => TeamCommand::Show {
            name: raw.require_positional(1, "gb-client: team show requires <name>")?,
        },
        "create" => TeamCommand::Create {
            name: raw.require_positional(1, "gb-client: team create requires <name>")?,
            options: options()?,
        },
        "revise" | "update" => TeamCommand::Revise {
            name: raw.require_positional(1, "gb-client: team revise requires <name>")?,
            version: raw
                .uint("version", "--version")?
                .ok_or_else(|| "gb-client: team revise requires --version <n>".to_string())?,
            options: options()?,
        },
        "validate" | "activate" | "deactivate" => {
            let name = raw.require_positional(
                1,
                "gb-client: team validate|activate|deactivate requires <name>",
            )?;
            let version = raw.uint("version", "--version")?;
            match action {
                "validate" => TeamCommand::Validate { name, version },
                "activate" => TeamCommand::Activate { name, version },
                _ => TeamCommand::Deactivate { name, version },
            }
        }
        _ => return Err(
            "gb-client: team expects list, show, create, revise, validate, activate, or deactivate"
                .to_string(),
        ),
    })
}

fn parse_profile(rest: Vec<String>) -> Result<ProfileCommand, String> {
    let raw = split(
        rest,
        &[
            "worker",
            "instruction",
            "model",
            "capability",
            "grant",
            "restriction",
            "composition",
            "member",
            "skill",
            "mcp",
            "secret-ref",
            "compaction-budget",
            "revision",
            "expected",
        ],
        &[],
    )?;
    let action = raw.positional(0).unwrap_or("");
    let worker = raw.get("worker");
    let options = || -> Result<ProfileRevisionOptions, String> {
        Ok(ProfileRevisionOptions {
            instruction: raw.get("instruction").ok_or_else(|| {
                "gb-client: profile create/revise requires --instruction".to_string()
            })?,
            model: raw.get("model"),
            capabilities: raw.all("capability"),
            grants: raw.all("grant"),
            restrictions: raw.all("restriction"),
            composition: raw.get("composition"),
            members: raw.all("member"),
            skills: raw.all("skill"),
            mcp: raw.all("mcp"),
            secret_refs: raw.all("secret-ref"),
            compaction_budget: raw.uint("compaction-budget", "--compaction-budget")?,
        })
    };
    let action = match action {
        "list" => ProfileAction::List,
        "show" => ProfileAction::Show {
            name: raw.require_positional(1, "gb-client: profile show requires <name>")?,
        },
        "create" => ProfileAction::Create {
            name: raw.require_positional(1, "gb-client: profile create requires <name>")?,
            options: options()?,
        },
        "revise" | "update" => ProfileAction::Revise {
            name: raw.require_positional(1, "gb-client: profile revise requires <name>")?,
            expected: raw
                .uint("expected", "--expected")?
                .ok_or_else(|| "gb-client: profile revise requires --expected <n>".to_string())?,
            options: options()?,
        },
        "validate" => ProfileAction::Validate {
            name: raw.require_positional(1, "gb-client: profile validate requires <name>")?,
            revision: raw.uint("revision", "--revision")?,
        },
        "activate" => ProfileAction::Activate {
            name: raw.require_positional(1, "gb-client: profile activate requires <name>")?,
            revision: raw
                .uint("revision", "--revision")?
                .ok_or_else(|| "gb-client: profile activate requires --revision <n>".to_string())?,
        },
        "deactivate" => ProfileAction::Deactivate {
            name: raw.require_positional(1, "gb-client: profile deactivate requires <name>")?,
        },
        "delete" | "remove" => ProfileAction::Delete {
            name: raw.require_positional(1, "gb-client: profile delete requires <name>")?,
        },
        _ => {
            return Err(
                "gb-client: profile expects list, show, create, revise, validate, activate, deactivate, or delete"
                    .to_string(),
            )
        }
    };
    Ok(ProfileCommand { worker, action })
}

fn parse_task(name: &str, rest: Vec<String>) -> Result<TaskCommand, String> {
    let raw = split(
        rest,
        &[
            "worker",
            "agent",
            "input",
            "session",
            "key",
            "profile",
            "team",
            "team-version",
            "after",
            "limit",
        ],
        &[],
    )?;
    Ok(match name {
        "submit" => TaskCommand::Submit(SubmitOptions {
            worker: raw.get("worker"),
            agent: raw.get("agent"),
            input: raw.get("input"),
            session: raw.get("session"),
            key: raw.get("key"),
            profile: raw.get("profile"),
            team: raw.get("team"),
            team_version: raw.uint("team-version", "--team-version")?,
        }),
        "task" => TaskCommand::Show {
            id: raw.require_positional(0, "gb-client: task requires a task id")?,
        },
        "cancel" => TaskCommand::Cancel {
            id: raw.require_positional(0, "gb-client: cancel requires a task id")?,
        },
        "events" => TaskCommand::Events {
            id: raw.require_positional(0, "gb-client: events requires a task id")?,
            after: raw.uint("after", "--after")?.unwrap_or(0),
            limit: raw.usize("limit", "--limit")?.unwrap_or(500),
        },
        _ => TaskCommand::Watch {
            id: raw.require_positional(0, "gb-client: watch requires a task id")?,
            after: raw.uint("after", "--after")?.unwrap_or(0),
        },
    })
}

fn parse_artifact(rest: Vec<String>) -> Result<ArtifactCommand, String> {
    let raw = split(rest, &["output"], &["force"])?;
    Ok(match raw.positional(0).unwrap_or("") {
        "list" => ArtifactCommand::List {
            task: raw.require_positional(1, "gb-client: artifact list requires <task-id>")?,
        },
        "show" => ArtifactCommand::Show {
            task: raw.require_positional(
                1,
                "gb-client: artifact show requires <task-id> <artifact-id>",
            )?,
            artifact: raw.require_positional(
                2,
                "gb-client: artifact show requires <task-id> <artifact-id>",
            )?,
        },
        "download" => ArtifactCommand::Download {
            task: raw.require_positional(
                1,
                "gb-client: artifact download requires <task-id> <artifact-id>",
            )?,
            artifact: raw.require_positional(
                2,
                "gb-client: artifact download requires <task-id> <artifact-id>",
            )?,
            output: raw.require(
                "output",
                "gb-client: artifact download requires --output <path>",
            )?,
            force: raw.has("force"),
        },
        _ => return Err("gb-client: artifact expects list, show, or download".into()),
    })
}

fn parse_fleet(rest: Vec<String>) -> Result<FleetCommand, String> {
    let raw = split(
        rest,
        &[
            "operation",
            "instance",
            "worker",
            "release",
            "addr",
            "config",
            "workspace",
            "grant",
            "secret-ref",
            "profile-name",
            "profile-instruction",
            "profile-member",
            "profile-capability",
            "profile-grant",
            "control-token",
            "authorize",
            "work-policy",
            "verify-input",
        ],
        &["read-only", "profile-create"],
    )?;
    Ok(match raw.positional(0).unwrap_or("") {
        "hosts" => FleetCommand::Hosts,
        "host" => FleetCommand::Host {
            host: raw.require_positional(1, "gb-client: fleet host requires <host>")?,
        },
        "provision" => FleetCommand::Provision {
            host: raw.require_positional(1, "gb-client: fleet provision requires <host>")?,
            options: FleetProvisionOptions {
                operation: raw.require(
                    "operation",
                    "gb-client: fleet provision requires --operation, --instance, --worker, and --release",
                )?,
                instance: raw.require(
                    "instance",
                    "gb-client: fleet provision requires --operation, --instance, --worker, and --release",
                )?,
                worker: raw.require(
                    "worker",
                    "gb-client: fleet provision requires --operation, --instance, --worker, and --release",
                )?,
                release: raw.require(
                    "release",
                    "gb-client: fleet provision requires --operation, --instance, --worker, and --release",
                )?,
                config: raw.get("config"),
                workspace: raw.get("workspace"),
                read_only: raw.has("read-only"),
                grants: raw.all("grant"),
                secret_refs: raw.all("secret-ref"),
                profile_name: raw.get("profile-name"),
                profile_create: raw.has("profile-create"),
                profile_instruction: raw.get("profile-instruction"),
                profile_members: raw.all("profile-member"),
                profile_capabilities: raw.all("profile-capability"),
                profile_grants: raw.all("profile-grant"),
                addr: raw.get("addr"),
                control_token: raw.get("control-token"),
            },
        },
        "inspect" => FleetCommand::Inspect {
            host: raw.require_positional(1, "gb-client: fleet inspect requires <host> <instance>")?,
            instance: raw.require_positional(
                2,
                "gb-client: fleet inspect requires <host> <instance>",
            )?,
        },
        "upgrade" => {
            let host =
                raw.require_positional(1, "gb-client: fleet upgrade requires <host> <instance>")?;
            let instance = raw.require_positional(
                2,
                "gb-client: fleet upgrade requires <host> <instance>",
            )?;
            let work_policy = match raw.get("work-policy").as_deref() {
                None | Some("drain") => WorkPolicy::Drain,
                Some("pin") => WorkPolicy::Pin,
                Some("migrate") => WorkPolicy::Migrate,
                Some(_) => {
                    return Err("gb-client: --work-policy must be drain, pin, or migrate".into())
                }
            };
            FleetCommand::Upgrade {
                host,
                instance,
                options: FleetUpgradeOptions {
                    operation: raw.require(
                        "operation",
                        "gb-client: fleet upgrade requires --operation and --release",
                    )?,
                    release: raw.require(
                        "release",
                        "gb-client: fleet upgrade requires --operation and --release",
                    )?,
                    authorized_permissions: raw.all("authorize"),
                    work_policy,
                    verify_input: raw.get("verify-input"),
                    addr: raw.get("addr"),
                },
            }
        }
        "rollback" => FleetCommand::Rollback {
            host: raw.require_positional(1, "gb-client: fleet rollback requires <host> <instance>")?,
            instance: raw.require_positional(
                2,
                "gb-client: fleet rollback requires <host> <instance>",
            )?,
            operation: raw.require("operation", "gb-client: fleet rollback requires --operation")?,
        },
        _ => {
            return Err(
                "gb-client: fleet expects hosts, host, provision, inspect, upgrade, or rollback"
                    .to_string(),
            )
        }
    })
}

fn parse_approval(name: &str, rest: Vec<String>) -> Result<ApprovalCommand, String> {
    let raw = split(rest, &["digest"], &["pending"])?;
    let command = if name == "approvals" {
        ApprovalCommand::List {
            task: raw.require_positional(0, "gb-client: approvals requires <task-id>")?,
            pending: raw.has("pending"),
        }
    } else {
        let task = raw.require_positional(
            0,
            &format!("gb-client: {name} requires <task-id> <approval-id>"),
        )?;
        let approval = raw.require_positional(
            1,
            &format!("gb-client: {name} requires <task-id> <approval-id>"),
        )?;
        match name {
            "approve" => ApprovalCommand::Decide {
                task,
                approval,
                decision: "approved".into(),
                digest: raw.get("digest"),
            },
            "reject" => ApprovalCommand::Decide {
                task,
                approval,
                decision: "rejected".into(),
                digest: raw.get("digest"),
            },
            _ => ApprovalCommand::Revoke { task, approval },
        }
    };
    Ok(command)
}

fn parse_control(rest: Vec<String>) -> Result<ControlCommand, String> {
    let raw = split(
        rest,
        &[
            "worker",
            "session",
            "ttl",
            "generation",
            "epoch",
            "navigate",
            "click",
            "type",
            "text",
            "extract",
        ],
        &[],
    )?;
    Ok(match raw.positional(0).unwrap_or("") {
        "acquire" => ControlCommand::Acquire {
            worker: raw.require("worker", "gb-client: control requires --worker <id>")?,
            session: raw.require("session", "gb-client: control requires --session <id>")?,
            ttl: raw.uint("ttl", "--ttl")?,
        },
        "status" => ControlCommand::Status {
            worker: raw.require("worker", "gb-client: control requires --worker <id>")?,
            session: raw.require("session", "gb-client: control requires --session <id>")?,
        },
        "handback" => ControlCommand::HandBack {
            worker: raw.require("worker", "gb-client: control requires --worker <id>")?,
            session: raw.require("session", "gb-client: control requires --session <id>")?,
            generation: raw.uint("generation", "--generation")?.ok_or_else(|| {
                "gb-client: control handback requires --generation <n> and --epoch <epoch>"
                    .to_string()
            })?,
            epoch: raw.require(
                "epoch",
                "gb-client: control handback requires --generation <n> and --epoch <epoch>",
            )?,
        },
        "audit" => ControlCommand::Audit {
            worker: raw.require("worker", "gb-client: control requires --worker <id>")?,
            session: raw.require("session", "gb-client: control requires --session <id>")?,
        },
        "command" => {
            let worker = raw.require("worker", "gb-client: control requires --worker <id>")?;
            let session = raw.require("session", "gb-client: control requires --session <id>")?;
            let generation = raw.uint("generation", "--generation")?.ok_or_else(|| {
                "gb-client: control command requires --generation <n> and --epoch <epoch>"
                    .to_string()
            })?;
            let epoch = raw.require(
                "epoch",
                "gb-client: control command requires --generation <n> and --epoch <epoch>",
            )?;
            if let Some(url) = raw.get("navigate") {
                ControlCommand::Command {
                    worker,
                    session,
                    generation,
                    epoch,
                    command: common::ControlCommand::Navigate { url },
                }
            } else if let Some(selector) = raw.get("click") {
                ControlCommand::Command {
                    worker,
                    session,
                    generation,
                    epoch,
                    command: common::ControlCommand::Click { selector },
                }
            } else if let Some(selector) = raw.get("type") {
                ControlCommand::Command {
                    worker,
                    session,
                    generation,
                    epoch,
                    command: common::ControlCommand::Type {
                        selector,
                        text: raw.get("text").unwrap_or_default(),
                    },
                }
            } else if let Some(selector) = raw.get("extract") {
                ControlCommand::Command {
                    worker,
                    session,
                    generation,
                    epoch,
                    command: common::ControlCommand::ExtractText { selector },
                }
            } else {
                return Err(
                    "gb-client: control command requires --navigate, --click, --type, or --extract"
                        .into(),
                );
            }
        }
        _ => {
            return Err(
                "gb-client: control expects acquire, status, command, handback, or audit".into(),
            )
        }
    })
}

fn parse_diagnose(rest: Vec<String>) -> Result<DiagnoseCommand, String> {
    let raw = split(rest, &["worker"], &[])?;
    Ok(DiagnoseCommand {
        worker: raw.require("worker", "gb-client: diagnose requires --worker <id>")?,
        task: raw.positional(0).map(str::to_string),
    })
}

/// Parse-domain flags from `rest`.
fn split(
    tokens: Vec<String>,
    values: &[&'static str],
    booleans: &[&'static str],
) -> Result<Raw, String> {
    let mut raw = Raw::default();
    let mut tokens = tokens.into_iter();
    while let Some(token) = tokens.next() {
        if let Some(name) = token.strip_prefix("--") {
            let matched = values
                .iter()
                .find(|value| **value == name)
                .copied()
                .map(|name| (name, true))
                .or_else(|| {
                    booleans
                        .iter()
                        .find(|boolean| **boolean == name)
                        .copied()
                        .map(|name| (name, false))
                });
            let Some((name, takes_value)) = matched else {
                return Err("gb-client: unrecognized option".into());
            };
            if takes_value {
                raw.values.entry(name).or_default().push(
                    tokens
                        .next()
                        .ok_or_else(|| "gb-client: option requires a value".to_string())?,
                );
            } else {
                raw.booleans.insert(name);
            }
        } else {
            raw.positionals.push(token);
        }
    }
    Ok(raw)
}

/// Every flag that consumes the following token. Used only to find the command
/// token when flags appear before it.
fn takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--gateway"
            | "--token"
            | "--worker"
            | "--agent"
            | "--input"
            | "--session"
            | "--key"
            | "--capability"
            | "--protocol"
            | "--locality"
            | "--health"
            | "--after"
            | "--limit"
            | "--revision"
            | "--version"
            | "--expected"
            | "--master"
            | "--member"
            | "--grant"
            | "--restriction"
            | "--instruction"
            | "--model"
            | "--adapter"
            | "--allowed-model"
            | "--modality"
            | "--residency"
            | "--context-window"
            | "--output"
            | "--composition"
            | "--max-depth"
            | "--max-fanout"
            | "--skill"
            | "--mcp"
            | "--secret-ref"
            | "--compaction-budget"
            | "--profile"
            | "--team"
            | "--team-version"
            | "--endpoint"
            | "--instance"
            | "--package"
            | "--generation"
            | "--epoch"
            | "--ttl"
            | "--digest"
            | "--navigate"
            | "--click"
            | "--type"
            | "--text"
            | "--extract"
            | "--operation"
            | "--release"
            | "--addr"
            | "--config"
            | "--workspace"
            | "--profile-name"
            | "--profile-instruction"
            | "--profile-member"
            | "--profile-capability"
            | "--profile-grant"
            | "--authorize"
            | "--work-policy"
            | "--verify-input"
            | "--control-token"
            | "--provider"
    )
}

fn find_command(args: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        if argument == "--" {
            return (index + 1 < args.len()).then_some(index + 1);
        }
        if argument.starts_with('-') {
            index += if takes_value(argument) { 2 } else { 1 };
            continue;
        }
        return Some(index);
    }
    None
}

fn peel_globals(
    args: &[String],
    command_index: Option<usize>,
) -> Result<(ConnectionOptions, bool, Vec<String>), String> {
    let mut connection = ConnectionOptions::default();
    let mut json = false;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if Some(index) == command_index {
            index += 1;
            continue;
        }
        let argument = args[index].as_str();
        match argument {
            "--json" => json = true,
            "--gateway" => {
                connection.gateway = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or_else(|| "gb-client: option requires a value".to_string())?,
                );
                index += 1;
            }
            "--token" => {
                connection.token = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or_else(|| "gb-client: option requires a value".to_string())?,
                );
                index += 1;
            }
            _ => {
                rest.push(argument.to_string());
                if takes_value(argument) {
                    if let Some(value) = args.get(index + 1) {
                        rest.push(value.clone());
                        index += 1;
                    }
                }
            }
        }
        index += 1;
    }
    Ok((connection, json, rest))
}

#[derive(Default)]
struct Raw {
    positionals: Vec<String>,
    values: BTreeMap<&'static str, Vec<String>>,
    booleans: BTreeSet<&'static str>,
}

impl Raw {
    fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }

    fn require_positional(&self, index: usize, message: &str) -> Result<String, String> {
        self.positional(index)
            .map(str::to_string)
            .ok_or_else(|| message.to_string())
    }

    fn get(&self, name: &'static str) -> Option<String> {
        self.values
            .get(name)
            .and_then(|values| values.last().cloned())
    }

    fn all(&self, name: &'static str) -> Vec<String> {
        self.values.get(name).cloned().unwrap_or_default()
    }

    fn has(&self, name: &'static str) -> bool {
        self.booleans.contains(name)
    }

    fn require(&self, name: &'static str, message: &str) -> Result<String, String> {
        self.get(name).ok_or_else(|| message.to_string())
    }

    fn uint(&self, name: &'static str, label: &str) -> Result<Option<u64>, String> {
        self.get(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| format!("gb-client: {label} requires an integer"))
            })
            .transpose()
    }

    fn u32(&self, name: &'static str, label: &str) -> Result<Option<u32>, String> {
        self.get(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| format!("gb-client: {label} requires an integer"))
            })
            .transpose()
    }

    fn usize(&self, name: &'static str, label: &str) -> Result<Option<usize>, String> {
        self.get(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| format!("gb-client: {label} requires a positive integer"))
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Cli {
        parse(
            args.iter()
                .map(|argument| (*argument).to_string())
                .collect(),
        )
        .expect("arguments parse")
    }

    #[test]
    fn bare_invocation_is_chat() {
        let cli = parsed(&[]);
        assert!(matches!(cli.command, Command::Chat(_)));
        assert!(!cli.json);
    }

    #[test]
    fn chat_owns_worker_and_agent_flags() {
        let cli = parsed(&["chat", "--worker", "worker-a", "--agent", "echo"]);
        let Command::Chat(options) = cli.command else {
            panic!("expected chat");
        };
        assert_eq!(options.worker.as_deref(), Some("worker-a"));
        assert_eq!(options.agent.as_deref(), Some("echo"));
        assert!(parse(vec!["chat".into(), "extra".into()]).is_err());
    }

    #[test]
    fn global_flags_work_before_or_after_the_command() {
        let cli = parsed(&[
            "--gateway",
            "http://127.0.0.1:8088",
            "--token",
            "token",
            "provider",
            "list",
            "--json",
        ]);
        assert!(cli.json);
        assert_eq!(
            cli.connection.gateway.as_deref(),
            Some("http://127.0.0.1:8088")
        );
        assert!(matches!(
            cli.command,
            Command::Provider(ProviderCommand::List)
        ));
    }

    #[test]
    fn provider_add_owns_metadata_flags() {
        let cli = parsed(&[
            "provider",
            "add",
            "local",
            "--adapter",
            "ollama",
            "--model",
            "llama",
            "--endpoint",
            "http://127.0.0.1:11434",
            "--allowed-model",
            "llama",
            "--worker",
            "worker-a",
            "--worker",
            "worker-b",
            "--local",
            "--json",
        ]);
        let Command::Provider(ProviderCommand::Add { id, options }) = cli.command else {
            panic!("expected provider add");
        };
        assert_eq!(id, "local");
        assert_eq!(options.adapter, "ollama");
        assert_eq!(options.model, "llama");
        assert_eq!(options.allowed_workers, ["worker-a", "worker-b"]);
        assert!(options.local);
        assert!(cli.json);
    }

    #[test]
    fn provider_update_tracks_revision_and_toggle_flags() {
        let cli = parsed(&[
            "provider",
            "update",
            "local",
            "--revision",
            "3",
            "--disable",
            "--clear-workers",
            "--no-auto-default",
        ]);
        let Command::Provider(ProviderCommand::Update { id, options }) = cli.command else {
            panic!("expected provider update");
        };
        assert_eq!(id, "local");
        assert_eq!(options.revision, 3);
        assert_eq!(options.active, Some(false));
        assert_eq!(options.allowed_workers, Some(Vec::new()));
        assert_eq!(options.allow_auto_default, Some(false));
    }

    #[test]
    fn worker_lifecycle_commands_parse() {
        let cli = parsed(&[
            "worker",
            "add",
            "worker-c",
            "http://127.0.0.1:9091",
            "--instance",
            "one",
            "--package",
            "echo",
            "--capability",
            "echo",
            "--no-preflight",
        ]);
        let Command::Worker(WorkerCommand::Add(options)) = cli.command else {
            panic!("expected worker add");
        };
        assert_eq!(options.id, "worker-c");
        assert_eq!(options.endpoint, "http://127.0.0.1:9091");
        assert_eq!(options.capabilities, Some(vec!["echo".to_string()]));
        assert!(!options.preflight);

        let cli = parsed(&[
            "worker",
            "update",
            "worker-c",
            "--revision",
            "2",
            "--endpoint",
            "http://127.0.0.1:9092",
        ]);
        let Command::Worker(WorkerCommand::Update { id, options }) = cli.command else {
            panic!("expected worker update");
        };
        assert_eq!(id, "worker-c");
        assert_eq!(options.revision, 2);
    }

    #[test]
    fn worker_discovery_owns_filter_flags() {
        let cli = parsed(&[
            "workers",
            "--capability",
            "general",
            "--locality",
            "local",
            "--health",
            "healthy",
        ]);
        let Command::Workers(query) = cli.command else {
            panic!("expected workers");
        };
        assert_eq!(query.capability.as_deref(), Some("general"));
        assert_eq!(query.locality.as_deref(), Some("local"));
        assert_eq!(query.health.as_deref(), Some("healthy"));
    }

    #[test]
    fn tasks_own_submission_and_stream_flags() {
        let cli = parsed(&[
            "submit",
            "--worker",
            "worker-a",
            "--agent",
            "general",
            "--input",
            "hello",
            "--session",
            "session-1",
        ]);
        let Command::Task(TaskCommand::Submit(options)) = cli.command else {
            panic!("expected submit");
        };
        assert_eq!(options.worker.as_deref(), Some("worker-a"));
        assert_eq!(options.input.as_deref(), Some("hello"));

        let cli = parsed(&["events", "task-1", "--after", "4", "--limit", "20"]);
        let Command::Task(TaskCommand::Events { id, after, limit }) = cli.command else {
            panic!("expected events");
        };
        assert_eq!(id, "task-1");
        assert_eq!(after, 4);
        assert_eq!(limit, 20);
    }

    #[test]
    fn model_commands_are_provider_scoped() {
        let cli = parsed(&["model", "allow", "local", "--model", "llama"]);
        let Command::Model(ModelCommand::Allow { provider, models }) = cli.command else {
            panic!("expected model allow");
        };
        assert_eq!(provider, "local");
        assert_eq!(models, ["llama"]);

        let cli = parsed(&["models", "clear", "local"]);
        assert!(matches!(
            cli.command,
            Command::Model(ModelCommand::Clear { .. })
        ));
    }

    #[test]
    fn team_and_profile_commands_own_their_flags() {
        let cli = parsed(&[
            "team",
            "create",
            "alpha",
            "--master",
            "worker-a",
            "--member",
            "worker-a:general",
            "--max-depth",
            "2",
        ]);
        let Command::Team(TeamCommand::Create { name, options }) = cli.command else {
            panic!("expected team create");
        };
        assert_eq!(name, "alpha");
        assert_eq!(options.master.as_deref(), Some("worker-a"));
        assert_eq!(options.max_depth, Some(2));

        let cli = parsed(&[
            "profile",
            "create",
            "research",
            "--worker",
            "worker-a",
            "--instruction",
            "be useful",
            "--grant",
            "task:submit",
        ]);
        let Command::Profile(ProfileCommand {
            worker,
            action: ProfileAction::Create { name, options },
        }) = cli.command
        else {
            panic!("expected profile create");
        };
        assert_eq!(worker.as_deref(), Some("worker-a"));
        assert_eq!(name, "research");
        assert_eq!(options.instruction, "be useful");
    }

    #[test]
    fn malformed_values_and_unknown_flags_fail_before_execution() {
        assert!(parse(vec!["provider".into(), "list".into(), "--nope".into()]).is_err());
        assert!(parse(vec![
            "worker".into(),
            "update".into(),
            "worker-a".into(),
            "--revision".into(),
            "many".into()
        ])
        .is_err());
        assert!(parse(vec!["task".into()]).is_err());
        assert!(parse(vec!["chat".into(), "extra".into()]).is_err());
    }

    #[test]
    fn manage_and_health_are_typed_commands() {
        assert!(matches!(parsed(&["manage"]).command, Command::Manage));
        assert!(matches!(parsed(&["health"]).command, Command::Health));
    }
}
