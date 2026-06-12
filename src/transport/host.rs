//! stdin/stdout transport for host-target builds, CI, and PTY testing.
//!
//! Wraps the process's stdin/stdout as an [`embedded_io`] byte stream so the
//! same [`crate::server::Framer`] that drives the device also drives the host
//! build. `embedded-io`'s `std` feature does not bridge `std::io::Error` to
//! `embedded_io::Error`, so we wrap it in [`IoError`] here.
//!
//! `write` flushes immediately: NDJSON responses must reach the host as soon as
//! they're produced (a buffered stdout would make a PTY client time out).

use std::io::{self, Read as _, Write as _};

use embedded_io::{ErrorKind, ErrorType};

/// `embedded_io::Error` wrapper around [`std::io::Error`].
#[derive(Debug)]
pub struct IoError(pub io::Error);

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for IoError {}

impl embedded_io::Error for IoError {
    fn kind(&self) -> ErrorKind {
        // embedded-io maps most std ErrorKinds, but its `From` goes the other
        // way; `Other` is a safe, lossless-enough classification here.
        ErrorKind::Other
    }
}

/// stdin (read) + stdout (write) as one byte-stream transport.
pub struct HostTransport {
    stdin: io::Stdin,
    stdout: io::Stdout,
}

impl HostTransport {
    pub fn new() -> Self {
        Self {
            stdin: io::stdin(),
            stdout: io::stdout(),
        }
    }
}

impl Default for HostTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorType for HostTransport {
    type Error = IoError;
}

impl embedded_io::Read for HostTransport {
    /// Returns `Ok(0)` on EOF (stdin closed), which the host loop treats as
    /// "stop".
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.stdin.read(buf).map_err(IoError)
    }
}

impl embedded_io::Write for HostTransport {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.stdout.write_all(buf).map_err(IoError)?;
        // Flush per write so each response line is delivered immediately.
        self.stdout.flush().map_err(IoError)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.stdout.flush().map_err(IoError)
    }
}
