//! git_upload_pack — smart-HTTP upload-pack WASM integration.
//!
//! Handles the two endpoints git clients hit during fetch/clone:
//!
//!   * `GET  /{owner}/{repo}.git/info/refs?service=git-upload-pack`
//!     → advertisement (ref list + capabilities, wrapped in pkt-line).
//!   * `POST /{owner}/{repo}.git/git-upload-pack`
//!     → want/have negotiation + pack-v2 emission.
//!
//! Slice B piece 1 (this commit): real ref advertisement — queries
//! /tdata/Refs for the repo and emits one line per active ref so
//! `git clone` sees the repo as non-empty and proceeds to POST.
//!
//! Piece 2 (next): pack emission on POST.

#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::{BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use temper_wasm_sdk::http_stream::InboundHttp;
use temper_wasm_sdk::prelude::*;
use tg_wire::{advertise_info_refs, emit_pack, encode_into, flush, AdvertisedRef, ObjectKind, PackObject, Service};

const TEMPER_API: &str = "http://127.0.0.1:3000";
const SYSTEM_TENANT: &str = "default";
const SYSTEM_PRINCIPAL: &str = "git-upload-pack";

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        let http_value = ctx
            .http_request
            .clone()
            .ok_or_else(|| "git_upload_pack requires HttpEndpoint dispatch (http_request missing)".to_string())?;
        let http: InboundHttp = serde_json::from_value(http_value)
            .map_err(|e| format!("http_request parse error: {e}"))?;

        let raw = http.path.as_str();
        let path = raw.split('?').next().unwrap_or(raw);

        if http.method == "GET" && path.ends_with("/info/refs") {
            return serve_info_refs(&ctx, &http);
        }
        if http.method == "POST" && path.ends_with("/git-upload-pack") {
            return serve_upload_pack(&ctx, &http);
        }
        respond_text(&http, 404, "text/plain", "no upload-pack route matches")
    }
}

