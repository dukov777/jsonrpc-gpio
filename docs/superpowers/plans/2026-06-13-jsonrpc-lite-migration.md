# jsonrpc-lite Envelope Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hand-rolled JSON-RPC 2.0 envelope/response/error types with the `jsonrpc-lite` crate, while keeping the strongly-typed `Request` enum dispatch and fixing the `method-not-found` vs `invalid-params` error-code bug.

**Architecture:** `jsonrpc-lite` owns parsing the inbound envelope (`JsonRpc`), the `Id` type, and serializing `Success`/`Error` responses. `dispatch.rs::process_line` parses the envelope, treats missing-id requests as notifications, classifies the method name first (→ method-not-found), then deserializes `params` into the existing internally-tagged `protocol::Request` enum (→ invalid-params on failure), and dispatches over the unchanged `GpioBackend`/`LedBackend` traits. `server.rs` (NDJSON framing) is untouched.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `jsonrpc-lite = "0.7"`, `rstest` (dev). Host tests run on macOS/Linux; device build is cfg-gated to `espidf`.

**Spec:** `docs/superpowers/specs/2026-06-13-jsonrpc-lite-migration-design.md`

---

## Verified `jsonrpc-lite` v0.7.0 API (reference for all tasks)

```rust
// jsonrpc_lite::JsonRpc (untagged enum: Request | Notification | Success | Error)
pub fn JsonRpc::success<I: Into<Id>>(id: I, result: &serde_json::Value) -> JsonRpc
pub fn JsonRpc::error<I: Into<Id>>(id: I, error: jsonrpc_lite::Error) -> JsonRpc
pub fn JsonRpc::get_method(&self) -> Option<&str>
pub fn JsonRpc::get_params(&self) -> Option<Params>   // Params: Array(Vec<Value>) | Map(Map<String,Value>) | None(())
pub fn JsonRpc::get_id(&self) -> Option<Id>           // None for a notification (no id field)

// jsonrpc_lite::Id (untagged): Num(i64) | Str(String) | None(())
// From<i64>, From<String>, From<()>  — NOTE: no From<&str>

// jsonrpc_lite::Error { pub code: i64, pub message: String, pub data: Option<Value> }  // fields are public
pub fn Error::parse_error() -> Error          // -32700
pub fn Error::invalid_request() -> Error      // -32600
pub fn Error::method_not_found() -> Error     // -32601
pub fn Error::invalid_params() -> Error       // -32602
```

> If any signature differs slightly from the above when you compile, follow the compiler — these were read from the v0.7.0 source but the TDD steps will catch mismatches.

---

## Task 1: Add the `jsonrpc-lite` dependency

**Files:**
- Modify: `Cargo.toml` (`[dependencies]` section)

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, under `[dependencies]` (next to `serde_json = "1"`), add:

```toml
# JSON-RPC 2.0 envelope/Id/Error data structures. Pure serde types over
# serde_json (no new transitive deps); builds on std, which esp-idf provides.
jsonrpc-lite = "0.7"
```

- [ ] **Step 2: Verify it resolves and the tree still builds**

Run: `cargo build`
Expected: Builds successfully; `Cargo.lock` now contains `jsonrpc-lite v0.7.0`.

- [ ] **Step 3: Verify the existing test baseline is still green**

Run: `cargo test`
Expected: PASS — 29 unit + 4 integration tests pass (unchanged baseline).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add jsonrpc-lite dependency"
```

---

## Task 2: Rewrite `dispatch.rs::process_line` over `jsonrpc-lite`

This is the core change. `protocol.rs` still exports the old types at this point, so the
crate keeps compiling between steps; Task 3 removes the now-unused old types.

**Files:**
- Modify: `src/dispatch.rs` (imports at top; `GpioError` impl ~lines 29-43; `process_line` ~lines 59-97; tests ~lines 250-420)

- [ ] **Step 1: Write the failing test for the invalid-params fix**

Add this test inside the `#[cfg(test)] mod tests` block in `src/dispatch.rs`:

```rust
#[test]
fn known_method_with_bad_params_is_invalid_params_not_method_not_found() {
    let mut gpio = MockGpio::new();
    // gpio_write is a known method, but `level` is missing -> bad params.
    let resp = call(
        br#"{"jsonrpc":"2.0","id":1,"method":"gpio_write","params":{"pin":2}}"#,
        &mut gpio,
    );
    assert_eq!(resp["error"]["code"], json!(-32602), "bad params on a known method -> invalid params");
    assert_eq!(resp["id"], json!(1));
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test --lib known_method_with_bad_params -- --nocapture`
Expected: FAIL — current code returns `-32601` (method not found) for this input.

- [ ] **Step 3: Replace the imports at the top of `dispatch.rs`**

Replace the `use crate::protocol::{...}` block (lines ~11-14) with:

```rust
use jsonrpc_lite::{Error as RpcError, Id, JsonRpc, Params};
use serde_json::{json, Value};

use crate::protocol::{PinMode, Request};

/// Custom server-error code for backend/hardware failures (no jsonrpc-lite
/// constructor exists for it).
pub const SERVER_ERROR: i64 = -32000;
```

(Keep the existing `use serde_json::{json, Value};` line de-duplicated — there should be exactly one.)

- [ ] **Step 4: Replace `GpioError`'s `code`/`message` methods with an `Error` mapping**

Replace the `impl GpioError { ... }` block (lines ~29-43) with:

```rust
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
```

- [ ] **Step 5: Rewrite `process_line` and add the `serialize` helper**

