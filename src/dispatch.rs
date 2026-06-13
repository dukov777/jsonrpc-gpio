//! GPIO method dispatch.
//!
//! [`process_line`] turns one request line into one response line, delegating
//! the actual pin operations to a [`GpioBackend`]. The host build uses
//! [`MockGpio`] (an in-memory pin map) so the whole RPC surface is testable on
//! Linux/macOS; the device build supplies a real backend over `esp-idf-hal`
//! (§7) behind the same trait.

use serde_json::{json, Value};

use crate::protocol::{
    parse_request, PinMode, RawEnvelope, Request, Response, INVALID_PARAMS, METHOD_NOT_FOUND,
    PARSE_ERROR, SERVER_ERROR,
};

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
    pub fn code(&self) -> i32 {
        match self {
            GpioError::InvalidPin(_) => INVALID_PARAMS,
            GpioError::Backend(_) => SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            GpioError::InvalidPin(pin) => format!("invalid pin: {pin}"),
            GpioError::Backend(msg) => msg.clone(),
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

/// Parse, dispatch, and serialize one request line.
///
/// Returns `Some(response)` for requests, `None` for valid notifications
/// (no `id` field — JSON-RPC 2.0 §4 requires these to be silently ignored).
pub fn process_line(
    line: &[u8],
    gpio: &mut impl GpioBackend,
    led: &mut impl LedBackend,
) -> Option<String> {
    // Phase 1: extract id and method name. Malformed JSON → PARSE_ERROR/null.
    let raw: RawEnvelope = match serde_json::from_slice(line) {
        Ok(r) => r,
        Err(_) => return Some(Response::error(Value::Null, PARSE_ERROR, "parse error").to_json()),
    };

    // Notification: no id field → no response.
    let id = match raw.id {
        Some(id) => id,
        None => return None,
    };

    // Phase 2: full parse to get typed params.
    let env = match parse_request(line) {
        Ok(env) => env,
        Err(_) => return Some(Response::error(id, METHOD_NOT_FOUND, "method not found").to_json()),
    };

    let outcome = match env.request {
        Request::GpioConfig { pin, mode } => gpio.config(pin, mode).map(|()| json!({ "ok": true })),
        Request::GpioWrite { pin, level } => gpio.write(pin, level).map(|()| json!({ "ok": true })),
        Request::GpioRead { pin } => gpio.read(pin).map(|level| json!({ "level": level })),
        Request::LedSet { r, g, b } => led.set_rgb(r, g, b).map(|()| json!({ "ok": true })),
    };

    Some(match outcome {
        Ok(result) => Response::result(id, result).to_json(),
        Err(e) => Response::error(id, e.code(), e.message()).to_json(),
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
        self.levels.insert(pin, level);
        Ok(())
    }

    fn read(&mut self, pin: u8) -> Result<u8, GpioError> {
        check_pin(pin)?;
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
            br#"{"jsonrpc":"2.0","id":1,"method":"gpio_write","params":{"pin":2,"level":1}}"#,
            &mut gpio,
        );
        let resp = call(
            br#"{"jsonrpc":"2.0","id":2,"method":"gpio_read","params":{"pin":2}}"#,
            &mut gpio,
        );
        assert_eq!(resp, json!({"jsonrpc":"2.0","result":{"level":1},"id":2}));
    }

    #[test]
    fn read_of_unset_pin_defaults_to_zero() {
        let mut gpio = MockGpio::new();
        let resp = call(
            br#"{"jsonrpc":"2.0","id":9,"method":"gpio_read","params":{"pin":7}}"#,
            &mut gpio,
        );
        assert_eq!(resp["result"], json!({"level": 0}));
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
        assert_eq!(resp["error"]["code"], json!(INVALID_PARAMS));
        assert_eq!(resp["id"], json!(5));
        assert!(resp.get("result").is_none());
    }

    #[test]
    fn malformed_line_is_parse_error_with_null_id() {
        let mut gpio = MockGpio::new();
        let resp = call(b"{ this is not json", &mut gpio);
        assert_eq!(resp["error"]["code"], json!(PARSE_ERROR));
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
    fn string_id_is_echoed_back() {
        let mut gpio = MockGpio::new();
        let resp = call(
            br#"{"jsonrpc":"2.0","id":"req-42","method":"gpio_read","params":{"pin":1}}"#,
            &mut gpio,
        );
        assert_eq!(resp["id"], json!("req-42"));
    }
}
