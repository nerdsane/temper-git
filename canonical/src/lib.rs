//! # tg-canonical — byte-exact git object serialization + SHA-1
//!
//! This crate is the foundation of temper-git's byte-exact compat
//! contract (see [ADR-0003][adr-0003]). It does exactly one thing:
//! serialize and hash git objects identically to what `git hash-object`
//! produces.
//!
//! Every object type has two operations:
//!
//! 1. `canonical_bytes(...)` — the exact byte sequence git-core uses as
//!    the input to SHA-1. For a blob, that's `blob <len>\0<content>`.
//!    For a tree, it's the sorted entries concatenated with raw 20-byte
//!    hashes. For a commit, it's the header-lines + blank + message.
//!    For a tag, similar.
//! 2. `hash(...)` — SHA-1 of the canonical bytes, returned as a 40-char
//!    lower-hex string.
//!
//! ## Hash-byte-match contract
//!
//! Every test in this crate asserts that our output matches
//! `git hash-object` / `git cat-file` for a fixture on disk. If a test
//! fails, the contract is broken and we fix it before anything else ships.
//!
//! ## No-deps
//!
//! SHA-1 is implemented inline in [`sha1`]. We deliberately do not
//! depend on `sha1`/`sha-1`/`openssl` — (a) SHA-1 is not a primitive
//! we need updates for (git-core is locked to SHA-1 for decades);
//! (b) fewer crates in the WASM graph = smaller binaries + cleaner
//! supply chain.
//!
//! [adr-0003]: https://github.com/nerdsane/temper-git/blob/main/docs/adr/0003-byte-exact-git-compat.md

#![forbid(unsafe_code)]

pub mod blob;
pub mod commit;
pub mod mode;
pub mod parse;
pub mod sha1;
pub mod tag;
pub mod tree;

pub use blob::{blob_canonical_bytes, blob_hash};
pub use commit::{Commit, commit_canonical_bytes, commit_hash};
pub use mode::Mode;
pub use parse::{
    parse_commit, parse_commit_refs, parse_tag, parse_tree, CommitRefs, ParsedCommit,
    ParsedTag, ParsedTreeEntry,
};
pub use sha1::{Sha1, sha1_hex};
pub use tag::{Tag, tag_canonical_bytes, tag_hash};
pub use tree::{TreeEntry, tree_canonical_bytes, tree_hash};

/// A 20-byte SHA-1 digest, rendered as 40 lowercase hex characters.
///
/// Used everywhere a git-object identity is exchanged.
pub type Oid = String;
