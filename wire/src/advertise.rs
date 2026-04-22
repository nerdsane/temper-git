//! Smart-HTTP `/info/refs` advertisement.
//!
//! Serves the initial handshake for both `git-upload-pack` (fetch)
//! and `git-receive-pack` (push). Response format, per
//! gitprotocol-http(5):
//!
//! ```text
//!   001e# service=git-upload-pack\n
//!   0000
//!   <sha> HEAD\0<capabilities>\n   (or <sha> <first-ref>\0<caps>\n)
//!   <sha> refs/heads/main\n
//!   ...
//!   0000
//! ```
//!
//! The first data pkt-line carries the capability block attached to
//! the first ref with `\0`. Subsequent refs are bare `<sha> <name>\n`.
//! If the repo has NO refs, we emit a single "capabilities^{}" line
//! carrying a zero-sha + the caps block — the convention git uses
//! to advertise capabilities on an empty repo.
//!
//! Expected Content-Type:
//!   application/x-git-upload-pack-advertisement   for fetch
//!   application/x-git-receive-pack-advertisement  for push

use crate::capabilities::{receive_pack_capabilities, upload_pack_capabilities};
use crate::pkt_line::{encode_into, flush, PktLineError};

/// Which service is being advertised on `/info/refs?service=...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// On-wire name as it appears in the `# service=...` preamble.
    pub fn wire_name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
        }
    }

    /// Content-Type header value for the HTTP response.
    pub fn content_type(self) -> &'static str {
        match self {
            Service::UploadPack => "application/x-git-upload-pack-advertisement",
            Service::ReceivePack => "application/x-git-receive-pack-advertisement",
        }
    }

    fn capabilities(self) -> String {
        match self {
            Service::UploadPack => upload_pack_capabilities(),
            Service::ReceivePack => receive_pack_capabilities(),
        }
    }
}

/// One ref line in the advertisement: `<sha> <fully-qualified-name>`.
/// Sha is the 40-char lowercase-hex of the object the ref points at.
/// For a symbolic HEAD, prefer ref_name="HEAD" and sha=commit it
/// resolves to (we do not advertise symrefs in v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedRef<'a> {
    pub sha: &'a str,
    pub name: &'a str,
}

/// Zero-sha used by git to mean "object of this type does not
/// exist" (empty repo advertisement, or a receive-pack delete
/// command's old_sha).
pub const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// Build a smart-HTTP `/info/refs` advertisement body.
///
/// `refs` should already be sorted in a deterministic order — git
/// clients don't require ordering, but byte-exact replay across runs
/// matters for our harness. Convention matching `git ls-remote`:
/// HEAD first (if advertised), then refs/ in lexicographic order.
pub fn advertise_info_refs(
    service: Service,
    refs: &[AdvertisedRef<'_>],
) -> Result<Vec<u8>, PktLineError> {
    let mut buf: Vec<u8> = Vec::new();

    // Preamble: `# service=<name>\n` then flush.
    let preamble = format!("# service={}\n", service.wire_name());
    encode_into(&mut buf, preamble.as_bytes())?;
    flush(&mut buf);

    let caps = service.capabilities();

    if refs.is_empty() {
        // Empty-repo convention: advertise capabilities attached to
        // the sentinel "capabilities^{}" pseudo-ref on a zero sha.
        let line = format!("{ZERO_SHA} capabilities^{{}}\0{caps}\n");
        encode_into(&mut buf, line.as_bytes())?;
    } else {
        // First ref carries capabilities block.
        let first = &refs[0];
        let line = format!("{} {}\0{caps}\n", first.sha, first.name);
        encode_into(&mut buf, line.as_bytes())?;
        for r in &refs[1..] {
            let line = format!("{} {}\n", r.sha, r.name);
            encode_into(&mut buf, line.as_bytes())?;
        }
    }

    flush(&mut buf);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_str(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn preamble_shape_upload_pack() {
        let out = advertise_info_refs(Service::UploadPack, &[]).unwrap();
        // "# service=git-upload-pack\n" is 26 bytes → pkt-line 001e.
        assert_eq!(&out[..34], b"001e# service=git-upload-pack\n0000");
    }

    #[test]
    fn preamble_shape_receive_pack() {
        let out = advertise_info_refs(Service::ReceivePack, &[]).unwrap();
        // "# service=git-receive-pack\n" is 27 bytes → pkt-line 001f.
        assert_eq!(&out[..35], b"001f# service=git-receive-pack\n0000");
    }

    #[test]
    fn trailing_flush_present() {
        let out = advertise_info_refs(Service::UploadPack, &[]).unwrap();
        assert!(out.ends_with(b"0000"));
    }

    #[test]
    fn empty_repo_emits_capabilities_pseudo_ref() {
        let out = advertise_info_refs(Service::UploadPack, &[]).unwrap();
        let s = body_str(&out);
        assert!(s.contains("capabilities^{}"));
        assert!(s.contains(ZERO_SHA));
        assert!(s.contains("side-band-64k"));
    }

    #[test]
    fn single_ref_carries_capabilities_on_first_line() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let refs = &[AdvertisedRef { sha, name: "HEAD" }];
        let out = advertise_info_refs(Service::UploadPack, refs).unwrap();
        let s = body_str(&out);
        // First ref line includes NUL followed by capabilities.
        let first = s.find(sha).expect("sha must appear");
        let nul = s[first..].find('\0').expect("first ref line must have NUL separator");
        let caps_start = first + nul + 1;
        assert!(s[caps_start..].contains("side-band-64k"));
    }

    #[test]
    fn subsequent_refs_do_not_repeat_capabilities() {
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        let sha2 = "89abcdef0123456789abcdef0123456789abcdef";
        let refs = &[
            AdvertisedRef { sha: sha1, name: "HEAD" },
            AdvertisedRef { sha: sha2, name: "refs/heads/main" },
        ];
        let out = advertise_info_refs(Service::UploadPack, refs).unwrap();
        let s = body_str(&out);
        // Capability block appears exactly once.
        let occurrences = s.matches("side-band-64k").count();
        assert_eq!(occurrences, 1);
        // Second ref is present as bare line.
        assert!(s.contains("refs/heads/main"));
    }

    #[test]
    fn ref_line_length_prefix_computed_correctly() {
        // Build a known-size ref line and verify the 4-byte hex
        // length prefix is correct.
        let sha = "a".repeat(40);
        let refs = &[AdvertisedRef { sha: &sha, name: "refs/heads/x" }];
        let out = advertise_info_refs(Service::UploadPack, refs).unwrap();

        // Skip preamble (001e + 26 + 0000 = 34 bytes).
        let after_preamble = &out[34..];
        // First 4 chars are the ref-line pkt-line length.
        let hex = std::str::from_utf8(&after_preamble[..4]).unwrap();
        let declared = u32::from_str_radix(hex, 16).unwrap() as usize;
        // Find the next pkt-line boundary (flush or next length).
        // We know there's only one ref, so after this line comes the
        // trailing flush.
        let expected_end = declared;
        // `0000` flush immediately after.
        assert_eq!(&after_preamble[expected_end..expected_end + 4], b"0000");
    }
}
