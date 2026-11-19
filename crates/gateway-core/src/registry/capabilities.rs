//! Endpoint normalization, locality, approved capability narrowing, and summaries.

use std::collections::BTreeSet;

use common::{AdminWorkerSummary, ApiError, CapabilityDescriptor, LOCALITY_LOCAL, LOCALITY_REMOTE};

use super::model::{ApprovalRecord, Record, Verified};

pub(super) fn normalize_endpoint(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_string()
}

pub(super) fn locality(endpoint: &str) -> String {
    let loopback = endpoint.starts_with("http://127.0.0.1")
        || endpoint.starts_with("http://localhost")
        || endpoint.starts_with("http://[::1]")
        || endpoint.starts_with("https://127.0.0.1")
        || endpoint.starts_with("https://localhost")
        || endpoint.starts_with("https://[::1]");
    if loopback {
        LOCALITY_LOCAL.into()
    } else {
        LOCALITY_REMOTE.into()
    }
}

pub(super) fn approved_capabilities(
    approval: &ApprovalRecord,
    verified: &Verified,
) -> Result<(Vec<CapabilityDescriptor>, Vec<String>), ApiError> {
    if verified.package.trim().is_empty() {
        return Err(ApiError::new(
            "worker.package_mismatch",
            "the verified Worker did not report a package identity",
        ));
    }
    if verified.protocol != common::PROTOCOL_VERSION {
        return Err(ApiError::new(
            "worker.protocol_unsupported",
            format!("Worker protocol '{}' is not supported", verified.protocol),
        ));
    }
    if let Some(instance) = &approval.instance {
        if instance != &verified.instance {
            return Err(ApiError::new(
                "worker.instance_mismatch",
                "verified instance identity does not match the approved identity",
            ));
        }
    }
    if let Some(package) = &approval.package {
        if package != &verified.package {
            return Err(ApiError::new(
                "worker.package_mismatch",
                "verified package identity does not match the approved package",
            ));
        }
    }
    let Some(approved) = &approval.capabilities else {
        return Ok((
            verified.capabilities.clone(),
            verified
                .capabilities
                .iter()
                .map(|capability| capability.id.clone())
                .collect(),
        ));
    };
    let declared: BTreeSet<&str> = verified
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect();
    for capability in approved {
        if !declared.contains(capability.as_str()) {
            return Err(ApiError::new(
                "worker.capability_mismatch",
                format!("approved capability '{capability}' is not advertised by the Worker"),
            ));
        }
    }
    let effective = verified
        .capabilities
        .iter()
        .filter(|capability| approved.iter().any(|id| id == &capability.id))
        .cloned()
        .collect();
    Ok((effective, approved.clone()))
}

pub(super) fn admin_summary(
    approval: &ApprovalRecord,
    record: Option<&Record>,
    now: i64,
) -> AdminWorkerSummary {
    let status = if approval.decommissioned
        || record.is_some_and(|record| record.status == "decommissioned")
    {
        "decommissioned"
    } else if !approval.enabled || record.is_some_and(|record| record.status == "disabled") {
        "disabled"
    } else {
        match record {
            Some(record) => record.status.as_str(),
            None => "approved",
        }
    };
    let instance = approval
        .instance
        .clone()
        .or_else(|| record.map(|record| record.instance.clone()))
        .unwrap_or_default();
    let package = approval
        .package
        .clone()
        .or_else(|| record.map(|record| record.package.clone()))
        .unwrap_or_default();
    AdminWorkerSummary {
        id: approval.id.clone(),
        endpoint: approval.endpoint.clone(),
        instance,
        package,
        capabilities: approval.capabilities.clone(),
        approved_capabilities: record
            .map(|record| record.approved_capabilities.clone())
            .or_else(|| approval.capabilities.clone())
            .unwrap_or_default(),
        declared_capabilities: record
            .map(|record| record.declared_capabilities.clone())
            .unwrap_or_default(),
        enabled: approval.enabled && !approval.decommissioned,
        revision: approval.revision,
        source: approval.source.label().to_string(),
        status: status.to_string(),
        reachable: record.is_some_and(|record| record.reachable(now)),
        control_token_set: !approval.control_token_hash.is_empty(),
        incarnation: record
            .map(|record| record.incarnation.clone())
            .unwrap_or_default(),
        incarnations: record
            .map(|record| record.incarnations.clone())
            .unwrap_or_default(),
        locality: record
            .map(|record| record.locality.clone())
            .unwrap_or_else(|| locality(&approval.endpoint)),
        lease_expires_at: record.map_or(0, |record| record.lease_expires_at),
        renewed_at: record.map_or(0, |record| record.renewed_at),
        created_at: approval.created_at,
        updated_at: approval.updated_at,
    }
}
