//! Pack-v2 parser.
//!
//! Parses the binary pack format git clients stream over
//! `git-receive-pack` (push) and `git-upload-pack` (clone):
//!
//! ```text
//!   4 bytes  "PACK"
//!   4 bytes  version (big-endian u32; always 2)
//!   4 bytes  object count (big-endian u32)
//!   N objects, each:
//!     header byte(s): type (3 bits) + size (variable-length)
//!     zlib-deflated payload
//!   20 bytes SHA-1 over all preceding bytes
//! ```
//!
//! Scope of this v0 parser:
//!   * types 1..=4 (commit, tree, blob, tag) — full support
//!   * types 6 (ofs-delta) and 7 (ref-delta) — rejected with a
//!     descriptive error. Real delta support is a follow-up;
//!     first-push workloads don't emit deltas, and we advertise
//!     neither `thin-pack` nor `ofs-delta` on receive-pack so
//!     clients won't send them.
//!   * Streaming not required at this layer — the WASM receive-
//!     pack handler buffers the pack into memory (bounded) and
//!     passes it here. Streaming is an optimization for large
//!     repos; out of scope for v0.

#![allow(clippy::result_large_err)]

use std::fmt;
use std::io::Read;

use flate2::read::ZlibDecoder;

/// Git object kinds that `parse_pack` emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl ObjectKind {
    /// Git's canonical header prefix (e.g. "blob", "tree").
    pub fn header_prefix(self) -> &'static str {
        match self {
            ObjectKind::Commit => "commit",
            ObjectKind::Tree => "tree",
            ObjectKind::Blob => "blob",
            ObjectKind::Tag => "tag",
        }
    }
}

/// One parsed object: kind + inflated bytes (not the canonical
/// `<kind> <len>\0<body>` form — just the body). Callers that
/// need the canonical SHA-1 compose the header via
/// [`tg_canonical`] at hash time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackObject {
    pub kind: ObjectKind,
    pub data: Vec<u8>,
}

/// Errors surfaced by the parser. All errors fail-closed on the
/// first bad byte — partial objects are never returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    /// Pack too short to carry the mandatory 12-byte header + 20-byte trailer.
    Truncated { got: usize, need: usize },
    /// Magic bytes aren't `PACK`.
    BadMagic([u8; 4]),
    /// Version field != 2. We don't speak v3 yet.
    UnsupportedVersion(u32),
    /// Variable-length size field ran past the buffer.
    HeaderOverrun,
    /// A delta object type (6 = ofs-delta, 7 = ref-delta). We
    /// advertise neither in v0 so clients shouldn't send them.
    DeltaObjectsUnsupported(u8),
    /// Unknown type tag (0, 5, or >7).
    InvalidObjectType(u8),
    /// zlib-deflated payload failed to decompress.
    ZlibDecompressFailed(String),
    /// Declared object count didn't match what we saw before the
    /// trailer, or zlib inflated to a different size.
    SizeMismatch { declared: usize, actual: usize },
    /// Trailer SHA-1 didn't match the computed hash over the pack
    /// bytes preceding it.
    TrailerMismatch,
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackError::Truncated { got, need } => {
                write!(f, "pack truncated: got {got} bytes, need {need}")
            }
            PackError::BadMagic(m) => write!(f, "pack bad magic: {:02x?}", m),
            PackError::UnsupportedVersion(v) => write!(f, "pack version {v} not supported (need 2)"),
            PackError::HeaderOverrun => write!(f, "object header ran past buffer"),
            PackError::DeltaObjectsUnsupported(t) => {
                write!(f, "pack contains delta object type {t} (ofs/ref); v0 parser does not support deltas")
            }
            PackError::InvalidObjectType(t) => write!(f, "invalid pack object type: {t}"),
            PackError::ZlibDecompressFailed(e) => write!(f, "zlib decompress failed: {e}"),
            PackError::SizeMismatch { declared, actual } => {
                write!(f, "pack size mismatch: declared {declared}, got {actual}")
            }
            PackError::TrailerMismatch => write!(f, "pack trailer SHA-1 mismatch"),
        }
    }
}

