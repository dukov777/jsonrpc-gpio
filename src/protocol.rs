//! JSON-RPC 2.0 request typing for the GPIO server.
//!
//! The envelope, `Id`, and error/response objects are provided by the
//! `jsonrpc-lite` crate. This module keeps only the strongly-typed method
//! enum: an internally-tagged `Request` (`tag = "method", content = "params"`)
//! so each method's params are deserialized into a typed variant and a
//! malformed/missing param is a deserialization error rather than a runtime
//! `None` to unwrap.

use serde::Deserialize;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json};

    /// Deserialize the internally-tagged shape `process_line` rebuilds.
    fn parse(method: &str, params: serde_json::Value) -> Result<Request, serde_json::Error> {
        from_value(json!({ "method": method, "params": params }))
    }

    #[test]
    fn parses_gpio_write() {
        let req = parse("gpio_write", json!({"pin":2,"level":1})).expect("valid");
        assert_eq!(req, Request::GpioWrite { pin: 2, level: 1 });
    }

    #[test]
    fn parses_gpio_config_with_input_pullup() {
        let req = parse("gpio_config", json!({"pin":4,"mode":"input_pullup"})).expect("valid");
        assert_eq!(
            req,
            Request::GpioConfig { pin: 4, mode: PinMode::InputPullup }
        );
    }

    #[test]
    fn parses_led_set() {
        let req = parse("led_set", json!({"r":0,"g":16,"b":0})).expect("valid");
        assert_eq!(req, Request::LedSet { r: 0, g: 16, b: 0 });
    }

    #[test]
    fn parses_gpio_read() {
        let req = parse("gpio_read", json!({"pin":5})).expect("valid");
        assert_eq!(req, Request::GpioRead { pin: 5 });
    }

    #[test]
    fn unknown_method_is_a_parse_error() {
        assert!(parse("gpio_explode", json!({"pin":2})).is_err());
    }

    #[test]
    fn missing_param_is_a_parse_error() {
        // gpio_write requires `level`.
        assert!(parse("gpio_write", json!({"pin":2})).is_err());
    }
}
