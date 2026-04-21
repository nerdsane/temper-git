//! Blob object serialization.
//!
//! A git blob is the simplest object: just file contents. Its canonical
//! form is:
//!
//! ```text
//! blob <content-length-decimal>\0<raw content bytes>
//! ```
//!
//! Every byte is exactly what `git hash-object -w` produces given the
//! same input. No trailing newline is added; the raw content is
//! emitted verbatim.

use crate::sha1::Sha1;
use crate::Oid;

/// Build the exact byte sequence git-core uses as the input to SHA-1
/// for a blob of `content` bytes.
///
/// Output: `b"blob " + decimal_len + b"\0" + content`.
pub fn blob_canonical_bytes(content: &[u8]) -> Vec<u8> {
    let header = format!("blob {}\0", content.len());
    let mut out = Vec::with_capacity(header.len() + content.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(content);
    out
}

/// The 40-char lowercase SHA-1 hex of a blob's canonical bytes. Matches
/// `git hash-object -t blob <file>` byte-for-byte.
pub fn blob_hash(content: &[u8]) -> Oid {
    let mut h = Sha1::new();
    let header = format!("blob {}\0", content.len());
    h.update(header.as_bytes());
    h.update(content);
    h.hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Well-known hash: the empty blob. git ships a sentinel for this —
    /// any git repo contains this object under
    /// `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`.
    #[test]
    fn empty_blob() {
        assert_eq!(blob_hash(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    /// `git hash-object` for `hello\n` → known constant. Try it:
    /// `printf 'hello\n' | git hash-object --stdin`.
    #[test]
    fn hello_newline() {
        assert_eq!(
            blob_hash(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    /// `hello` without trailing newline — different hash, different
    /// canonical bytes.
    #[test]
    fn hello_no_newline() {
        assert_eq!(
            blob_hash(b"hello"),
            "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
        );
    }

    #[test]
    fn canonical_bytes_header_format() {
        let b = blob_canonical_bytes(b"abc");
        assert_eq!(&b, b"blob 3\0abc");
    }

    #[test]
    fn canonical_bytes_empty() {
        let b = blob_canonical_bytes(b"");
        assert_eq!(&b, b"blob 0\0");
    }

    #[test]
    fn binary_content() {
        // Blob containing the NUL byte mid-content. Real tree of a repo
        // can have this in executables / images. Our serializer must
        // pass NUL through verbatim.
        let content = b"ELF\0\x01\x02\x03";
        let bytes = blob_canonical_bytes(content);
        // Header: "blob 7\0"
        assert_eq!(&bytes[..7], b"blob 7\0");
        assert_eq!(&bytes[7..], content);
    }

    #[test]
    fn large_content_length_formatting() {
        // length 10000 should render as "10000" (no padding, no separators).
        let content = vec![0u8; 10_000];
        let bytes = blob_canonical_bytes(&content);
        assert!(bytes.starts_with(b"blob 10000\0"));
    }
}
