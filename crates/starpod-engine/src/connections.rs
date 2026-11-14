//! The engine's open connections, as the controller reports them.
//!
//! `GET /connections` answers with the traffic totals since the engine
//! started and one entry per open connection. The shape follows the mihomo
//! API the engine mirrors:
//!
//! ```json
//! {
//!   "downloadTotal": 123,
//!   "uploadTotal": 45,
//!   "connections": [{
//!     "id": "…",
//!     "metadata": {
//!       "network": "tcp", "type": "HTTPS", "host": "example.com",
//!       "sourceIP": "127.0.0.1", "sourcePort": "51234",
//!       "destinationIP": "93.184.216.34", "destinationPort": "443",
//!       "process": "chrome.exe", "processPath": "C:\\…\\chrome.exe"
//!     },
//!     "upload": 10, "download": 20, "start": "2026-11-14T12:03:44.123+03:00",
//!     "chains": ["Node A", "Proxy"], "rule": "DOMAIN-SUFFIX", "rulePayload": "example.com"
//!   }]
//! }
//! ```
//!
//! Ports arrive as strings in mihomo's output and as numbers in some other
//! builds; both are read. A `null` connection list means no connections.
//!
//! The payload carries the user's destinations and process names. Nothing
//! here logs it, and the types are plain data for the app to show.

use serde_json::Value;

use crate::error::{EngineError, EngineResult};

/// The answer of `GET /connections`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectionsReply {
    /// Bytes uploaded since the engine started.
    pub upload_total: u64,
    /// Bytes downloaded since the engine started.
    pub download_total: u64,
    /// The open connections, in the order the engine lists them.
    pub connections: Vec<EngineConnection>,
}

/// One open connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineConnection {
    /// The engine's id. It is the handle a close request names.
    pub id: String,
    /// `tcp` or `udp`.
    pub network: String,
    /// The inbound type, such as `HTTPS`, `HTTP` or `Socks5`.
    pub kind: String,
    /// The requested host name. Empty when the connection had only an IP.
    pub host: String,
    /// The local address the client connected from.
    pub source_ip: String,
    /// The local port the client connected from.
    pub source_port: String,
    /// The address the client asked for.
    pub destination_ip: String,
    /// The port the client asked for.
    pub destination_port: String,
    /// The process name, when the engine could tell it (TUN and some modes).
    pub process: String,
    /// The process's executable path. Empty when unknown.
    pub process_path: String,
    /// Bytes this connection has uploaded.
    pub upload: u64,
    /// Bytes this connection has downloaded.
    pub download: u64,
    /// When the connection opened, as RFC 3339 with the engine's offset.
    pub start: String,
    /// The outbounds the connection went through, in the engine's order.
    pub chains: Vec<String>,
    /// The rule that matched, for example `DOMAIN-SUFFIX`.
    pub rule: String,
    /// The matched rule's payload, for example `example.com`.
    pub rule_payload: String,
}

/// Read the answer of `GET /connections`.
///
/// `id` is the only field a connection must carry: without it the connection
/// cannot be closed or tracked, so an entry without one is an error rather
/// than a connection with a made-up id.
pub fn parse_connections(value: &Value) -> EngineResult<ConnectionsReply> {
    let items: Vec<&Value> = match value.get("connections") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().collect(),
        Some(_) => return Err(malformed("connection list")),
    };
    Ok(ConnectionsReply {
        upload_total: unsigned(value.get("uploadTotal")),
        download_total: unsigned(value.get("downloadTotal")),
        connections: items
            .into_iter()
            .map(connection)
            .collect::<EngineResult<Vec<_>>>()?,
    })
}

