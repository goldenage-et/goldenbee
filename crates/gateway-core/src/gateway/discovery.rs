//! Model discovery for Gateway-managed providers.
//!
//! Discovery is server-side only. The provider credential is attached to the
//! outbound request and never copied into an error, log, audit record, or
//! response. A failed discovery produces a stable code and an actionable,
//! redacted message; it never invents a model.

use common::{ApiError, MAX_DISCOVERED_MODELS};
use std::collections::BTreeSet;

/// Discover model identifiers for one provider generation.
pub(crate) async fn discover(
    adapter: &str,
    endpoint: Option<&str>,
    credential: Option<&str>,
    allowed_models: Option<&[String]>,
) -> Result<Vec<String>, ApiError> {
    let url = discovery_url(adapter, endpoint)?;
    if matches!(adapter, "openrouter" | "openai-compatible") && credential.is_none() {
        return Err(ApiError::new(
            "provider.discovery_credential_missing",
            "write a provider credential to run model discovery",
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        // A redirect must not move an authenticated discovery request to another
        // host or downgrade the transport.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| {
            ApiError::new(
                "provider.discovery_unreachable",
                "provider discovery HTTP client could not be built",
            )
        })?;
    let mut request = client.get(&url);
    if matches!(adapter, "openrouter" | "openai-compatible") {
        if let Some(credential) = credential {
            request = request.bearer_auth(credential);
        }
    }
    let response = request.send().await.map_err(|_| {
        ApiError::new(
            "provider.discovery_unreachable",
            "provider discovery endpoint could not be reached",
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        let code = if matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            "provider.discovery_auth"
        } else {
            "provider.discovery_http"
        };
        return Err(ApiError::new(
            code,
            format!("provider discovery endpoint rejected the request with HTTP {status}"),
        ));
    }
    let bytes = response.bytes().await.map_err(|_| {
        ApiError::new(
            "provider.discovery_invalid",
            "provider discovery response could not be read",
        )
    })?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            "provider.discovery_invalid",
            "provider discovery response is not valid JSON",
        )
    })?;
    let models = parse_models(adapter, &value)?;
    let filtered = filter_models(models, allowed_models)?;
    if filtered.is_empty() {
        return Err(ApiError::new(
            "provider.discovery_empty",
            "provider discovery returned no usable models",
        ));
    }
    Ok(filtered)
}

fn discovery_url(adapter: &str, endpoint: Option<&str>) -> Result<String, ApiError> {
    let (raw, path) = match adapter {
        "openrouter" => (endpoint.unwrap_or("https://openrouter.ai/api/v1"), "models"),
        "ollama" => (endpoint.unwrap_or("http://127.0.0.1:11434"), "api/tags"),
        "openai-compatible" => (
            endpoint.ok_or_else(|| {
                ApiError::new(
                    "provider.discovery_endpoint_missing",
                    "openai-compatible discovery requires an endpoint",
                )
            })?,
            "models",
        ),
        other => {
            return Err(ApiError::new(
                "provider.discovery_unsupported",
                format!("adapter '{other}' does not support model discovery"),
            ))
        }
    };
    let url = reqwest::Url::parse(raw).map_err(|_| {
        ApiError::new(
            "provider.discovery_endpoint_invalid",
            "provider endpoint is not a valid discovery URL",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::new(
            "provider.discovery_endpoint_invalid",
            "provider discovery requires an HTTP or HTTPS endpoint",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::new(
            "provider.discovery_endpoint_invalid",
            "provider discovery endpoint must not embed credentials",
        ));
    }
    Ok(format!(
        "{}/{}",
        raw.trim_end_matches('/'),
        path.trim_start_matches('/')
    ))
}

fn parse_models(adapter: &str, value: &serde_json::Value) -> Result<Vec<String>, ApiError> {
    let items = if adapter == "ollama" {
        value.get("models")
    } else {
        value.get("data").or_else(|| value.get("models"))
    };
    let Some(items) = items.and_then(serde_json::Value::as_array) else {
        return Err(ApiError::new(
            "provider.discovery_invalid",
            "provider discovery response does not contain a model list",
        ));
    };
    let mut models = Vec::new();
    for item in items {
        let model = if let Some(model) = item.as_str() {
            Some(model)
        } else if adapter == "ollama" {
            item.get("name")
                .or_else(|| item.get("model"))
                .and_then(serde_json::Value::as_str)
        } else {
            item.get("id")
                .or_else(|| item.get("name"))
                .or_else(|| item.get("model"))
                .and_then(serde_json::Value::as_str)
        };
        if let Some(model) = model {
            models.push(model.to_string());
        }
    }
    Ok(models)
}

fn filter_models(
    models: Vec<String>,
    allowed_models: Option<&[String]>,
) -> Result<Vec<String>, ApiError> {
    let mut unique = BTreeSet::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            continue;
        }
        if let Some(allowed) = allowed_models {
            if !allowed.iter().any(|value| value == model) {
                continue;
            }
        }
        unique.insert(model.to_string());
        if unique.len() > MAX_DISCOVERED_MODELS {
            return Err(ApiError::new(
                "provider.discovery_too_many",
                "provider discovery returned more models than this Gateway accepts; set allowed_models",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_shape_is_deduplicated_and_filtered() {
        let value = serde_json::json!({
            "data": [
                {"id": "beta"},
                {"id": "alpha"},
                {"id": "beta"},
                {"id": ""},
                {"not": "a model"}
            ]
        });
        let models = parse_models("openai-compatible", &value).expect("parse");
        let filtered = filter_models(models, Some(&["alpha".into()])).expect("filter");
        assert_eq!(filtered, vec!["alpha".to_string()]);
    }

    #[test]
    fn ollama_shape_prefers_name() {
        let value = serde_json::json!({
            "models": [
                {"name": "llama3:latest", "model": "llama3:latest"},
                {"name": "qwen2.5:7b"}
            ]
        });
        let models = parse_models("ollama", &value).expect("parse");
        assert_eq!(
            filter_models(models, None).expect("filter"),
            vec!["llama3:latest".to_string(), "qwen2.5:7b".to_string()]
        );
    }

    #[test]
    fn malformed_and_unsupported_shapes_fail_stably() {
        let error = parse_models(
            "openai-compatible",
            &serde_json::json!({"models": {"not": "an array"}}),
        )
        .expect_err("malformed");
        assert_eq!(error.code, "provider.discovery_invalid");
        let error = discovery_url("http-json", None).expect_err("unsupported");
        assert_eq!(error.code, "provider.discovery_unsupported");
    }

    #[test]
    fn discovery_url_uses_adapter_defaults_and_rejects_credentials() {
        assert_eq!(
            discovery_url("openrouter", None).expect("default"),
            "https://openrouter.ai/api/v1/models"
        );
        assert_eq!(
            discovery_url("ollama", None).expect("default"),
            "http://127.0.0.1:11434/api/tags"
        );
        assert!(discovery_url("openai-compatible", None).is_err());
        assert!(
            discovery_url("openai-compatible", Some("http://user:pass@127.0.0.1:1/v1")).is_err()
        );
    }
}
