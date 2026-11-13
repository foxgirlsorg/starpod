//! The REST client for the engine's controller API.
//!
//! The controller is the engine's own HTTP API, on loopback, protected by a
//! bearer secret. The client here is the only place in the app that knows
//! the route names.
//!
//! # Secrets
//!
//! The secret is sent in the `Authorization: Bearer` header, never in a
//! URL and never in a query string — a query string ends up in access logs,
//! in `ps` output and in proxy logs. The engine also accepts `?token=` for
//! WebSocket upgrades, but the app does not use that path.
//!
//! No request or response body is logged. The engine's responses carry node
//! names and traffic figures, which are the user's data, and the request
//! body of a `PUT /configs` carries the whole configuration — including
//! subscription URLs, which carry tokens.
//!
//! # Address
//!
//! The controller binds to `127.0.0.1` only. It is never bound to a LAN
//! address: an open controller API is a remote-configuration takeover of
//! the user's machine, and GOAL.md §6 rules that out ("Avoiding accidental
//! exposure of local services or management interfaces").

use std::time::Duration;

use serde_json::Value;

use crate::error::{EngineError, EngineResult};

/// How long a request may take before it is abandoned.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A client for the engine's controller API.
#[derive(Clone)]
pub struct ControllerClient {
    http: reqwest::Client,
    base: String,
    secret: String,
}

impl ControllerClient {
    /// Build a client for a controller at `base` (e.g. `http://127.0.0.1:9090`).
    pub fn new(base: impl Into<String>, secret: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            // The controller is on loopback; a proxy in the environment
            // would break the connection and is a surprise the user does
            // not need.
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            secret: secret.into(),
        }
    }

    /// The controller's base address.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Whether a secret is set.
    ///
    /// The engine runs without one when the config omits `secret:`, in which
    /// case the controller accepts unauthenticated requests. The app always
    /// sets one.
    pub fn has_secret(&self) -> bool {
        !self.secret.is_empty()
    }

    /// A `GET` request.
    pub async fn get(&self, path: &str) -> EngineResult<Value> {
        self.send(reqwest::Method::GET, path, None).await
    }

    /// A `PATCH` request with a JSON body.
    pub async fn patch(&self, path: &str, body: &Value) -> EngineResult<Value> {
        self.send(reqwest::Method::PATCH, path, Some(body)).await
    }

    /// A `PUT` request with a JSON body.
    pub async fn put(&self, path: &str, body: &Value) -> EngineResult<Value> {
        self.send(reqwest::Method::PUT, path, Some(body)).await
    }

    /// A `POST` request with a JSON body.
    pub async fn post(&self, path: &str, body: &Value) -> EngineResult<Value> {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    /// A `DELETE` request.
    pub async fn delete(&self, path: &str) -> EngineResult<Value> {
        self.send(reqwest::Method::DELETE, path, None).await
    }

    /// Whether the controller is answering.
    ///
    /// This is a liveness probe, not a health check: it asks the engine's
    /// root route, which answers `{"hello":"meow"}` and needs no secret
    /// beyond the one the client carries.
    pub async fn is_ready(&self) -> bool {
        self.get("/").await.is_ok()
    }

    /// The engine's version.
    pub async fn version(&self) -> EngineResult<String> {
        let value = self.get("/version").await?;
        Ok(value
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// The engine's current configuration state.
    pub async fn configs(&self) -> EngineResult<Value> {
        self.get("/configs").await
    }

    /// Switch a `select` group's member.
    pub async fn select(&self, group: &str, member: &str) -> EngineResult<()> {
        let path = format!("/proxies/{}", encode_path_segment(group));
        let body = serde_json::json!({ "name": member });
        self.put(&path, &body).await?;
        Ok(())
    }

    /// Reload the configuration from a file the engine can read.
    ///
    /// The engine closes its open connections as part of a reload, so the
    /// app warns the user before calling this.
    pub async fn reload_from_file(&self, path: &std::path::Path) -> EngineResult<()> {
        let body = serde_json::json!({ "path": path.to_string_lossy() });
        self.put("/configs", &body).await?;
        Ok(())
    }

    /// Reload the configuration from a payload the engine receives directly.
    pub async fn reload_from_payload(&self, yaml: &str) -> EngineResult<()> {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::STANDARD.encode(yaml.as_bytes());
        let body = serde_json::json!({ "payload": payload });
        self.put("/configs", &body).await?;
        Ok(())
    }

    /// Change the routing mode, the log level, and the TUN settings in one
    /// request.
    ///
    /// The engine's `PATCH /configs` applies the TUN change first and rolls
    /// it back if it fails, so the mode and the log level are left alone
    /// when the TUN change cannot be made.
    pub async fn patch_runtime(
        &self,
        mode: Option<&str>,
        log_level: Option<&str>,
        tun: Option<Value>,
    ) -> EngineResult<()> {
        let mut body = serde_json::Map::new();
        if let Some(mode) = mode {
            body.insert("mode".to_string(), Value::String(mode.to_string()));
        }
        if let Some(log_level) = log_level {
            body.insert(
                "log-level".to_string(),
                Value::String(log_level.to_string()),
            );
        }
        if let Some(tun) = tun {
            body.insert("tun".to_string(), tun);
        }
        self.patch("/configs", &Value::Object(body)).await?;
        Ok(())
    }

    /// Send one request and interpret the answer.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> EngineResult<Value> {
        let url = format!("{}{}", self.base, path);
        let mut request = self.http.request(method, &url);
        if !self.secret.is_empty() {
            request = request.bearer_auth(&self.secret);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                EngineError::Timeout
            } else if e.is_connect() {
                EngineError::EngineDown
            } else {
                EngineError::Transport(e.to_string())
            }
        })?;

        let status = response.status();
        if status.is_success() {
            // A 204 carries no body; `json()` on an empty body would fail,
            // and an empty answer is a success.
            let text = response
                .text()
                .await
                .map_err(|e| EngineError::Transport(e.to_string()))?;
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(|e| EngineError::Transport(e.to_string()));
        }

        // The engine's error bodies are `{"message": "..."}`. The message is
        // the engine's own explanation and is safe to show: the engine does
        // not put the API secret or a subscription token in it.
        let message = response
            .text()
            .await
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();

        Err(match status.as_u16() {
            401 => EngineError::Unauthorized,
            404 => EngineError::NotFound,
            400 => EngineError::BadRequest(message),
            _ => EngineError::Transport(format!("the engine answered {status}")),
        })
    }
}

/// Percent-encode a path segment.
///
/// A proxy group's name is arbitrary text — it can contain spaces, slashes
/// and non-ASCII characters — and it goes into a URL path.
pub fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_segment_is_encoded() {
        assert_eq!(encode_path_segment("HK 01"), "HK%2001");
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(encode_path_segment("plain-Name_1.0~"), "plain-Name_1.0~");
    }

    #[test]
    fn a_non_ascii_name_is_encoded_byte_by_byte() {
        assert_eq!(encode_path_segment("香港"), "%E9%A6%99%E6%B8%AF");
    }

    #[test]
    fn the_base_address_loses_its_trailing_slash() {
        let client = ControllerClient::new("http://127.0.0.1:9090/", "secret");
        assert_eq!(client.base(), "http://127.0.0.1:9090");
    }

    #[test]
    fn a_client_without_a_secret_says_so() {
        assert!(!ControllerClient::new("http://127.0.0.1:9090", "").has_secret());
        assert!(ControllerClient::new("http://127.0.0.1:9090", "s").has_secret());
    }
}
