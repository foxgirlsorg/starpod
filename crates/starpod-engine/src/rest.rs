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

use crate::connections::{ConnectionsReply, parse_connections};
use crate::error::{EngineError, EngineResult};
use crate::reassert::{EngineSubscription, LiveProxy};

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

    /// Measure a proxy's latency against `url`, in milliseconds.
    ///
    /// The route is `GET /proxies/{name}/delay` (ARCHITECTURE §1.3). Its
    /// query parameters `url` and `timeout` follow the mihomo API the engine
    /// mirrors; the contract document does not spell them out, so the
    /// integration test below pins what the app sends.
    pub async fn delay(&self, proxy: &str, url: &str, timeout_ms: u32) -> EngineResult<u32> {
        let path = format!(
            "/proxies/{}/delay?url={}&timeout={timeout_ms}",
            encode_path_segment(proxy),
            encode_path_segment(url),
        );
        let value = self.get(&path).await?;
        value
            .get("delay")
            .and_then(Value::as_u64)
            .and_then(|delay| u32::try_from(delay).ok())
            .ok_or_else(|| EngineError::Internal("the engine returned no delay".to_string()))
    }

    /// Measure every member of a proxy group at once.
    ///
    /// `GET /group/{name}/delay?url=&timeout=`. The engine answers with an
    /// object that maps each member's name to its delay in milliseconds. A
    /// member that did not answer is absent from the object, or its value is
    /// `null`; such a member is left out of the result rather than given zero.
    pub async fn group_delay(
        &self,
        group: &str,
        url: &str,
        timeout_ms: u32,
    ) -> EngineResult<std::collections::BTreeMap<String, u32>> {
        let path = format!(
            "/group/{}/delay?url={}&timeout={timeout_ms}",
            encode_path_segment(group),
            encode_path_segment(url),
        );
        let value = self.get(&path).await?;
        let object = value.as_object().ok_or_else(|| {
            EngineError::Internal("the engine returned no group delays".to_string())
        })?;
        Ok(object
            .iter()
            .filter_map(|(name, delay)| {
                let delay = u32::try_from(delay.as_u64()?).ok()?;
                Some((name.clone(), delay))
            })
            .collect())
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

    /// The engine-managed subscriptions, as the engine lists them.
    ///
    /// `GET /api/subscriptions`. The fields the app relies on (`name`,
    /// `last_updated`) are documented in ARCHITECTURE §3.2.1; the envelope
    /// is parsed by [`parse_subscriptions`].
    pub async fn engine_subscriptions(&self) -> EngineResult<Vec<EngineSubscription>> {
        parse_subscriptions(&self.get("/api/subscriptions").await?)
    }

    /// The engine's rule list, in evaluation order. `GET /rules`.
    pub async fn rules(&self) -> EngineResult<Vec<String>> {
        parse_rules(&self.get("/rules").await?)
    }

    /// Replace the engine's rule list. `POST /rules` with
    /// `{"rules": [...]}`, the body ARCHITECTURE §4.6 gives. The engine's
    /// answer is not documented, so any success is accepted; the caller
    /// reads the list back to verify the order took.
    pub async fn replace_rules(&self, rules: &[String]) -> EngineResult<()> {
        self.post("/rules", &serde_json::json!({ "rules": rules }))
            .await?;
        Ok(())
    }

    /// The proxies in the engine's live configuration. `GET /proxies`.
    pub async fn proxies(&self) -> EngineResult<Vec<LiveProxy>> {
        parse_proxies(&self.get("/proxies").await?)
    }

    /// The engine's open connections, with the traffic totals. `GET /connections`.
    pub async fn connections(&self) -> EngineResult<ConnectionsReply> {
        parse_connections(&self.get("/connections").await?)
    }

    /// Close one connection. `DELETE /connections/{id}`.
    pub async fn close_connection(&self, id: &str) -> EngineResult<()> {
        let path = format!("/connections/{}", encode_path_segment(id));
        self.delete(&path).await?;
        Ok(())
    }

    /// Close every connection. `DELETE /connections`.
    pub async fn close_all_connections(&self) -> EngineResult<()> {
        self.delete("/connections").await?;
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

/// Read the answer of `GET /api/subscriptions`.
///
/// ASSUMED envelope: a bare JSON array, or an object with a `subscriptions`
/// array. Each entry needs a `name`; `last_updated` is kept as the engine
/// sent it, because only its change matters to the app.
pub fn parse_subscriptions(value: &Value) -> EngineResult<Vec<EngineSubscription>> {
    array_of(value, "subscriptions", "subscription list")?
        .iter()
        .map(|item| {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("subscription list"))?;
            Ok(EngineSubscription {
                name: name.to_string(),
                last_updated: item.get("last_updated").filter(|v| !v.is_null()).cloned(),
            })
        })
        .collect()
}

/// Read the answer of `GET /rules`.
///
/// ASSUMED envelope: a bare array, or an object with a `rules` array, of
/// rule strings in the engine's own syntax. An object entry is refused
/// rather than rebuilt from its fields: re-posting a rule the app cannot
/// reproduce byte for byte would change the user's configuration.
pub fn parse_rules(value: &Value) -> EngineResult<Vec<String>> {
    array_of(value, "rules", "rule list")?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| malformed("rule list (an entry is not a rule string)"))
        })
        .collect()
}

