//! General Worker Package executable.
//!
//! Package code supplies one compiled Worker definition. The shared runtime
//! host owns CLI parsing, configuration, asset loading, serving, and shutdown.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Local development convenience: load .env from the working directory or
    // its parents. Existing process variables always win.
    let _ = dotenvy::dotenv();
    let worker = match general::worker() {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("general: {error}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("general: runtime could not start: {error}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(worker.run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "general: {}: {}",
                error.code,
                common::redact(&error.message)
            );
            ExitCode::from(if error.code.starts_with("cli.") { 2 } else { 1 })
        }
    }
}
