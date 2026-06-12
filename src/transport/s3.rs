//! ESP32-S3 USB Serial/JTAG transport — **DEFERRED stub** (plan §7).
//!
//! This is the starting point for the separate S3 hardware milestone. It is
//! only compiled when targeting the device (`#[cfg(target_os = "espidf")]` in
//! the parent module) and every method is `todo!()` until that milestone picks
//! it up. It deliberately does NOT touch the host build.
//!
//! # What to implement
//!
//! Wrap `esp_idf_hal::usb_serial::UsbSerialDriver` (the S3's built-in USB
//! Serial/JTAG controller on D-/D+ = GPIO19/GPIO20 — no external bridge chip)
//! and implement [`embedded_io::Read`] + [`embedded_io::Write`] over it so the
//! existing [`crate::server::Framer`] drives it unchanged.
//!
//! # Hard requirements (from the S3 USB Serial/JTAG quirks)
//!
//! - **Finite read tick.** `read` must return `Ok(0)` when no byte arrives
//!   within a short timeout (e.g. 10 ms), not block forever — the device loop
//!   treats `Ok(0)` as "no data yet, keep feeding the watchdog and continue".
//! - **Finite write timeout. Never infinite.** If the host has not opened the
//!   port (or has disconnected), a write must time out and the response is
//!   dropped — the classic S3 failure mode is the device blocking on write /
//!   hanging at startup until the host opens the serial port.
//! - Map a write timeout to an `embedded_io::Error` with
//!   `ErrorKind::TimedOut`; the framer already logs-and-drops on write error.
//!
//! # Tests to add for this milestone (hardware-in-the-loop)
//!
//! 1. Start the device with no host attached, then attach — the loop must not
//!    have wedged, and a write issued while unopened must time out + drop.
//! 2. Host disconnects mid-session — writes time out, never block forever.
//! 3. Confirm the task watchdog stays fed given the blocking loop (the finite
//!    read tick should already yield enough).

use embedded_io::{ErrorKind, ErrorType};

/// Byte-stream transport over the ESP32-S3 USB Serial/JTAG controller.
///
/// Stub: holds nothing yet. The milestone adds the `UsbSerialDriver` handle and
/// the read/write timeouts described in the module docs.
pub struct S3Transport {
    // milestone: driver: esp_idf_hal::usb_serial::UsbSerialDriver<'static>,
    // milestone: read_tick: core::time::Duration,
    // milestone: write_timeout: core::time::Duration,
    _private: (),
}

/// Transport error carrying an `embedded_io` error kind (e.g. `TimedOut` when a
/// write to an unopened/closed host port times out).
#[derive(Debug)]
pub struct S3Error {
    pub kind: ErrorKind,
}

impl core::fmt::Display for S3Error {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "S3 USB Serial/JTAG error: {:?}", self.kind)
    }
}

// Under embedded-io's `std` feature (on for the esp-idf std target),
// `embedded_io::Error` requires `std::error::Error` as a supertrait.
impl std::error::Error for S3Error {}

impl embedded_io::Error for S3Error {
    fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl ErrorType for S3Transport {
    type Error = S3Error;
}

impl embedded_io::Read for S3Transport {
    /// Read up to `buf.len()` bytes within a finite tick; `Ok(0)` on no-data.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let _ = buf;
        todo!("S3 milestone: read with a finite tick, Ok(0) on timeout")
    }
}

impl embedded_io::Write for S3Transport {
    /// Write within a finite timeout; on timeout return `ErrorKind::TimedOut`
    /// so the framer drops the response instead of hanging the loop.
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let _ = buf;
        todo!("S3 milestone: write with a finite timeout, never infinite")
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        todo!("S3 milestone: flush within a finite timeout")
    }
}
