//! Goldenbee product CLI.
//!
//! Every Gateway command goes through the reusable Rust SDK. Gateway profiles
//! live in a local SQLite database. The CLI never talks to a Worker directly and
//! carries no native ADK dependency.

mod cli;
mod commands;
mod repl;
mod session;
mod tui;

use cli::args::{parse, Cli, Command};
use cli::connect::client;
use cli::help::help;
use cli::output::usage;
use std::process::ExitCode;

async fn run(arguments: Vec<String>) -> ExitCode {
    let Cli {
        connection,
        json,
        command,
    } = match parse(arguments) {
        Ok(cli) => cli,
        Err(message) => return usage(&message),
    };
    match command {
        Command::Chat(options) => commands::chat::chat_command(options).await,
        Command::Gateway(command) => commands::gateway::gateway_command(command, json).await,
        Command::Manage => match client(&connection).await {
            Ok((_client, connection)) => commands::chat::manage_command(connection).await,
            Err(message) => usage(&message),
        },
        command => match client(&connection).await {
            Ok((client, _connection)) => commands::dispatch(command, json, &client).await,
            Err(message) => usage(&message),
        },
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1
        && matches!(
            args.first().map(String::as_str),
            Some("--help" | "-h" | "help")
        )
    {
        help();
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 && matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!(concat!("gb-client ", env!("CARGO_PKG_VERSION")));
        return ExitCode::SUCCESS;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return usage("gb-client: async runtime could not start"),
    };
    runtime.block_on(run(args))
}
