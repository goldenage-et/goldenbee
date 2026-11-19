//! Per-session WebDriver facade.
//!
//! The pinned `adk-browser` crate exposes Chrome preferences but not Firefox
//! preferences. The harness needs Firefox to send traffic through the session
//! egress proxy and to save downloads into the session directory. This loopback
//! facade forwards ordinary WebDriver commands unchanged and adds only the
//! session-scoped capabilities at `POST /session`. It is an adapter, not a
//! second WebDriver implementation.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, Response, StatusCode};
use axum::Router;
use common::ApiError;
use serde_json::{json, Map, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Session capabilities captured from the driver's New Session response.
#[derive(Debug, Default, Clone)]
pub struct DriverSession {
    /// Browser profile directory reported by the driver.
    pub profile: Option<String>,
    /// Browser process id reported by the driver.
    pub process_id: Option<u32>,
}

#[derive(Clone)]
struct FacadeState {
    upstream: String,
    capabilities: Value,
    client: reqwest::Client,
    session: Arc<Mutex<DriverSession>>,
}

/// Loopback WebDriver facade for one browser session.
pub struct WebDriverFacade {
    address: SocketAddr,
    session: Arc<Mutex<DriverSession>>,
    task: tokio::task::JoinHandle<()>,
}

impl WebDriverFacade {
    /// Start a facade in front of `upstream`, injecting `capabilities` into the
    /// W3C New Session request.
    pub async fn start(upstream: String, capabilities: Value) -> Result<Self, ApiError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| {
                ApiError::new(
                    "browser.webdriver_failed",
                    format!("browser WebDriver facade could not bind: {error}"),
                )
            })?;
        let address = listener.local_addr().map_err(|error| {
            ApiError::new(
                "browser.webdriver_failed",
                format!("browser WebDriver facade has no address: {error}"),
            )
        })?;
        let session = Arc::new(Mutex::new(DriverSession::default()));
        let state = FacadeState {
            upstream,
            capabilities,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|error| {
                    ApiError::new(
                        "browser.webdriver_failed",
                        format!("browser WebDriver client could not start: {error}"),
                    )
                })?,
            session: session.clone(),
        };
        let app = Router::new().fallback(forward).with_state(state);
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::warn!(%error, "browser WebDriver facade stopped");
            }
        });
        Ok(Self {
            address,
            session,
            task,
        })
    }

    /// URL the native browser session should use as its WebDriver endpoint.
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Driver-reported session facts for diagnostics and cleanup checks.
    pub async fn driver_session(&self) -> DriverSession {
        self.session.lock().await.clone()
    }

    /// Stop the facade listener.
    pub fn shutdown(&self) {
        self.task.abort();
    }
}

impl Drop for WebDriverFacade {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn forward(State(state): State<FacadeState>, request: Request<Body>) -> Response<Body> {
    let method = request.method().clone();
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let mut headers = request.headers().clone();
    let body = match axum::body::to_bytes(request.into_body(), 4 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "WebDriver request body failed"),
    };
    let body = if method == Method::POST && path == "/session" {
        match inject_capabilities(&body, &state.capabilities) {
            Ok(body) => body,
            Err(error) => return error_response(StatusCode::BAD_REQUEST, &error.message),
        }
    } else {
        body.to_vec()
    };

    let url = format!("{}{}", state.upstream.trim_end_matches('/'), path);
    let upstream_method = match reqwest::Method::from_bytes(method.as_str().as_bytes()) {
        Ok(method) => method,
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "WebDriver method is invalid"),
    };
    let mut builder = state.client.request(upstream_method, url);
    headers.remove(axum::http::header::HOST);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.remove(axum::http::header::CONNECTION);
    headers.remove(axum::http::header::TRANSFER_ENCODING);
    builder = builder.headers(header_map(&headers));
    if !body.is_empty() {
        builder = builder.body(body.clone());
    }
    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("WebDriver is unavailable: {error}"),
            )
        }
    };

    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("WebDriver response failed: {error}"),
            )
        }
    };
    if method == Method::POST && path == "/session" && status.is_success() {
        capture_session(&bytes, &state.session).await;
    }
    let mut builder = Response::builder().status(status);
    for (name, value) in &response_headers {
        if matches!(
            name.as_str(),
            "connection" | "transfer-encoding" | "content-length"
        ) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| error_response(StatusCode::BAD_GATEWAY, "WebDriver response invalid"))
}

