//! Integration tests that shell out to the real `git` CLI and assert
//! our canonical serialization + SHA-1 matches byte-for-byte.
//!
//! This is the gate from ADR-0003 "Byte-exact git compatibility": if
//! any test here fails, the compat contract is broken and no
//! downstream work can proceed.
//!
//! Run: `cargo test --package tg-canonical --test git_parity`.
//! Skip: set `TG_SKIP_GIT_PARITY=1` (useful in CI environments
//! without a git binary; not recommended — CI should install git).

use std::io::Write;
use std::process::{Command, Stdio};

use tg_canonical::{blob_hash, commit_hash, tag_hash, tree_hash, Commit, Mode, Tag, TreeEntry};

fn git_available() -> bool {
    if std::env::var("TG_SKIP_GIT_PARITY").ok().as_deref() == Some("1") {
        return false;
    }
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `git hash-object --stdin -t <type>` with `content` piped in.
/// Returns the 40-char hex hash that git computed.
fn git_hash_object(content: &[u8], kind: &str) -> String {
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin", "-t", kind])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git hash-object");
    child
        .stdin
        .as_mut()
        .expect("git stdin")
        .write_all(content)
        .expect("write content");
    let out = child.wait_with_output().expect("git hash-object output");
    assert!(
        out.status.success(),
        "git hash-object failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("git stdout utf8")
        .trim()
        .to_string()
}

/// Create a throwaway git repo at a tempdir; return the path.
fn mktemp_repo() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "tg-canonical-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).expect("mkdir temp repo");
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&base)
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    base
}

// ── Blobs ────────────────────────────────────────────────────────────

#[test]
fn blob_empty_matches_git() {
    if !git_available() {
        return;
    }
    assert_eq!(blob_hash(b""), git_hash_object(b"", "blob"));
}

#[test]
fn blob_hello_newline_matches_git() {
    if !git_available() {
        return;
    }
    assert_eq!(blob_hash(b"hello\n"), git_hash_object(b"hello\n", "blob"));
}

#[test]
fn blob_binary_with_nul_matches_git() {
    if !git_available() {
        return;
    }
    let content: Vec<u8> = (0u8..=255).collect();
    assert_eq!(blob_hash(&content), git_hash_object(&content, "blob"));
}

#[test]
fn blob_1mib_matches_git() {
    if !git_available() {
        return;
    }
    let content = vec![0xAAu8; 1024 * 1024];
    assert_eq!(blob_hash(&content), git_hash_object(&content, "blob"));
}

// ── Trees ────────────────────────────────────────────────────────────

