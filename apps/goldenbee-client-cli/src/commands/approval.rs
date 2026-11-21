//! Task approval inspection and decisions.

use crate::cli::output::{api_error, json, usage};
use common::*;
use sdk::Client;
use std::process::ExitCode;

/// Approval commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCommand {
    /// List approvals for one task.
    List {
        /// Task id.
        task: String,
        /// Only pending approvals.
        pending: bool,
    },
    /// Approve or reject one approval.
    Decide {
        /// Task id.
        task: String,
        /// Approval id.
        approval: String,
        /// `approved` or `rejected`.
        decision: String,
        /// Optional observed operation digest.
        digest: Option<String>,
    },
    /// Revoke one approval.
    Revoke {
        /// Task id.
        task: String,
        /// Approval id.
        approval: String,
    },
}

pub(crate) async fn approval_command(
    command: ApprovalCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        ApprovalCommand::List { task, pending } => match client.approvals(&task, pending).await {
            Ok(approvals) => {
                if json_output {
                    return match json(&approvals) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                approval_table(&approvals);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        ApprovalCommand::Revoke { task, approval } => {
            match client.revoke_approval(&task, &approval).await {
                Ok(approval) => {
                    if json_output {
                        return match json(&approval) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    approval_table(std::slice::from_ref(&approval));
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        ApprovalCommand::Decide {
            task,
            approval,
            decision,
            digest,
        } => {
            // The observed digest is the Worker record unless the approver
            // supplied an explicit one. The Gateway rejects any changed binding.
            let digest = match digest {
                Some(digest) if digest.trim().is_empty() => {
                    return usage("gb-client: --digest requires the exact operation digest");
                }
                Some(digest) => digest,
                None => match client.approval(&task, &approval).await {
                    Ok(recorded) => recorded.approval.digest,
                    Err(error) => return api_error(error),
                },
            };
            let decision = ApprovalDecision {
                decision,
                digest,
                answer: None,
            };
            match client.decide_approval(&task, &approval, &decision).await {
                Ok(approval) => {
                    if json_output {
                        return match json(&approval) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    approval_table(std::slice::from_ref(&approval));
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
    }
}

pub(crate) fn approval_table(approvals: &[GatewayApproval]) {
    if approvals.is_empty() {
        println!("no approvals");
        return;
    }
    for approval in approvals {
        println!(
            "{} {} {} {} {} {} {} {}",
            approval.task_id,
            approval.approval.id,
            approval.worker,
            approval.approval.status,
            approval.approval.operation,
            approval.approval.target,
            approval.approval.digest,
            approval.approval.expires_at
        );
    }
}
