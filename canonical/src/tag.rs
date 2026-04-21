//! Annotated tag object serialization.
//!
//! Canonical form:
//!
//! ```text
//! object <target_sha>\n
//! type <target_type>\n       ("commit" | "tree" | "blob" | "tag")
//! tag <tag_name>\n
//! tagger <name> <<email>> <unix_ts> <tz>\n
//! \n
//! <message bytes>
//! [-----BEGIN PGP SIGNATURE-----\n...\n-----END PGP SIGNATURE-----\n]
//! ```
//!
//! Lightweight tags (a ref pointing directly at a commit) don't have a
//! Tag object at all — they're just a Ref row. This module handles
//! annotated tags only.
//!
//! Unlike commits, tag PGP signatures are appended **after** the message
//! (not emitted as a `gpgsig` header). Don't conflate the two.

use crate::sha1::Sha1;
use crate::Oid;

/// An annotated tag in structured form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Object being tagged.
    pub object: Oid,
    /// `commit` | `tree` | `blob` | `tag`.
    pub target_type: String,
    /// Tag name (e.g. `v1.0.0`).
    pub tag: String,
    /// `Name <email> <unix_ts> <tz>`.
    pub tagger: String,
    /// Message body. Caller controls trailing newlines.
    pub message: String,
    /// Optional signature. When present, appended verbatim after the
    /// message — the caller is responsible for the `\n` separator
    /// between message and signature, and for including the
    /// `BEGIN/END PGP SIGNATURE` block markers.
    pub pgp_signature: Option<String>,
}

/// Canonical byte sequence for the tag, input to SHA-1.
pub fn tag_canonical_bytes(tag: &Tag) -> Vec<u8> {
    let body = tag_body_bytes(tag);
    let header = format!("tag {}\0", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&body);
    out
}

/// SHA-1 hex of the tag's canonical bytes. Matches the 40-char
/// digest git assigns to an annotated tag.
pub fn tag_hash(tag: &Tag) -> Oid {
    let bytes = tag_canonical_bytes(tag);
    let mut h = Sha1::new();
    h.update(&bytes);
    h.hex()
}

fn tag_body_bytes(tag: &Tag) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"object ");
    out.extend_from_slice(tag.object.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(b"type ");
    out.extend_from_slice(tag.target_type.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(b"tag ");
    out.extend_from_slice(tag.tag.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(b"tagger ");
    out.extend_from_slice(tag.tagger.as_bytes());
    out.push(b'\n');
    out.push(b'\n');
    out.extend_from_slice(tag.message.as_bytes());
    if let Some(sig) = &tag.pgp_signature {
        out.extend_from_slice(sig.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_bytes_structure() {
        let t = Tag {
            object: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string(),
            target_type: "commit".to_string(),
            tag: "v1.0.0".to_string(),
            tagger: "T <t@e.com> 1 +0000".to_string(),
            message: "release one\n".to_string(),
            pgp_signature: None,
        };
        let bytes = tag_canonical_bytes(&t);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("tag "));
        let body_start = s.find('\0').unwrap() + 1;
        let body = &s[body_start..];
        assert!(body.starts_with("object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n"));
        assert!(body.contains("\ntype commit\n"));
        assert!(body.contains("\ntag v1.0.0\n"));
        assert!(body.contains("\ntagger T <t@e.com> 1 +0000\n"));
        assert!(body.ends_with("\nrelease one\n"));
    }

    #[test]
    fn signed_tag_appends_sig_after_message() {
        let t = Tag {
            object: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string(),
            target_type: "commit".to_string(),
            tag: "v1.0.0".to_string(),
            tagger: "T <t@e.com> 1 +0000".to_string(),
            message: "release\n".to_string(),
            pgp_signature: Some(
                "-----BEGIN PGP SIGNATURE-----\nABC\n-----END PGP SIGNATURE-----\n".to_string(),
            ),
        };
        let bytes = tag_canonical_bytes(&t);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.ends_with("-----END PGP SIGNATURE-----\n"));
        // Signature follows message without a `gpgsig` header.
        assert!(s.contains("\nrelease\n-----BEGIN PGP SIGNATURE-----\n"));
    }
}
