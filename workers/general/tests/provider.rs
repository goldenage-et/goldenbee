//! Operator-selected providers resolve for file-loaded and manual agents.
//!
//! Each test starts the packaged `general` executable with an operator-chosen
//! configuration directory. The executable is never rebuilt between cases.

use serde_json::Value;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

mod fixture;

fn stub() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    let count = Arc::new(AtomicUsize::new(0));
    let requests = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { continue };
            requests.fetch_add(1, Ordering::SeqCst);
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match socket.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                    Err(_) => break,
                }
                let text = String::from_utf8_lossy(&buffer);
                if let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_end = header_end + 4;
                    let length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buffer.len() >= header_end + length {
                        break;
                    }
                }
            }
            let body = r#"{"text":"fixture: configured provider"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.flush();
        }
    });
    (format!("http://{address}/generate"), count)
}

fn cli_json(config: &Path, arguments: &[&str]) -> Value {
    cli_json_env(config, arguments, &[])
}

fn cli_json_env(config: &Path, arguments: &[&str], env: &[(&str, &str)]) -> Value {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let mut args = vec!["--config", config.to_str().expect("config path")];
    args.extend_from_slice(arguments);
    let mut command = Command::new(env!("CARGO_BIN_EXE_general"));
    command.args(args).current_dir(cwd.path());
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("general runs");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn cli(config: &Path, arguments: &[&str]) -> Output {
    let cwd = tempfile::tempdir().expect("unrelated cwd");
    let mut args = vec!["--config", config.to_str().expect("config path")];
    args.extend_from_slice(arguments);
    Command::new(env!("CARGO_BIN_EXE_general"))
        .args(args)
        .current_dir(cwd.path())
        .output()
        .expect("general runs")
}

fn with_http_provider(config: &Path, provider: &str, endpoint: &str) {
    let mut text = std::fs::read_to_string(config).expect("worker config");
    text.push_str(&format!(
        "\n[providers.{provider}]\nadapter = \"http-json\"\nmodel = \"fixture-model\"\nendpoint = \"{endpoint}\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = true\nresidency = \"local\"\ncontext_window = 8192\ncost_rank = 0\ndata_class = \"local\"\n"
    ));
    std::fs::write(config, text).expect("provider config");
}

fn with_remote_provider(config: &Path, provider: &str, endpoint: &str, credential: &str) {
    let text = std::fs::read_to_string(config).expect("worker config");
    // Root keys must appear before the first table.
    let mut text = text.replacen("\n[paths]", "\nallow_remote = true\n\n[paths]", 1);
    text.push_str(&format!(
        "\n[providers.{provider}]\nadapter = \"openai-compatible\"\nmodel = \"fixture-model\"\nendpoint = \"{endpoint}\"\nmodalities = [\"text\"]\ntool_calling = true\nlocal = false\nresidency = \"remote\"\ncontext_window = 8192\ncredential = \"{credential}\"\ncost_rank = 5\ndata_class = \"shared\"\n"
    ));
    std::fs::write(config, text).expect("provider config");
}

/// OpenAI-protocol streaming fixture. The native Runner requests a stream, so
/// the fixture answers with the provider's SSE chat-completion shape.
fn chat_stub() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    let count = Arc::new(AtomicUsize::new(0));
    let requests = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { continue };
            requests.fetch_add(1, Ordering::SeqCst);
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match socket.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                    Err(_) => break,
                }
                let text = String::from_utf8_lossy(&buffer);
                if let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_end = header_end + 4;
                    let length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buffer.len() >= header_end + length {
                        break;
                    }
                }
            }
            let chunk = serde_json::json!({
                "id": "chatcmpl-fixture",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "fixture",
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": "remote: configured provider"}, "finish_reason": null}]
            });
            let done = serde_json::json!({
                "id": "chatcmpl-fixture",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "fixture",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
            });
            let body = format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.flush();
        }
    });
    (format!("http://{address}/v1"), count)
}

#[test]
fn file_loaded_and_manual_agents_resolve_the_same_operator_provider() {
    let (endpoint, requests) = stub();
    let data = tempfile::tempdir().expect("fixture directory");
    let fixture = fixture::write(data.path(), Default::default());
    with_http_provider(&fixture.config, "deterministic", &endpoint);

    let file_agent = cli_json(
        &fixture.config,
        &[
            "run",
            "--agent",
            "general",
            "--input",
            "hello from a file agent",
            "--session",
            "file-session",
            "--json",
        ],
    );
    assert_eq!(
        file_agent["output"], "fixture: configured provider",
        "{file_agent}"
    );
    assert_eq!(file_agent["resolved_model"]["provider"], "deterministic");
    assert_eq!(file_agent["resolved_model"]["adapter"], "http-json");

    let manual = cli_json(
        &fixture.config,
        &[
            "run",
            "--agent",
            "manual",
            "--input",
            "hello from a manual agent",
            "--session",
            "manual-session",
            "--json",
        ],
    );
    assert_eq!(manual["output"], "fixture: configured provider", "{manual}");
    assert_eq!(manual["resolved_model"]["provider"], "deterministic");
    assert_eq!(manual["resolved_model"]["adapter"], "http-json");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "both agents must use the configured endpoint"
    );
}