fn header_map(headers: &axum::http::HeaderMap) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            map.append(name, value);
        }
    }
    map
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    let body = json!({ "value": { "error": "session not created", "message": message } });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn inject_capabilities(body: &[u8], injected: &Value) -> Result<Vec<u8>, ApiError> {
    let mut request: Value = serde_json::from_slice(body).map_err(|error| {
        ApiError::new(
            "browser.webdriver_failed",
            format!("WebDriver New Session body is invalid: {error}"),
        )
    })?;
    merge_capabilities(&mut request, "capabilities", injected);
    merge_capabilities(&mut request, "desiredCapabilities", injected);
    serde_json::to_vec(&request).map_err(|error| {
        ApiError::new(
            "browser.webdriver_failed",
            format!("WebDriver New Session body could not be encoded: {error}"),
        )
    })
}

fn merge_capabilities(request: &mut Value, key: &str, injected: &Value) {
    let Some(capabilities) = request.get_mut(key) else {
        return;
    };
    if let Some(always) = capabilities.get_mut("alwaysMatch") {
        // W3C forbids repeating a key in firstMatch that already appears in
        // alwaysMatch, so inject into exactly one of them.
        merge_object(always, injected);
    } else if let Some(first) = capabilities
        .get_mut("firstMatch")
        .and_then(Value::as_array_mut)
    {
        if let Some(entry) = first.first_mut() {
            merge_object(entry, injected);
        }
    }
    if key == "desiredCapabilities" {
        merge_object(capabilities, injected);
    }
}

fn merge_object(target: &mut Value, injected: &Value) {
    let (Some(target), Some(injected)) = (target.as_object_mut(), injected.as_object()) else {
        return;
    };
    for (key, value) in injected {
        match (target.get_mut(key), value) {
            (Some(Value::Object(existing)), Value::Object(extra)) => {
                merge_object_map(existing, extra);
            }
            (Some(Value::Array(existing)), Value::Array(extra)) => {
                existing.extend(extra.iter().cloned());
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn merge_object_map(target: &mut Map<String, Value>, injected: &Map<String, Value>) {
    for (key, value) in injected {
        match (target.get_mut(key), value) {
            (Some(Value::Object(existing)), Value::Object(extra)) => {
                merge_object_map(existing, extra);
            }
            (Some(Value::Array(existing)), Value::Array(extra)) => {
                existing.extend(extra.iter().cloned());
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

async fn capture_session(body: &[u8], session: &Arc<Mutex<DriverSession>>) {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return;
    };
    let capabilities = &value["value"]["capabilities"];
    let mut captured = session.lock().await;
    captured.profile = capabilities["moz:profile"]
        .as_str()
        .map(ToString::to_string)
        .or_else(|| capabilities["se:profile"].as_str().map(ToString::to_string));
    captured.process_id = capabilities["moz:processID"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_merged_into_w3c_and_legacy_forms() {
        let mut request = json!({
            "capabilities": {
                "alwaysMatch": { "browserName": "firefox", "moz:firefoxOptions": { "args": ["-headless"] } },
                "firstMatch": [{}]
            },
            "desiredCapabilities": { "browserName": "firefox" }
        });
        let injected = json!({
            "moz:firefoxOptions": { "prefs": { "network.proxy.type": 1 } }
        });
        merge_capabilities(&mut request, "capabilities", &injected);
        merge_capabilities(&mut request, "desiredCapabilities", &injected);
        assert_eq!(
            request["capabilities"]["alwaysMatch"]["moz:firefoxOptions"]["args"][0],
            "-headless"
        );
        assert_eq!(
            request["capabilities"]["alwaysMatch"]["moz:firefoxOptions"]["prefs"]
                ["network.proxy.type"],
            1
        );
        assert!(request["capabilities"]["firstMatch"][0]
            .get("moz:firefoxOptions")
            .is_none());
        assert_eq!(
            request["desiredCapabilities"]["moz:firefoxOptions"]["prefs"]["network.proxy.type"],
            1
        );
    }

    #[test]
    fn injected_browser_arguments_are_appended() {
        let mut request = json!({
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "chrome",
                    "goog:chromeOptions": { "args": ["--headless"] }
                }
            }
        });
        let injected = json!({
            "goog:chromeOptions": {
                "args": ["--proxy-server=http://127.0.0.1:1"]
            }
        });
        merge_capabilities(&mut request, "capabilities", &injected);
        let args = request["capabilities"]["alwaysMatch"]["goog:chromeOptions"]["args"]
            .as_array()
            .unwrap();
        assert!(args.iter().any(|arg| arg == "--headless"));
        assert!(args
            .iter()
            .any(|arg| arg == "--proxy-server=http://127.0.0.1:1"));
    }
}
