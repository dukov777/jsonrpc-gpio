//! GPIO method dispatch.
//!
//! [`process_line`] turns one request line into one response line, delegating
//! the actual pin operations to a [`GpioBackend`]. The host build uses
//! [`MockGpio`] (an in-memory pin map) so the whole RPC surface is testable on
//! Linux/macOS; the device build supplies a real backend over `esp-idf-hal`
//! (§7) behind the same trait.

use jsonrpc_lite::{Error as RpcError, Id, JsonRpc, Params};
use serde_json::{json, Value};

use crate::protocol::{PinMode, Request};

/// Custom server-error code for backend/hardware failures (no jsonrpc-lite
/// constructor exists for it).
pub const SERVER_ERROR: i64 = -32000;

/// Highest GPIO number on the ESP32-S3 (GPIO0..=GPIO48). The mock validates
/// against this range; a real backend should use a tighter allowlist.
pub const MAX_PIN: u8 = 48;

/// A GPIO operation failure, carrying its JSON-RPC error code.
#[derive(Debug, PartialEq, Eq)]
pub enum GpioError {
    /// Pin number outside the controllable range — maps to `-32602`.
    InvalidPin(u8),
    /// Backend/hardware failure — maps to `-32000`.
    Backend(String),
}

impl GpioError {
    /// Map this failure to a `jsonrpc-lite` error object.
    fn to_rpc_error(&self) -> RpcError {
        match self {
            GpioError::InvalidPin(pin) => RpcError {
                code: -32602, // invalid params
                message: format!("invalid pin: {pin}"),
                data: None,
            },
            GpioError::Backend(msg) => RpcError {
                code: SERVER_ERROR,
                message: msg.clone(),
                data: None,
            },
        }
    }
}

/// The pin operations the dispatcher needs. Implemented by [`MockGpio`] on the
/// host and by a real `esp-idf-hal` backend on the device.
pub trait GpioBackend {
    fn config(&mut self, pin: u8, mode: PinMode) -> Result<(), GpioError>;
    fn write(&mut self, pin: u8, level: u8) -> Result<(), GpioError>;
    fn read(&mut self, pin: u8) -> Result<u8, GpioError>;
}

/// The on-board RGB LED operation. Implemented by [`MockLed`] on the host and
/// by the WS2812 RMT driver on the device.
pub trait LedBackend {
    fn set_rgb(&mut self, r: u8, g: u8, b: u8) -> Result<(), GpioError>;
}

/// Serialize a `JsonRpc` response to a single-line JSON string (no trailing
/// newline; `server.rs` appends the `\n`).
fn serialize(rpc: JsonRpc) -> String {
    serde_json::to_string(&rpc).expect("JsonRpc serializes")
}

/// The methods this server understands. Used to distinguish an unknown method
/// (-32601) from bad params on a known method (-32602).
const KNOWN_METHODS: [&str; 4] = ["gpio_config", "gpio_write", "gpio_read", "led_set"];

/// Parse, dispatch, and serialize one request line.
///
/// Returns `Some(response)` for requests, `None` for notifications (no `id`
/// field — JSON-RPC 2.0 §4 requires these to be silently ignored).
pub fn process_line(
    line: &[u8],
    gpio: &mut impl GpioBackend,
    led: &mut impl LedBackend,
) -> Option<String> {
    // Phase 1: parse the envelope. Malformed JSON -> parse error, null id.
    let rpc: JsonRpc = match serde_json::from_slice(line) {
        Ok(rpc) => rpc,
        Err(_) => return Some(serialize(JsonRpc::error(Id::None(()), RpcError::parse_error()))),
    };

    // Notification (no id field) -> no response.
    let id = rpc.get_id()?;

    // Not a request (e.g. an inbound Success/Error object) -> invalid request.
    let method = match rpc.get_method() {
        Some(m) => m.to_owned(),
        None => return Some(serialize(JsonRpc::error(id, RpcError::invalid_request()))),
    };

    // Phase 2: classify the method, then type its params.
    if !KNOWN_METHODS.contains(&method.as_str()) {
        return Some(serialize(JsonRpc::error(id, RpcError::method_not_found())));
    }

    // Rebuild the internally-tagged shape our `Request` enum expects:
    // {"method": "...", "params": {...}}.
    let params = rpc.get_params().unwrap_or(Params::None(()));
    let envelope = json!({
        "method": method,
        "params": serde_json::to_value(&params).expect("Params serializes"),
    });
    let request: Request = match serde_json::from_value(envelope) {
        Ok(req) => req,
        Err(_) => return Some(serialize(JsonRpc::error(id, RpcError::invalid_params()))),
    };

    let outcome = match request {
        Request::GpioConfig { pin, mode } => gpio.config(pin, mode).map(|()| json!({ "ok": true })),
        Request::GpioWrite { pin, level } => gpio.write(pin, level).map(|()| json!({ "ok": true })),
        Request::GpioRead { pin } => gpio.read(pin).map(|level| json!({ "level": level })),
        Request::LedSet { r, g, b } => led.set_rgb(r, g, b).map(|()| json!({ "ok": true })),
    };

    Some(match outcome {
        Ok(result) => serialize(JsonRpc::success(id, &result)),
        Err(e) => serialize(JsonRpc::error(id, e.to_rpc_error())),
    })
}