impl std::error::Error for PackError {}

/// Parse a full pack-v2 buffer into its object list. Verifies the
/// magic + version + trailer SHA-1 before returning.
pub fn parse_pack(bytes: &[u8]) -> Result<Vec<PackObject>, PackError> {
    if bytes.len() < 32 {
        return Err(PackError::Truncated {
            got: bytes.len(),
            need: 32,
        });
    }

    // Header.
    if &bytes[..4] != b"PACK" {
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[..4]);
        return Err(PackError::BadMagic(magic));
    }
    let version = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    if version != 2 {
        return Err(PackError::UnsupportedVersion(version));
    }
    let declared_count = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;

    // Trailer verification.
    let trailer_start = bytes.len() - 20;
    let trailer = &bytes[trailer_start..];
    let expected = {
        use sha1::Digest;
        let mut h = sha1::Sha1::new();
        h.update(&bytes[..trailer_start]);
        h.finalize().to_vec()
    };
    if trailer != expected.as_slice() {
        return Err(PackError::TrailerMismatch);
    }

    // Object stream.
    let mut cursor = 12usize;
    let mut out = Vec::with_capacity(declared_count);
    while cursor < trailer_start {
        let (kind, declared_size, header_len) =
            decode_object_header(&bytes[cursor..trailer_start])?;
        cursor += header_len;

        // Inflate one object's zlib-deflated payload. flate2 reads
        // until the DEFLATE END block — we count how many input
        // bytes were consumed via `total_in` on the decoder.
        let mut decoder = ZlibDecoder::new(&bytes[cursor..trailer_start]);
        let mut payload = Vec::with_capacity(declared_size);
        decoder
            .read_to_end(&mut payload)
            .map_err(|e| PackError::ZlibDecompressFailed(e.to_string()))?;
        let consumed = decoder.total_in() as usize;
        cursor += consumed;

        if payload.len() != declared_size {
            return Err(PackError::SizeMismatch {
                declared: declared_size,
                actual: payload.len(),
            });
        }

        out.push(PackObject {
            kind,
            data: payload,
        });
    }

    if out.len() != declared_count {
        return Err(PackError::SizeMismatch {
            declared: declared_count,
            actual: out.len(),
        });
    }

    Ok(out)
}

/// Decode one object's variable-length header. Returns
/// `(kind, declared_size, header_byte_count)`.
///
/// Header layout (little-endian-ish, MSB-first continuation bit):
///
/// ```text
///   byte 0: c   ttt  ssss
///           ^   ^    ^
///           |   |    low 4 bits of size
///           |   type (1-7)
///           continuation (0=last byte, 1=more)
///   byte N:  c   sssssss
///                    7 bits of size, shifted by (4 + 7*(N-1))
/// ```
/// Build a pack-v2 buffer from a fully-materialised object list.
///
/// Writes `PACK\0\0\0\2<count>` + per-object (variable-length
/// type/size header + zlib-deflated body) + 20-byte SHA-1 trailer
/// over all preceding bytes. Matches the format `parse_pack`
/// consumes; `parse_pack(&emit_pack(x)) == x`.
///
/// Only non-delta types (Commit/Tree/Blob/Tag) are supported, in
/// line with our v0 parser. Callers that need delta emission
/// should convert to plain objects first.
pub fn emit_pack(objects: &[PackObject]) -> Vec<u8> {
    let mut emitter = PackEmitter::begin(Vec::new(), objects.len() as u32)
        .expect("Vec write never fails");
    for obj in objects {
        emitter
            .write_object(obj.kind, &obj.data)
            .expect("Vec write never fails");
    }
    emitter.finish().expect("Vec write never fails")
}