/// Read the answer of `GET /proxies`.
///
/// ASSUMED envelope: an object whose `proxies` is either a map keyed by
/// name (the mihomo style the engine mirrors) or an array, or a bare array
/// or map of the same. `server` and `port` are optional; the engine may not
/// report them, and a caller must treat their absence as "unknown".
pub fn parse_proxies(value: &Value) -> EngineResult<Vec<LiveProxy>> {
    let container = value.get("proxies").unwrap_or(value);
    match container {
        Value::Object(map) => map
            .iter()
            .map(|(key, entry)| live_proxy(entry, Some(key)))
            .collect(),
        Value::Array(items) => items.iter().map(|entry| live_proxy(entry, None)).collect(),
        _ => Err(malformed("proxy list")),
    }
}

fn live_proxy(entry: &Value, key: Option<&str>) -> EngineResult<LiveProxy> {
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .or(key)
        .ok_or_else(|| malformed("proxy list (an entry has no name)"))?;
    Ok(LiveProxy {
        name: name.to_string(),
        server: entry
            .get("server")
            .and_then(Value::as_str)
            .map(str::to_string),
        port: entry
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok()),
        now: entry
            .get("now")
            .and_then(Value::as_str)
            .filter(|now| !now.is_empty())
            .map(str::to_string),
    })
}

/// The array inside an answer, which is either the answer itself or the
/// value of `key`.
fn array_of<'a>(value: &'a Value, key: &str, what: &str) -> EngineResult<&'a Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get(key).and_then(Value::as_array))
        .ok_or_else(|| malformed(what))
}

