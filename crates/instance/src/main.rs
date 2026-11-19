//! Local Worker instance lifecycle command.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match instance::run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("gb-instance: {}: {}", error.code, error.message);
            ExitCode::from(1)
        }
    }
}
