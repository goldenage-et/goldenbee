//! Shared cross-process fixture: operator config plus file-defined assets.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub struct Fixture {
    pub config: PathBuf,
    pub assets: PathBuf,
    pub agents: PathBuf,
    pub prompts: PathBuf,
}

pub struct Options {
    pub instance: String,
    pub token: Option<String>,
    pub model: String,
    pub principals: Vec<(String, String, String)>,
    pub settings: Vec<(String, String)>,
    pub token_scopes: Vec<String>,
}

/// Optional stream limits applied to one fixture Worker.
#[derive(Debug, Clone, Default)]
pub struct StreamSettings {
    pub event_retention: Option<usize>,
    pub max_streams: Option<usize>,
    pub stream_buffer: Option<usize>,
    pub stream_keepalive_ms: Option<u64>,
}

/// Optional Gateway registration and lease heartbeat configuration.
pub struct GatewayFixture {
    pub url: String,
    pub control_token: String,
    pub renew_interval_ms: u64,
    pub advertise_url: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            instance: "general-test".into(),
            token: Some("dev-token".into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        }
    }
}

pub fn write(data: &Path, options: Options) -> Fixture {
    write_with_gateway(data, options, None)
}

pub fn write_stream(data: &Path, options: Options, stream: StreamSettings) -> Fixture {
    write_inner(data, options, None, stream)
}

pub fn write_with_gateway(
    data: &Path,
    options: Options,
    gateway: Option<GatewayFixture>,
) -> Fixture {
    write_inner(data, options, gateway, StreamSettings::default())
}

fn write_inner(
    data: &Path,
    options: Options,
    gateway: Option<GatewayFixture>,
    stream: StreamSettings,
) -> Fixture {
    let assets = data.join("assets");
    let agents = assets.join("agents");
    let prompts = assets.join("prompts");
    let skills = assets.join("skills").join("tone");
    std::fs::create_dir_all(&agents).expect("agent directory");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::create_dir_all(&skills).expect("skill directory");
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: tone\ndescription: Keep replies concise and factual.\n---\nPrefer short, direct answers.",
    )
    .expect("skill");
    std::fs::write(
        prompts.join("general.md"),
        "You are the Goldenbee General Worker. Answer usefully and concisely.",
    )
    .expect("general prompt");
    std::fs::write(
        prompts.join("echo.md"),
        "Repeat the user's message. Keep it short.",
    )
    .expect("echo prompt");
    for (name, instruction) in [("general", "general"), ("echo", "echo")] {
        let (tools, skills) = if name == "general" {
            (
                "tools = [\"exec_command\", \"save_artifact\", \"remember_memory\"]\n",
                "skills = [\"tone\"]\n",
            )
        } else {
            ("", "")
        };
        std::fs::write(
            agents.join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\ndescription = \"{name} native ADK agent\"\nmodel = \"{}\"\nprompt = \"{instruction}\"\n{tools}{skills}",
                options.model
            ),
        )
        .expect("agent definition");
    }

    let mut config = String::new();
    config.push_str(&format!("instance = \"{}\"\n", options.instance));
    config.push_str("namespace = \"local\"\ndefault_agent = \"general\"\n");
    if let Some(token) = &options.token {
        config.push_str(&format!("token = \"{token}\"\n"));
        config.push_str(&format!(
            "token_scopes = [{}]\n",
            options
                .token_scopes
                .iter()
                .map(|scope| format!("\"{scope}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(value) = stream.event_retention {
        config.push_str(&format!("event_retention = {value}\n"));
    }
    if let Some(value) = stream.max_streams {
        config.push_str(&format!("max_streams = {value}\n"));
    }
    if let Some(value) = stream.stream_buffer {
        config.push_str(&format!("stream_buffer = {value}\n"));
    }
    if let Some(value) = stream.stream_keepalive_ms {
        config.push_str(&format!("stream_keepalive_ms = {value}\n"));
    }
    for (token, namespace, principal) in &options.principals {
        config.push_str(&format!(
            "\n[[tokens]]\ntoken = \"{token}\"\nnamespace = \"{namespace}\"\nprincipal = \"{principal}\"\nscopes = [\"task:submit\", \"task:read\", \"session:write\", \"artifact:read\"]\n"
        ));
    }
    config.push_str(
        "\n[paths]\nagents = \"assets/agents\"\nprompts = \"assets/prompts\"\nskills = \"assets/skills\"\n",
    );
    config.push_str(&format!(
        "\n[storage]\ndirectory = \"{}\"\n",
        data.display()
    ));
    if let Some(gateway) = &gateway {
        config.push_str(&format!(
            "\n[gateway]\nurl = \"{}\"\ncontrol_token = \"{}\"\nrenew_interval_ms = {}\n",
            gateway.url, gateway.control_token, gateway.renew_interval_ms
        ));
        if let Some(advertise) = &gateway.advertise_url {
            config.push_str(&format!("advertise_url = \"{advertise}\"\n"));
        }
    }
    if !options.settings.is_empty() {
        config.push_str("\n[settings]\n");
        for (key, value) in &options.settings {
            config.push_str(&format!("{key} = \"{value}\"\n"));
        }
    }
    let config_path = data.join("worker.toml");
    std::fs::write(&config_path, config).expect("worker config");
    Fixture {
        config: config_path,
        assets,
        agents,
        prompts,
    }
}

/// Insert a top-level TOML assignment before the first table header.
pub fn set_top_level(fixture: &Fixture, assignment: &str) {
    let text = std::fs::read_to_string(&fixture.config).expect("worker config");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let position = lines
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    lines.insert(position, assignment.to_string());
    std::fs::write(&fixture.config, format!("{}\n", lines.join("\n")))
        .expect("worker config update");
}

pub fn set_agent_prompt(fixture: &Fixture, agent: &str, prompt: &str) {
    std::fs::write(fixture.prompts.join(format!("{agent}.md")), prompt).expect("prompt update");
}

pub fn set_agent_tools(fixture: &Fixture, agent: &str, tools: &[&str]) {
    let path = fixture.agents.join(format!("{agent}.toml"));
    let current = std::fs::read_to_string(&path).expect("agent definition");
    let mut lines: Vec<String> = current
        .lines()
        .filter(|line| !line.starts_with("tools ="))
        .map(str::to_string)
        .collect();
    if !tools.is_empty() {
        lines.push(format!(
            "tools = [{}]",
            tools
                .iter()
                .map(|tool| format!("\"{tool}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    std::fs::write(path, format!("{}\n", lines.join("\n"))).expect("agent update");
}
