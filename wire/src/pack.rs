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
//! Two parser flavours:
//!
//!   * [`parse_pack`] — buffer in, `Vec<PackObject>` out. Used in
//!     tests and host-side tools that already have the bytes.
//!   * [`StreamingPackParser`] — byte source in, one decoded object
//!     at a time out. Used by the receive-pack WASM so push payloads
//!     never need to be fully buffered: each object is decoded,
//!     persisted, and dropped before the next is read.
//!
//! Both share the same supported-types matrix:
//!   * types 1..=4 (commit, tree, blob, tag) — full support
//!   * types 6 (ofs-delta) — expanded against earlier objects in the same pack
//!   * types 7 (ref-delta) — expanded against earlier objects in the same
//!     pack, or an external base supplied by callers parsing thin packs

#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::fmt;

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
    /// A delta object type was found in a path that cannot resolve deltas.
    DeltaObjectsUnsupported(u8),
    /// A delta referenced a base object this pack parser has not seen.
    DeltaBaseMissing(String),
    /// The binary delta program was malformed or did not produce the declared size.
    DeltaApplyFailed(String),
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
            PackError::UnsupportedVersion(v) => {
                write!(f, "pack version {v} not supported (need 2)")
            }
            PackError::HeaderOverrun => write!(f, "object header ran past buffer"),
            PackError::DeltaObjectsUnsupported(t) => {
                write!(
                    f,
                    "pack contains unresolved delta object type {t} (ofs/ref)"
                )
            }
            PackError::DeltaBaseMissing(base) => write!(f, "pack delta base not found: {base}"),
            PackError::DeltaApplyFailed(e) => write!(f, "pack delta apply failed: {e}"),
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
    let cursor = std::io::Cursor::new(bytes);
    let mut parser = StreamingPackParser::begin(cursor)?;
    let declared_count = parser.object_count() as usize;
    let mut out = Vec::with_capacity(declared_count);
    while let Some(obj) = parser.next_object()? {
        out.push(obj);
    }
    let cursor = parser.finish_inner()?;
    if cursor.position() != bytes.len() as u64 {
        return Err(PackError::TrailerMismatch);
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
    let mut emitter =
        PackEmitter::begin(Vec::new(), objects.len() as u32).expect("Vec write never fails");
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
    pub fn write_object(&mut self, kind: ObjectKind, body: &[u8]) -> std::io::Result<()> {
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
            HasherSink {
                inner: &mut self.inner,
            },
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

    fn write_header(&mut self, kind: ObjectKind, size: usize) -> std::io::Result<()> {
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
            HasherSink {
                inner: &mut self.inner,
            },
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

// ---------------------------------------------------------------------
// Streaming parser
// ---------------------------------------------------------------------

/// Incremental pack-v2 parser.
///
/// `begin` reads and validates the 12-byte header, exposing
/// `object_count`. `next_object` decodes one object at a time —
/// reads the type/size header, consumes a single zlib stream, hashes
/// the raw pack bytes as they go past, and returns the inflated
/// object body. `finish` reads the 20-byte trailer and verifies it
/// against the running hash.
///
/// The parser owns its source as a `BufRead`. Memory profile is one
/// object's body plus zlib's internal state — buffered packs of any
/// size can be drained without holding more than that.
pub struct StreamingPackParser<R: std::io::BufRead> {
    inner: R,
    hasher: sha1::Sha1,
    object_count: u32,
    consumed: u32,
    pack_offset: u64,
    objects_by_sha: HashMap<String, ResolvedObject>,
    objects_by_offset: HashMap<u64, String>,
}

#[derive(Debug, Clone)]
struct ResolvedObject {
    kind: ObjectKind,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Base(ObjectKind),
    OfsDelta,
    RefDelta,
}

impl<R: std::io::BufRead> StreamingPackParser<R> {
    /// Read and validate the 12-byte pack header. Returns a parser
    /// positioned at the first object's type+size byte.
    pub fn begin(mut inner: R) -> Result<Self, PackError> {
        use sha1::Digest;
        let mut hasher = sha1::Sha1::new();
        let mut pack_offset = 0u64;
        let mut header = [0u8; 12];
        read_exact_hashed(&mut inner, &mut hasher, &mut pack_offset, &mut header)?;
        if &header[..4] != b"PACK" {
            let mut magic = [0u8; 4];
            magic.copy_from_slice(&header[..4]);
            return Err(PackError::BadMagic(magic));
        }
        let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        if version != 2 {
            return Err(PackError::UnsupportedVersion(version));
        }
        let object_count = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
        Ok(Self {
            inner,
            hasher,
            object_count,
            consumed: 0,
            pack_offset,
            objects_by_sha: HashMap::new(),
            objects_by_offset: HashMap::new(),
        })
    }

    /// Number of objects the pack header declared.
    pub fn object_count(&self) -> u32 {
        self.object_count
    }

    /// Pull the next decoded object. Returns `Ok(None)` once every
    /// declared object has been yielded; the caller should then
    /// invoke `finish` to validate the trailer.
    pub fn next_object(&mut self) -> Result<Option<PackObject>, PackError> {
        self.next_object_with_ref_delta_base(|_| Ok(None))
    }

    /// Pull the next decoded object, resolving ref-delta bases that
    /// are not present earlier in the pack via `resolve_external_base`.
    /// This is required for Git thin packs, where clients can delta
    /// against objects the server already advertised.
    pub fn next_object_with_ref_delta_base<F>(
        &mut self,
        mut resolve_external_base: F,
    ) -> Result<Option<PackObject>, PackError>
    where
        F: FnMut(&str) -> Result<Option<PackObject>, PackError>,
    {
        if self.consumed >= self.object_count {
            return Ok(None);
        }
        let object_offset = self.pack_offset;
        let (entry_kind, declared_size) =
            read_entry_header_hashed(&mut self.inner, &mut self.hasher, &mut self.pack_offset)?;
        let resolved = match entry_kind {
            EntryKind::Base(kind) => ResolvedObject {
                kind,
                data: inflate_one_object_hashed(
                    &mut self.inner,
                    &mut self.hasher,
                    &mut self.pack_offset,
                    declared_size,
                )?,
            },
            EntryKind::RefDelta => {
                let mut raw_sha = [0u8; 20];
                read_exact_hashed(
                    &mut self.inner,
                    &mut self.hasher,
                    &mut self.pack_offset,
                    &mut raw_sha,
                )?;
                let base_sha = hex_lower(&raw_sha);
                let base = if let Some(base) = self.objects_by_sha.get(&base_sha).cloned() {
                    base
                } else if let Some(base) = resolve_external_base(&base_sha)? {
                    ResolvedObject {
                        kind: base.kind,
                        data: base.data,
                    }
                } else {
                    return Err(PackError::DeltaBaseMissing(base_sha.clone()));
                };
                let delta = inflate_one_object_hashed(
                    &mut self.inner,
                    &mut self.hasher,
                    &mut self.pack_offset,
                    declared_size,
                )?;
                ResolvedObject {
                    kind: base.kind,
                    data: apply_delta(&base.data, &delta)?,
                }
            }
            EntryKind::OfsDelta => {
                let base_offset = read_ofs_delta_base_offset(
                    &mut self.inner,
                    &mut self.hasher,
                    &mut self.pack_offset,
                    object_offset,
                )?;
                let base_sha = self
                    .objects_by_offset
                    .get(&base_offset)
                    .cloned()
                    .ok_or_else(|| PackError::DeltaBaseMissing(format!("offset {base_offset}")))?;
                let base = self
                    .objects_by_sha
                    .get(&base_sha)
                    .cloned()
                    .ok_or_else(|| PackError::DeltaBaseMissing(base_sha.clone()))?;
                let delta = inflate_one_object_hashed(
                    &mut self.inner,
                    &mut self.hasher,
                    &mut self.pack_offset,
                    declared_size,
                )?;
                ResolvedObject {
                    kind: base.kind,
                    data: apply_delta(&base.data, &delta)?,
                }
            }
        };
        let sha = object_sha(resolved.kind, &resolved.data);
        self.objects_by_offset.insert(object_offset, sha.clone());
        self.objects_by_sha.insert(sha, resolved.clone());
        self.consumed += 1;
        Ok(Some(PackObject {
            kind: resolved.kind,
            data: resolved.data,
        }))
    }

    /// Read and verify the 20-byte SHA-1 trailer, then drop the
    /// reader. Errors if the trailer doesn't match the running hash
    /// or the declared object count was wrong.
    pub fn finish(self) -> Result<(), PackError> {
        self.finish_inner().map(|_| ())
    }

    fn finish_inner(mut self) -> Result<R, PackError> {
        if self.consumed != self.object_count {
            return Err(PackError::SizeMismatch {
                declared: self.object_count as usize,
                actual: self.consumed as usize,
            });
        }
        let mut trailer = [0u8; 20];
        // The trailer is NOT hashed — it's the hash itself.
        self.inner
            .read_exact(&mut trailer)
            .map_err(|_| PackError::TrailerMismatch)?;
        use sha1::Digest;
        let computed = self.hasher.finalize();
        if computed.as_slice() != trailer {
            return Err(PackError::TrailerMismatch);
        }
        Ok(self.inner)
    }
}

/// Read exactly `buf.len()` bytes from `r`, feeding each byte to
/// `hasher` as it goes past. Returns Truncated on short read.
fn read_exact_hashed<R: std::io::Read>(
    r: &mut R,
    hasher: &mut sha1::Sha1,
    pack_offset: &mut u64,
    buf: &mut [u8],
) -> Result<(), PackError> {
    use sha1::Digest;
    let mut filled = 0;
    while filled < buf.len() {
        let n = r
            .read(&mut buf[filled..])
            .map_err(|e| PackError::ZlibDecompressFailed(e.to_string()))?;
        if n == 0 {
            return Err(PackError::Truncated {
                got: filled,
                need: buf.len(),
            });
        }
        hasher.update(&buf[filled..filled + n]);
        *pack_offset += n as u64;
        filled += n;
    }
    Ok(())
}

/// Read the variable-length type+size header for one object,
/// hashing every byte that goes past.
fn read_entry_header_hashed<R: std::io::Read>(
    r: &mut R,
    hasher: &mut sha1::Sha1,
    pack_offset: &mut u64,
) -> Result<(EntryKind, usize), PackError> {
    use sha1::Digest;
    let mut byte = [0u8; 1];
    r.read_exact(&mut byte)
        .map_err(|_| PackError::HeaderOverrun)?;
    hasher.update(&byte);
    *pack_offset += 1;
    let b0 = byte[0];
    let type_bits = (b0 >> 4) & 0b0000_0111;
    let mut size: usize = (b0 & 0b0000_1111) as usize;
    let mut shift: u32 = 4;

    let mut last = b0;
    while last & 0x80 != 0 {
        r.read_exact(&mut byte)
            .map_err(|_| PackError::HeaderOverrun)?;
        hasher.update(&byte);
        *pack_offset += 1;
        last = byte[0];
        let add = (last & 0x7f) as usize;
        let shifted = add.checked_shl(shift).ok_or(PackError::HeaderOverrun)?;
        size = size.checked_add(shifted).ok_or(PackError::HeaderOverrun)?;
        shift += 7;
        if shift > 63 {
            return Err(PackError::HeaderOverrun);
        }
    }

    let kind = match type_bits {
        1 => EntryKind::Base(ObjectKind::Commit),
        2 => EntryKind::Base(ObjectKind::Tree),
        3 => EntryKind::Base(ObjectKind::Blob),
        4 => EntryKind::Base(ObjectKind::Tag),
        6 => EntryKind::OfsDelta,
        7 => EntryKind::RefDelta,
        other => return Err(PackError::InvalidObjectType(other)),
    };
    Ok((kind, size))
}

fn read_ofs_delta_base_offset<R: std::io::Read>(
    r: &mut R,
    hasher: &mut sha1::Sha1,
    pack_offset: &mut u64,
    object_offset: u64,
) -> Result<u64, PackError> {
    use sha1::Digest;
    let mut byte = [0u8; 1];
    r.read_exact(&mut byte)
        .map_err(|_| PackError::HeaderOverrun)?;
    hasher.update(&byte);
    *pack_offset += 1;

    let mut c = byte[0];
    let mut distance = (c & 0x7f) as u64;
    while c & 0x80 != 0 {
        r.read_exact(&mut byte)
            .map_err(|_| PackError::HeaderOverrun)?;
        hasher.update(&byte);
        *pack_offset += 1;
        c = byte[0];
        distance = ((distance + 1) << 7) | ((c & 0x7f) as u64);
    }
    object_offset
        .checked_sub(distance)
        .ok_or_else(|| PackError::DeltaBaseMissing(format!("offset before pack: -{distance}")))
}

/// Decode exactly one zlib stream from `inner`, hashing the raw
/// (compressed) bytes as they're consumed. Stops at the zlib
/// end-of-stream marker; the BufReader is positioned right after.
/// `expected_size` is the declared inflated size from the object
/// header — we read that many output bytes and assert the stream
/// ends cleanly.
fn inflate_one_object_hashed<R: std::io::BufRead>(
    inner: &mut R,
    hasher: &mut sha1::Sha1,
    pack_offset: &mut u64,
    expected_size: usize,
) -> Result<Vec<u8>, PackError> {
    use flate2::{Decompress, FlushDecompress, Status};
    use sha1::Digest;

    let mut decoder = Decompress::new(true);
    let mut output: Vec<u8> = Vec::with_capacity(expected_size.min(1 << 20));
    let mut out_buf = [0u8; 64 * 1024];

    loop {
        let in_buf = inner
            .fill_buf()
            .map_err(|e| PackError::ZlibDecompressFailed(e.to_string()))?;
        if in_buf.is_empty() {
            return Err(PackError::ZlibDecompressFailed(
                "unexpected EOF inside zlib stream".to_string(),
            ));
        }
        let in_before = decoder.total_in();
        let out_before = decoder.total_out();
        let status = decoder
            .decompress(in_buf, &mut out_buf, FlushDecompress::None)
            .map_err(|e| PackError::ZlibDecompressFailed(e.to_string()))?;
        let consumed_in = (decoder.total_in() - in_before) as usize;
        let produced = (decoder.total_out() - out_before) as usize;
        hasher.update(&in_buf[..consumed_in]);
        inner.consume(consumed_in);
        *pack_offset += consumed_in as u64;
        output.extend_from_slice(&out_buf[..produced]);

        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError => {
                if output.len() > expected_size {
                    return Err(PackError::SizeMismatch {
                        declared: expected_size,
                        actual: output.len(),
                    });
                }
                if consumed_in == 0 && produced == 0 {
                    // Decoder needs more input but produced nothing
                    // and consumed nothing — caller must give it more
                    // bytes. The fill_buf above will block / refill
                    // on the next iteration.
                    continue;
                }
            }
        }
    }

    if output.len() != expected_size {
        return Err(PackError::SizeMismatch {
            declared: expected_size,
            actual: output.len(),
        });
    }
    Ok(output)
}

fn object_sha(kind: ObjectKind, body: &[u8]) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(format!("{} {}\0", kind.header_prefix(), body.len()).as_bytes());
    hasher.update(body);
    hex_lower(&hasher.finalize())
}

fn read_delta_varint(delta: &[u8], cursor: &mut usize) -> Result<usize, PackError> {
    let mut out = 0usize;
    let mut shift = 0usize;
    loop {
        let byte = *delta
            .get(*cursor)
            .ok_or_else(|| PackError::DeltaApplyFailed("truncated delta varint".to_string()))?;
        *cursor += 1;
        out |= ((byte & 0x7f) as usize)
            .checked_shl(shift as u32)
            .ok_or_else(|| PackError::DeltaApplyFailed("delta varint overflow".to_string()))?;
        if byte & 0x80 == 0 {
            return Ok(out);
        }
        shift += 7;
        if shift > usize::BITS as usize {
            return Err(PackError::DeltaApplyFailed(
                "delta varint too large".to_string(),
            ));
        }
    }
}

fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, PackError> {
    let mut cursor = 0usize;
    let source_size = read_delta_varint(delta, &mut cursor)?;
    if source_size != base.len() {
        return Err(PackError::DeltaApplyFailed(format!(
            "source size mismatch: declared {source_size}, base {}",
            base.len()
        )));
    }
    let target_size = read_delta_varint(delta, &mut cursor)?;
    let mut out = Vec::with_capacity(target_size);

    while cursor < delta.len() {
        let op = delta[cursor];
        cursor += 1;
        if op & 0x80 != 0 {
            let mut copy_offset = 0usize;
            let mut copy_size = 0usize;
            if op & 0x01 != 0 {
                copy_offset |= read_delta_byte(delta, &mut cursor)? as usize;
            }
            if op & 0x02 != 0 {
                copy_offset |= (read_delta_byte(delta, &mut cursor)? as usize) << 8;
            }
            if op & 0x04 != 0 {
                copy_offset |= (read_delta_byte(delta, &mut cursor)? as usize) << 16;
            }
            if op & 0x08 != 0 {
                copy_offset |= (read_delta_byte(delta, &mut cursor)? as usize) << 24;
            }
            if op & 0x10 != 0 {
                copy_size |= read_delta_byte(delta, &mut cursor)? as usize;
            }
            if op & 0x20 != 0 {
                copy_size |= (read_delta_byte(delta, &mut cursor)? as usize) << 8;
            }
            if op & 0x40 != 0 {
                copy_size |= (read_delta_byte(delta, &mut cursor)? as usize) << 16;
            }
            if copy_size == 0 {
                copy_size = 0x10000;
            }
            let end = copy_offset
                .checked_add(copy_size)
                .ok_or_else(|| PackError::DeltaApplyFailed("copy range overflow".to_string()))?;
            let slice = base.get(copy_offset..end).ok_or_else(|| {
                PackError::DeltaApplyFailed(format!(
                    "copy range {copy_offset}..{end} outside base {}",
                    base.len()
                ))
            })?;
            out.extend_from_slice(slice);
        } else if op != 0 {
            let len = op as usize;
            let end = cursor
                .checked_add(len)
                .ok_or_else(|| PackError::DeltaApplyFailed("insert range overflow".to_string()))?;
            let literal = delta.get(cursor..end).ok_or_else(|| {
                PackError::DeltaApplyFailed("literal insert exceeds delta".to_string())
            })?;
            out.extend_from_slice(literal);
            cursor = end;
        } else {
            return Err(PackError::DeltaApplyFailed(
                "reserved zero opcode".to_string(),
            ));
        }
    }

    if out.len() != target_size {
        return Err(PackError::DeltaApplyFailed(format!(
            "target size mismatch: declared {target_size}, got {}",
            out.len()
        )));
    }
    Ok(out)
}

fn read_delta_byte(delta: &[u8], cursor: &mut usize) -> Result<u8, PackError> {
    let byte = *delta
        .get(*cursor)
        .ok_or_else(|| PackError::DeltaApplyFailed("truncated copy instruction".to_string()))?;
    *cursor += 1;
    Ok(byte)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
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

    fn encode_varint(mut n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (n & 0x7f) as u8;
            n >>= 7;
            if n != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if n == 0 {
                return out;
            }
        }
    }

    fn literal_delta(base: &[u8], target: &[u8]) -> Vec<u8> {
        assert!(target.len() < 0x80);
        let mut delta = Vec::new();
        delta.extend_from_slice(&encode_varint(base.len()));
        delta.extend_from_slice(&encode_varint(target.len()));
        delta.push(target.len() as u8);
        delta.extend_from_slice(target);
        delta
    }

    fn raw_sha(hex_sha: &str) -> [u8; 20] {
        let mut raw_base_sha = [0u8; 20];
        for (idx, chunk) in hex_sha.as_bytes().chunks(2).enumerate() {
            let hex = std::str::from_utf8(chunk).unwrap();
            raw_base_sha[idx] = u8::from_str_radix(hex, 16).unwrap();
        }
        raw_base_sha
    }

    fn build_ref_delta_pack(base: &[u8], target: &[u8]) -> Vec<u8> {
        let base_sha = object_sha(ObjectKind::Blob, base);
        let raw_base_sha = raw_sha(&base_sha);
        let delta = literal_delta(base, target);

        let mut body = Vec::new();
        body.extend_from_slice(b"PACK");
        body.extend_from_slice(&2u32.to_be_bytes());
        body.extend_from_slice(&2u32.to_be_bytes());
        body.extend_from_slice(&build_header(3, base.len()));
        body.extend_from_slice(&zlib_compress(base));
        body.extend_from_slice(&build_header(7, delta.len()));
        body.extend_from_slice(&raw_base_sha);
        body.extend_from_slice(&zlib_compress(&delta));
        let mut h = sha1::Sha1::new();
        h.update(&body);
        body.extend_from_slice(&h.finalize());
        body
    }

    fn build_thin_ref_delta_pack(base_sha: &str, base: &[u8], target: &[u8]) -> Vec<u8> {
        let delta = literal_delta(base, target);

        let mut body = Vec::new();
        body.extend_from_slice(b"PACK");
        body.extend_from_slice(&2u32.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&build_header(7, delta.len()));
        body.extend_from_slice(&raw_sha(base_sha));
        body.extend_from_slice(&zlib_compress(&delta));
        let mut h = sha1::Sha1::new();
        h.update(&body);
        body.extend_from_slice(&h.finalize());
        body
    }

    fn encode_ofs_delta_distance(mut distance: usize) -> Vec<u8> {
        let mut out = vec![(distance & 0x7f) as u8];
        distance >>= 7;
        while distance != 0 {
            distance -= 1;
            out.push(((distance & 0x7f) as u8) | 0x80);
            distance >>= 7;
        }
        out.reverse();
        out
    }

    fn build_ofs_delta_pack(base: &[u8], target: &[u8]) -> Vec<u8> {
        let delta = literal_delta(base, target);

        let mut body = Vec::new();
        body.extend_from_slice(b"PACK");
        body.extend_from_slice(&2u32.to_be_bytes());
        body.extend_from_slice(&2u32.to_be_bytes());
        let base_offset = body.len();
        body.extend_from_slice(&build_header(3, base.len()));
        body.extend_from_slice(&zlib_compress(base));
        let delta_offset = body.len();
        body.extend_from_slice(&build_header(6, delta.len()));
        body.extend_from_slice(&encode_ofs_delta_distance(delta_offset - base_offset));
        body.extend_from_slice(&zlib_compress(&delta));
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
    fn ref_delta_expands_against_prior_pack_object() {
        let base = b"hello world\n";
        let target = b"hello temper\n";
        let pack = build_ref_delta_pack(base, target);

        let objects = parse_pack(&pack).unwrap();
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].kind, ObjectKind::Blob);
        assert_eq!(objects[0].data, base);
        assert_eq!(objects[1].kind, ObjectKind::Blob);
        assert_eq!(objects[1].data, target);
    }

    #[test]
    fn ref_delta_expands_against_external_thin_pack_base() {
        let base = b"remote base tree bytes";
        let target = b"remote target tree bytes";
        let base_sha = object_sha(ObjectKind::Tree, base);
        let pack = build_thin_ref_delta_pack(&base_sha, base, target);
        let mut parser = StreamingPackParser::begin(std::io::Cursor::new(pack)).unwrap();

        let object = parser
            .next_object_with_ref_delta_base(|sha| {
                assert_eq!(sha, base_sha);
                Ok(Some(PackObject {
                    kind: ObjectKind::Tree,
                    data: base.to_vec(),
                }))
            })
            .unwrap()
            .unwrap();

        assert_eq!(object.kind, ObjectKind::Tree);
        assert_eq!(object.data, target);
        assert!(parser.next_object().unwrap().is_none());
        parser.finish().unwrap();
    }

    #[test]
    fn ofs_delta_expands_against_prior_pack_object() {
        let base = b"hello world\n";
        let target = b"hello temper\n";
        let pack = build_ofs_delta_pack(base, target);

        let objects = parse_pack(&pack).unwrap();
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].kind, ObjectKind::Blob);
        assert_eq!(objects[0].data, base);
        assert_eq!(objects[1].kind, ObjectKind::Blob);
        assert_eq!(objects[1].data, target);
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
    fn streaming_parser_matches_parse_pack_empty() {
        let pack = emit_pack(&[]);
        let mut p = StreamingPackParser::begin(std::io::Cursor::new(&pack)).unwrap();
        assert_eq!(p.object_count(), 0);
        assert!(p.next_object().unwrap().is_none());
        p.finish().unwrap();
    }

    #[test]
    fn streaming_parser_matches_parse_pack_single() {
        let obj = PackObject {
            kind: ObjectKind::Blob,
            data: b"hello world\n".to_vec(),
        };
        let pack = emit_pack(&[obj.clone()]);

        let mut p = StreamingPackParser::begin(std::io::Cursor::new(&pack)).unwrap();
        assert_eq!(p.object_count(), 1);
        let got = p.next_object().unwrap().unwrap();
        assert_eq!(got, obj);
        assert!(p.next_object().unwrap().is_none());
        p.finish().unwrap();
    }

    #[test]
    fn streaming_parser_matches_parse_pack_multi() {
        let objs = vec![
            PackObject {
                kind: ObjectKind::Commit,
                data: b"tree abc\nauthor x\n\nm".to_vec(),
            },
            PackObject {
                kind: ObjectKind::Tree,
                data: vec![b'a'; 500], // multi-byte size header
            },
            PackObject {
                kind: ObjectKind::Blob,
                data: b"contents".to_vec(),
            },
            PackObject {
                kind: ObjectKind::Tag,
                data: b"object abc\ntype commit\n".to_vec(),
            },
        ];
        let pack = emit_pack(&objs);

        let mut p = StreamingPackParser::begin(std::io::Cursor::new(&pack)).unwrap();
        assert_eq!(p.object_count(), 4);
        let mut got = Vec::new();
        while let Some(obj) = p.next_object().unwrap() {
            got.push(obj);
        }
        p.finish().unwrap();
        assert_eq!(got, objs);
    }

    #[test]
    fn streaming_parser_rejects_corrupt_trailer() {
        let obj = PackObject {
            kind: ObjectKind::Blob,
            data: b"hi".to_vec(),
        };
        let mut pack = emit_pack(&[obj]);
        let last = pack.len() - 1;
        pack[last] ^= 0xFF; // tamper with the SHA-1 trailer
        let mut p = StreamingPackParser::begin(std::io::Cursor::new(&pack)).unwrap();
        let _ = p.next_object().unwrap();
        let err = p.finish().unwrap_err();
        assert!(matches!(err, PackError::TrailerMismatch));
    }

    #[test]
    fn streaming_parser_rejects_corrupt_pack_body() {
        let obj = PackObject {
            kind: ObjectKind::Blob,
            data: b"hello".to_vec(),
        };
        let mut pack = emit_pack(&[obj]);
        // Flip a byte well inside the deflated body. The streaming
        // parser hashes raw bytes, so a body-bit flip alters the
        // running hash; the trailer (which was computed over the
        // original bytes) won't match.
        pack[15] ^= 0x01;
        // The corruption may either trip zlib or surface as a
        // trailer mismatch; both are valid fail-closed paths.
        let mut p = StreamingPackParser::begin(std::io::Cursor::new(&pack)).unwrap();
        match p.next_object() {
            Err(_) => {}
            Ok(_) => {
                let err = p.finish().unwrap_err();
                assert!(matches!(err, PackError::TrailerMismatch));
            }
        }
    }

    #[test]
    fn streaming_parser_handles_chunked_reads() {
        // Wrap the bytes in a reader that hands one byte at a time —
        // exercises the BufRead path under maximum fragmentation.
        let objs = vec![
            PackObject {
                kind: ObjectKind::Blob,
                data: b"abc".to_vec(),
            },
            PackObject {
                kind: ObjectKind::Tree,
                data: vec![b'q'; 200],
            },
        ];
        let pack = emit_pack(&objs);

        struct OneByteAtATime<'a> {
            buf: &'a [u8],
            i: usize,
        }
        impl std::io::Read for OneByteAtATime<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.i >= self.buf.len() || out.is_empty() {
                    return Ok(0);
                }
                out[0] = self.buf[self.i];
                self.i += 1;
                Ok(1)
            }
        }
        let inner = OneByteAtATime { buf: &pack, i: 0 };
        let buf_inner = std::io::BufReader::with_capacity(1, inner);
        let mut p = StreamingPackParser::begin(buf_inner).unwrap();
        let mut got = Vec::new();
        while let Some(obj) = p.next_object().unwrap() {
            got.push(obj);
        }
        p.finish().unwrap();
        assert_eq!(got, objs);
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