fn malformed(what: &str) -> EngineError {
    EngineError::Internal(format!(
        "the engine's answer for the {what} has an unexpected shape"
    ))
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

    #[tokio::test]
    async fn a_selection_is_a_put_that_carries_the_secret_in_the_header() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/proxies/Auto"))
            .and(header("authorization", "Bearer s3cret"))
            .and(body_json(serde_json::json!({ "name": "Node B" })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = ControllerClient::new(server.uri(), "s3cret");
        client
            .select("Auto", "Node B")
            .await
            .expect("the selection is accepted");
    }

    #[tokio::test]
    async fn a_delay_probe_sends_the_url_and_timeout_and_reads_the_delay() {
        use wiremock::matchers::{method, path_regex, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/proxies/.+/delay$"))
            .and(query_param("timeout", "5000"))
            .and(query_param("url", "https://www.gstatic.com/generate_204"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "delay": 123 })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = ControllerClient::new(server.uri(), "s");
        let delay = client
            .delay("Node B", "https://www.gstatic.com/generate_204", 5000)
            .await
            .expect("the probe answers");
        assert_eq!(delay, 123);
    }

    #[test]
    fn the_subscription_list_is_read_with_its_last_update_kept_as_sent() {
        let bare = serde_json::json!([
            { "name": "Sub", "last_updated": "2026-11-13T10:00:00Z", "interval": 3600 },
            { "name": "Other", "last_updated": null }
        ]);
        let listed = parse_subscriptions(&bare).expect("a bare list parses");
        assert_eq!(listed[0].name, "Sub");
        assert_eq!(
            listed[0].last_updated,
            Some(serde_json::json!("2026-11-13T10:00:00Z"))
        );
        assert_eq!(listed[1].last_updated, None);

        let wrapped = serde_json::json!({ "subscriptions": [{ "name": "Sub" }] });
        assert_eq!(parse_subscriptions(&wrapped).unwrap()[0].name, "Sub");
        assert!(parse_subscriptions(&serde_json::json!({ "x": 1 })).is_err());
    }

    #[test]
    fn a_rule_list_of_strings_is_read_and_an_object_entry_is_refused() {
        let strings = serde_json::json!({ "rules": ["DOMAIN,a.com,DIRECT", "MATCH,PROXY"] });
        assert_eq!(
            parse_rules(&strings).unwrap(),
            vec!["DOMAIN,a.com,DIRECT".to_string(), "MATCH,PROXY".to_string()]
        );
        let objects = serde_json::json!([{ "type": "DomainSuffix", "payload": "a.com" }]);
        assert!(parse_rules(&objects).is_err());
    }

    #[test]
    fn the_proxy_list_is_read_in_both_the_map_and_the_array_form() {
        let map = serde_json::json!({
            "proxies": {
                "Node A": { "type": "trojan", "server": "a.example", "port": 443 },
                "Auto": { "type": "url-test" }
            }
        });
        let mut listed = parse_proxies(&map).expect("the map form parses");
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(listed[0].name, "Auto");
        assert_eq!(listed[0].server, None);
        assert_eq!(listed[1].server.as_deref(), Some("a.example"));
        assert_eq!(listed[1].port, Some(443));

        let array = serde_json::json!({ "proxies": [{ "name": "Node B" }] });
        assert_eq!(parse_proxies(&array).unwrap()[0].name, "Node B");
    }

    #[test]
    fn a_groups_current_member_is_read_as_now() {
        let listed = serde_json::json!({
            "proxies": {
                "Proxy": { "type": "Selector", "now": "Node A", "all": ["Node A"] },
                "Node A": { "type": "trojan", "server": "a.example", "port": 443 },
                "Blank": { "type": "Selector", "now": "" }
            }
        });
        let mut listed = parse_proxies(&listed).expect("the map form parses");
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(listed[0].name, "Blank");
        assert_eq!(listed[0].now, None, "an empty now is no selection");
        assert_eq!(listed[1].name, "Node A");
        assert_eq!(listed[1].now, None, "a node has no current member");
        assert_eq!(listed[2].name, "Proxy");
        assert_eq!(listed[2].now.as_deref(), Some("Node A"));
    }

    #[tokio::test]
    async fn the_rule_order_is_posted_as_a_rules_list_with_the_secret_in_the_header() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rules"))
            .and(header("authorization", "Bearer s3cret"))
            .and(body_json(serde_json::json!({
                "rules": ["DOMAIN,a.com,DIRECT", "MATCH,PROXY"]
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = ControllerClient::new(server.uri(), "s3cret");
        client
            .replace_rules(&["DOMAIN,a.com,DIRECT".to_string(), "MATCH,PROXY".to_string()])
            .await
            .expect("the rules are accepted");
    }

    #[tokio::test]
    async fn the_subscription_rules_and_proxies_are_fetched_from_their_routes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/subscriptions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!([{ "name": "Sub", "last_updated": 1700000000 }]),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rules"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "rules": ["MATCH,PROXY"] })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/proxies"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "proxies": { "Node A": { "server": "a.example", "port": 1 } } }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let client = ControllerClient::new(server.uri(), "s");
        let subs = client.engine_subscriptions().await.unwrap();
        assert_eq!(subs[0].last_updated, Some(serde_json::json!(1700000000)));
        assert_eq!(
            client.rules().await.unwrap(),
            vec!["MATCH,PROXY".to_string()]
        );
        assert_eq!(client.proxies().await.unwrap()[0].name, "Node A");
    }

    #[tokio::test]
    async fn connections_are_listed_and_closed_by_their_routes() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/connections"))
            .and(header("authorization", "Bearer s"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uploadTotal": 7,
                "downloadTotal": 9,
                "connections": [{ "id": "c1", "metadata": {}, "chains": ["Proxy"] }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/connections/c1"))
            .and(header("authorization", "Bearer s"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/connections"))
            .and(header("authorization", "Bearer s"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = ControllerClient::new(server.uri(), "s");
        let reply = client.connections().await.expect("the list is read");
        assert_eq!(reply.upload_total, 7);
        assert_eq!(reply.connections[0].chains, vec!["Proxy".to_string()]);
        client
            .close_connection("c1")
            .await
            .expect("one connection closes");
        client
            .close_all_connections()
            .await
            .expect("all connections close");
    }

    #[tokio::test]
    async fn a_delay_answer_without_a_delay_is_an_error_not_zero() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let client = ControllerClient::new(server.uri(), "s");
        assert!(
            client
                .delay("Node B", "https://example.com", 5000)
                .await
                .is_err()
        );
    }
}