fn serve_info_refs(ctx: &Context, http: &InboundHttp) -> Result<Value, String> {
    let service = match query_param(http, "service").as_deref() {
        Some("git-upload-pack") | None => Service::UploadPack,
        Some("git-receive-pack") => Service::ReceivePack,
        Some(other) => {
            return respond_text(
                http,
                400,
                "text/plain",
                &format!("unknown service '{other}' on /info/refs"),
            );
        }
    };

    // Derive the convention-based Repository id from path params.
    let owner = http.params.get("owner").cloned().unwrap_or_default();
    let repo = http.params.get("repo").cloned().unwrap_or_default();
    let repository_id = format!("rp-{owner}-{repo}");

    // Query /tdata/Refs and filter client-side on RepositoryId. We
    // don't assume $filter support yet; the payload is small enough
    // (tens of refs typical) that full-list-then-filter is fine.
    let refs_rows = fetch_refs_for_repo(ctx, &repository_id)?;

    let owned: Vec<(String, String)> = refs_rows
        .into_iter()
        .filter(|r| r.status == "Active")
        .map(|r| (r.target_sha, r.name))
        .collect();
    let refs: Vec<AdvertisedRef<'_>> = owned
        .iter()
        .map(|(sha, name)| AdvertisedRef {
            sha: sha.as_str(),
            name: name.as_str(),
        })
        .collect();

    let body = advertise_info_refs(service, &refs)
        .map_err(|e| format!("advertise_info_refs: {e}"))?;

    http.submit_response_head(
        200,
        &[
            ("content-type", service.content_type()),
            ("cache-control", "no-cache"),
        ],
    )
    .map_err(|e| format!("submit_response_head: {e}"))?;

    let mut writer = http.response_body();
    writer
        .write_all_chunk(&body)
        .map_err(|e| format!("response_body write: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("response_body close: {e}"))?;

    Ok(json!({
        "bytes_written": body.len(),
        "ref_count": refs.len(),
        "repository_id": repository_id,
    }))
}

struct RefRow {
    name: String,
    target_sha: String,
    status: String,
}

fn fetch_refs_for_repo(
    ctx: &Context,
    repository_id: &str,
) -> Result<Vec<RefRow>, String> {
    let url = format!("{TEMPER_API}/tdata/Refs");
    let resp = ctx
        .http_call(
            "GET",
            &url,
            &admin_headers(),
            "",
        )
        .map_err(|e| format!("fetch refs: {e}"))?;
    if !(200..400).contains(&resp.status) {
        return Err(format!("fetch refs status {}", resp.status));
    }
    let parsed: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| format!("refs parse: {e}"))?;
    let items = parsed.get("value").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut rows = Vec::with_capacity(items.len());
    for row in items {
        let fields = row.get("fields").cloned().unwrap_or(serde_json::Value::Null);
        let repo = fields
            .get("RepositoryId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if repo != repository_id {
            continue;
        }
        let name = fields
            .get("Name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let target_sha = fields
            .get("TargetCommitSha")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status = fields
            .get("Status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() || target_sha.is_empty() {
            continue;
        }
        rows.push(RefRow {
            name,
            target_sha,
            status,
        });
    }
    // Deterministic order: HEAD first (if present), then refs/ sorted.
    rows.sort_by(|a, b| {
        let a_is_head = a.name == "HEAD";
        let b_is_head = b.name == "HEAD";
        match (a_is_head, b_is_head) {
            (true, false) => core::cmp::Ordering::Less,
            (false, true) => core::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });
    Ok(rows)
}

fn admin_headers() -> Vec<(String, String)> {
    alloc::vec![
        ("X-Tenant-Id".to_string(), SYSTEM_TENANT.to_string()),
        ("X-Temper-Principal-Kind".to_string(), "Admin".to_string()),
        ("X-Temper-Principal-Id".to_string(), SYSTEM_PRINCIPAL.to_string()),
        ("X-Temper-Agent-Type".to_string(), "system".to_string()),
    ]
}

fn respond_text(
    http: &InboundHttp,
    status: u16,
    content_type: &str,
    body: &str,
) -> Result<Value, String> {
    http.submit_response_head(status, &[("content-type", content_type)])
        .map_err(|e| format!("submit_response_head: {e}"))?;
    let mut writer = http.response_body();
    writer
        .write_all_chunk(body.as_bytes())
        .map_err(|e| format!("response_body write: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("response_body close: {e}"))?;
    Ok(json!({ "status": status }))
}

fn query_param(http: &InboundHttp, key: &str) -> Option<String> {
    let qs = http.path.splitn(2, '?').nth(1)?;
    for pair in qs.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        let v = it.next().unwrap_or("");
        if k == key {
            return Some(v.to_string());
        }
    }
    None
}

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const READ_CHUNK: usize = 16 * 1024;

fn serve_upload_pack(ctx: &Context, http: &InboundHttp) -> Result<Value, String> {
    // 1. Read request body.
    let mut body = Vec::new();
    let mut scratch = alloc::vec![0u8; READ_CHUNK];
    let mut reader = http.request_body();
    loop {
        match reader.read_next_chunk(&mut scratch) {
            Ok(None) => break,
            Ok(Some(n)) => {
                if body.len() + n > MAX_BODY_BYTES {
                    return Err("request body too large".into());
                }
                body.extend_from_slice(&scratch[..n]);
            }
            Err(e) => return Err(format!("read body: {e}")),
        }
    }

    // 2. Parse want/have/done.
    let parsed = parse_upload_request(&body)?;
    let owner = http.params.get("owner").cloned().unwrap_or_default();
    let repo = http.params.get("repo").cloned().unwrap_or_default();
    let repository_id = format!("rp-{owner}-{repo}");

    // 3. Walk the DAG. Start from wants; skip anything the client
    //    already has (listed in `haves`). v0: naive — no negotiation,
    //    just serve everything reachable minus the haves.
    let have_set: BTreeSet<String> = parsed.haves.iter().cloned().collect();
    let mut visited: BTreeSet<String> = have_set.clone();
    let mut queue: VecDeque<(String, ObjectKind)> = VecDeque::new();
    for want in &parsed.wants {
        queue.push_back((want.clone(), ObjectKind::Commit));
    }

    let mut objects: Vec<PackObject> = Vec::new();
    while let Some((sha, kind)) = queue.pop_front() {
        if !visited.insert(sha.clone()) {
            continue;
        }
        let raw_body = fetch_object_body(ctx, kind, &sha, &repository_id)?;
        match kind {
            ObjectKind::Commit => {
                let refs = tg_canonical::parse_commit_refs(&raw_body)
                    .map_err(|e| format!("commit {sha}: {e}"))?;
                queue.push_back((refs.tree, ObjectKind::Tree));
                for p in refs.parents {
                    queue.push_back((p, ObjectKind::Commit));
                }
            }
            ObjectKind::Tree => {
                let entries = tg_canonical::parse_tree(&raw_body)
                    .map_err(|e| format!("tree {sha}: {e}"))?;
                for entry in entries {
                    let k = if entry.is_tree {
                        ObjectKind::Tree
                    } else {
                        ObjectKind::Blob
                    };
                    queue.push_back((entry.sha, k));
                }
            }
            _ => {}
        }
        objects.push(PackObject {
            kind,
            data: raw_body,
        });
    }

    // 4. Emit pack.
    let pack = emit_pack(&objects);

    // 5. Build response. First line is NAK (we don't negotiate in v0),
    //    then pack is wrapped in side-band-64k channel 1 if negotiated.
    let mut resp = Vec::new();
    encode_into(&mut resp, b"NAK\n").map_err(|e| format!("nak: {e}"))?;

    let sideband = parsed.capabilities.iter().any(|c| c == "side-band-64k");
    if sideband {
        for chunk in pack.chunks(65515) {
            let mut payload = Vec::with_capacity(1 + chunk.len());
            payload.push(0x01);
            payload.extend_from_slice(chunk);
            encode_into(&mut resp, &payload)
                .map_err(|e| format!("sideband pkt: {e}"))?;
        }
        flush(&mut resp);
    } else {
        resp.extend_from_slice(&pack);
    }

    http.submit_response_head(
        200,
        &[
            ("content-type", "application/x-git-upload-pack-result"),
            ("cache-control", "no-cache"),
        ],
    )
    .map_err(|e| format!("head: {e}"))?;
    let mut writer = http.response_body();
    writer
        .write_all_chunk(&resp)
        .map_err(|e| format!("body write: {e}"))?;
    writer.finish().map_err(|e| format!("body close: {e}"))?;

    Ok(json!({
        "wants": parsed.wants.len(),
        "objects": objects.len(),
        "pack_bytes": pack.len(),
    }))
}

struct UploadRequest {
    wants: Vec<String>,
    haves: Vec<String>,
    capabilities: Vec<String>,
}

fn parse_upload_request(buf: &[u8]) -> Result<UploadRequest, String> {
    let mut wants = Vec::new();
    let mut haves = Vec::new();
    let mut capabilities: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        let len_str =
            core::str::from_utf8(&buf[i..i + 4]).map_err(|_| "pkt-line len non-utf8")?;
        let declared =
            usize::from_str_radix(len_str, 16).map_err(|_| "pkt-line len non-hex")?;
        if declared == 0 {
            i += 4;
            continue; // flush between wants and haves/done
        }
        if declared < 4 || i + declared > buf.len() {
            break;
        }
        let payload = &buf[i + 4..i + declared];
        i += declared;
        let line = core::str::from_utf8(payload).map_err(|_| "pkt-line non-utf8")?;
        let line = line.trim_end_matches('\n');
        if let Some(rest) = line.strip_prefix("want ") {
            // First want carries capabilities after a space.
            let mut parts = rest.splitn(2, ' ');
            let sha = parts.next().unwrap_or("").to_string();
            if !sha.is_empty() {
                wants.push(sha);
            }
            if capabilities.is_empty()
                && let Some(caps) = parts.next()
            {
                capabilities = caps.split_whitespace().map(|s| s.to_string()).collect();
            }
        } else if let Some(sha) = line.strip_prefix("have ") {
            haves.push(sha.to_string());
        } else if line == "done" {
            break;
        }
    }
    if wants.is_empty() {
        return Err("no wants in upload-pack request".into());
    }
    Ok(UploadRequest {
        wants,
        haves,
        capabilities,
    })
}

fn fetch_object_body(
    ctx: &Context,
    kind: ObjectKind,
    sha: &str,
    _repo_id: &str,
) -> Result<Vec<u8>, String> {
    let set = match kind {
        ObjectKind::Commit => "Commits",
        ObjectKind::Tree => "Trees",
        ObjectKind::Blob => "Blobs",
        ObjectKind::Tag => "Tags",
    };
    // OData key fetch.
    let url = format!("{TEMPER_API}/tdata/{set}('{sha}')");
    let resp = ctx
        .http_call("GET", &url, &admin_headers(), "")
        .map_err(|e| format!("fetch {set}({sha}): {e}"))?;
    if !(200..400).contains(&resp.status) {
        return Err(format!("{set}({sha}) status {}", resp.status));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&resp.body).map_err(|e| format!("object json: {e}"))?;
    let fields = parsed
        .get("fields")
        .ok_or_else(|| format!("{set}({sha}): no fields"))?;
    let canonical_b64 = fields
        .get("CanonicalBytes")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{set}({sha}): no CanonicalBytes"))?;
    let canonical = B64
        .decode(canonical_b64)
        .map_err(|e| format!("base64 decode: {e}"))?;
    // Strip `<kind> <len>\0` prefix → body bytes for the pack.
    let nul = canonical
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| format!("{set}({sha}): no NUL in canonical"))?;
    Ok(canonical[nul + 1..].to_vec())
}
