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
{"jsonrpc":"2.0","id":4,"method":"led_set","params":{"r":0,"g":16,"b":0}}
```

`mode` ∈ `input | output | input_pullup`. `id` is `number | string | null`.
`led_set` drives the on-board WS2812 (GPIO48); `r/g/b` are 0–255, `0,0,0` = off.
On the host build it drives an in-memory mock LED (so it's testable); on the
device it drives the real WS2812 via RMT.

Error codes: `-32700` parse error, `-32601` method not found, `-32602` invalid
params (e.g. pin out of range), `-32000` server/GPIO error.

## Validation client

`host_client.py` exercises the full RPC surface with incrementing ids, asserted
id matching, and a read timeout:

```bash
# Host build over a real PTY (stdlib only, no pyserial needed):
python3 host_client.py --spawn ./target/aarch64-apple-darwin/debug/jsonrpc-gpio

# Real ESP32-S3 hardware (stdlib only):
python3 host_client.py --port /dev/cu.usbmodemXXXX --baud 115200

# Control the on-board LED directly:
python3 host_client.py --port /dev/cu.usbmodemXXXX --led 0,16,0   # green
python3 host_client.py --port /dev/cu.usbmodemXXXX --led 0,0,0    # off

# Blink (client-side: toggles led_set on/off; Ctrl-C or --count to stop):
python3 host_client.py --port /dev/cu.usbmodemXXXX --blink 0,0,32           # blink blue until Ctrl-C
python3 host_client.py --port /dev/cu.usbmodemXXXX --blink 0,16,0 --count 5 --period 0.25
```

## S3 device support

The ESP32-S3 firmware is implemented and **cross-compiles** (`cargo build-s3`
→ Xtensa ELF):

- `transport::s3::S3Transport` wraps `esp-idf-hal`'s `UsbSerialDriver` (USB
  Serial/JTAG on GPIO19/GPIO20) with **finite** read/write timeouts and an
  `is_connected()` guard — never the driver's default infinite block.
- `dispatch::EspGpio` is a raw `esp-idf-sys` backend (`gpio_set_direction` /
  `gpio_set_level` / `gpio_get_level`) driving any validated pin by runtime
  number, behind the same `GpioBackend` trait as the host mock.
- `main`'s device entry builds the driver + backend and runs the framer loop
  (finite read tick, `Ok(0)` → continue to feed the watchdog).
- `rgb::Ws2812` (demo, not in the RPC surface) lights the on-board WS2812 LED
  on GPIO48 dim green at boot via the RMT peripheral — proof the addressable-LED
  path works. A plain `gpio_write(48, …)` can't drive it (WS2812 needs the
  ~800 kHz one-wire protocol).

Flash + validate (validated on an ESP32-S3 rev v0.2):

```bash
espflash flash --port /dev/cu.usbmodemXXXX \
  target/xtensa-esp32s3-espidf/debug/jsonrpc-gpio
python3 host_client.py --port /dev/cu.usbmodemXXXX --timeout 8   # -> PASS
```

The full RPC surface round-trips on real hardware over USB Serial/JTAG
(`gpio_config`/`write`/`read`, with `output` pins reading back the driven
level).

**Still owed — explicit fault-injection tests** (require manual host
attach/detach, can't run in host CI). See `src/transport/s3.rs` module docs:
(1) boot with no host attached then attach, (2) host disconnects mid-session,
(3) watchdog stays fed. The `is_connected()` write guard + finite timeouts that
back these are implemented; the formal pass/fail tests are not yet scripted.

[`embedded_io::Read`]: https://docs.rs/embedded-io
[`embedded_io::Write`]: https://docs.rs/embedded-io
