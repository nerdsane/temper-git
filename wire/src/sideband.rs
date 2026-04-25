//! side-band-64k framing for upload-pack / receive-pack responses.
//!
//! Once the client and server negotiate `side-band-64k`, every byte
//! of the pack stream (and any progress / error messages) is wrapped
//! in pkt-line frames whose payload is prefixed with a single
//! channel byte:
//!
//! ```text
//!   <4-hex length><channel><payload>
//!     channel 0x01: pack data
//!     channel 0x02: progress (textual, shown by the client)
//!     channel 0x03: error  (textual, fatal)
//! ```
//!
//! `length` is the 4-hex-ASCII pkt-line size including itself, so
//! payload is at most `0xfff0 - 4 - 1 = 65515` bytes per frame.
//!
//! `SidebandWriter` lets a producer (e.g. the pack emitter) write
//! arbitrary chunks; the writer buffers up to one frame and flushes
//! complete frames as they fill.

use std::io::{self, Write};

/// Channel 1 — pack bytes.
pub const CHANNEL_PACK: u8 = 0x01;
/// Channel 2 — progress text (informational).
pub const CHANNEL_PROGRESS: u8 = 0x02;
/// Channel 3 — fatal error text.
pub const CHANNEL_ERROR: u8 = 0x03;

/// Maximum payload bytes per sideband frame. Equal to the pkt-line
/// envelope cap (`0xfff0`) minus the 4-hex length and 1-byte channel.
pub const MAX_FRAME_PAYLOAD: usize = 0xfff0 - 4 - 1;

/// Writes its input as a stream of `0x01` (pack-data) sideband
/// frames to the underlying writer. Buffers one in-progress frame;
/// flushes it on overflow or `finish`.
///
/// Every call to `write` accepts the entire buffer (split across
/// frames if needed) and returns `Ok(buf.len())`.
pub struct SidebandWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> SidebandWriter<W> {
    /// Wrap `inner`. Frames will be channel-1 (pack data).
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(MAX_FRAME_PAYLOAD),
        }
    }

    /// Flush any pending frame and return the underlying writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.flush_pending()?;
        Ok(self.inner)
    }

    /// Send a progress message on channel 2 immediately. Flushes the
    /// pending pack frame first so ordering is preserved.
    pub fn write_progress(&mut self, msg: &str) -> io::Result<()> {
        self.flush_pending()?;
        self.write_frame(CHANNEL_PROGRESS, msg.as_bytes())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let payload = std::mem::take(&mut self.buf);
        self.write_frame(CHANNEL_PACK, &payload)?;
        Ok(())
    }

    fn write_frame(&mut self, channel: u8, payload: &[u8]) -> io::Result<()> {
        debug_assert!(payload.len() <= MAX_FRAME_PAYLOAD);
        let total = 4 + 1 + payload.len();
        let prefix = format!("{:04x}", total);
        self.inner.write_all(prefix.as_bytes())?;
        self.inner.write_all(&[channel])?;
        self.inner.write_all(payload)?;
        Ok(())
    }
}

impl<W: Write> Write for SidebandWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < buf.len() {
            let space = MAX_FRAME_PAYLOAD - self.buf.len();
            let take = (buf.len() - written).min(space);
            self.buf.extend_from_slice(&buf[written..written + take]);
            written += take;
            if self.buf.len() == MAX_FRAME_PAYLOAD {
                self.flush_pending()?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Don't flush partial frames on `flush` — frame fill is
        // size-driven; eager flushing would emit many tiny frames.
        // The caller `finish`es to flush the tail.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_frames(buf: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            assert!(i + 5 <= buf.len(), "truncated at {i}");
            let len_hex = std::str::from_utf8(&buf[i..i + 4]).unwrap();
            let total = usize::from_str_radix(len_hex, 16).unwrap();
            assert!(total >= 5);
            let channel = buf[i + 4];
            let payload = buf[i + 5..i + total].to_vec();
            out.push((channel, payload));
            i += total;
        }
        assert_eq!(i, buf.len());
        out
    }

    #[test]
    fn small_write_one_frame() {
        let mut out = Vec::new();
        let mut w = SidebandWriter::new(&mut out);
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();
        let frames = parse_frames(&out);
        assert_eq!(frames, vec![(CHANNEL_PACK, b"hi".to_vec())]);
    }

    #[test]
    fn writes_split_at_frame_boundary() {
        let mut out = Vec::new();
        let mut w = SidebandWriter::new(&mut out);
        let big = vec![0xAAu8; MAX_FRAME_PAYLOAD + 100];
        w.write_all(&big).unwrap();
        w.finish().unwrap();
        let frames = parse_frames(&out);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].0, CHANNEL_PACK);
        assert_eq!(frames[0].1.len(), MAX_FRAME_PAYLOAD);
        assert_eq!(frames[1].0, CHANNEL_PACK);
        assert_eq!(frames[1].1.len(), 100);
    }

    #[test]
    fn many_small_writes_coalesce() {
        let mut out = Vec::new();
        let mut w = SidebandWriter::new(&mut out);
        for _ in 0..1000 {
            w.write_all(b"x").unwrap();
        }
        w.finish().unwrap();
        let frames = parse_frames(&out);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].1.len(), 1000);
    }

    #[test]
    fn progress_flushes_pending_pack_frame() {
        let mut out = Vec::new();
        let mut w = SidebandWriter::new(&mut out);
        w.write_all(b"pack-bytes").unwrap();
        w.write_progress("counting").unwrap();
        w.write_all(b"more-pack").unwrap();
        w.finish().unwrap();
        let frames = parse_frames(&out);
        assert_eq!(
            frames,
            vec![
                (CHANNEL_PACK, b"pack-bytes".to_vec()),
                (CHANNEL_PROGRESS, b"counting".to_vec()),
                (CHANNEL_PACK, b"more-pack".to_vec()),
            ]
        );
    }

    #[test]
    fn empty_finish_emits_nothing() {
        let mut out = Vec::new();
        let w = SidebandWriter::new(&mut out);
        w.finish().unwrap();
        assert!(out.is_empty());
    }
}
