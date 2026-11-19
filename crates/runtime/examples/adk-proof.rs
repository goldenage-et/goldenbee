//! Runnable native ADK standalone proof.
//!
//! ```sh
//! cargo run -p sdk --example adk-proof -- --input hello --input second
//! cargo run -p sdk --example adk-proof -- --endpoint http://127.0.0.1:8787 --input hello
//! ```

use runtime::native::model::{model_for, DeterministicModel, HttpModel};
use runtime::native::proof::ProofHarness;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Debug)]
struct Args {
    app: String,
    user: String,
    session: String,
    inputs: Vec<String>,
    endpoint: Option<String>,
    json: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        app: "goldenbee-proof".into(),
        user: "proof-user".into(),
        session: "proof-session".into(),
        inputs: Vec::new(),
        endpoint: None,
        json: false,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--app" => args.app = iter.next().ok_or("--app requires a value")?,
            "--user" => args.user = iter.next().ok_or("--user requires a value")?,
            "--session" => args.session = iter.next().ok_or("--session requires a value")?,
            "--input" => args
                .inputs
                .push(iter.next().ok_or("--input requires a value")?),
            "--endpoint" => args.endpoint = Some(iter.next().ok_or("--endpoint requires a value")?),
            "--json" => args.json = true,
            "--help" | "-h" => {
                return Err("usage: adk-proof [--app NAME] [--user ID] [--session ID] [--input TEXT]... [--endpoint URL] [--json]".into());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if args.inputs.is_empty() {
        args.inputs.push("hello".into());
        args.inputs.push("second".into());
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let model: Arc<dyn adk_rust::Llm> = match &args.endpoint {
        Some(endpoint) => Arc::new(HttpModel::new(endpoint, "http")),
        None => Arc::new(DeterministicModel::new()),
    };
    let harness = match ProofHarness::new(args.app, model) {
        Ok(harness) => harness,
        Err(error) => {
            eprintln!("config error: {error}");
            return ExitCode::FAILURE;
        }
    };
    for input in &args.inputs {
        match harness.run_turn(&args.user, &args.session, input).await {
            Ok(turn) => {
                if args.json {
                    match serde_json::to_string_pretty(&turn) {
                        Ok(json) => println!("{json}"),
                        Err(error) => {
                            eprintln!("serialization error: {error}");
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!(
                        "input={input}\noutput={}\ninvocation={}\nevents={} history={}\npartial_events={}\n",
                        turn.output,
                        turn.invocation_id,
                        turn.events.len(),
                        turn.history_len,
                        turn.events.iter().filter(|event| event.partial).count()
                    );
                }
            }
            Err(error) => {
                eprintln!("provider or configuration error: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    // Keep the default model helper referenced for documentation tests.
    let _ = model_for("deterministic");
    ExitCode::SUCCESS
}