fn connection(item: &Value) -> EngineResult<EngineConnection> {
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| malformed("connection list (an entry has no id)"))?;
    let meta = item.get("metadata").unwrap_or(&Value::Null);
    let text = |key: &str| meta.get(key).map(scalar).unwrap_or_default();
    let chains = item
        .get("chains")
        .and_then(Value::as_array)
        .map(|chains| {
            chains
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(EngineConnection {
        id: id.to_string(),
        network: text("network"),
        kind: text("type"),
        host: text("host"),
        source_ip: text("sourceIP"),
        source_port: text("sourcePort"),
        destination_ip: text("destinationIP"),
        destination_port: text("destinationPort"),
        process: text("process"),
        process_path: text("processPath"),
        upload: unsigned(item.get("upload")),
        download: unsigned(item.get("download")),
        start: item
            .get("start")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        chains,
        rule: item
            .get("rule")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        rule_payload: item
            .get("rulePayload")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// A metadata field as text. Strings are kept; numbers (ports, in some
/// builds) are written out; anything else is empty.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

/// A byte count. Missing, null or negative values read as zero.
fn unsigned(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .and_then(Value::as_f64)
                .filter(|n| *n >= 0.0)
                .map(|n| n as u64)
        })
        .unwrap_or(0)
}

fn malformed(what: &str) -> EngineError {
    EngineError::Internal(format!(
        "the engine's answer for the {what} has an unexpected shape"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload in the shape mihomo sends, with string ports.
    fn sample() -> Value {
        serde_json::json!({
            "downloadTotal": 4096,
            "uploadTotal": 1024,
            "connections": [
                {
                    "id": "c1",
                    "metadata": {
                        "network": "tcp",
                        "type": "HTTPS",
                        "host": "example.com",
                        "sourceIP": "127.0.0.1",
                        "sourcePort": "51234",
                        "destinationIP": "93.184.216.34",
                        "destinationPort": "443",
                        "process": "chrome.exe",
                        "processPath": "C:\\Program Files\\chrome.exe"
                    },
                    "upload": 10,
                    "download": 2000,
                    "start": "2026-11-14T12:03:44.123456789+03:00",
                    "chains": ["Node A", "Proxy"],
                    "rule": "DOMAIN-SUFFIX",
                    "rulePayload": "example.com"
                },
                {
                    "id": "c2",
                    "metadata": {
                        "network": "udp",
                        "type": "Socks5",
                        "host": "",
                        "destinationIP": "1.1.1.1",
                        "destinationPort": 53
                    },
                    "upload": 5,
                    "download": 7,
                    "start": "2026-11-14T12:04:00+03:00",
                    "chains": ["DIRECT"],
                    "rule": "MATCH",
                    "rulePayload": ""
                }
            ]
        })
    }

    #[test]
    fn a_mihomo_payload_is_read_in_full() {
        let reply = parse_connections(&sample()).expect("the sample parses");
        assert_eq!(reply.upload_total, 1024);
        assert_eq!(reply.download_total, 4096);
        assert_eq!(reply.connections.len(), 2);

        let first = &reply.connections[0];
        assert_eq!(first.id, "c1");
        assert_eq!(first.network, "tcp");
        assert_eq!(first.kind, "HTTPS");
        assert_eq!(first.host, "example.com");
        assert_eq!(first.source_ip, "127.0.0.1");
        assert_eq!(first.source_port, "51234");
        assert_eq!(first.destination_ip, "93.184.216.34");
        assert_eq!(first.destination_port, "443");
        assert_eq!(first.process, "chrome.exe");
        assert_eq!(first.process_path, "C:\\Program Files\\chrome.exe");
        assert_eq!(first.upload, 10);
        assert_eq!(first.download, 2000);
        assert_eq!(first.start, "2026-11-14T12:03:44.123456789+03:00");
        assert_eq!(
            first.chains,
            vec!["Node A".to_string(), "Proxy".to_string()]
        );
        assert_eq!(first.rule, "DOMAIN-SUFFIX");
        assert_eq!(first.rule_payload, "example.com");
    }

    #[test]
    fn a_numeric_port_and_missing_fields_read_as_text_and_empty() {
        let reply = parse_connections(&sample()).expect("the sample parses");
        let second = &reply.connections[1];
        assert_eq!(
            second.destination_port, "53",
            "a numeric port is written out"
        );
        assert_eq!(second.host, "");
        assert_eq!(second.process, "", "no process is an empty name");
        assert_eq!(second.source_ip, "");
    }

    #[test]
    fn a_null_or_absent_list_is_no_connections() {
        for payload in [
            serde_json::json!({ "uploadTotal": 0, "downloadTotal": 0, "connections": null }),
            serde_json::json!({ "uploadTotal": 3 }),
        ] {
            let reply = parse_connections(&payload).expect("an empty answer parses");
            assert!(reply.connections.is_empty());
        }
    }

    #[test]
    fn a_connection_without_an_id_is_refused() {
        let payload = serde_json::json!({ "connections": [{ "metadata": {} }] });
        assert!(parse_connections(&payload).is_err());
    }

    #[test]
    fn a_list_that_is_not_a_list_is_refused() {
        let payload = serde_json::json!({ "connections": "nope" });
        assert!(parse_connections(&payload).is_err());
    }
}
