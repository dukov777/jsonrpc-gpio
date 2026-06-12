//! Test-only helpers shared across module unit tests. Compiled only under
//! `#[cfg(test)]`, so nothing here ships in the library or the firmware.
//!
//! For most framer tests a plain `Vec<u8>` (an `embedded_io::Write`) and a
//! `&[u8]` (an `embedded_io::Read`) already work with no wrapper. These doubles
//! exist for the cases that need chunked reads or a failing/finite sink.

use std::collections::VecDeque;

use embedded_io::{Error, ErrorKind, ErrorType, Read, Write};

/// An [`embedded_io::Read`] that yields at most `chunk` bytes per `read` call,
/// returning `Ok(0)` once drained. Models a transport that delivers a frame
/// split across several reads (or coalesces several frames into one read).
pub struct ChunkedReader {
    data: VecDeque<u8>,
    chunk: usize,
}

impl ChunkedReader {
    /// Read up to `chunk` bytes per call.
    pub fn new(data: &[u8], chunk: usize) -> Self {
        Self {
            data: data.iter().copied().collect(),
            chunk: chunk.max(1),
        }
    }
}

impl ErrorType for ChunkedReader {
    type Error = core::convert::Infallible;
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = buf.len().min(self.chunk).min(self.data.len());
        for (i, b) in self.data.drain(..n).enumerate() {
            buf[i] = b;
        }
        Ok(n)
    }
}

/// An [`embedded_io::Write`] whose `write` fails after `ok_writes` successful
/// calls — models a transport whose host port closes mid-session so the write
/// times out. The framer must drop the response and keep looping, not hang.
pub struct FailingWriter {
    pub ok_writes: usize,
    pub written: Vec<u8>,
}

impl FailingWriter {
    pub fn new(ok_writes: usize) -> Self {
        Self {
            ok_writes,
            written: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct WriteTimeout;

impl std::fmt::Display for WriteTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "write timed out")
    }
}

impl std::error::Error for WriteTimeout {}

impl Error for WriteTimeout {
    fn kind(&self) -> ErrorKind {
        ErrorKind::TimedOut
    }
}

impl ErrorType for FailingWriter {
    type Error = WriteTimeout;
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if self.ok_writes == 0 {
            return Err(WriteTimeout);
        }
        self.ok_writes -= 1;
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