/// In-memory GPIO backend for host builds and tests: a pin->level map plus the
/// last configured mode per pin. No hardware required.
#[derive(Default)]
pub struct MockGpio {
    levels: std::collections::HashMap<u8, u8>,
    modes: std::collections::HashMap<u8, PinMode>,
}

impl MockGpio {
    pub fn new() -> Self {
        Self::default()
    }

    /// The mode last set for `pin`, if any (test/inspection helper).
    pub fn mode_of(&self, pin: u8) -> Option<PinMode> {
        self.modes.get(&pin).copied()
    }
}

/// In-memory LED backend for host builds and tests: records the last color set.
#[derive(Default)]
pub struct MockLed {
    last: Option<(u8, u8, u8)>,
}

impl MockLed {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last color set, if any (test/inspection helper).
    pub fn last(&self) -> Option<(u8, u8, u8)> {
        self.last
    }
}

impl LedBackend for MockLed {
    fn set_rgb(&mut self, r: u8, g: u8, b: u8) -> Result<(), GpioError> {
        self.last = Some((r, g, b));
        Ok(())
    }
}

fn check_pin(pin: u8) -> Result<(), GpioError> {
    if pin <= MAX_PIN {
        Ok(())
    } else {
        Err(GpioError::InvalidPin(pin))
    }
}

impl GpioBackend for MockGpio {
    fn config(&mut self, pin: u8, mode: PinMode) -> Result<(), GpioError> {
        check_pin(pin)?;
        self.modes.insert(pin, mode);
        Ok(())
    }

    fn write(&mut self, pin: u8, level: u8) -> Result<(), GpioError> {
        check_pin(pin)?;
        if !self.modes.contains_key(&pin) {
            return Err(GpioError::Backend("pin not configured".into()));
        }
        self.levels.insert(pin, u8::from(level != 0));
        Ok(())
    }

    fn read(&mut self, pin: u8) -> Result<u8, GpioError> {
        check_pin(pin)?;
        if !self.modes.contains_key(&pin) {
            return Err(GpioError::Backend("pin not configured".into()));
        }
        Ok(self.levels.get(&pin).copied().unwrap_or(0))
    }
}

/// Real GPIO backend for the ESP32-S3, over raw `esp-idf-sys` so any pin can be
/// driven by runtime number with a runtime mode (the typed `PinDriver` encodes
/// mode in its type, which fights this protocol). Pins are validated against
/// [`MAX_PIN`]; the unsafe FFI calls are single C calls with no aliasing.
///
/// `output` maps to `INPUT_OUTPUT` (input buffer enabled) so a `gpio_read`
/// after a `gpio_write` reads back the driven level — matching the host mock's
/// write→read semantics. `input`/`input_pullup` read an external signal.
#[cfg(target_os = "espidf")]
#[derive(Default)]
pub struct EspGpio;

