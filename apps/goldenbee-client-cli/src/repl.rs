//! Interactive Gateway session.
//!
//! Opens one resolved Gateway profile, selects one reachable Worker and agent,
//! then either hands the session to the full-screen chat (when stdin and
//! stdout are terminals) or runs the line-oriented fallback used by scripts and
//! tests. `/worker` and `/agent` switch the current target without leaving the
//! session.

use crate::cli::profiles::Profile;
use crate::session;
use common::{ApiError, GatewaySubmitRequest, Limits, WorkerSummary};
use sdk::Client;
use std::io::{self, Write};
use std::process::ExitCode;

/// Open an interactive session for one stored profile.
pub async fn open(profile: Profile, worker: Option<String>, agent: Option<String>) -> ExitCode {
    let client = Client::new(profile.url.clone(), profile.token.clone());
    if let Err(error) = client.health().await {
        session::print_error(&error);
        return ExitCode::from(1);
    }
    let mut workers = match session::reachable(&client).await {
        Ok(workers) => workers,
        Err(error) => {
            session::print_error(&error);
            return ExitCode::from(1);
        }
    };
    if workers.is_empty() {
        eprintln!(
            "gb-client: no reachable Worker is registered with '{}'",
            profile.name
        );
        return ExitCode::from(1);
    }
    let mut worker = match worker {
        Some(id) => match workers.iter().find(|worker| worker.id == id) {
            Some(worker) => worker.id.clone(),
            None => {
                eprintln!("gb-client: Worker '{id}' is not reachable");
                return ExitCode::from(1);
            }
        },
        None => {
            let id = workers[0].id.clone();
            if workers.len() > 1 && !crate::tui::supported() {
                println!("using Worker '{id}'; type /worker to change");
            }
            id
        }
    };
    let mut agent = match session::first_agent(&workers, &worker, agent.as_deref()) {
        Ok(agent) => agent,
        Err(error) => {
            session::print_error(&error);
            return ExitCode::from(1);
        }
    };

    if crate::tui::supported() {
        return crate::tui::run(profile, client, workers, worker, agent).await;
    }

    println!("goldenbee · {}", profile.name);
    println!("gateway  {}", profile.url);
    println!("worker   {worker}");
    println!("agent    {agent}");
    println!("Type /help for commands, /exit to quit.");
    let mut session_id = session::session_id();
    let mut key = 0u64;
    let mut line = String::new();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        line.clear();
        match io::stdin().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/exit" || input == "/quit" {
            break;
        }
        if input == "/help" {
            println!("/worker [id]  list or switch the Worker");
            println!("/agents       list agent capabilities on this Worker");
            println!("/agent [name] list or switch the agent");
            println!("/workers      list reachable Workers");
            println!("/gateway      show the active Gateway");
            println!("/new          start a new session");
            println!("/exit         leave");
            continue;
        }
        if input == "/gateway" {
            println!("{} {}", profile.name, profile.url);
            continue;
        }
        if input == "/new" {
            session_id = session::session_id();
            println!("new session {session_id}");
            continue;
        }
        if input == "/workers" {
            match session::reachable(&client).await {
                Ok(current) => {
                    workers = current;
                    list_workers(&workers, &worker);
                }
                Err(error) => session::print_error(&error),
            }
            continue;
        }
        if let Some(rest) = input.strip_prefix("/worker") {
            let wanted = rest.trim();
            workers = match session::reachable(&client).await {
                Ok(current) => current,
                Err(error) => {
                    session::print_error(&error);
                    continue;
                }
            };
            let selection = if wanted.is_empty() {
                if workers.is_empty() {
                    eprintln!("gb-client: no reachable Worker is registered");
                    continue;
                }
                choose_worker(&workers, &worker)
            } else if workers.iter().any(|candidate| candidate.id == wanted) {
                Some(wanted.to_string())
            } else {
                eprintln!("gb-client: Worker '{wanted}' is not reachable");
                continue;
            };
            if let Some(selected) = selection {
                let next_agent = match session::first_agent(&workers, &selected, None) {
                    Ok(agent) => agent,
                    Err(error) => {
                        session::print_error(&error);
                        continue;
                    }
                };
                worker = selected;
                agent = next_agent;
                println!("worker   {worker}");
                println!("agent    {agent}");
            }
            continue;
        }
        if input == "/agents" {
            for id in session::capabilities(&workers, &worker) {
                println!("{id}{}", if id == agent { " active" } else { "" });
            }
            continue;
        }
        if let Some(rest) = input.strip_prefix("/agent") {
            let wanted = rest.trim();
            let caps = session::capabilities(&workers, &worker);
            let selection = if wanted.is_empty() {
                choose_agent(&caps, &agent)
            } else if caps.iter().any(|id| id == wanted) {
                Some(wanted.to_string())
            } else {
                eprintln!("gb-client: Worker '{worker}' has no agent '{wanted}'");
                continue;
            };
            if let Some(selected) = selection {
                agent = selected;
                println!("agent    {agent}");
            }
            continue;
        }
        if input.starts_with('/') {
            eprintln!("gb-client: unknown command '{input}'; type /help");
            continue;
        }

        key += 1;
        let request = GatewaySubmitRequest {
            worker: Some(worker.clone()),
            agent: agent.clone(),
            session_id: session_id.clone(),
            input: input.to_string(),
            idempotency_key: format!("cli-{}-{key}", std::process::id()),
            profile: None,
            team: None,
            team_version: None,
            limits: Limits::default(),
        };
        let response = match client.submit(&request).await {
            Ok(response) => response,
            Err(error) => {
                session::print_error(&error);
                continue;
            }
        };
        if let Err(error) = stream_task(&client, &response.task_id).await {
            session::print_error(&error);
        }
    }
    println!();
    ExitCode::SUCCESS
}

