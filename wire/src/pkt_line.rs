//! Pkt-line framing per gitprotocol-pack(5).
//!
//! Each data pkt-line is a 4-char lowercase-hex length (inclusive of
//! the 4 length chars) followed by that many bytes of payload. A
//! flush packet is the literal 4 bytes `0000`. A delim packet is
//! `0001`; an end-of-response packet is `0002` (both v2 only — we
//! emit them lazily when a caller asks, not here).
//!
//! Max payload size per pkt-line is 65520 bytes (payload limit =
//! LINE_MAX - 4 = 65524 - 4). Git rejects pkt-lines larger than
//! that. Callers that need to emit more should chunk.

use std::fmt;

/// Maximum payload bytes allowed per pkt-line. Git's LINE_MAX = 65524
/// is the on-wire cap INCLUDING the 4-char length prefix, so the
/// payload limit is 4 bytes less.
pub const MAX_PAYLOAD: usize = 65520;

/// On-wire flush packet. `0000` literally.
pub const FLUSH: &[u8; 4] = b"0000";

/// Errors from pkt-line construction. Parsing errors are not yet
/// used — they land when we build the upload-pack negotiator.
#[derive(Debug, PartialEq, Eq)]
pub enum PktLineError {
    /// Payload exceeds [`MAX_PAYLOAD`].
    PayloadTooLarge(usize),
}

impl fmt::Display for PktLineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PktLineError::PayloadTooLarge(n) => {
                write!(f, "pkt-line payload {n} bytes exceeds MAX_PAYLOAD {MAX_PAYLOAD}")
            }
        }
    }
}

impl std::error::Error for PktLineError {}

/// Encode `payload` into a new pkt-line on the wire.
///
/// Caller-supplied payload is copied verbatim — we do not append a
/// newline or do any framing transformation. Callers that want a
/// trailing newline (as upload-pack advertisements conventionally
/// do) must include it themselves.
pub fn encode(payload: &[u8]) -> Result<Vec<u8>, PktLineError> {
    let mut out = Vec::with_capacity(4 + payload.len());
    encode_into(&mut out, payload)?;
    Ok(out)
}

/// Encode `payload` into an existing buffer. Returns the number of
/// bytes appended (always `4 + payload.len()`).
pub fn encode_into(buf: &mut Vec<u8>, payload: &[u8]) -> Result<usize, PktLineError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(PktLineError::PayloadTooLarge(payload.len()));
    }
    let total = payload.len() + 4;
    // 4-char lowercase hex; git is specific about the case.
    let header = format!("{total:04x}");
    debug_assert_eq!(header.len(), 4);
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(payload);
    Ok(4 + payload.len())
}

/// Append a flush packet (`0000`) to `buf`.
pub fn flush(buf: &mut Vec<u8>) {
    buf.extend_from_slice(FLUSH);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_payload() {
        let out = encode(b"").unwrap();
        assert_eq!(&out, b"0004");
    }

    #[test]
    fn hello_payload() {
        // "hello\n" is 6 bytes, total 10 → 000a.
        let out = encode(b"hello\n").unwrap();
        assert_eq!(&out, b"000ahello\n");
    }

    #[test]
    fn encode_into_appends_and_reports_bytes() {
        let mut buf = Vec::from(&b"prefix"[..]);
        let n = encode_into(&mut buf, b"hi\n").unwrap();
        assert_eq!(n, 7);
        assert_eq!(&buf, b"prefix0007hi\n");
    }

    #[test]
    fn flush_is_literal_zero_bytes() {
        let mut buf = Vec::new();
        flush(&mut buf);
        assert_eq!(&buf, b"0000");
    }

    #[test]
    fn payload_exactly_max_is_ok() {
        let payload = vec![b'x'; MAX_PAYLOAD];
        let out = encode(&payload).unwrap();
        assert_eq!(out.len(), 4 + MAX_PAYLOAD);
        assert_eq!(&out[..4], b"fff4"); // 65524 = 0xfff4
    }

    #[test]
    fn payload_above_max_rejected() {
        let payload = vec![b'x'; MAX_PAYLOAD + 1];
        let err = encode(&payload).unwrap_err();
        assert_eq!(err, PktLineError::PayloadTooLarge(MAX_PAYLOAD + 1));
    }

    #[test]
    fn lowercase_hex_required() {
        // Spec-case check — git refuses uppercase-hex length prefixes.
        let out = encode(&vec![0u8; 10]).unwrap();
        let header = std::str::from_utf8(&out[..4]).unwrap();
        assert_eq!(header, header.to_lowercase());
        assert_eq!(header, "000e");
    }
}