#[cfg(target_os = "espidf")]
impl EspGpio {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "espidf")]
impl GpioBackend for EspGpio {
    fn config(&mut self, pin: u8, mode: PinMode) -> Result<(), GpioError> {
        use esp_idf_hal::sys::{
            gpio_mode_t_GPIO_MODE_INPUT, gpio_mode_t_GPIO_MODE_INPUT_OUTPUT,
            gpio_pull_mode_t_GPIO_FLOATING, gpio_pull_mode_t_GPIO_PULLUP_ONLY, gpio_set_direction,
            gpio_set_pull_mode,
        };
        check_pin(pin)?;
        let num = pin as esp_idf_hal::sys::gpio_num_t;
        let (dir, pull) = match mode {
            PinMode::Input => (gpio_mode_t_GPIO_MODE_INPUT, gpio_pull_mode_t_GPIO_FLOATING),
            // INPUT_OUTPUT (not plain OUTPUT) so reads see the driven level.
            PinMode::Output => (
                gpio_mode_t_GPIO_MODE_INPUT_OUTPUT,
                gpio_pull_mode_t_GPIO_FLOATING,
            ),
            PinMode::InputPullup => (
                gpio_mode_t_GPIO_MODE_INPUT,
                gpio_pull_mode_t_GPIO_PULLUP_ONLY,
            ),
        };
        // SAFETY: `num` is a validated GPIO number; both are plain C calls.
        esp_ok(unsafe { gpio_set_direction(num, dir) })?;
        esp_ok(unsafe { gpio_set_pull_mode(num, pull) })?;
        Ok(())
    }

    fn write(&mut self, pin: u8, level: u8) -> Result<(), GpioError> {
        use esp_idf_hal::sys::gpio_set_level;
        check_pin(pin)?;
        let num = pin as esp_idf_hal::sys::gpio_num_t;
        // SAFETY: validated pin number; single C call.
        esp_ok(unsafe { gpio_set_level(num, u32::from(level != 0)) })
    }

    fn read(&mut self, pin: u8) -> Result<u8, GpioError> {
        use esp_idf_hal::sys::gpio_get_level;
        check_pin(pin)?;
        let num = pin as esp_idf_hal::sys::gpio_num_t;
        // SAFETY: validated pin number; single C call returning the level.
        let level = unsafe { gpio_get_level(num) };
        Ok(u8::from(level != 0))
    }
}

