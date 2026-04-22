//! Byte-level parity tests against the real `git` binary.
//!
//! For each test: build a minimal bare repository in a tmp dir, run
//! `git-upload-pack --http-backend-info-refs` (or receive-pack
//! equivalent) to get git's own advertisement body, then diff
//! against what `tg-wire::advertise_info_refs` produces.
//!
//! Parity is structural, not byte-exact: git's capability block
//! differs (it includes `agent=git/...`, `symref=...`, and may vary
//! by version) so we compare the framing and the ref list, not the
//! capability tokens. See `compare_advertisements` below.
//!
//! If `git` isn't on PATH or doesn't support the backend flag, we
//! skip rather than fail (developer may be on a stripped image).

use std::path::Path;
use std::process::Command;

use tg_wire::{advertise_info_refs, AdvertisedRef, Service};

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn init_bare(dir: &Path) {
    let out = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(dir)
        .output()
        .expect("git init --bare");
    assert!(out.status.success(), "git init failed: {:?}", out);
}

fn init_working(dir: &Path) {
    let out = Command::new("git")
        .args(["init", "--quiet"])
        .arg(dir)
        .output()
        .expect("git init");
    assert!(out.status.success(), "git init failed: {:?}", out);
    // Minimum config so commits can be made.
    for (k, v) in [
        ("user.name", "Tester"),
        ("user.email", "t@example.invalid"),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
        ("init.defaultBranch", "main"),
    ] {
        let out = Command::new("git")
            .args(["-C"])
            .arg(dir)
            .args(["config", k, v])
            .output()
            .expect("git config");
        assert!(out.status.success());
    }
}

/// Run `git upload-pack --http-backend-info-refs <repo>` and return
/// the advertisement body bytes.
fn git_info_refs(repo_dir: &Path, service: Service) -> Option<Vec<u8>> {
    let subcommand = match service {
        Service::UploadPack => "upload-pack",
        Service::ReceivePack => "receive-pack",
    };
    let out = Command::new("git")
        .args([subcommand, "--http-backend-info-refs"])
        .arg(repo_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        // Older gits (< 2.28) lack --http-backend-info-refs; skip.
        return None;
    }
    Some(out.stdout)
}

fn list_refs(repo_dir: &Path) -> Vec<AdvertisedRef<'static>> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(repo_dir)
        .args(["show-ref"])
        .output()
        .expect("git show-ref");
    if !out.status.success() {
        // Empty repo.
        return Vec::new();
    }
    let body = String::from_utf8(out.stdout).unwrap();
    body.lines()
        .filter_map(|line| {
            let mut it = line.splitn(2, ' ');
            let sha = it.next()?;
            let name = it.next()?;
            // Leak the Strings to get 'static &str for the test.
            Some(AdvertisedRef {
                sha: Box::leak(sha.to_string().into_boxed_str()),
                name: Box::leak(name.to_string().into_boxed_str()),
            })
        })
        .collect()
}

fn head_commit(repo_dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(repo_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// Compare our body (with `# service=X\n0000` preamble) against
/// git's upload-pack output (which has NO preamble — that's added by
/// git-http-backend, not by upload-pack itself).
///
/// Our preamble is stripped, then we check:
///   * Both advertise the same refs (sha + name tokens).
///   * Both end with a flush packet.
///   * Empty repo emits the `capabilities^{}` pseudo-ref with
///     ZERO_SHA in both bodies.
///
/// Capability tokens differ in content (git includes
/// `symref=HEAD:refs/heads/*`, a version-specific `agent=git/...`,
/// and an object-format advertisement we don't yet emit). We check
/// those aren't part of the wire shape contract — they're
/// negotiated, not structural.
fn compare_advertisements(
    ours: &[u8],
    theirs: &[u8],
    expected_refs: &[AdvertisedRef<'_>],
) {
    let o = String::from_utf8_lossy(ours);
    let t = String::from_utf8_lossy(theirs);

    // Strip our preamble (upload-pack --http-backend-info-refs does
    // not emit one; only git-http-backend CGI does).
    let preamble_end = o.find("\n0000").expect("ours has preamble") + 5;
    let ours_body = &o[preamble_end..];

    // Both end with a flush packet.
    assert!(ours_body.ends_with("0000"), "ours does not end in flush");
    assert!(t.ends_with("0000"), "theirs does not end in flush");

    // Each expected ref appears (by sha + name token) in both bodies.
    for r in expected_refs {
        assert!(
            t.contains(r.sha) && t.contains(r.name),
            "git advertisement missing ref {} {}",
            r.sha,
            r.name
        );
        assert!(
            ours_body.contains(r.sha) && ours_body.contains(r.name),
            "our advertisement missing ref {} {}",
            r.sha,
            r.name
        );
    }

    // Empty repo: both bodies start with ZERO_SHA + capabilities^{}.
    if expected_refs.is_empty() {
        assert!(
            ours_body.contains("capabilities^{}"),
            "ours missing capabilities^{{}} pseudo-ref: {ours_body}"
        );
        assert!(ours_body.contains(tg_wire::ZERO_SHA));
        assert!(
            t.contains("capabilities^{}"),
            "theirs missing capabilities^{{}} pseudo-ref: {t}"
        );
    }
}

#[test]
fn empty_bare_repo_upload_pack() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("tg-wire-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    init_bare(&tmp);

    let ours = advertise_info_refs(Service::UploadPack, &[]).unwrap();
    let Some(theirs) = git_info_refs(&tmp, Service::UploadPack) else {
        eprintln!("git version lacks --http-backend-info-refs; skipping");
        std::fs::remove_dir_all(&tmp).ok();
        return;
    };
    compare_advertisements(&ours, &theirs, &[]);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn repo_with_one_commit_upload_pack() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("tg-wire-parity-one-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    init_working(&tmp);
    // Write a file, add, commit.
    std::fs::write(tmp.join("README"), "hi\n").unwrap();
    for args in [
        vec!["-C", tmp.to_str().unwrap(), "add", "README"],
        vec!["-C", tmp.to_str().unwrap(), "commit", "-m", "initial"],
    ] {
        let out = Command::new("git").args(&args).output().expect("git");
        assert!(out.status.success(), "git {:?} failed: {:?}", args, out);
    }

    let head = head_commit(&tmp).expect("HEAD after commit");
    let refs = list_refs(&tmp);
    assert!(!refs.is_empty(), "expected at least one ref after commit");
    let ours_refs: Vec<AdvertisedRef<'_>> = refs.iter().cloned().collect();

    let ours = advertise_info_refs(Service::UploadPack, &ours_refs).unwrap();
    let Some(theirs) = git_info_refs(&tmp, Service::UploadPack) else {
        eprintln!("git version lacks --http-backend-info-refs; skipping");
        std::fs::remove_dir_all(&tmp).ok();
        return;
    };
    compare_advertisements(&ours, &theirs, &ours_refs);
    // Sanity: our body mentions the HEAD sha.
    assert!(String::from_utf8_lossy(&ours).contains(&head));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn empty_bare_repo_receive_pack() {
    if !git_available() {
        eprintln!("git not available; skipping");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("tg-wire-parity-rp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    init_bare(&tmp);

    let ours = advertise_info_refs(Service::ReceivePack, &[]).unwrap();
    let Some(theirs) = git_info_refs(&tmp, Service::ReceivePack) else {
        eprintln!("git version lacks --http-backend-info-refs; skipping");
        std::fs::remove_dir_all(&tmp).ok();
        return;
    };
    compare_advertisements(&ours, &theirs, &[]);
    std::fs::remove_dir_all(&tmp).ok();
}
