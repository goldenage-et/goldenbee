//! Interactive chat and management session entry points.

use crate::cli::connect::Connection;
use crate::cli::output::usage;
use crate::cli::profiles::Store;
use crate::repl;
use crate::session;
use crate::tui;
use sdk::Client;
use std::process::ExitCode;

/// Flags owned by the chat command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatOptions {
    /// Initial Worker id.
    pub worker: Option<String>,
    /// Initial agent capability.
    pub agent: Option<String>,
}

/// Open chat on the active stored profile.
pub(crate) async fn chat_command(options: ChatOptions) -> ExitCode {
    let store = match Store::open().await {
        Ok(store) => store,
        Err(error) => return crate::cli::output::api_error(error),
    };
    match store.active().await {
        Ok(Some(profile)) => repl::open(profile, options.worker, options.agent).await,
        Ok(None) => usage(
            "gb-client: no active Gateway. Add one with `gb-client gw add <name> <url> <token>`.",
        ),
        Err(error) => crate::cli::output::api_error(error),
    }
}

/// Open the management pages directly.
pub(crate) async fn manage_command(connection: Connection) -> ExitCode {
    if !tui::supported() {
        return usage("gb-client: manage requires a terminal on stdin and stdout");
    }
    let client = Client::new(connection.url.clone(), connection.token);
    let workers = session::reachable(&client).await.unwrap_or_default();
    let worker = workers
        .first()
        .map(|worker| worker.id.clone())
        .unwrap_or_default();
    let agent = session::first_agent(&workers, &worker, None).unwrap_or_default();
    let profile = connection.profile.unwrap_or_else(|| "gateway".to_string());
    tui::manage(profile, connection.url, client, workers, worker, agent).await
}
