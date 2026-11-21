//! One naming and grant grammar for every MCP surface.
//!
//! Tools, resources, and prompts share this module so a namespaced identifier
//! and its authorization grant cannot drift between discovery, dispatch, and
//! policy checks. Tools keep the native ADK-compatible `mcp__<server>__<tool>`
//! shape; resources and prompts use `mcp:<server>:resource:<uri>` and
//! `mcp:<server>:prompt:<name>`.

use std::collections::BTreeSet;

/// Prefix of an exposed MCP tool name.
pub const TOOL_PREFIX: &str = "mcp__";
/// Separator between the server and the tool in an exposed tool name.
pub const TOOL_SEPARATOR: &str = "__";

/// Exposed, ADK-compatible tool name.
pub fn tool_id(server: &str, tool: &str) -> String {
    format!(
        "{TOOL_PREFIX}{}{TOOL_SEPARATOR}{}",
        sanitize(server),
        sanitize(tool)
    )
}

/// Configured server id parsed from an exposed tool name.
pub fn server_of_tool(name: &str) -> Option<&str> {
    name.strip_prefix(TOOL_PREFIX)?
        .split_once(TOOL_SEPARATOR)
        .map(|(server, _)| server)
}

/// Stable namespaced resource id.
pub fn resource_id(server: &str, uri: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        crate::mcp::IDENTIFIER_ROOT,
        server,
        common::mcp::RESOURCE_ID_KIND,
        uri
    )
}

/// Stable namespaced prompt id.
pub fn prompt_id(server: &str, name: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        crate::mcp::IDENTIFIER_ROOT,
        server,
        common::mcp::PROMPT_ID_KIND,
        name
    )
}

/// Grant that authorizes one protected tool.
pub fn tool_grant(server: &str, tool: &str) -> String {
    format!("mcp:{server}:{tool}")
}

/// Grant that authorizes one protected resource.
pub fn resource_grant(server: &str, uri: &str) -> String {
    format!("mcp:{server}:resource:{uri}")
}

/// Grant that authorizes one protected prompt.
pub fn prompt_grant(server: &str, name: &str) -> String {
    format!("mcp:{server}:prompt:{name}")
}

/// Grant that authorizes a whole connection for a profile revision.
pub fn connection_grant(server: &str) -> String {
    format!("mcp:{server}")
}

/// Whether one grant set contains a grant or the instance-wide wildcard.
pub fn granted(grants: &BTreeSet<String>, grant: &str) -> bool {
    grants.contains("*") || grants.contains(grant)
}

/// Replace every character that cannot appear in an exposed tool name.
pub fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_ids_round_trip_through_the_configured_server() {
        let id = tool_id("local", "record_effect");
        assert_eq!(id, "mcp__local__record_effect");
        assert_eq!(server_of_tool(&id), Some("local"));
        assert_eq!(server_of_tool("exec_command"), None);
    }

    #[test]
    fn resources_and_prompts_share_the_namespace_root() {
        assert_eq!(
            resource_id("tools", "file:///notes/a.txt"),
            "mcp:tools:resource:file:///notes/a.txt"
        );
        assert_eq!(
            prompt_id("tools", "summarize"),
            "mcp:tools:prompt:summarize"
        );
        assert_eq!(
            resource_grant("tools", "file:///notes/a.txt"),
            "mcp:tools:resource:file:///notes/a.txt"
        );
        assert_eq!(
            prompt_grant("tools", "summarize"),
            "mcp:tools:prompt:summarize"
        );
    }

    #[test]
    fn only_the_exact_grant_or_wildcard_authorizes() {
        let grants = BTreeSet::from(["mcp:tools:resource:file:///a".to_string()]);
        assert!(granted(&grants, &resource_grant("tools", "file:///a")));
        assert!(!granted(&grants, &resource_grant("tools", "file:///b")));
        assert!(granted(&BTreeSet::from(["*".to_string()]), "anything"));
    }
}