#[test]
fn operator_configuration_directories_select_different_models_without_recompilation() {
    let (endpoint, _requests) = stub();

    let deterministic_dir = tempfile::tempdir().expect("deterministic fixture");
    let deterministic = fixture::write(deterministic_dir.path(), Default::default());
    let local = cli_json(
        &deterministic.config,
        &[
            "run",
            "--agent",
            "general",
            "--input",
            "same task",
            "--session",
            "session-one",
            "--json",
        ],
    );
    assert_eq!(local["resolved_model"]["adapter"], "deterministic");
    assert!(
        local["output"]
            .as_str()
            .unwrap_or_default()
            .starts_with("echo:"),
        "{local}"
    );

    let configured_dir = tempfile::tempdir().expect("configured fixture");
    let configured = fixture::write(configured_dir.path(), Default::default());
    with_http_provider(&configured.config, "deterministic", &endpoint);
    let remote = cli_json(
        &configured.config,
        &[
            "run",
            "--agent",
            "general",
            "--input",
            "same task",
            "--session",
            "session-one",
            "--json",
        ],
    );
    assert_eq!(remote["resolved_model"]["adapter"], "http-json");
    assert_eq!(remote["output"], "fixture: configured provider", "{remote}");
}

#[test]
fn local_and_remote_endpoint_configurations_run_the_same_task_contract() {
    let (local_endpoint, _local_requests) = stub();
    let local_dir = tempfile::tempdir().expect("local fixture");
    let local_fixture = fixture::write(local_dir.path(), Default::default());
    with_http_provider(&local_fixture.config, "deterministic", &local_endpoint);
    let local = cli_json(
        &local_fixture.config,
        &[
            "run",
            "--agent",
            "general",
            "--input",
            "same contract",
            "--session",
            "contract-session",
            "--json",
        ],
    );
    assert_eq!(local["output"], "fixture: configured provider");
    assert_eq!(local["resolved_model"]["local"], true);
    assert_eq!(local["resolved_model"]["residency"], "local");

    let (remote_endpoint, remote_requests) = chat_stub();
    let remote_dir = tempfile::tempdir().expect("remote fixture");
    let remote_fixture = fixture::write(remote_dir.path(), Default::default());
    with_remote_provider(
        &remote_fixture.config,
        "deterministic",
        &remote_endpoint,
        "env:GOLDENBEE_TEST_REMOTE_KEY",
    );
    let remote = cli_json_env(
        &remote_fixture.config,
        &[
            "run",
            "--agent",
            "general",
            "--input",
            "same contract",
            "--session",
            "contract-session",
            "--json",
        ],
        &[("GOLDENBEE_TEST_REMOTE_KEY", "fixture-remote-key")],
    );
    assert_eq!(remote["output"], "remote: configured provider", "{remote}");
    assert_eq!(remote["resolved_model"]["adapter"], "openai-compatible");
    assert_eq!(remote["resolved_model"]["local"], false);
    assert_eq!(remote["resolved_model"]["residency"], "remote");
    assert_eq!(remote_requests.load(Ordering::SeqCst), 1);
}

#[test]
fn unknown_provider_fails_before_execution_without_leaking_configuration() {
    let data = tempfile::tempdir().expect("fixture directory");
    let fixture = fixture::write(data.path(), Default::default());
    let mut text = std::fs::read_to_string(&fixture.config).expect("worker config");
    text.push_str(
        "\n[providers.broken]\nadapter = \"openai-compatible\"\nmodel = \"m\"\nendpoint = \"http://127.0.0.1:1/v1\"\nlocal = false\nresidency = \"remote\"\ncredential = \"env:GOLDENBEE_MISSING_TEST_KEY\"\n",
    );
    std::fs::write(&fixture.config, text).expect("provider config");
    let agent = fixture.agents.join("general.toml");
    let definition = std::fs::read_to_string(&agent).expect("agent definition");
    std::fs::write(
        &agent,
        definition.replace("model = \"deterministic\"", "model = \"broken\""),
    )
    .expect("agent model");

    let output = cli(
        &fixture.config,
        &[
            "run",
            "--agent",
            "general",
            "--input",
            "hello",
            "--session",
            "broken-session",
        ],
    );
    assert!(
        !output.status.success(),
        "invalid provider must fail startup"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("model_unresolved")
            || stderr.contains("credential")
            || stderr.contains("remote_not_approved"),
        "unexpected diagnostic: {stderr}"
    );
    assert!(!stderr.contains("GOLDENBEE_MISSING_TEST_KEY="));
}
