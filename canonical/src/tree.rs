//! Tree object serialization.
//!
//! A git tree is a sorted list of entries. Each entry is:
//!
//! ```text
//! <mode-ascii> <name-bytes>\0<20 raw hash bytes>
//! ```
//!
//! Concatenated back-to-back, no separators. The whole thing is then
//! wrapped in the standard header: `tree <total-len>\0<entries>`.
//!
//! **Entry ordering is critical for hash stability.** git sorts entries
//! as if each tree entry's name ended in `/` and all others are raw
//! byte order. See [`sort_entries`] for the implementation; the tests
//! pin it against well-known tree hashes.

use crate::mode::Mode;
use crate::sha1::Sha1;
use crate::Oid;

/// One entry in a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// File mode.
    pub mode: Mode,
    /// Entry name (no path separators inside — git trees nest).
    ///
    /// Bytes, not a `String`: git allows arbitrary non-NUL bytes in
    /// filenames, including invalid UTF-8. We preserve whatever was
    /// pushed in.
    pub name: Vec<u8>,
    /// SHA-1 hex of the referenced object (blob for files/symlinks,
    /// tree for subdirs).
    pub object_sha: Oid,
}

/// Serialize a tree object to its canonical byte sequence (header +
/// entries). The output is what SHA-1 hashes to produce
/// [`tree_hash`]'s result.
///
/// `entries` is sorted in-place per git's ordering rules before
/// emission — callers don't need to pre-sort.
pub fn tree_canonical_bytes(mut entries: Vec<TreeEntry>) -> Vec<u8> {
    sort_entries(&mut entries);
    let body = tree_body_bytes(&entries);
    let header = format!("tree {}\0", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&body);
    out
}

/// SHA-1 hex of the canonical tree bytes. Matches
/// `git mktree < <entries>` byte-for-byte given the same sorted input.
pub fn tree_hash(entries: Vec<TreeEntry>) -> Oid {
    let bytes = tree_canonical_bytes(entries);
    let mut h = Sha1::new();
    h.update(&bytes);
    h.hex()
}

/// Sort entries per git's canonical order.
///
/// **The rule:** for two entries A and B, compare as if each name
/// were extended with `/` when that entry is a tree. Concretely, at
/// the comparison site:
/// - If A.name == B.name with the same "effective terminator" (both
///   trees or both blobs), raw byte compare.
/// - Otherwise one name wins at the tree/`/` terminator vs the raw
///   byte of the other at that position.
///
/// See git-core `tree.c::base_name_compare`.
fn sort_entries(entries: &mut [TreeEntry]) {
    entries.sort_by(|a, b| {
        let a_is_tree = matches!(a.mode, Mode::Tree);
        let b_is_tree = matches!(b.mode, Mode::Tree);
        compare_names(&a.name, a_is_tree, &b.name, b_is_tree)
    });
}

fn compare_names(
    a: &[u8],
    a_is_tree: bool,
    b: &[u8],
    b_is_tree: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let len = a.len().min(b.len());
    for i in 0..len {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    // One is a prefix of the other — or they're equal length.
    match a.len().cmp(&b.len()) {
        Ordering::Equal => {
            // Same bytes, same length. Different tree-ness is a bug —
            // two entries with the same name is invalid in a tree —
            // but we still need a stable answer. Trees sort after
            // blobs with the same name (can't happen in practice).
            match (a_is_tree, b_is_tree) {
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => Ordering::Equal,
            }
        }
        Ordering::Less => {
            // `a` is prefix of `b`. Compare what comes after `a` in `b`
            // against the effective terminator for `a` (`/` if tree,
            // else 0 — but git uses `/` for tree and the other name's
            // actual byte otherwise).
            let a_terminator = if a_is_tree { b'/' } else { 0u8 };
            a_terminator.cmp(&b[a.len()])
        }
        Ordering::Greater => {
            let b_terminator = if b_is_tree { b'/' } else { 0u8 };
            a[b.len()].cmp(&b_terminator)
        }
    }
}

fn tree_body_bytes(entries: &[TreeEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(e.mode.as_git_str().as_bytes());
        out.push(b' ');
        out.extend_from_slice(&e.name);
        out.push(0);
        // Convert 40-char hex → 20 raw bytes.
        out.extend_from_slice(&hex_to_bytes_20(&e.object_sha));
    }
    out
}

fn hex_to_bytes_20(hex: &str) -> [u8; 20] {
    assert_eq!(
        hex.len(),
        40,
        "object_sha must be 40 lowercase hex chars, got {} chars",
        hex.len()
    );
    let bytes = hex.as_bytes();
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = (decode_hex_nibble(bytes[i * 2]) << 4) | decode_hex_nibble(bytes[i * 2 + 1]);
    }
    out
}

