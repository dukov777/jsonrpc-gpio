//! NDJSON line framing for the JSON-RPC server.
//!
//! [`Framer`] is the transport-agnostic, fully host-testable core: feed it
//! byte chunks as they arrive from any [`embedded_io::Read`], and for every
//! complete `\n`-terminated line it invokes a dispatch callback and writes the
//! response (plus a trailing `\n`) to any [`embedded_io::Write`].
//!
//! The blocking read loop itself lives in the entry points (`main.rs`), because
//! the meaning of a zero-length read differs by transport: on the host a `0`
//! from stdin is EOF (stop), while on the device a `0` from a finite read tick
//! is "no data yet" (continue). The framing — the part with real logic — is
//! here and tested in isolation.
//!
//! **Overflow discipline:** a line longer than the buffer capacity `N` is not
//! truncated-and-parsed (which could mis-frame the tail as a fresh request).
//! Instead the framer enters a `skipping` state, discards bytes until the next
//! `\n`, and only then resumes. This is a correctness property, not an
//! optimization.

use embedded_io::Write;

/// Default line-buffer capacity. GPIO request lines are tens of bytes; 256
/// leaves generous headroom while bounding on-device memory.
pub const LINE_CAP: usize = 256;

/// Assembles `\n`-terminated lines from arbitrary byte chunks.
///
/// `N` is the maximum line length in bytes; longer lines are dropped per the
/// overflow discipline described in the module docs.
pub struct Framer<const N: usize = LINE_CAP> {
    buf: heapless::Vec<u8, N>,
    /// True while discarding the tail of an oversized line until the next `\n`.
    skipping: bool,
}

impl<const N: usize> Default for Framer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Framer<N> {
    pub fn new() -> Self {
        Self {
            buf: heapless::Vec::new(),
            skipping: false,
        }
    }

    /// Feed one chunk of freshly-read bytes. For each complete line, calls
    /// `dispatch(line)` and writes `dispatch`'s response followed by `\n` to
    /// `out`. A write error is logged and the response dropped — the loop keeps
    /// running so a closed/unopened host port can't wedge the server.
    pub fn push<W, F>(&mut self, chunk: &[u8], out: &mut W, dispatch: &mut F)
    where
        W: Write,
        F: FnMut(&[u8]) -> String,
    {
        for &byte in chunk {
            if byte == b'\n' {
                self.end_line(out, dispatch);
            } else if !self.skipping && self.buf.push(byte).is_err() {
                // Buffer full and more non-newline bytes arriving: this line is
                // oversized. Discard what we have and ignore the rest until the
                // next newline so the tail can't be mis-parsed as a new frame.
                self.buf.clear();
                self.skipping = true;
            }
        }
    }

    /// Handle a completed line (a `\n` was seen): dispatch it and write the
    /// response, then reset for the next line.
    fn end_line<W, F>(&mut self, out: &mut W, dispatch: &mut F)
    where
        W: Write,
        F: FnMut(&[u8]) -> String,
    {
        if self.skipping {
            // We were discarding an oversized line; this newline ends it.
            self.skipping = false;
            self.buf.clear();
            return;
        }

        // Tolerate CRLF terminators by dropping a trailing '\r'.
        let line = match self.buf.last() {
            Some(b'\r') => &self.buf[..self.buf.len() - 1],
            _ => &self.buf[..],
        };

        if !line.is_empty() {
            let response = dispatch(line);
            // One write so a finite-timeout transport sees a single frame; a
            // write error means the host port is closed — drop and keep going.
            let mut framed = response.into_bytes();
            framed.push(b'\n');
            if let Err(e) = out.write_all(&framed) {
                log::warn!("dropping response, transport write failed: {e:?}");
            }
        }

        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ChunkedReader, FailingWriter};
    use embedded_io::Read;
    use std::cell::Cell;

    /// A dispatch that echoes the line back verbatim and counts invocations.
    fn echo_counting(calls: &Cell<usize>) -> impl FnMut(&[u8]) -> String + '_ {
        move |line: &[u8]| {
            calls.set(calls.get() + 1);
            String::from_utf8_lossy(line).into_owned()
        }
    }

    #[test]
    fn dispatches_a_single_complete_frame() {
        let calls = Cell::new(0);
        let mut out: Vec<u8> = Vec::new();
        let mut f = Framer::<LINE_CAP>::new();

        f.push(b"hello\n", &mut out, &mut echo_counting(&calls));

        assert_eq!(out, b"hello\n");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn dispatches_two_frames_coalesced_in_one_chunk() {
        let calls = Cell::new(0);
        let mut out: Vec<u8> = Vec::new();
        let mut f = Framer::<LINE_CAP>::new();

        f.push(b"a\nb\n", &mut out, &mut echo_counting(&calls));

        assert_eq!(out, b"a\nb\n");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn assembles_a_frame_split_across_pushes() {
        let calls = Cell::new(0);
        let mut out: Vec<u8> = Vec::new();
        let mut f = Framer::<LINE_CAP>::new();

        f.push(b"ab", &mut out, &mut echo_counting(&calls));
        assert_eq!(calls.get(), 0, "no newline yet -> no dispatch");

        f.push(b"c\n", &mut out, &mut echo_counting(&calls));
        assert_eq!(out, b"abc\n");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn oversized_frame_is_dropped_and_following_frame_still_works() {
        let calls = Cell::new(0);
        let mut out: Vec<u8> = Vec::new();
        // Capacity 4: "abcdefgh" overflows, "ok" fits.
        let mut f = Framer::<4>::new();

        f.push(b"abcdefgh\nok\n", &mut out, &mut echo_counting(&calls));

        assert_eq!(out, b"ok\n", "only the valid frame produces a response");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn write_failure_drops_response_and_keeps_processing() {
        let calls = Cell::new(0);
        // Every write fails (closed host port). Framer must not panic and must
        // still dispatch the second frame.
        let mut out = FailingWriter::new(0);
        let mut f = Framer::<LINE_CAP>::new();

        f.push(b"a\nb\n", &mut out, &mut echo_counting(&calls));

        assert_eq!(calls.get(), 2, "kept looping past the write failure");
        assert!(out.written.is_empty(), "nothing drained to a closed port");
    }

    #[test]
    fn blank_lines_are_ignored() {
        let calls = Cell::new(0);
        let mut out: Vec<u8> = Vec::new();
        let mut f = Framer::<LINE_CAP>::new();

        f.push(b"\n\nx\n", &mut out, &mut echo_counting(&calls));

        assert_eq!(out, b"x\n");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn reassembles_two_frames_read_one_byte_at_a_time() {
        // Maximal fragmentation: a transport that delivers a single byte per
        // read (the worst case for framing). Two frames must still surface.
        let calls = Cell::new(0);
        let mut reader = ChunkedReader::new(b"first\nsecond\n", 1);
        let mut out: Vec<u8> = Vec::new();
        let mut f = Framer::<LINE_CAP>::new();
        let mut dispatch = echo_counting(&calls);

        let mut chunk = [0u8; 8];
        loop {
            let n = reader.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            f.push(&chunk[..n], &mut out, &mut dispatch);
        }

        assert_eq!(out, b"first\nsecond\n");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn trailing_carriage_return_is_trimmed() {
        let calls = Cell::new(0);
        let mut out: Vec<u8> = Vec::new();
        let mut f = Framer::<LINE_CAP>::new();

        f.push(b"x\r\n", &mut out, &mut echo_counting(&calls));

        // CR stripped before dispatch; response itself is just "x\n".
        assert_eq!(out, b"x\n");
        assert_eq!(calls.get(), 1);
    }
}
