//! Minimal parsers for commit + tree bodies — just enough to walk
//! the reachable-object DAG for upload-pack.
//!
//! These complement the serialisation functions in `commit.rs` and
//! `tree.rs`. The serialisers go struct → bytes (for hashing). These
//! parsers go bytes → sha references, so we can start from a commit
//! SHA and enumerate every blob/tree/commit reachable from it.
//!
//! Deliberately narrow: we extract ONLY the fields needed for the
//! walk (tree + parent SHAs for commits; child SHAs + kinds for
//! trees). Author / committer / message are preserved on disk via
//! `CanonicalBytes` but not decoded here.

use alloc::string::String;
use alloc::vec::Vec;

extern crate alloc;

/// Parsed commit: just the SHAs we need for the DAG walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRefs {
    pub tree: String,
    pub parents: Vec<String>,
}

/// Parse the canonical body bytes of a commit object (the bytes
/// that were zlib-deflated into the pack, i.e. NOT including the
/// `commit <len>\0` header prefix).
pub fn parse_commit_refs(body: &[u8]) -> Result<CommitRefs, &'static str> {
    let text = core::str::from_utf8(body).map_err(|_| "commit body not UTF-8")?;
    let mut tree: Option<String> = None;
    let mut parents: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = line.strip_prefix("tree ") {
            tree = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("parent ") {
            parents.push(rest.trim().to_string());
        }
        // author / committer / gpgsig etc. — ignored at this layer.
    }
    let tree = tree.ok_or("commit missing tree")?;
    Ok(CommitRefs { tree, parents })
}

/// Fully decoded commit: every header field plus the message.
///
/// `gpg_signature` is `Some` iff the commit had a `gpgsig` header.
/// Continuation lines (the multi-line PGP body) are joined back into
/// the original byte sequence with `\n ` markers stripped, so the
/// returned signature is the bare PEM block. Callers that need to
/// re-emit a canonical commit should keep the original bytes via
/// `CanonicalBytes`; this is for OData metadata fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committer: String,
    pub message: String,
    pub gpg_signature: Option<String>,
}

/// Full commit parse: extracts every metadata field needed to populate
/// the OData `Commit` row.
pub fn parse_commit(body: &[u8]) -> Result<ParsedCommit, &'static str> {
    let text = core::str::from_utf8(body).map_err(|_| "commit body not UTF-8")?;

    // Headers end at the first empty line; everything after is the
    // message. Track byte offset so the message can include any
    // newlines verbatim.
    let blank = find_blank_header_line(text).ok_or("commit missing blank line")?;
    let (header_block, message_block) = text.split_at(blank);
    // message_block starts with the blank "\n\n" or "\n"; strip the
    // single header-terminating newline so the message itself starts
    // cleanly.
    let message = message_block.strip_prefix("\n\n").unwrap_or(
        message_block.strip_prefix('\n').unwrap_or(message_block),
    );

    let headers = collect_headers(header_block);
    let mut tree: Option<String> = None;
    let mut parents: Vec<String> = Vec::new();
    let mut author: Option<String> = None;
    let mut committer: Option<String> = None;
    let mut gpg_signature: Option<String> = None;

    for (key, value) in headers {
        match key {
            "tree" => tree = Some(value),
            "parent" => parents.push(value),
            "author" => author = Some(value),
            "committer" => committer = Some(value),
            "gpgsig" => gpg_signature = Some(value),
            _ => {}
        }
    }

    Ok(ParsedCommit {
        tree: tree.ok_or("commit missing tree")?,
        parents,
        author: author.ok_or("commit missing author")?,
        committer: committer.ok_or("commit missing committer")?,
        message: message.to_string(),
        gpg_signature,
    })
}

/// Fully decoded annotated tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTag {
    pub object: String,
    pub target_type: String,
    pub tag: String,
    pub tagger: String,
    pub message: String,
    pub gpg_signature: Option<String>,
}

/// Parse an annotated tag's body bytes (without the `tag <len>\0`
/// header prefix).
pub fn parse_tag(body: &[u8]) -> Result<ParsedTag, &'static str> {
    let text = core::str::from_utf8(body).map_err(|_| "tag body not UTF-8")?;
    let blank = find_blank_header_line(text).ok_or("tag missing blank line")?;
    let (header_block, message_block) = text.split_at(blank);
    let message = message_block.strip_prefix("\n\n").unwrap_or(
        message_block.strip_prefix('\n').unwrap_or(message_block),
    );

    let mut object: Option<String> = None;
    let mut target_type: Option<String> = None;
    let mut tag: Option<String> = None;
    let mut tagger: Option<String> = None;
    let mut gpg_signature: Option<String> = None;

    for (key, value) in collect_headers(header_block) {
        match key {
            "object" => object = Some(value),
            "type" => target_type = Some(value),
            "tag" => tag = Some(value),
            "tagger" => tagger = Some(value),
            "gpgsig" => gpg_signature = Some(value),
            _ => {}
        }
    }

    Ok(ParsedTag {
        object: object.ok_or("tag missing object")?,
        target_type: target_type.ok_or("tag missing type")?,
        tag: tag.ok_or("tag missing tag")?,
        tagger: tagger.ok_or("tag missing tagger")?,
        message: message.to_string(),
        gpg_signature,
    })
}