Replace the entire `process_line` function (lines ~59-97) with:

```rust
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
```

- [ ] **Step 6: Update the existing dispatch tests that assert old error constants/exact strings**

In `mod tests`, three tests reference removed names (`INVALID_PARAMS`, `PARSE_ERROR`, `METHOD_NOT_FOUND`) or assert byte-exact JSON. Update them to structural/numeric checks:

In `out_of_range_pin_is_invalid_params`, replace `json!(INVALID_PARAMS)` with `json!(-32602)`.

In `malformed_line_is_parse_error_with_null_id`, replace `json!(PARSE_ERROR)` with `json!(-32700)`.

In `unknown_method_returns_method_not_found_with_id`, replace `json!(METHOD_NOT_FOUND)` with `json!(-32601)`.

In `write_then_read_returns_stored_level`, the assertion compares the whole object. jsonrpc-lite may order members differently, so compare members individually:

```rust
    assert_eq!(resp["result"], json!({"level":1}));
    assert_eq!(resp["id"], json!(2));
    assert!(resp.get("error").is_none());
```

(Leave all other dispatch tests as-is — they already use `serde_json::Value` field access.)

- [ ] **Step 7: Run the dispatch tests**

Run: `cargo test --lib dispatch`
Expected: PASS — including the new `known_method_with_bad_params_*` test and all updated tests.

- [ ] **Step 8: Commit**

```bash
git add src/dispatch.rs
git commit -m "feat: dispatch over jsonrpc-lite; fix invalid-params vs method-not-found"
```

---

## Task 3: Remove the now-dead hand-rolled types from `protocol.rs`

**Files:**
- Modify: `src/protocol.rs` (replace nearly the whole file)

- [ ] **Step 1: Replace `protocol.rs` with the trimmed version**

Replace the entire contents of `src/protocol.rs` with:

```rust
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
```

- [ ] **Step 2: Run the protocol tests**

Run: `cargo test --lib protocol`
Expected: PASS — 6 tests.

- [ ] **Step 3: Build the whole crate to confirm no dangling references**

Run: `cargo build`
Expected: Builds with no errors and no `dead_code`/unused-import warnings from `protocol.rs` or `dispatch.rs`.

- [ ] **Step 4: Commit**

```bash
git add src/protocol.rs
git commit -m "refactor: drop hand-rolled envelope/response types, keep typed Request"
```

---

## Task 4: Lock in the id-fidelity edge cases (notification vs explicit-null id)

**Files:**
- Modify: `src/dispatch.rs` (tests block)

- [ ] **Step 1: Write the failing tests for id handling**

Add these two tests inside `mod tests` in `src/dispatch.rs`:

```rust
#[test]
fn missing_id_is_a_notification_with_no_response() {
    let mut gpio = MockGpio::new();
    let mut led = MockLed::new();
    let result = process_line(
        br#"{"jsonrpc":"2.0","method":"gpio_read","params":{"pin":1}}"#,
        &mut gpio,
        &mut led,
    );
    assert!(result.is_none(), "a request without an id is a notification");
}

#[test]
fn explicit_null_id_gets_a_response_with_null_id() {
    let mut gpio = MockGpio::new();
    // gpio_read on an unconfigured pin -> error response, but the point is the id.
    let resp = call(
        br#"{"jsonrpc":"2.0","id":null,"method":"gpio_read","params":{"pin":1}}"#,
        &mut gpio,
    );
    assert_eq!(resp["id"], Value::Null, "explicit null id is echoed as null");
    assert!(resp.get("error").is_some());
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --lib -- missing_id_is_a_notification explicit_null_id_gets_a_response`
Expected: PASS — confirms `jsonrpc-lite` untagged parsing maps missing-id → `Notification` and explicit-null → `Request` with `Id::None`. If `explicit_null_id_*` instead returns `None` (the crate collapsed null into a notification), update the test to assert `result.is_none()` AND record this behavior deviation in the spec's risk section, then re-run.

- [ ] **Step 3: Commit**

```bash
git add src/dispatch.rs
git commit -m "test: pin down notification vs explicit-null-id behavior"
```

---

## Task 5: Full verification

**Files:** none (verification only)

- [ ] **Step 1: Run the entire test suite**

Run: `cargo test`
Expected: PASS — all unit + integration tests green (the baseline behavioral tests plus the 3 new tests).

- [ ] **Step 2: Confirm no stale references to removed symbols remain**

Run: `grep -rn "Envelope\|RpcError\|RawEnvelope\|\.to_json()\|PARSE_ERROR\|METHOD_NOT_FOUND\|INVALID_PARAMS\b\|parse_request" src/`
Expected: No matches (the only `SERVER_ERROR` reference, if any, is the new `pub const` in `dispatch.rs`).

- [ ] **Step 3: Lint**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings.

- [ ] **Step 4: Final commit if anything was adjusted**

```bash
git add -A
git commit -m "chore: jsonrpc-lite migration verification fixups" || echo "nothing to commit"
```

---

## Acceptance criteria (from spec)
- `jsonrpc-lite` is the only envelope/serialization mechanism; no `Response`/`RpcError`/`Envelope`/`RawEnvelope`/`to_json` remain.
- Error-code table satisfied: parse=-32700/null id, notification=no response, unknown method=-32601, bad params on known method=-32602, pin out of range=-32602, backend=-32000, success=`result`.
- `cargo test` and `cargo build` green for the host target; `clippy` clean.
- `server.rs`, the `GpioBackend`/`LedBackend` traits, and the cfg-gated `EspGpio` are unchanged.