/// Map an `esp_err_t` to a [`GpioError`] (`ESP_OK` == 0).
#[cfg(target_os = "espidf")]
fn esp_ok(code: esp_idf_hal::sys::esp_err_t) -> Result<(), GpioError> {
    if code == 0 {
        Ok(())
    } else {
        Err(GpioError::Backend(format!("esp_err_t {code}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(line: &[u8], gpio: &mut MockGpio) -> Value {
        let mut led = MockLed::new();
        let s = process_line(line, gpio, &mut led).expect("request produces a response");
        serde_json::from_str(&s).expect("response is valid JSON")
    }

    #[test]
    fn led_set_drives_the_led_backend_and_acks() {
        let mut gpio = MockGpio::new();
        let mut led = MockLed::new();
        let resp: Value = serde_json::from_str(
            &process_line(
                br#"{"jsonrpc":"2.0","id":1,"method":"led_set","params":{"r":0,"g":16,"b":0}}"#,
                &mut gpio,
                &mut led,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(resp["result"], json!({ "ok": true }));
        assert_eq!(led.last(), Some((0, 16, 0)));
    }

    #[test]
    fn led_set_zero_is_off() {
        let mut gpio = MockGpio::new();
        let mut led = MockLed::new();
        process_line(
            br#"{"jsonrpc":"2.0","id":1,"method":"led_set","params":{"r":0,"g":0,"b":0}}"#,
            &mut gpio,
            &mut led,
        )
        .unwrap();
        assert_eq!(led.last(), Some((0, 0, 0)));
    }

    #[test]
    fn write_then_read_returns_stored_level() {
        let mut gpio = MockGpio::new();
        call(
            br#"{"jsonrpc":"2.0","id":0,"method":"gpio_config","params":{"pin":2,"mode":"output"}}"#,
            &mut gpio,
        );
        call(
            br#"{"jsonrpc":"2.0","id":1,"method":"gpio_write","params":{"pin":2,"level":1}}"#,
            &mut gpio,
        );
        let resp = call(
            br#"{"jsonrpc":"2.0","id":2,"method":"gpio_read","params":{"pin":2}}"#,
            &mut gpio,
        );
        assert_eq!(resp["result"], json!({"level":1}));
        assert_eq!(resp["id"], json!(2));
        assert!(resp.get("error").is_none());
    }

    #[test]
    fn read_of_unconfigured_pin_returns_error() {
        let mut gpio = MockGpio::new();
        let resp = call(
            br#"{"jsonrpc":"2.0","id":9,"method":"gpio_read","params":{"pin":7}}"#,
            &mut gpio,
        );
        assert!(resp.get("error").is_some(), "expected error for unconfigured pin read");
        assert!(resp.get("result").is_none());
    }

    #[test]
    fn write_normalises_level_to_binary() {
        let mut gpio = MockGpio::new();
        call(
            br#"{"jsonrpc":"2.0","id":1,"method":"gpio_config","params":{"pin":3,"mode":"output"}}"#,
            &mut gpio,
        );
        call(
            br#"{"jsonrpc":"2.0","id":2,"method":"gpio_write","params":{"pin":3,"level":2}}"#,
            &mut gpio,
        );
        let resp = call(
            br#"{"jsonrpc":"2.0","id":3,"method":"gpio_read","params":{"pin":3}}"#,
            &mut gpio,
        );
        assert_eq!(resp["result"]["level"], json!(1), "level=2 must normalise to 1");
    }

    #[test]
    fn write_to_unconfigured_pin_returns_error() {
        let mut gpio = MockGpio::new();
        let resp = call(
            br#"{"jsonrpc":"2.0","id":10,"method":"gpio_write","params":{"pin":5,"level":1}}"#,
            &mut gpio,
        );
        assert!(resp.get("error").is_some(), "expected error for unconfigured pin write");
        assert!(resp.get("result").is_none());
    }

    #[test]
    fn config_records_mode_and_acks() {
        let mut gpio = MockGpio::new();
        let resp = call(
            br#"{"jsonrpc":"2.0","id":3,"method":"gpio_config","params":{"pin":4,"mode":"input_pullup"}}"#,
            &mut gpio,
        );
        assert_eq!(resp["result"], json!({"ok": true}));
        assert_eq!(gpio.mode_of(4), Some(PinMode::InputPullup));
    }

    #[test]
    fn out_of_range_pin_is_invalid_params() {
        let mut gpio = MockGpio::new();
        let resp = call(
            br#"{"jsonrpc":"2.0","id":5,"method":"gpio_write","params":{"pin":200,"level":1}}"#,
            &mut gpio,
        );
        assert_eq!(resp["error"]["code"], json!(-32602));
        assert_eq!(resp["id"], json!(5));
        assert!(resp.get("result").is_none());
    }

    #[test]
    fn malformed_line_is_parse_error_with_null_id() {
        let mut gpio = MockGpio::new();
        let resp = call(b"{ this is not json", &mut gpio);
        assert_eq!(resp["error"]["code"], json!(-32700));
        assert_eq!(resp["id"], Value::Null);
    }

    #[test]
    fn notification_returns_no_response() {
        let mut gpio = MockGpio::new();
        let mut led = MockLed::new();
        let result = process_line(
            br#"{"jsonrpc":"2.0","method":"gpio_read","params":{"pin":1}}"#,
            &mut gpio,
            &mut led,
        );
        assert!(result.is_none(), "notifications must produce no response");
    }

    #[test]
    fn unknown_method_returns_method_not_found_with_id() {
        let mut gpio = MockGpio::new();
        let mut led = MockLed::new();
        let response = process_line(
            br#"{"jsonrpc":"2.0","id":42,"method":"gpio_explode","params":{}}"#,
            &mut gpio,
            &mut led,
        )
        .expect("unknown method produces a response");
        let resp: Value = serde_json::from_str(&response).expect("valid JSON");
        assert_eq!(resp["error"]["code"], json!(-32601));
        assert_eq!(resp["id"], json!(42));
    }

    #[test]
    fn known_method_with_bad_params_is_invalid_params_not_method_not_found() {
        let mut gpio = MockGpio::new();
        let resp = call(
            br#"{"jsonrpc":"2.0","id":1,"method":"gpio_write","params":{"pin":2}}"#,
            &mut gpio,
        );
        assert_eq!(resp["error"]["code"], json!(-32602), "bad params on a known method -> invalid params");
        assert_eq!(resp["id"], json!(1));
    }

    #[test]
    fn string_id_is_echoed_back() {
        let mut gpio = MockGpio::new();
        call(
            br#"{"jsonrpc":"2.0","id":0,"method":"gpio_config","params":{"pin":1,"mode":"input"}}"#,
            &mut gpio,
        );
        let resp = call(
            br#"{"jsonrpc":"2.0","id":"req-42","method":"gpio_read","params":{"pin":1}}"#,
            &mut gpio,
        );
        assert_eq!(resp["id"], json!("req-42"));
    }
}
