# Benchmarks & footprint

Measurements taken on an **ESP32-S3** (WeAct ESP32-S3-B, rev v0.2, 16 MB flash),
ESP-IDF v5.5.2, talking over the built-in **USB Serial/JTAG** at `/dev/cu.usbmodem101`.
Host: Apple Silicon macOS (`aarch64-apple-darwin`).

**Cable architecture.** Each USB connector has one job: the **native USB Serial/JTAG**
CDC (GPIO19/20) carries RPC only, while the **USB-UART bridge** (UART0, GPIO43/44)
carries the console *and* is the flashing port. The USB Serial/JTAG secondary
console is disabled (`CONFIG_ESP_CONSOLE_SECONDARY_NONE=y`), so log traffic never
shares the pipe with — or perturbs — the RPC latency measured below; the native
port reads 0 idle bytes. Driving RPC over UART0 instead is transport-bound by the
baud rate (~16 ms at 115200), which is why the fast native USB-CDC carries RPC.

## RPC roundtrip latency

End-to-end roundtrip = time from sending a request to receiving the matching
response, measured client-side around the full path (client encode → USB-CDC out
→ device frame/parse/dispatch → USB-CDC in → client decode). 1000× `gpio_read`,
10 warm-up discarded.

Reproduce:

```bash
python3 host_client.py --port /dev/cu.usbmodem101 --bench 1000
cargo run --release --example serial_bench --target aarch64-apple-darwin -- /dev/cu.usbmodem101 1000
```

| Client | min | p50 | mean | p95 | p99 | max | req/s | stdev |
|--------|----:|----:|-----:|----:|----:|-----:|------:|------:|
| Python | 3.60 | 3.71 | 3.71 | 3.78 | 3.81 | 3.85 | 269 | 0.04 |
| Rust   | 3.63 | 3.71 | 3.73 | 3.80 | 3.90 | 10.44 | 268 | 0.32 |

(all latencies in ms)

**Finding:** Python and Rust land on the *same* number (~3.7 ms median, ~268
req/s). The roundtrip is **transport-bound** — the USB-CDC framing/poll interval
sets the floor, so the client language is irrelevant and the device's JSON
parse + dispatch + GPIO work is negligible inside it. The Python distribution is
tighter (stdev 0.04 ms); the Rust client showed a slightly fatter tail (a few
OS-scheduling outliers to ~10 ms) with the same center.

## Flash footprint

Flashable image (`espflash save-image`):

| Build | Image size |
|-------|-----------:|
| debug (`cargo build-s3`) | 713 KB |
| **release** (`cargo build-s3 --release`) | **499 KB** |

Per-component breakdown of the release image (from the linker map via
`esp-idf-size`):

| Component | Size | Share |
|-----------|-----:|------:|
| Rust `std` + `core` + `alloc` | 173.6 KB | 35% |
| ESP-IDF core (hw_support, system, spi_flash, heap, hal, soc, rom, vfs, log, timer, pthread…) | ~170 KB | 34% |
| Rust panic/backtrace (demangle, gimli, addr2line, miniz, object) | 37.4 KB | 7.6% |
| serde_json + serde | 33.2 KB | 6.7% |
| application (`jsonrpc_gpio`) | 30.7 KB | 6.2% |
| newlib + libc | 30.2 KB | 6.1% |
| FreeRTOS | 16.4 KB | 3.3% |
| legacy drivers incl. RMT/LED (`libdriver.a`) | 6.6 KB | 1.3% |
| USB-Serial-JTAG driver | 3.5 KB | 0.7% |
| GPIO driver | 3.5 KB | 0.7% |
| **Wi-Fi stack** | **0 KB** | — |
| **BLE/BT stack** | **0 KB** | — |

Wi-Fi/BT are linked-out by `--gc-sections` because the app never initializes
them. The two giants — Rust `std` (35%) and the ESP-IDF core runtime (34%) — are
the fixed cost of the `std`-on-ESP-IDF approach; the application itself is ~31 KB.

## RAM footprint

ESP32-S3 internal SRAM (~512 KB total, ~358 KB app-usable). Static usage from the
release link:

| | Size |
|---|-----:|
| `.data` + `.bss` (variables) | 14.3 KB |
| code placed in RAM for speed (DIRAM `.text` + IRAM) | 57.5 KB |
| RTC RAM | 56 B |
| **Total static RAM** | **~72 KB** |

~279 KB of internal SRAM remains free at link time for the heap + task stacks;
runtime free heap at boot is ~250–270 KB after FreeRTOS, driver buffers, and the
8 KB main-task stack. The application's own RAM is < 1 KB (the 256-byte framer
buffer + small driver buffers); RAM, like flash, is dominated by the framework.

## "What if it were pure C?"

Estimated from the breakdown above plus a real minimal ESP-IDF C `hello_world`
(~167 KB):

- **Flash: ~180–220 KB** (vs 499 KB Rust release), ~2–2.5× smaller. The savings
  are almost entirely Rust-specific: `std` (174 KB) + panic/backtrace (37 KB) +
  serde (33 KB) + Rust↔IDF glue (~20 KB), minus a small C JSON parser.
- **RAM: roughly the same** — dominated by FreeRTOS + IDF, identical in C.
- The real floor is **ESP-IDF itself (~210 KB)**, not the language. Going far
  below that means dropping ESP-IDF (bare-metal), which loses the driver stack.

## Fault-injection (resilience)

Hardware-in-the-loop tests of the USB Serial/JTAG transport's failure modes,
run on the connected ESP32-S3 via `s3_fault_tests.py` (cannot run in host CI —
they manipulate the serial port and reset the device). All passing:

| Scenario | Injected fault | Result |
|----------|----------------|--------|
| boot without host | reset + 6 s headless (`is_connected()==false`), then attach | **PASS** — boots without a blocking write, serves on attach |
| disconnect under load | 3× fire 10 requests then yank the port mid-write | **PASS** — writes time out + drop, serves after every reconnect, never wedges |
| watchdog idle | hold idle 20 s, scan for reset / `task_wdt` / panic markers | **PASS** — no reset, still responsive |

Reproduce:

```bash
python3 s3_fault_tests.py --port /dev/cu.usbmodem101 \
    --elf target/xtensa-esp32s3-espidf/debug/jsonrpc-gpio
```

These validate the transport design: the `is_connected()` write guard + finite
write timeout (drop a response rather than block forever) and the finite read
tick (yields each loop, keeping the task watchdog fed).