fn git_write_tree(repo: &std::path::Path, entries: &[TreeEntry]) -> String {
    // Build the same tree in the real repo via `git update-index` +
    // `git write-tree`. We use `update-index --add --cacheinfo
    // <mode>,<sha>,<path>` to inject entries without touching the
    // filesystem.
    for e in entries {
        let mode = e.mode.as_git_str();
        let cacheinfo = format!("{},{},{}", mode, e.object_sha, String::from_utf8_lossy(&e.name));
        let out = Command::new("git")
            .args(["update-index", "--add", "--cacheinfo", &cacheinfo])
            .current_dir(repo)
            .output()
            .expect("git update-index");
        assert!(
            out.status.success(),
            "git update-index failed for {:?}: {}",
            cacheinfo,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = Command::new("git")
        .arg("write-tree")
        .current_dir(repo)
        .output()
        .expect("git write-tree");
    assert!(
        out.status.success(),
        "git write-tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn seed_blob(repo: &std::path::Path, content: &[u8]) -> String {
    // Write the blob into the repo's object store via `git hash-object -w`.
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "--stdin", "-t", "blob"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git hash-object -w");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(content)
        .expect("write blob");
    let out = child.wait_with_output().expect("git hash-object output");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn tree_empty_matches_git() {
    if !git_available() {
        return;
    }
    let repo = mktemp_repo();
    let git_sha = {
        // Writing an empty tree: git update-index with nothing, then
        // write-tree. But we need a clean index — fresh repo gives us
        // that.
        let out = Command::new("git")
            .arg("write-tree")
            .current_dir(&repo)
            .output()
            .expect("git write-tree");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    assert_eq!(tree_hash(vec![]), git_sha);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn tree_single_file_matches_git() {
    if !git_available() {
        return;
    }
    let repo = mktemp_repo();
    let blob_sha = seed_blob(&repo, b"hello\n");
    let entries = vec![TreeEntry {
        mode: Mode::RegularFile,
        name: b"a".to_vec(),
        object_sha: blob_sha.clone(),
    }];
    let ours = tree_hash(entries.clone());
    let theirs = git_write_tree(&repo, &entries);
    assert_eq!(ours, theirs);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn tree_sort_order_matches_git() {
    if !git_available() {
        return;
    }
    let repo = mktemp_repo();
    let blob = seed_blob(&repo, b"x");
    // Intentionally unsorted — git sorts internally; we sort
    // internally; both should end at the same hash.
    let entries = vec![
        TreeEntry {
            mode: Mode::RegularFile,
            name: b"zeta".to_vec(),
            object_sha: blob.clone(),
        },
        TreeEntry {
            mode: Mode::RegularFile,
            name: b"alpha".to_vec(),
            object_sha: blob.clone(),
        },
        TreeEntry {
            mode: Mode::RegularFile,
            name: b"gamma".to_vec(),
            object_sha: blob.clone(),
        },
    ];
    let ours = tree_hash(entries.clone());
    let theirs = git_write_tree(&repo, &entries);
    assert_eq!(ours, theirs);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn tree_mixed_blob_and_tree_matches_git() {
    if !git_available() {
        return;
    }
    let repo = mktemp_repo();
    let blob = seed_blob(&repo, b"x");

    // Build the subtree via update-index + write-tree in an isolated
    // index scope (clear index first, then add just inner entry).
    Command::new("git")
        .args(["read-tree", "--empty"])
        .current_dir(&repo)
        .output()
        .expect("reset index");
    let subtree_entries = vec![TreeEntry {
        mode: Mode::RegularFile,
        name: b"inner".to_vec(),
        object_sha: blob.clone(),
    }];
    let subtree_sha = git_write_tree(&repo, &subtree_entries);

    // Reset index and build the outer tree: one blob + one subtree.
    // git update-index --cacheinfo with mode 040000 doesn't actually
    // work for mounting a subtree (cacheinfo expects file-like
    // entries), so we use read-tree --prefix= to mount the subtree
    // under "subdir/", then add the file via update-index.
    Command::new("git")
        .args(["read-tree", "--empty"])
        .current_dir(&repo)
        .output()
        .expect("reset index");
    let rt = Command::new("git")
        .args(["read-tree", "--prefix=subdir/", &subtree_sha])
        .current_dir(&repo)
        .output()
        .expect("git read-tree --prefix");
    assert!(
        rt.status.success(),
        "git read-tree --prefix failed: {}",
        String::from_utf8_lossy(&rt.stderr)
    );
    let add_file = Command::new("git")
        .args([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{},file.txt", blob),
        ])
        .current_dir(&repo)
        .output()
        .expect("git update-index file.txt");
    assert!(add_file.status.success());
    let their = {
        let out = Command::new("git")
            .arg("write-tree")
            .current_dir(&repo)
            .output()
            .expect("git write-tree");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    let ours = tree_hash(vec![
        TreeEntry {
            mode: Mode::RegularFile,
            name: b"file.txt".to_vec(),
            object_sha: blob.clone(),
        },
        TreeEntry {
            mode: Mode::Tree,
            name: b"subdir".to_vec(),
            object_sha: subtree_sha,
        },
    ]);
    assert_eq!(ours, their);
    let _ = std::fs::remove_dir_all(&repo);
}

// ── Commits ──────────────────────────────────────────────────────────

#[test]
fn commit_minimal_matches_git() {
    if !git_available() {
        return;
    }
    let repo = mktemp_repo();
    let tree_sha = {
        let out = Command::new("git")
            .arg("write-tree")
            .current_dir(&repo)
            .output()
            .expect("git write-tree");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    // Build the same commit via git commit-tree with pinned env vars.
    let their_sha = Command::new("git")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "1234567890 +0000")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_DATE", "1234567890 +0000")
        .args(["commit-tree", &tree_sha, "-m", "hello"])
        .current_dir(&repo)
        .output()
        .expect("git commit-tree");
    assert!(their_sha.status.success());
    let their_hex = String::from_utf8(their_sha.stdout)
        .unwrap()
        .trim()
        .to_string();
    let ours = commit_hash(&Commit {
        tree: tree_sha,
        parents: vec![],
        author: "Test <test@example.com> 1234567890 +0000".to_string(),
        committer: "Test <test@example.com> 1234567890 +0000".to_string(),
        pgp_signature: None,
        message: "hello\n".to_string(),
    });
    assert_eq!(ours, their_hex);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn commit_with_parent_matches_git() {
    if !git_available() {
        return;
    }
    let repo = mktemp_repo();
    let tree_out = Command::new("git")
        .arg("write-tree")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        tree_out.status.success(),
        "write-tree failed: {}",
        String::from_utf8_lossy(&tree_out.stderr)
    );
    let tree_sha = String::from_utf8(tree_out.stdout).unwrap().trim().to_string();
    assert_eq!(tree_sha.len(), 40, "tree_sha malformed: {:?}", tree_sha);

    let p1 = Command::new("git")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_AUTHOR_DATE", "1000000001 +0000")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_DATE", "1000000001 +0000")
        .args(["commit-tree", &tree_sha, "-m", "c1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        p1.status.success(),
        "commit-tree c1 failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&p1.stdout),
        String::from_utf8_lossy(&p1.stderr)
    );
    let parent_sha = String::from_utf8(p1.stdout).unwrap().trim().to_string();
    assert_eq!(parent_sha.len(), 40, "parent_sha malformed: {:?}", parent_sha);

    let p2 = Command::new("git")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_AUTHOR_DATE", "1000000002 +0000")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_DATE", "1000000002 +0000")
        .args(["commit-tree", &tree_sha, "-p", &parent_sha, "-m", "c2"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        p2.status.success(),
        "git commit-tree -p failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&p2.stdout),
        String::from_utf8_lossy(&p2.stderr)
    );
    let their = String::from_utf8(p2.stdout).unwrap().trim().to_string();
    let ours = commit_hash(&Commit {
        tree: tree_sha,
        parents: vec![parent_sha],
        author: "T <t@e.com> 1000000002 +0000".to_string(),
        committer: "T <t@e.com> 1000000002 +0000".to_string(),
        pgp_signature: None,
        message: "c2\n".to_string(),
    });
    assert_eq!(ours, their);
    let _ = std::fs::remove_dir_all(&repo);
}

// ── Tags ─────────────────────────────────────────────────────────────

#[test]
fn annotated_tag_matches_git() {
    if !git_available() {
        return;
    }
    let repo = mktemp_repo();
    let tree_sha = {
        let out = Command::new("git")
            .arg("write-tree")
            .current_dir(&repo)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let commit_output = Command::new("git")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@e.com")
        .env("GIT_AUTHOR_DATE", "1000000001 +0000")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@e.com")
        .env("GIT_COMMITTER_DATE", "1000000001 +0000")
        .args(["commit-tree", &tree_sha, "-m", "c"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        commit_output.status.success(),
        "commit-tree failed: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );
    let commit_sha = String::from_utf8(commit_output.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(commit_sha.len(), 40, "commit_sha malformed: {:?}", commit_sha);
    // Build the tag body (no header — `git hash-object -t tag` adds
    // its own `tag <len>\0` header before hashing).
    let tag_body = format!(
        "object {}\ntype commit\ntag v1\ntagger T <t@e.com> 1000000001 +0000\n\nrelease\n",
        commit_sha
    );
    let their = git_hash_object(tag_body.as_bytes(), "tag");
    let ours = tag_hash(&Tag {
        object: commit_sha,
        target_type: "commit".to_string(),
        tag: "v1".to_string(),
        tagger: "T <t@e.com> 1000000001 +0000".to_string(),
        message: "release\n".to_string(),
        pgp_signature: None,
    });
    assert_eq!(ours, their);
    let _ = std::fs::remove_dir_all(&repo);
}
