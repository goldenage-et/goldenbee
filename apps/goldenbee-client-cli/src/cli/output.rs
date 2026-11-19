use common::ApiError;
use std::process::ExitCode;

pub(crate) fn usage(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}

pub(crate) fn api_error(error: ApiError) -> ExitCode {
    eprintln!("gb-client: {}: {}", error.code, error.message);
    if !error.recovery.is_empty() {
        eprintln!("recovery: {}", error.recovery);
    }
    for (key, value) in &error.details {
        eprintln!("{key}: {value}");
    }
    ExitCode::from(error_exit(&error.code))
}

/// Stable exit classes keep shell automation from treating an authorization or
/// transport-uncertain outcome as an ordinary validation failure.
pub(crate) fn error_exit(code: &str) -> u8 {
    if code.starts_with("scope.")
        || code.starts_with("auth.")
        || code.ends_with("_denied")
        || matches!(
            code,
            "team.member_hidden" | "profile.grant_escalation" | "approval.denied"
        )
    {
        4
    } else if matches!(
        code,
        "sdk.transport"
            | "task.unknown_submission"
            | "storage.unavailable"
            | "route.worker_unavailable"
            | "worker.endpoint_unavailable"
    ) {
        5
    } else if code.starts_with("validation.")
        || code.ends_with("_conflict")
        || code.ends_with("_mismatch")
        || code.starts_with("profile.")
        || code.starts_with("team.")
        || code == "route.invalid_worker_response"
    {
        3
    } else {
        1
    }
}

pub(crate) fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<(), ExitCode> {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            println!("{rendered}");
            Ok(())
        }
        Err(_) => Err(api_error(ApiError::new(
            "cli.render",
            "response could not be rendered as JSON",
        ))),
    }
}

pub(crate) fn default_key() -> String {
    format!(
        "gb-client-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )
}

pub(crate) fn state_name(state: common::TaskState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{state:?}"))
}
