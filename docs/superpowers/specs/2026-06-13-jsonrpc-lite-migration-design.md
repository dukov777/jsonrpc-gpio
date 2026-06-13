# Migrate the JSON-RPC envelope to `jsonrpc-lite`

**Date:** 2026-06-13
**Status:** Approved, pending implementation

## Goals
- Retire hand-rolled JSON-RPC 2.0 envelope code (`Envelope`, `Response`, `RpcError`,
  `RawEnvelope`, error-code constants, `to_json`).
- Reach genuine JSON-RPC 2.0 correctness — in particular, fix the current
  `method-not-found` vs `invalid-params` conflation.

## Non-goals
- Batch requests (JSON-RPC array form).
- Transport, framing, or async changes — `server.rs` (NDJSON `Framer`) is untouched;
  it only traffics in `Option<String>`.
- New GPIO/LED methods. The `GpioBackend` / `LedBackend` traits and the `MockGpio` /
  `EspGpio` / `MockLed` backends are unchanged.

## Dependency
- Add `jsonrpc-lite = "0.7"` (latest is v0.7.0). Its only dependencies are
  `serde` / `serde_json` / `serde_derive`, all already in the tree, so it adds no new
  transitive weight. Builds on `std`, which esp-idf provides on-device.

## Design — what changes, module by module

### `Cargo.toml`
Add `jsonrpc-lite = "0.7"` to `[dependencies]`.

### `protocol.rs`
- **Delete:** `Envelope`, `Response`, `RpcError`, `RawEnvelope`, `to_json`, and the
  `PARSE_ERROR` / `METHOD_NOT_FOUND` / `INVALID_PARAMS` / `SERVER_ERROR` constants.
  `jsonrpc_lite::Error` provides `parse_error()`, `method_not_found()`,
  `invalid_params()`, and arbitrary-code construction for the `-32000` backend error.
- **Keep:** `PinMode` and the strongly-typed, internally-tagged `Request` enum — this
  is the typed-dispatch core. Per-method params stay compile-time typed via
  `serde_json::from_value` into `Request`.
- The `-32000` backend code stays a named constant somewhere reachable by dispatch
  (e.g. `pub const SERVER_ERROR: i64 = -32000;`), since jsonrpc-lite has no built-in
  constructor for it.

### `dispatch.rs` — the heart of the change
Rewrite `process_line` to:
1. `serde_json::from_str::<JsonRpc>(line)` → on error, respond with
   `Error::parse_error()` and `Id::None` (serializes id `null`).
2. Match the parsed `JsonRpc`:
   - `Notification` → return `None` (no response).
   - `Request { method, params, id }` → continue.
   - Inbound `Success` / `Error` → treat as an invalid request (respond with
     `invalid_request` / parse-error-style error, id echoed if present).
3. **Classify the method name first** against the known set
   (`gpio_config`, `gpio_write`, `gpio_read`, `led_set`). Unknown ⇒ `method_not_found`,
   echoing the id.
4. Known method ⇒ deserialize `params` into the typed `Request` enum via
   `serde_json::from_value`. Failure ⇒ `invalid_params`, echoing the id.
   **This is the spec fix:** bad params on a known method now return `-32602`, not
   `-32601`.
5. Dispatch the typed `Request` exactly as today. Map `GpioError`:
   `InvalidPin → invalid_params`, `Backend(msg) → Error { code: -32000, message: msg }`.
6. Serialize the `JsonRpc::Success` / `JsonRpc::Error` response to a single-line
   string (no trailing newline — `server.rs` appends `\n`).

The cleanest way to do steps 3–4 without duplicating the method list is a small
unit-only `MethodName` enum (or an explicit `match method { "gpio_write" => ... }`)
to classify known vs unknown before deserializing params. The implementer chooses;
the requirement is that the two error codes are distinguished correctly.

### `server.rs`
Unchanged.

## Error-code mapping (target behavior)
| Situation | Code | id |
|---|---|---|
| Malformed JSON | `-32700` parse error | `null` |
| Missing `id` (notification) | — (no response) | — |
| Unknown method | `-32601` method not found | echoed |
| Known method, bad/missing params | `-32602` invalid params | echoed |
| Pin out of range | `-32602` invalid params | echoed |
| Backend/hardware failure | `-32000` server error | echoed |
| Success | `result` member | echoed |

## Risks / edge cases the implementer MUST handle
1. **`Id` fidelity.** `jsonrpc_lite::Id` is `Num(i64) | Str(String) | None(())`. Verify:
   - A request with a **missing** `id` is parsed as `Notification` ⇒ no response.
   - A request with an explicit `"id": null` still produces a response with id `null`.
   - Numeric and string ids round-trip (`1` and `"req-42"`).
   If jsonrpc-lite collapses explicit-null and missing into the same `Id::None`, document
   the resulting behavior and ensure it still matches the spec table above (notification =
   no `id` member at all). Add a dedicated test for the missing-vs-null distinction.
2. **Exact-string test assertions become structural.** Current tests assert byte-exact
   JSON (e.g. `{"jsonrpc":"2.0","result":{"level":1},"id":1}`). jsonrpc-lite may order
   members differently. Convert those to `serde_json::Value` structural comparisons; keep
   the behavioral assertions (codes, ids, presence/absence of `result`/`error`).

## Testing
- All 29 `protocol`/`dispatch` unit tests + 4 integration tests are the regression
  baseline (currently green). Preserve their behavioral intent.
- New tests:
  - Known method + bad params ⇒ `-32602` (was `-32601`).
  - Missing id ⇒ no response; explicit null id ⇒ response with null id.
- `cargo test` must be green on the host. Device build (`espidf` target) is not compiled
  in CI here but the `EspGpio` cfg-gated code must remain untouched and still reference
  the same trait surface.

## Acceptance criteria
- `jsonrpc-lite` is the only envelope/serialization mechanism; no hand-rolled
  `Response`/`RpcError`/`Envelope`/`RawEnvelope` remain.
- The error-code table above is fully satisfied, verified by tests.
- `cargo test` green; `cargo build` green for the host target.
- `server.rs` and the backend traits are unchanged.
