# jsonrpc-gpio

A strictly synchronous, single-in-flight **JSON-RPC 2.0 GPIO control server** in
Rust for the ESP32-S3 (ESP-IDF / `std`), with a fully host-testable core.

One request → one response, newline-delimited JSON (NDJSON) over a byte stream.
No task spawning, no async runtime, no pending-id table — the host's own
request/readline cadence is the only flow control.

## Architecture

The JSON-RPC core never touches USB/UART/stdio directly. It is generic over
[`embedded_io::Read`] + [`embedded_io::Write`], so the same framing + dispatch
logic runs over stdio on the host and over the S3 USB Serial/JTAG controller on
the device. `Vec<u8>`/`&[u8]` implement those traits, so the core unit-tests on
the host with **no mock crates**.

```
                         ┌─────────────────────────────────────┐
  bytes  ──read chunks──▶│ server::Framer  (NDJSON line framing,│
                         │   bounded buffer, overflow discipline)│
                         └───────────────┬─────────────────────┘
                                         │ one complete line
                                         ▼
                         ┌─────────────────────────────────────┐
                         │ dispatch::process_line              │
                         │   parse → GpioBackend → Response     │
                         └───────────────┬─────────────────────┘
                                         │ response + '\n'
  bytes  ◀──write_all────────────────────┘
```

| Module | Role |
|--------|------|
| `protocol` | serde envelope/request/response types, JSON-RPC 2.0 error codes |
| `server`   | `Framer`: transport-agnostic NDJSON framing + overflow handling |
| `dispatch` | `process_line`, the `GpioBackend` trait, host `MockGpio` |
| `transport::host` | stdin/stdout as an `embedded_io` stream (host builds, CI, PTY) |
| `transport::s3`   | ESP32-S3 USB Serial/JTAG (**deferred** stub, device-only) |

The host build is the native host target (no ESP-IDF). Device code is gated by
`#[cfg(target_os = "espidf")]`, and the `esp-idf-*` crates are pulled in only
for that target — there are no `host`/`s3` Cargo features to juggle.

## Build & test

```bash
# Host: unit + end-to-end tests on the native target (aarch64-apple-darwin).
cargo test-host

# Manual: pipe NDJSON through the host binary.
printf '{"jsonrpc":"2.0","id":1,"method":"gpio_read","params":{"pin":2}}\n' \
  | cargo run -q

# Device firmware (ESP32-S3) — see "S3 milestone" below.
cargo build-s3
```

## Protocol

Methods (params shown):

```jsonc
{"jsonrpc":"2.0","id":1,"method":"gpio_config","params":{"pin":2,"mode":"output"}}
{"jsonrpc":"2.0","id":2,"method":"gpio_write","params":{"pin":2,"level":1}}
{"jsonrpc":"2.0","id":3,"method":"gpio_read","params":{"pin":2}}
```

`mode` ∈ `input | output | input_pullup`. `id` is `number | string | null`.

Error codes: `-32700` parse error, `-32601` method not found, `-32602` invalid
params (e.g. pin out of range), `-32000` server/GPIO error.

## Validation client

`host_client.py` exercises the full RPC surface with incrementing ids, asserted
id matching, and a read timeout:

```bash
# Host build over a real PTY (stdlib only, no pyserial needed):
python3 host_client.py --spawn ./target/aarch64-apple-darwin/debug/jsonrpc-gpio

# Real ESP32-S3 hardware (needs pyserial):
python3 host_client.py --port /dev/tty.usbmodemXXXX --baud 115200
```

## S3 milestone (deferred)

The ESP32-S3 USB Serial/JTAG transport (`transport::s3`) is a documented stub.
It is the start of a separate hardware milestone — see the module docs in
`src/transport/s3.rs` for the required finite read tick / finite write timeout
and the host-disconnect tests that must come first.

The full crate (including this stub and the ESP-IDF deps) **cross-compiles for
the ESP32-S3** — `cargo build-s3` produces a Xtensa firmware ELF. The stub's
methods are `todo!()`, so flashing it will panic at the transport step until
the milestone implements them.

[`embedded_io::Read`]: https://docs.rs/embedded-io
[`embedded_io::Write`]: https://docs.rs/embedded-io