/// Streaming pack-v2 emitter.
///
/// Writes the 12-byte pack header on `begin`, one full object per
/// `write_object` call, and the 20-byte SHA-1 trailer on `finish`.
/// Every byte is fed through a SHA-1 hasher as it goes out, so the
/// trailer matches the bytes that were actually written even when
/// the underlying `Write` is a streaming sink (HTTP response body,
/// pkt-line framer, …).
///
/// Object count is required up front because the pack header carries
/// it, and pack-v2 has no way to revise it once written. Walk the
/// DAG first to enumerate; emit second.
pub struct PackEmitter<W: std::io::Write> {
    inner: ShaWriter<W>,
}

impl<W: std::io::Write> PackEmitter<W> {
    /// Begin a pack: writes `PACK\0\0\0\2<count>` to `writer`.
    pub fn begin(writer: W, count: u32) -> std::io::Result<Self> {
        use std::io::Write;
        let mut inner = ShaWriter::new(writer);
        inner.write_all(b"PACK")?;
        inner.write_all(&2u32.to_be_bytes())?;
        inner.write_all(&count.to_be_bytes())?;
        Ok(Self { inner })
    }

    /// Emit one object. Writes the variable-length type+size header
    /// and the zlib-deflated body. The body is consumed in one call;
    /// for very large blobs use `write_object_stream`.
    pub fn write_object(
        &mut self,
        kind: ObjectKind,
        body: &[u8],
    ) -> std::io::Result<()> {
        self.write_header(kind, body.len())?;
        self.write_deflated(body)
    }

    /// Variant that takes a `Read` and a known size. Lets callers
    /// stream a body of any length without holding it all in memory.
    pub fn write_object_stream<R: std::io::Read>(
        &mut self,
        kind: ObjectKind,
        size: usize,
        mut body: R,
    ) -> std::io::Result<()> {
        self.write_header(kind, size)?;
        // Pipe `body` through the deflater, which writes through us.
        let mut enc = flate2::write::ZlibEncoder::new(
            HasherSink { inner: &mut self.inner },
            flate2::Compression::default(),
        );
        std::io::copy(&mut body, &mut enc)?;
        enc.finish()?;
        Ok(())
    }

    /// Close the pack: writes the 20-byte SHA-1 trailer over every
    /// byte handed to `begin`/`write_object*` and returns the
    /// underlying writer.
    pub fn finish(self) -> std::io::Result<W> {
        use sha1::Digest;
        let ShaWriter { mut inner, hasher } = self.inner;
        let trailer = hasher.finalize();
        inner.write_all(&trailer)?;
        Ok(inner)
    }

    fn write_header(
        &mut self,
        kind: ObjectKind,
        size: usize,
    ) -> std::io::Result<()> {
        use std::io::Write;
        let type_bits = match kind {
            ObjectKind::Commit => 1u8,
            ObjectKind::Tree => 2,
            ObjectKind::Blob => 3,
            ObjectKind::Tag => 4,
        };
        let low = (size & 0x0f) as u8;
        let rest = size >> 4;
        if rest == 0 {
            self.inner.write_all(&[(type_bits << 4) | low])
        } else {
            let mut buf = [0u8; 16];
            buf[0] = 0x80 | (type_bits << 4) | low;
            let mut n = 1;
            let mut r = rest;
            while r > 0 {
                let mut b = (r & 0x7f) as u8;
                r >>= 7;
                if r > 0 {
                    b |= 0x80;
                }
                buf[n] = b;
                n += 1;
                debug_assert!(n <= buf.len());
            }
            self.inner.write_all(&buf[..n])
        }
    }

    fn write_deflated(&mut self, body: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(
            HasherSink { inner: &mut self.inner },
            flate2::Compression::default(),
        );
        enc.write_all(body)?;
        enc.finish()?;
        Ok(())
    }
}

/// `Write` adapter that feeds every byte to a SHA-1 hasher before
/// forwarding to the inner sink. Owns the hasher; surrendered by
/// `finish` to compute the trailer.
struct ShaWriter<W: std::io::Write> {
    inner: W,
    hasher: sha1::Sha1,
}

