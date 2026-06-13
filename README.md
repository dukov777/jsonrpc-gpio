# jsonrpc-gpio

[![host-ci](https://github.com/dukov777/jsonrpc-gpio/actions/workflows/host-ci.yml/badge.svg)](https://github.com/dukov777/jsonrpc-gpio/actions/workflows/host-ci.yml)
[![resilience: fault-injection passing](https://img.shields.io/badge/resilience-fault--injection%20passing-brightgreen)](BENCHMARKS.md#fault-injection-resilience)

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
# Needs no ESP-IDF / esp toolchain env.
cargo test-host

# Manual: pipe NDJSON through the host binary.
printf '{"jsonrpc":"2.0","id":1,"method":"gpio_read","params":{"pin":2}}\n' \
  | cargo run -q

# Device firmware (ESP32-S3). First source the esp toolchain env that espup
# installs (provides LIBCLANG_PATH for bindgen + the toolchain in PATH); the
# ESP-IDF itself is resolved from ESP_IDF_VERSION in .cargo/config.toml.
. ~/export-esp.sh
cargo build-s3            # debug
cargo build-s3 --release  # smaller image — see BENCHMARKS.md
```

Prerequisites for device builds: [`espup`](https://github.com/esp-rs/espup)
(`espup install`, which creates `~/export-esp.sh`) and
[`ldproxy`](https://github.com/esp-rs/embuild) (`cargo install ldproxy`).

See [BENCHMARKS.md](BENCHMARKS.md) for roundtrip latency, flash/RAM footprint,
and a pure-C size comparison.

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

# Drive / read GPIO pins (config -> write -> read; device retains config):
python3 host_client.py --port /dev/cu.usbmodemXXXX --config 45,output
python3 host_client.py --port /dev/cu.usbmodemXXXX --write 45,1
python3 host_client.py --port /dev/cu.usbmodemXXXX --read 45      # -> gpio_read(pin=45) -> 1
# (`mode` = input | output | input_pullup; reads report the real electrical
#  state — note GPIO45 is a strapping pin with an external pull-down on this board.)

# Control the on-board LED directly:
python3 host_client.py --port /dev/cu.usbmodemXXXX --led 0,16,0   # green
python3 host_client.py --port /dev/cu.usbmodemXXXX --led 0,0,0    # off

# Blink (client-side: toggles led_set on/off; Ctrl-C or --count to stop):
python3 host_client.py --port /dev/cu.usbmodemXXXX --blink 0,0,32           # blink blue until Ctrl-C
python3 host_client.py --port /dev/cu.usbmodemXXXX --blink 0,16,0 --count 5 --period 0.25

# Rainbow (client-side hue sweep; --duration s/cycle, --count cycles, --brightness 0-255):
python3 host_client.py --port /dev/cu.usbmodemXXXX --rainbow                       # forever, 5s/cycle
python3 host_client.py --port /dev/cu.usbmodemXXXX --rainbow --duration 5 --count 3
```

## Roundtrip latency benchmark

End-to-end RPC roundtrip (send request → matching response) measured two ways
against a real ESP32-S3 over USB Serial/JTAG:

```bash
# Python client benchmark:
python3 host_client.py --port /dev/cu.usbmodemXXXX --bench 1000

# Same benchmark in Rust (host target; serialport is a host-only dev-dep):
cargo run --release --example serial_bench --target aarch64-apple-darwin -- \
    /dev/cu.usbmodemXXXX 1000
```

Both report ~**3.7 ms median, ~268 req/s** — essentially identical. The roundtrip
is **transport-bound** (USB-CDC framing dominates), so the client language is
irrelevant and the device's JSON parse/dispatch/GPIO work is negligible inside
that floor.

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

### Device logs (console)

The console (`log::info!`, panics, the bootloader banner) is routed to **UART0**
(`CONFIG_ESP_CONSOLE_UART_DEFAULT`, GPIO43 TX / GPIO44 RX), *not* the JTAG port.
A serial port is exclusive, so this keeps the two streams on separate cables: the
JSON-RPC NDJSON runs over the USB Serial/JTAG port, and logs come out the external
USB‑UART bridge — they can't fight over the port, and logging can't corrupt the
RPC framing. With both cables attached you can monitor and drive the device at the
same time:

```bash
# Terminal 1 — live device logs over the USB-UART bridge (Ctrl-] to exit):
espflash monitor --port /dev/cu.wchusbserialXXXX --chip esp32s3

# Terminal 2 — RPC over USB Serial/JTAG, concurrently, no port conflict:
python3 host_client.py --port /dev/cu.usbmodemXXXX --read 45
```

Boot prints `cpu_start: GPIO 44 and 43 are used as console UART I/O pins`,
followed by the app's own `jsonrpc_gpio::esp: ...` lines.

**Fault-injection tests** (hardware-in-the-loop, can't run in host CI) —
`s3_fault_tests.py`, all passing on a real board:

```bash
python3 s3_fault_tests.py --port /dev/cu.usbmodemXXXX \
    --elf target/xtensa-esp32s3-espidf/debug/jsonrpc-gpio
```

1. boot with no host attached, then attach → serves (no startup write-wedge);
2. host disconnects mid-session under load → writes time out + drop, serves
   after reconnect (never wedges);
3. task watchdog stays fed under a long idle hold → no reset.

## License

MIT — see [LICENSE](LICENSE).

[`embedded_io::Read`]: https://docs.rs/embedded-io
[`embedded_io::Write`]: https://docs.rs/embedded-io
