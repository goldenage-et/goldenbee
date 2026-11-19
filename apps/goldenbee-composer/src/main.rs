//! Goldenbee host composer executable.

use std::process::ExitCode;

fn help() {
    println!(
        "Usage: gb-composer <command> [options]\n\n\
         Commands:\n  \
         serve --config <path> [--addr <address>]\n      \
         Provision, inspect, upgrade, and roll back local Worker instances\n      \
         for authenticated Gateway lifecycle operations.\n  \
         --version\n\n\
         Host credentials and filesystem roots are owned by the composer\n\
         configuration. The Gateway never receives host filesystem access."
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
        println!(concat!("gb-composer ", env!("CARGO_PKG_VERSION")));
        return ExitCode::SUCCESS;
    }
    if command != "serve" {
        eprintln!("gb-composer: unknown command");
        return ExitCode::from(2);
    }
    let mut config_path = None;
    let mut addr = None;
    let mut rest = args.get(1..).unwrap_or(&[]).iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--config" => match rest.next() {
                Some(value) => config_path = Some(value.clone()),
                None => {
                    eprintln!("gb-composer: --config requires a value");
                    return ExitCode::from(2);
                }
            },
            "--addr" => match rest.next() {
                Some(value) => addr = Some(value.clone()),
                None => {
                    eprintln!("gb-composer: --addr requires a value");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("gb-composer: unrecognized option");
                return ExitCode::from(2);
            }
        }
    }
    let Some(config_path) = config_path else {
        eprintln!("gb-composer: serve requires --config <path>");
        return ExitCode::from(2);
    };
    let config = match goldenbee_composer::Config::load(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("gb-composer: {}: {}", error.code, error.message);
            return ExitCode::from(1);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("gb-composer: async runtime could not start");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(goldenbee_composer::serve(config, addr)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gb-composer: {}: {}", error.code, error.message);
            ExitCode::from(1)
        }
    }
}