fn choose_worker(workers: &[WorkerSummary], current: &str) -> Option<String> {
    list_workers(workers, current);
    print!("worker> ");
    let _ = io::stdout().flush();
    let mut choice = String::new();
    if io::stdin().read_line(&mut choice).is_err() {
        return None;
    }
    let choice = choice.trim();
    if choice.is_empty() {
        return None;
    }
    if let Ok(index) = choice.parse::<usize>() {
        return index
            .checked_sub(1)
            .and_then(|index| workers.get(index))
            .map(|worker| worker.id.clone());
    }
    workers
        .iter()
        .find(|worker| worker.id == choice)
        .map(|worker| worker.id.clone())
}

fn choose_agent(capabilities: &[String], current: &str) -> Option<String> {
    for (index, id) in capabilities.iter().enumerate() {
        println!(
            "  {}. {id}{}",
            index + 1,
            if id == current { " active" } else { "" }
        );
    }
    print!("agent> ");
    let _ = io::stdout().flush();
    let mut choice = String::new();
    if io::stdin().read_line(&mut choice).is_err() {
        return None;
    }
    let choice = choice.trim();
    if choice.is_empty() {
        return None;
    }
    if let Ok(index) = choice.parse::<usize>() {
        return index
            .checked_sub(1)
            .and_then(|index| capabilities.get(index))
            .cloned();
    }
    capabilities
        .iter()
        .find(|id| id.as_str() == choice)
        .cloned()
}

fn list_workers(workers: &[WorkerSummary], current: &str) {
    for (index, worker) in workers.iter().enumerate() {
        println!(
            "  {}. {} ({}){}",
            index + 1,
            worker.id,
            worker.package,
            if worker.id == current { " active" } else { "" }
        );
    }
}

/// Follow the live task stream and print model text as it arrives.
///
/// The final task projection remains authoritative. If the live stream is
/// unavailable or drops, polling still produces the complete result. Text
/// already printed from model deltas is not printed twice.
async fn stream_task(client: &Client, task_id: &str) -> Result<(), ApiError> {
    let mut printed = String::new();
    let mut cursor = 0;
    // One reconnect uses the last observed sequence. The stream is GET-only, so
    // reconnecting can never submit a second prompt. A typed cursor rejection or
    // transport failure falls through to the authoritative poll below.
    for _ in 0..2 {
        let Ok(mut watch) = client.watch_from(task_id, cursor).await else {
            break;
        };
        let mut terminal = false;
        loop {
            match watch.next().await {
                Ok(Some(event)) => {
                    cursor = watch.last_durable_seq().max(event.seq);
                    match event.kind.as_str() {
                        "model.delta" => {
                            if let Some(text) =
                                event.payload.get("text").and_then(|text| text.as_str())
                            {
                                print!("{text}");
                                let _ = io::stdout().flush();
                                printed.push_str(text);
                            }
                        }
                        "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown" => {
                            terminal = true;
                            break;
                        }
                        _ => {}
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        cursor = watch.last_durable_seq().max(cursor);
        if terminal {
            break;
        }
    }
    let task = session::wait_terminal(client, task_id).await?;
    print_final(task, &printed);
    Ok(())
}

fn print_final(task: common::Task, printed: &str) {
    let output = task.output.unwrap_or_default();
    if printed.is_empty() {
        if !output.is_empty() {
            println!("{output}");
        }
    } else if let Some(rest) = output.strip_prefix(printed) {
        if !rest.is_empty() {
            print!("{rest}");
            let _ = io::stdout().flush();
        }
        println!();
    } else {
        println!();
        if !output.is_empty() {
            println!("{output}");
        }
    }
    if let Some(error) = task.error {
        session::print_error(&error);
    }
}