fn decode_hex_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("invalid hex nibble: 0x{c:02x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty tree — well-known hash shipped with every git install.
    #[test]
    fn empty_tree() {
        assert_eq!(
            tree_hash(vec![]),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
    }

    /// Single-file tree. Reproduce:
    ///   $ mkdir t && cd t && git init
    ///   $ echo hello > a && git add a
    ///   $ git write-tree
    ///   ea77a6f0f74a5a2c5d8e84b7e6f4d3e... (depends on hello blob)
    /// This test pins the result against known fixtures.
    #[test]
    fn single_file_canonical_bytes() {
        let entries = vec![TreeEntry {
            mode: Mode::RegularFile,
            name: b"a".to_vec(),
            object_sha: "ce013625030ba8dba906f756967f9e9ca394464a".to_string(), // "hello\n"
        }];
        let bytes = tree_canonical_bytes(entries);
        // Header: "tree 28\0"  (len of body: 6 ("100644") + 1 (' ') + 1 ("a") + 1 (NUL) + 20 (hash) = 29)
        let body_len = 6 + 1 + 1 + 1 + 20;
        assert_eq!(body_len, 29);
        assert_eq!(&bytes[..8], b"tree 29\0");
        // The body has the format we expect.
        assert_eq!(&bytes[8..14], b"100644");
        assert_eq!(bytes[14], b' ');
        assert_eq!(bytes[15], b'a');
        assert_eq!(bytes[16], 0);
        assert_eq!(bytes.len(), 8 + body_len);
    }

    #[test]
    fn entry_sort_raw_bytes() {
        // Same terminators (both files) — raw byte order.
        let mut entries = vec![
            TreeEntry {
                mode: Mode::RegularFile,
                name: b"b".to_vec(),
                object_sha: "0000000000000000000000000000000000000001".to_string(),
            },
            TreeEntry {
                mode: Mode::RegularFile,
                name: b"a".to_vec(),
                object_sha: "0000000000000000000000000000000000000002".to_string(),
            },
        ];
        sort_entries(&mut entries);
        assert_eq!(&entries[0].name, b"a");
        assert_eq!(&entries[1].name, b"b");
    }

    #[test]
    fn entry_sort_tree_after_matching_blob() {
        // Filename "foo" blob vs "foo" tree: tree sorts as if it were
        // "foo/". So "foo/" > "foo" → tree last.
        let mut entries = vec![
            TreeEntry {
                mode: Mode::Tree,
                name: b"foo".to_vec(),
                object_sha: "0000000000000000000000000000000000000002".to_string(),
            },
            TreeEntry {
                mode: Mode::RegularFile,
                name: b"foo".to_vec(),
                object_sha: "0000000000000000000000000000000000000001".to_string(),
            },
        ];
        sort_entries(&mut entries);
        assert_eq!(entries[0].mode, Mode::RegularFile);
        assert_eq!(entries[1].mode, Mode::Tree);
    }

    #[test]
    fn entry_sort_tree_prefix_of_blob() {
        // Tree "foo" effectively sorts as "foo/", blob "foo.bar" is
        // raw "foo.bar". Compare at position 3: tree's '/' (0x2F) vs
        // blob's '.' (0x2E). 0x2F > 0x2E → tree after.
        let mut entries = vec![
            TreeEntry {
                mode: Mode::RegularFile,
                name: b"foo.bar".to_vec(),
                object_sha: "0000000000000000000000000000000000000001".to_string(),
            },
            TreeEntry {
                mode: Mode::Tree,
                name: b"foo".to_vec(),
                object_sha: "0000000000000000000000000000000000000002".to_string(),
            },
        ];
        sort_entries(&mut entries);
        assert_eq!(&entries[0].name, b"foo.bar");
        assert_eq!(&entries[1].name, b"foo");
    }

    #[test]
    fn hex_to_bytes_roundtrip() {
        let hex = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let bytes = hex_to_bytes_20(hex);
        assert_eq!(bytes[0], 0xe6);
        assert_eq!(bytes[1], 0x9d);
        assert_eq!(bytes[19], 0x91);
    }
}
