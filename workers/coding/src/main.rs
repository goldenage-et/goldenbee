//! Coding Worker Package executable.
//!
//! Package code supplies one compiled Worker definition. The shared runtime
//! host owns CLI parsing, configuration, asset loading, serving, and shutdown.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Local development convenience: load .env from the working directory or
    // its parents. Existing process variables always win.
    let _ = dotenvy::dotenv();
    let worker = match coding::worker() {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("coding: {error}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("coding: runtime could not start: {error}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(worker.run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coding: {}: {}", error.code, common::redact(&error.message));
            ExitCode::from(common::exit_code(Some(&error), common::TaskState::Failed) as u8)
        }
    }
}
