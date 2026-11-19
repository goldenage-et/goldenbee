//! Goldenbee operator bootstrap and component dispatcher.
//!
//! `gb` installs the Gateway, composer, and independent Client executable from
//! one release directory and dispatches their existing command surfaces. It
//! does not link Gateway, composer, or Client implementation code.

mod cli;
mod dispatch;
mod download;
mod install;
mod paths;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run(std::env::args().skip(1).collect())
}