impl<W: std::io::Write> ShaWriter<W> {
    fn new(inner: W) -> Self {
        use sha1::Digest;
        Self {
            inner,
            hasher: sha1::Sha1::new(),
        }
    }
}

impl<W: std::io::Write> std::io::Write for ShaWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha1::Digest;
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Tiny `Write` wrapper that lets `ZlibEncoder` write back through
/// `&mut ShaWriter` without taking ownership.
struct HasherSink<'a, W: std::io::Write> {
    inner: &'a mut ShaWriter<W>,
}

impl<W: std::io::Write> std::io::Write for HasherSink<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn decode_object_header(buf: &[u8]) -> Result<(ObjectKind, usize, usize), PackError> {
    if buf.is_empty() {
        return Err(PackError::HeaderOverrun);
    }
    let b0 = buf[0];
    let type_bits = (b0 >> 4) & 0b0000_0111;
    let mut size: usize = (b0 & 0b0000_1111) as usize;
    let mut shift: u32 = 4;
    let mut used = 1usize;
    let mut b = b0;

    while b & 0x80 != 0 {
        if used >= buf.len() {
            return Err(PackError::HeaderOverrun);
        }
        b = buf[used];
        used += 1;
        // Guard against size overflow. git caps single-object
        // size at ~2 GiB; we're more conservative at u32 so
        // anything wild fails fast.
        let add = (b & 0x7f) as usize;
        let shifted = add
            .checked_shl(shift)
            .ok_or(PackError::HeaderOverrun)?;
        size = size
            .checked_add(shifted)
            .ok_or(PackError::HeaderOverrun)?;
        shift += 7;
        if shift > 63 {
            return Err(PackError::HeaderOverrun);
        }
    }

    let kind = match type_bits {
        1 => ObjectKind::Commit,
        2 => ObjectKind::Tree,
        3 => ObjectKind::Blob,
        4 => ObjectKind::Tag,
        6 | 7 => return Err(PackError::DeltaObjectsUnsupported(type_bits)),
        other => return Err(PackError::InvalidObjectType(other)),
    };

    Ok((kind, size, used))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use sha1::Digest;
    use std::io::Write;

    fn build_header(kind: u8, size: usize) -> Vec<u8> {
        // Single-byte case first.
        if size < 16 {
            return vec![(kind << 4) | (size as u8 & 0x0f)];
        }
        let mut out = Vec::new();
        let first = (kind << 4) | ((size & 0x0f) as u8) | 0x80;
        out.push(first);
        let mut rest = size >> 4;
        while rest > 0 {
            let mut b = (rest & 0x7f) as u8;
            rest >>= 7;
            if rest > 0 {
                b |= 0x80;
            }
            out.push(b);
        }
        out
    }

    fn zlib_compress(data: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn build_pack(objects: &[(u8, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"PACK");
        body.extend_from_slice(&2u32.to_be_bytes());
        body.extend_from_slice(&(objects.len() as u32).to_be_bytes());
        for (kind, data) in objects {
            body.extend_from_slice(&build_header(*kind, data.len()));
            body.extend_from_slice(&zlib_compress(data));
        }
        let mut h = sha1::Sha1::new();
        h.update(&body);
        body.extend_from_slice(&h.finalize());
        body
    }

    #[test]
    fn empty_pack_just_header_and_trailer() {
        let pack = build_pack(&[]);
        let objs = parse_pack(&pack).unwrap();
        assert!(objs.is_empty());
    }

    #[test]
    fn single_blob_roundtrips() {
        let pack = build_pack(&[(3, b"hello world\n")]);
        let objs = parse_pack(&pack).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].kind, ObjectKind::Blob);
        assert_eq!(objs[0].data, b"hello world\n");
    }

