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

// Device entry point (ESP32-S3): serve over the built-in USB Serial/JTAG
// controller with a real raw-FFI GPIO backend.
#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    esp::run()
}

#[cfg(target_os = "espidf")]
mod esp {
    use embedded_io::Read;

    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_hal::usb_serial::{config::Config as UsbConfig, UsbSerialDriver};

    use jsonrpc_gpio::dispatch::{process_line, EspGpio};
    use jsonrpc_gpio::rgb::Ws2812;
    use jsonrpc_gpio::server::{Framer, LINE_CAP};
    use jsonrpc_gpio::transport::s3::S3Transport;

    pub fn run() -> anyhow::Result<()> {
        esp_idf_svc::sys::link_patches();
        esp_idf_svc::log::EspLogger::initialize_default();
        log::info!("jsonrpc-gpio: starting on USB Serial/JTAG");

        let peripherals = Peripherals::take()?;

        // Demo: light the on-board WS2812 (GPIO48) dim green once, proving the
        // RMT path works. Not part of the JSON-RPC surface. (`rmt.channel0` is
        // the legacy RMT channel handle the demo driver uses.)
        #[allow(deprecated)]
        let mut led = Ws2812::new(peripherals.rmt.channel0, peripherals.pins.gpio48)?;
        led.set_rgb(0, 16, 0)?;
        log::info!("on-board WS2812 (GPIO48) set to green");

        // S3 USB Serial/JTAG: D- = GPIO19, D+ = GPIO20 (built-in, no bridge chip).
        let driver = UsbSerialDriver::new(
            peripherals.usb_serial,
            peripherals.pins.gpio19,
            peripherals.pins.gpio20,
            &UsbConfig::new(),
        )?;

        let mut transport = S3Transport::new(driver);
        let mut gpio = EspGpio::new();
        let mut framer = Framer::<LINE_CAP>::new();
        let mut dispatch = |line: &[u8]| process_line(line, &mut gpio);

        let mut chunk = [0u8; 64];
        loop {
            match transport.read(&mut chunk) {
                // Finite read tick elapsed with no data: keep looping (feeds the
                // task watchdog). On-device this is "no data yet", not EOF.
                Ok(0) => continue,
                Ok(n) => framer.push(&chunk[..n], &mut transport, &mut dispatch),
                Err(e) => log::warn!("transport read error (continuing): {e:?}"),
            }
        }
    }
}
