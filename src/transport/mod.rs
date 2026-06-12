//! Concrete byte-stream transports.
//!
//! The server core is generic over [`embedded_io::Read`] + [`embedded_io::Write`],
//! so a "transport" here is just a type implementing those traits:
//!
//! - [`host`] — stdin/stdout, used for host-target builds, CI, and PTY testing.
//! - `s3` — the ESP32-S3 USB Serial/JTAG controller (deferred, §7); only
//!   compiled when targeting the device.

pub mod host;

#[cfg(target_os = "espidf")]
pub mod s3;