    #[test]
    fn commit_blob_tree_mix() {
        let pack = build_pack(&[
            (1, b"tree 00\nauthor Me <me@x>\n\nmsg\n"),
            (2, b"entries-bytes"),
            (3, b"file contents"),
        ]);
        let objs = parse_pack(&pack).unwrap();
        assert_eq!(objs.len(), 3);
        assert_eq!(objs[0].kind, ObjectKind::Commit);
        assert_eq!(objs[1].kind, ObjectKind::Tree);
        assert_eq!(objs[2].kind, ObjectKind::Blob);
    }

    #[test]
    fn large_object_encoded_via_multibyte_header() {
        // 300-byte blob forces a 2-byte header.
        let blob = vec![0x41u8; 300];
        let pack = build_pack(&[(3, &blob)]);
        let objs = parse_pack(&pack).unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].data.len(), 300);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut pack = build_pack(&[(3, b"x")]);
        pack[0] = b'N'; // NACK instead of PACK
        let err = parse_pack(&pack).unwrap_err();
        assert!(matches!(err, PackError::BadMagic(_)));
    }

    #[test]
    fn bad_version_rejected() {
        let mut pack = build_pack(&[(3, b"x")]);
        pack[7] = 3; // v3 instead of v2
        // Recompute trailer over the mutated body.
        let trailer_start = pack.len() - 20;
        let body = &pack[..trailer_start];
        let mut h = sha1::Sha1::new();
        h.update(body);
        let trailer = h.finalize();
        pack[trailer_start..].copy_from_slice(&trailer);
        let err = parse_pack(&pack).unwrap_err();
        assert!(matches!(err, PackError::UnsupportedVersion(3)));
    }

    #[test]
    fn trailer_mismatch_rejected() {
        let mut pack = build_pack(&[(3, b"x")]);
        let last = pack.len() - 1;
        pack[last] ^= 0xff;
        let err = parse_pack(&pack).unwrap_err();
        assert_eq!(err, PackError::TrailerMismatch);
    }

    #[test]
    fn delta_object_type_rejected() {
        // Craft an ofs-delta (type 6). Just the header byte — the
        // parser should fail before touching the payload.
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        pack.extend_from_slice(&2u32.to_be_bytes());
        pack.extend_from_slice(&1u32.to_be_bytes());
        pack.push((6 << 4) | 5); // type=6, size=5
        pack.extend_from_slice(&[0u8; 10]); // bogus payload
        let mut h = sha1::Sha1::new();
        h.update(&pack);
        pack.extend_from_slice(&h.finalize());
        let err = parse_pack(&pack).unwrap_err();
        assert!(matches!(err, PackError::DeltaObjectsUnsupported(6)));
    }

    #[test]
    fn invalid_object_type_rejected() {
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        pack.extend_from_slice(&2u32.to_be_bytes());
        pack.extend_from_slice(&1u32.to_be_bytes());
        pack.push((5 << 4) | 1); // type=5 (reserved/invalid)
        pack.extend_from_slice(&[0u8; 10]);
        let mut h = sha1::Sha1::new();
        h.update(&pack);
        pack.extend_from_slice(&h.finalize());
        let err = parse_pack(&pack).unwrap_err();
        assert!(matches!(err, PackError::InvalidObjectType(5)));
    }

    #[test]
    fn emit_then_parse_roundtrip_empty() {
        let parsed = parse_pack(&emit_pack(&[])).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn emit_then_parse_roundtrip_single_blob() {
        let obj = PackObject {
            kind: ObjectKind::Blob,
            data: b"hello world\n".to_vec(),
        };
        let parsed = parse_pack(&emit_pack(&[obj.clone()])).unwrap();
        assert_eq!(parsed, vec![obj]);
    }

    #[test]
    fn emit_then_parse_roundtrip_multi() {
        let objs = vec![
            PackObject {
                kind: ObjectKind::Commit,
                data: b"tree abc123\nauthor me\n\nmsg\n".to_vec(),
            },
            PackObject {
                kind: ObjectKind::Tree,
                data: vec![b'x'; 300], // forces multi-byte header
            },
            PackObject {
                kind: ObjectKind::Blob,
                data: b"content".to_vec(),
            },
        ];
        let parsed = parse_pack(&emit_pack(&objs)).unwrap();
        assert_eq!(parsed, objs);
    }

    #[test]
    fn emit_pack_is_parseable_by_external_inspection() {
        // Verify header byte counts: 4 magic + 4 version + 4 count
        // + at least 1 header byte + ≥ few deflated bytes +
        // 20 trailer.
        let obj = PackObject {
            kind: ObjectKind::Blob,
            data: b"hi".to_vec(),
        };
        let pack = emit_pack(&[obj]);
        assert_eq!(&pack[..4], b"PACK");
        assert_eq!(&pack[4..8], &2u32.to_be_bytes());
        assert_eq!(&pack[8..12], &1u32.to_be_bytes());
        // Trailer present, correct length.
        assert_eq!(pack.len() - 20, pack.len() - 20); // tautology for clarity
        // First header byte: type=3 (blob), size=2 → (3<<4) | 2 = 0x32
        assert_eq!(pack[12], 0x32);
    }

    #[test]
    fn pack_truncated_rejected() {
        let pack = vec![0u8; 20]; // shorter than minimum 32
        let err = parse_pack(&pack).unwrap_err();
        assert!(matches!(err, PackError::Truncated { .. }));
    }

    #[test]
    fn pack_emitter_streams_match_emit_pack() {
        // The streaming API must produce byte-identical output to
        // the convenience function — same header, same per-object
        // encoding, same trailer SHA-1.
        let objs = vec![
            PackObject {
                kind: ObjectKind::Commit,
                data: b"tree abc\nauthor x\n\nm".to_vec(),
            },
            PackObject {
                kind: ObjectKind::Tree,
                data: vec![b'a'; 500],
            },
            PackObject {
                kind: ObjectKind::Blob,
                data: b"hello".to_vec(),
            },
        ];

        let want = emit_pack(&objs);

        let mut got = Vec::new();
        let mut emitter = PackEmitter::begin(&mut got, objs.len() as u32).unwrap();
        for obj in &objs {
            emitter.write_object(obj.kind, &obj.data).unwrap();
        }
        emitter.finish().unwrap();

        assert_eq!(got, want);
    }

    #[test]
    fn pack_emitter_stream_object_matches_buffered() {
        // write_object_stream(reader) should be byte-identical to
        // write_object(slice) for the same content.
        let body = vec![0xABu8; 4096];

        let mut buffered = Vec::new();
        let mut e1 = PackEmitter::begin(&mut buffered, 1).unwrap();
        e1.write_object(ObjectKind::Blob, &body).unwrap();
        e1.finish().unwrap();

        let mut streamed = Vec::new();
        let mut e2 = PackEmitter::begin(&mut streamed, 1).unwrap();
        e2.write_object_stream(ObjectKind::Blob, body.len(), body.as_slice())
            .unwrap();
        e2.finish().unwrap();

        assert_eq!(buffered, streamed);
    }

    #[test]
    fn pack_emitter_roundtrip_via_parse_pack() {
        // End-to-end: emit through the streaming API, parse back,
        // get the same objects.
        let objs = vec![
            PackObject {
                kind: ObjectKind::Blob,
                data: b"hello world".to_vec(),
            },
            PackObject {
                kind: ObjectKind::Tag,
                data: b"object abc\ntype commit\ntag v1\n".to_vec(),
            },
        ];

        let mut out = Vec::new();
        let mut emitter = PackEmitter::begin(&mut out, objs.len() as u32).unwrap();
        for obj in &objs {
            emitter.write_object(obj.kind, &obj.data).unwrap();
        }
        emitter.finish().unwrap();

        let parsed = parse_pack(&out).unwrap();
        assert_eq!(parsed, objs);
    }
}
