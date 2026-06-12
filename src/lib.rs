//! Host-testable core for the JSON-RPC 2.0 GPIO control server.
//!
//! The logic lives in small modules so it compiles and unit-tests on the host
//! (no ESP-IDF). The device entry point in `main.rs` wires these into the real
//! USB Serial/JTAG transport and GPIO peripherals.
//!
//! - [`protocol`] — serde request/response envelope types (JSON-RPC 2.0).
//! - [`dispatch`] — maps a request line to a response, over a [`dispatch::GpioBackend`].
//! - [`server`] — transport-agnostic NDJSON framing over [`embedded_io`] streams.
//! - [`transport`] — concrete byte-stream transports (host stdio; S3 deferred).

pub mod dispatch;
pub mod protocol;
pub mod server;
pub mod transport;

/// On-board WS2812 RGB LED (GPIO48) driver — device-only demo, not part of the
/// JSON-RPC surface.
#[cfg(target_os = "espidf")]
pub mod rgb;

#[cfg(test)]
mod test_support;
