//! Secret-safe diagnostics.

/// Redact secret canaries from diagnostics.
pub fn redact(text: &str) -> String {
    const MARKERS: &[&str] = &["sk-", "Bearer ", "token=", "password=", "api_key="];
    let mut result = text.to_string();
    for marker in MARKERS {
        while let Some(start) = result.find(marker) {
            let end = result[start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .map(|offset| start + offset)
                .unwrap_or(result.len());
            result.replace_range(start..end, "[REDACTED]");
        }
    }
    result
}
