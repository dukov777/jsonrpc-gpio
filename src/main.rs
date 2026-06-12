//! JSON-RPC 2.0 GPIO control server — entry point.
//!
//! The host build (any non-ESP target) runs the server over stdin/stdout with
//! an in-memory mock GPIO backend, for fast iteration, CI, and PTY testing.
//! The device build (ESP-IDF / ESP32-S3) is wired in §7 over the USB
//! Serial/JTAG transport with real GPIO.

#[cfg(not(target_os = "espidf"))]
fn main() {
    host::run();
}

/// Host-target server loop: read stdin chunks, frame NDJSON, dispatch against a
/// mock GPIO backend, write responses to stdout. Exits on stdin EOF.
#[cfg(not(target_os = "espidf"))]
mod host {
    use embedded_io::Read;

    use jsonrpc_gpio::dispatch::{process_line, MockGpio};
    use jsonrpc_gpio::server::{Framer, LINE_CAP};
    use jsonrpc_gpio::transport::host::HostTransport;

    pub fn run() {
        let mut gpio = MockGpio::new();
        let mut transport = HostTransport::new();
        let mut framer = Framer::<LINE_CAP>::new();
        let mut dispatch = |line: &[u8]| process_line(line, &mut gpio);

        let mut chunk = [0u8; 64];
        loop {
            match transport.read(&mut chunk) {
                Ok(0) => break, // stdin closed -> EOF -> stop.
                Ok(n) => framer.push(&chunk[..n], &mut transport, &mut dispatch),
                Err(e) => {
                    eprintln!("read error, stopping: {e:?}");
                    break;
                }
            }
        }
    }
}

// Device entry point (ESP32-S3) — wired in §7 with the real USB Serial/JTAG
// transport and GPIO peripherals.
#[cfg(target_os = "espidf")]
fn main() {
    esp::run();
}

#[cfg(target_os = "espidf")]
mod esp {
    pub fn run() {
        esp_idf_svc::sys::link_patches();
        esp_idf_svc::log::EspLogger::initialize_default();
        // The S3 USB Serial/JTAG transport + real GPIO backend land in §7.
        todo!("S3 transport + GPIO wiring (§7, deferred milestone)");
    }
}