/// Find the byte offset of the blank line that separates headers
/// from the message. Returns the offset of the `\n` that is
/// immediately followed by another `\n`, or `None` if no such pair.
fn find_blank_header_line(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Walk header lines, joining continuation lines (those starting
/// with a single space) back into their owning header value. Returns
/// `(key, value)` pairs in the order they appeared.
fn collect_headers(header_block: &str) -> Vec<(&str, String)> {
    let mut out: Vec<(&str, String)> = Vec::new();
    for line in header_block.split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix(' ') {
            // Continuation of the prior header (PGP body).
            if let Some((_, value)) = out.last_mut() {
                value.push('\n');
                value.push_str(rest);
            }
            continue;
        }
        match line.split_once(' ') {
            Some((key, value)) => out.push((key, value.to_string())),
            None => out.push((line, String::new())),
        }
    }
    out
}

/// One entry in a tree — mode + name + child SHA (and whether that
/// child is itself a tree, from the mode bits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTreeEntry {
    pub mode: String,
    pub name: String,
    pub sha: String,
    pub is_tree: bool,
}

/// Parse the canonical body bytes of a tree object.
///
/// Tree entries are concatenated:
///   `<mode> <name>\0<20 binary bytes>`
///
/// No length prefix, no separator — the 20-byte SHA is the fixed
/// ending. We iterate consuming one entry at a time.
pub fn parse_tree(body: &[u8]) -> Result<Vec<ParsedTreeEntry>, &'static str> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < body.len() {
        // Find the space that ends the mode.
        let sp = match body[i..].iter().position(|&b| b == b' ') {
            Some(p) => i + p,
            None => return Err("tree entry missing mode-separator space"),
        };
        let mode_bytes = &body[i..sp];
        let mode =
            core::str::from_utf8(mode_bytes).map_err(|_| "tree mode not UTF-8")?.to_string();

        // Name up to NUL.
        let after_sp = sp + 1;
        let nul = match body[after_sp..].iter().position(|&b| b == 0) {
            Some(p) => after_sp + p,
            None => return Err("tree entry missing name-terminator NUL"),
        };
        let name_bytes = &body[after_sp..nul];
        let name = core::str::from_utf8(name_bytes)
            .map_err(|_| "tree name not UTF-8")?
            .to_string();

        // 20-byte binary SHA.
        let sha_start = nul + 1;
        let sha_end = sha_start + 20;
        if sha_end > body.len() {
            return Err("tree entry SHA truncated");
        }
        let sha = hex(&body[sha_start..sha_end]);

        // Mode "40000" means tree; anything else is blob/commit/symlink.
        let is_tree = mode == "40000";

        out.push(ParsedTreeEntry {
            mode,
            name,
            sha,
            is_tree,
        });
        i = sha_end;
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commit_with_one_parent() {
        let body = b"tree 7d4a466af82cd6857c85c0296d5c23fc68cba887\n\
                     parent 3a21d1d7f95fda510925f0e5e2566abf137fb490\n\
                     author T <t@x> 1700000000 +0000\n\
                     committer T <t@x> 1700000000 +0000\n\
                     \n\
                     message body\n";
        let refs = parse_commit_refs(body).unwrap();
        assert_eq!(refs.tree, "7d4a466af82cd6857c85c0296d5c23fc68cba887");
        assert_eq!(refs.parents, vec!["3a21d1d7f95fda510925f0e5e2566abf137fb490"]);
    }

    #[test]
    fn parse_commit_no_parent() {
        let body = b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
                     author T <t@x> 0 +0000\n\
                     committer T <t@x> 0 +0000\n\
                     \n\
                     initial\n";
        let refs = parse_commit_refs(body).unwrap();
        assert!(refs.parents.is_empty());
    }

    #[test]
    fn parse_commit_multi_parent_merge() {
        let body = b"tree ffffffffffffffffffffffffffffffffffffffff\n\
                     parent 1111111111111111111111111111111111111111\n\
                     parent 2222222222222222222222222222222222222222\n\
                     author T <t@x> 0 +0000\n\
                     committer T <t@x> 0 +0000\n\
                     \n\
                     merge\n";
        let refs = parse_commit_refs(body).unwrap();
        assert_eq!(refs.parents.len(), 2);
    }

    #[test]
    fn parse_tree_single_entry() {
        // 100644 README\0<20 bytes of sha binary>
        let sha_bytes: [u8; 20] = [
            0xce, 0x01, 0x36, 0x25, 0x03, 0x0b, 0xa8, 0xdb, 0xa9, 0x06, 0xf7, 0x56, 0x96, 0x7f,
            0x9e, 0x9c, 0xa3, 0x94, 0x46, 0x4a,
        ];
        let mut body = Vec::new();
        body.extend_from_slice(b"100644 README\0");
        body.extend_from_slice(&sha_bytes);
        let entries = parse_tree(&body).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mode, "100644");
        assert_eq!(entries[0].name, "README");
        assert_eq!(entries[0].sha, "ce013625030ba8dba906f756967f9e9ca394464a");
        assert!(!entries[0].is_tree);
    }

    #[test]
    fn parse_tree_subtree_entry_flagged_is_tree() {
        let sha_bytes: [u8; 20] = [0u8; 20];
        let mut body = Vec::new();
        body.extend_from_slice(b"40000 subdir\0");
        body.extend_from_slice(&sha_bytes);
        let entries = parse_tree(&body).unwrap();
        assert!(entries[0].is_tree);
    }

    #[test]
    fn parse_tree_multi_entry() {
        let mut body = Vec::new();
        let sha_a: [u8; 20] = [0x11; 20];
        let sha_b: [u8; 20] = [0x22; 20];
        body.extend_from_slice(b"100644 a.txt\0");
        body.extend_from_slice(&sha_a);
        body.extend_from_slice(b"100644 b.txt\0");
        body.extend_from_slice(&sha_b);
        let entries = parse_tree(&body).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[1].name, "b.txt");
    }

    #[test]
    fn parse_tree_rejects_truncated_sha() {
        let mut body = Vec::new();
        body.extend_from_slice(b"100644 README\0");
        body.extend_from_slice(&[0u8; 10]); // only 10 bytes instead of 20
        assert!(parse_tree(&body).is_err());
    }

    #[test]
    fn parse_commit_full_extracts_metadata() {
        let body = b"tree 7d4a466af82cd6857c85c0296d5c23fc68cba887\n\
                     parent 3a21d1d7f95fda510925f0e5e2566abf137fb490\n\
                     author Alice <alice@example.com> 1700000000 +0000\n\
                     committer Bob <bob@example.com> 1700000100 -0500\n\
                     \n\
                     fix the bug\n\nlonger explanation here\n";
        let c = parse_commit(body).unwrap();
        assert_eq!(c.tree, "7d4a466af82cd6857c85c0296d5c23fc68cba887");
        assert_eq!(c.parents, vec!["3a21d1d7f95fda510925f0e5e2566abf137fb490"]);
        assert_eq!(c.author, "Alice <alice@example.com> 1700000000 +0000");
        assert_eq!(c.committer, "Bob <bob@example.com> 1700000100 -0500");
        assert_eq!(c.message, "fix the bug\n\nlonger explanation here\n");
        assert!(c.gpg_signature.is_none());
    }

    #[test]
    fn parse_commit_full_handles_merge() {
        let body = b"tree ffffffffffffffffffffffffffffffffffffffff\n\
                     parent 1111111111111111111111111111111111111111\n\
                     parent 2222222222222222222222222222222222222222\n\
                     author T <t@x> 0 +0000\n\
                     committer T <t@x> 0 +0000\n\
                     \n\
                     merge branch foo\n";
        let c = parse_commit(body).unwrap();
        assert_eq!(c.parents.len(), 2);
        assert_eq!(c.message, "merge branch foo\n");
    }

    #[test]
    fn parse_commit_full_recovers_gpg_signature() {
        // gpgsig is multi-line; continuation lines start with a
        // single space which we strip.
        let body = b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
                     author T <t@x> 0 +0000\n\
                     committer T <t@x> 0 +0000\n\
                     gpgsig -----BEGIN PGP SIGNATURE-----\n \n iQ123\n -----END PGP SIGNATURE-----\n\
                     \n\
                     signed commit\n";
        let c = parse_commit(body).unwrap();
        let sig = c.gpg_signature.expect("gpg signature present");
        assert!(sig.starts_with("-----BEGIN PGP SIGNATURE-----"));
        assert!(sig.ends_with("-----END PGP SIGNATURE-----"));
        assert!(sig.contains("iQ123"));
    }

    #[test]
    fn parse_tag_extracts_metadata() {
        let body = b"object 1234567890123456789012345678901234567890\n\
                     type commit\n\
                     tag v0.1.0\n\
                     tagger Releaser <r@x> 1700000000 +0000\n\
                     \n\
                     Release notes\n";
        let t = parse_tag(body).unwrap();
        assert_eq!(t.object, "1234567890123456789012345678901234567890");
        assert_eq!(t.target_type, "commit");
        assert_eq!(t.tag, "v0.1.0");
        assert_eq!(t.tagger, "Releaser <r@x> 1700000000 +0000");
        assert_eq!(t.message, "Release notes\n");
    }

    #[test]
    fn parse_commit_rejects_missing_tree() {
        let body = b"author T <t@x> 0 +0000\n\
                     committer T <t@x> 0 +0000\n\
                     \n\
                     no tree\n";
        assert!(parse_commit(body).is_err());
    }
}
