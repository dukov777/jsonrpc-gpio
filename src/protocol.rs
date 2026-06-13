//! JSON-RPC 2.0 envelope types for the GPIO server.
//!
//! Requests use an internally-tagged enum (`tag = "method"`, `content =
//! "params"`) so an unknown or malformed method is a deserialization error
//! rather than a runtime `None` to unwrap. Responses are built through the
//! constructors below and serialized with `to_json`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 error codes (subset used by this server).
pub const PARSE_ERROR: i32 = -32700;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const SERVER_ERROR: i32 = -32000;

/// GPIO pin mode. Wire form is snake_case: `input`, `output`, `input_pullup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinMode {
    Input,
    Output,
    InputPullup,
}

/// A GPIO request method + its params.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "gpio_config")]
    GpioConfig { pin: u8, mode: PinMode },
    #[serde(rename = "gpio_write")]
    GpioWrite { pin: u8, level: u8 },
    #[serde(rename = "gpio_read")]
    GpioRead { pin: u8 },
    #[serde(rename = "led_set")]
    LedSet { r: u8, g: u8, b: u8 },
}

/// Lightweight first-pass parse of the JSON-RPC envelope — just enough to
/// extract `id` and `method` before we know if the method is valid.
/// `id` is `Option` because notifications omit it.
#[derive(Debug, Deserialize)]
pub struct RawEnvelope {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
}

/// A full JSON-RPC 2.0 request envelope. `id` is `number | string | null` per
/// the spec, so it is kept as a raw [`Value`] and echoed back verbatim.
#[derive(Debug, PartialEq, Deserialize)]
pub struct Envelope {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(flatten)]
    pub request: Request,
}

/// A JSON-RPC 2.0 response. Exactly one of `result`/`error` is present.
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    pub id: Value,
}

/// The `error` member of a failed response.
#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl Response {
    /// A success response carrying `result`.
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        }
    }

    /// An error response with the given `code` and `message`.
    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
            id,
        }
    }

    /// Serialize to a single-line JSON string (no trailing newline).
    pub fn to_json(&self) -> String {
        // Both members derive `Serialize` infallibly (id is an owned `Value`),
        // so this never fails in practice.
        serde_json::to_string(self).expect("Response serializes")
    }
}

/// Parse one NDJSON line into a request envelope.
pub fn parse_request(line: &[u8]) -> Result<Envelope, serde_json::Error> {
    serde_json::from_slice(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notification_deserializes_with_none_id() {
        let line = br#"{"jsonrpc":"2.0","method":"gpio_read","params":{"pin":1}}"#;
        let env: RawEnvelope = serde_json::from_slice(line).expect("notification parses");
        assert!(env.id.is_none());
        assert_eq!(env.method, "gpio_read");
    }

    #[test]
    fn parses_gpio_write() {
        let line =
            br#"{"jsonrpc":"2.0","id":1,"method":"gpio_write","params":{"pin":2,"level":1}}"#;
        let env = parse_request(line).expect("valid request parses");
        assert_eq!(env.jsonrpc, "2.0");
        assert_eq!(env.id, json!(1));
        assert_eq!(env.request, Request::GpioWrite { pin: 2, level: 1 });
    }

    #[test]
    fn parses_gpio_config_with_input_pullup() {
        let line = br#"{"jsonrpc":"2.0","id":"abc","method":"gpio_config","params":{"pin":4,"mode":"input_pullup"}}"#;
        let env = parse_request(line).expect("valid request parses");
        assert_eq!(env.id, json!("abc"));
        assert_eq!(
            env.request,
            Request::GpioConfig {
                pin: 4,
                mode: PinMode::InputPullup
            }
        );
    }

    #[test]
    fn parses_led_set() {
        let line = br#"{"jsonrpc":"2.0","id":1,"method":"led_set","params":{"r":0,"g":16,"b":0}}"#;
        let env = parse_request(line).expect("valid request parses");
        assert_eq!(env.request, Request::LedSet { r: 0, g: 16, b: 0 });
    }

    #[test]
    fn parses_gpio_read() {
        let line = br#"{"jsonrpc":"2.0","id":7,"method":"gpio_read","params":{"pin":5}}"#;
        let env = parse_request(line).expect("valid request parses");
        assert_eq!(env.request, Request::GpioRead { pin: 5 });
    }

    #[test]
    fn malformed_line_is_a_parse_error() {
        assert!(parse_request(b"not json at all").is_err());
    }

    #[test]
    fn unknown_method_is_a_parse_error() {
        let line = br#"{"jsonrpc":"2.0","id":1,"method":"gpio_explode","params":{"pin":2}}"#;
        assert!(parse_request(line).is_err());
    }

    #[test]
    fn result_response_serializes_without_error_member() {
        let resp = Response::result(json!(1), json!({"level": 1}));
        let s = resp.to_json();
        assert_eq!(s, r#"{"jsonrpc":"2.0","result":{"level":1},"id":1}"#);
        assert!(!s.contains("error"));
    }

    #[test]
    fn error_response_serializes_without_result_member_and_null_id() {
        let resp = Response::error(Value::Null, PARSE_ERROR, "parse error");
        let s = resp.to_json();
        assert_eq!(
            s,
            r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"parse error"},"id":null}"#
        );
        assert!(!s.contains("result"));
    }
}
