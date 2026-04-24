//! Git-binary parity: build a real git repo with a commit, tell
//! `git pack-objects` to pack it, feed the pack into
//! `tg_wire::parse_pack`, and verify the object set matches what
//! `git cat-file` sees in the source repo.

use std::path::Path;
use std::process::Command;

use tg_wire::{parse_pack, ObjectKind};

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn init_working(dir: &Path) {
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .arg(dir)
        .status()
        .unwrap()
        .success());
    for (k, v) in [
        ("user.name", "Tester"),
        ("user.email", "t@example.invalid"),
        ("commit.gpgsign", "false"),
        ("init.defaultBranch", "main"),
    ] {
        Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "config", k, v])
            .status()
            .unwrap();
    }
}

fn commit_file(dir: &Path, name: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(name), content).unwrap();
    Command::new("git")
        .args(["-C", dir.to_str().unwrap(), "add", name])
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", dir.to_str().unwrap(), "commit", "-m", msg])
        .status()
        .unwrap();
}

/// Ask git to build a pack over all objects in the repo and
/// return its raw bytes.
fn build_pack_from_repo(dir: &Path) -> Vec<u8> {
    // List every object via `git rev-list --objects --all`, pipe
    // to `git pack-objects` with `--stdout` to capture the pack.
    let list = Command::new("git")
        .args(["-C", dir.to_str().unwrap(), "rev-list", "--objects", "--all"])
        .output()
        .unwrap();
    assert!(list.status.success());
    // Strip the "<sha> <path>" → "<sha>" (pack-objects only needs
    // SHAs; paths are an optional hint).
    let sha_list: Vec<u8> = list
        .stdout
        .split(|b| *b == b'\n')
        .filter_map(|line| line.split(|b| *b == b' ').next())
        .filter(|s| !s.is_empty())
        .flat_map(|s| {
            let mut v = s.to_vec();
            v.push(b'\n');
            v
        })
        .collect();

    let mut pack = Command::new("git")
        .args([
            "-C",
            dir.to_str().unwrap(),
            "pack-objects",
            "--stdout",
            "--no-reuse-delta",  // disable reuse of stored deltas
            "--no-reuse-object", // force re-serialization
            "--depth=0",         // no delta depth at all
            "--window=0",        // no delta window — forces plain entries
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        pack.stdin
            .as_mut()
            .unwrap()
            .write_all(&sha_list)
            .unwrap();
    }
    let out = pack.wait_with_output().unwrap();
    assert!(out.status.success(), "pack-objects failed: {:?}", out);
    out.stdout
}

#[test]
fn parse_pack_from_single_commit_repo() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }
    let tmp = std::env::temp_dir().join(format!(
        "tg-wire-pack-parity-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    init_working(&tmp);
    commit_file(&tmp, "README", "hello\n", "initial");

    let pack = build_pack_from_repo(&tmp);
    let objs = parse_pack(&pack).expect("pack parses");
    // We expect: 1 commit + 1 tree + 1 blob = 3 objects.
    assert_eq!(objs.len(), 3, "expected 3 objects, got {}", objs.len());
    let mut kinds: Vec<_> = objs.iter().map(|o| o.kind).collect();
    kinds.sort_by_key(|k| match k {
        ObjectKind::Commit => 0,
        ObjectKind::Tree => 1,
        ObjectKind::Blob => 2,
        ObjectKind::Tag => 3,
    });
    assert_eq!(
        kinds,
        vec![ObjectKind::Commit, ObjectKind::Tree, ObjectKind::Blob]
    );

    // Each object's body hashes to a SHA-1 that git has in its
    // object store. Verify by asking `git cat-file -t <sha>`.
    for obj in &objs {
        use sha1::Digest;
        let header = format!("{} {}\0", obj.kind.header_prefix(), obj.data.len());
        let mut h = sha1::Sha1::new();
        h.update(header.as_bytes());
        h.update(&obj.data);
        let sha = h.finalize();
        let sha_hex = sha
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();

        let check = Command::new("git")
            .args(["-C", tmp.to_str().unwrap(), "cat-file", "-t"])
            .arg(&sha_hex)
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "git doesn't know object {sha_hex}: {}",
            String::from_utf8_lossy(&check.stderr)
        );
        let ty = String::from_utf8_lossy(&check.stdout);
        assert_eq!(ty.trim(), obj.kind.header_prefix());
    }

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn parse_pack_from_multi_commit_repo() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }
    let tmp = std::env::temp_dir().join(format!(
        "tg-wire-pack-multi-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    init_working(&tmp);
    commit_file(&tmp, "a", "one\n", "first");
    commit_file(&tmp, "b", "two\n", "second");
    commit_file(&tmp, "c", "three\n", "third");

    let pack = build_pack_from_repo(&tmp);
    let objs = parse_pack(&pack).expect("pack parses");
    // 3 commits + 3 trees + 3 blobs.
    let commits = objs.iter().filter(|o| o.kind == ObjectKind::Commit).count();
    let trees = objs.iter().filter(|o| o.kind == ObjectKind::Tree).count();
    let blobs = objs.iter().filter(|o| o.kind == ObjectKind::Blob).count();
    assert_eq!(commits, 3);
    assert_eq!(trees, 3);
    assert_eq!(blobs, 3);

    std::fs::remove_dir_all(&tmp).ok();
}
