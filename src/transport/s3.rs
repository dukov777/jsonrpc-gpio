//! ESP32-S3 USB Serial/JTAG transport (plan §7).
//!
//! Wraps `esp_idf_hal`'s [`UsbSerialDriver`] — the S3's built-in USB
//! Serial/JTAG controller on D-/D+ = GPIO19/GPIO20, no external bridge chip —
//! and exposes it as an [`embedded_io`] byte stream so the existing
//! [`crate::server::Framer`] drives it unchanged.
//!
//! `UsbSerialDriver`'s own `embedded_io` impl hardcodes an *infinite*
//! (`delay::BLOCK`) timeout on read and write. That is exactly the failure mode
//! to avoid on this peripheral: a write blocks forever when the host has not
//! opened the port. So this wrapper deliberately bypasses that impl and calls
//! the driver's inherent `read`/`write` with **finite** `TickType` timeouts:
//!
//! - `read` uses a short tick (`READ_TICK_MS`); the driver returns `Ok(0)` when
//!   no byte arrives in that window, which the device loop treats as "no data
//!   yet, continue" (and the short block keeps the task watchdog fed).
//! - `write` first checks [`UsbSerialDriver::is_connected`] and returns a
//!   `TimedOut` error if the host port is closed/unopened, then writes with a
//!   bounded timeout (`WRITE_TIMEOUT_MS`); a zero-progress write also maps to
//!   `TimedOut`. Either way the framer logs-and-drops the response instead of
//!   hanging the loop.
//!
//! # Hardware tests still owed (host-in-the-loop, can't run on host CI)
//!
//! 1. Boot with no host attached, then attach — loop must not have wedged, and
//!    a write while unopened must time out + drop.
//! 2. Host disconnects mid-session — writes time out, never block forever.
//! 3. Task watchdog stays fed given the blocking read tick.

use embedded_io::ErrorKind;
use esp_idf_hal::delay::TickType;
use esp_idf_hal::sys::TickType_t;
use esp_idf_hal::usb_serial::UsbSerialDriver;

/// Read tick: how long `read` blocks waiting for bytes before returning Ok(0).
const READ_TICK_MS: u64 = 10;
/// Write timeout: bounded so an unopened/closed host port drops the response.
const WRITE_TIMEOUT_MS: u64 = 200;

/// Byte-stream transport over the ESP32-S3 USB Serial/JTAG controller.
pub struct S3Transport<'d> {
    driver: UsbSerialDriver<'d>,
    read_tick: TickType_t,
    write_timeout: TickType_t,
}

impl<'d> S3Transport<'d> {
    /// Wrap an already-installed [`UsbSerialDriver`] (built in `main` from the
    /// `usb_serial` peripheral + GPIO19/GPIO20).
    pub fn new(driver: UsbSerialDriver<'d>) -> Self {
        Self {
            driver,
            read_tick: TickType::new_millis(READ_TICK_MS).ticks(),
            write_timeout: TickType::new_millis(WRITE_TIMEOUT_MS).ticks(),
        }
    }
}

/// Transport error carrying an `embedded_io` error kind (`TimedOut` when a write
/// to an unopened/closed host port can't drain).
#[derive(Debug)]
pub struct S3Error {
    pub kind: ErrorKind,
}

impl S3Error {
    fn timed_out() -> Self {
        Self {
            kind: ErrorKind::TimedOut,
        }
    }

    fn other() -> Self {
        Self {
            kind: ErrorKind::Other,
        }
    }
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

impl embedded_io::ErrorType for S3Transport<'_> {
    type Error = S3Error;
}

impl embedded_io::Read for S3Transport<'_> {
    /// Read up to `buf.len()` bytes within a finite tick; `Ok(0)` on no-data.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.driver
            .read(buf, self.read_tick)
            .map_err(|_| S3Error::other())
    }
}

impl embedded_io::Write for S3Transport<'_> {
    /// Write within a finite timeout. Returns `TimedOut` (so the framer drops
    /// the response) when the host port is closed or nothing could be written.
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.driver.is_connected() {
            return Err(S3Error::timed_out());
        }
        let n = self
            .driver
            .write(buf, self.write_timeout)
            .map_err(|_| S3Error::other())?;
        if n == 0 {
            Err(S3Error::timed_out())
        } else {
            Ok(n)
        }
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // usb_serial_jtag write drains to the host buffer directly; there is no
        // separate finite-timeout flush to perform here.
        Ok(())
    }
}
