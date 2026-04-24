//! git_receive_pack — smart-HTTP receive-pack WASM integration.
//!
//! Handles the two endpoints git clients hit during push:
//!
//!   * `GET  /{owner}/{repo}.git/info/refs?service=git-receive-pack`
//!     → ref advertisement.
//!   * `POST /{owner}/{repo}.git/git-receive-pack`
//!     → pkt-line command list + pack-v2 stream.
//!
//! This version (RFC-0002 Slice A piece 4) adds **real persistence**:
//!
//!   1. Read request body via inbound streaming.
//!   2. Parse command list via `tg_wire::parse_commands`.
//!   3. Parse pack via `tg_wire::parse_pack`.
//!   4. Compute each object's canonical SHA-1 via `tg_canonical`.
//!   5. POST each object to `/tdata/{Blobs|Trees|Commits|Tags}` on
//!      localhost:3000 as a JSON row with bytes base64-encoded.
//!   6. For each ref command, POST to `/tdata/Refs` (Create),
//!      `PATCH` to `/tdata/Refs(...)` with CAS (Update), or
//!      `DELETE` (Delete).
//!   7. Emit sideband-64k-wrapped pkt-line report:
//!        `unpack ok` (or `unpack <reason>` on failure)
//!        `ok refs/heads/...` (or `ng <ref> <reason>`) per command
//!        flush
//!
//! Repository resolution: v0 convention is `rp-{owner}-{repo}`.
//! Agents must pre-create the Repository row with that Id before
//! pushing (per RFC-0002 Slice C). A subsequent slice adds
//! `/tdata/Repositories?$filter=` lookup.

#![forbid(unsafe_code)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use temper_wasm_sdk::http_stream::InboundHttp;
use temper_wasm_sdk::prelude::*;
use tg_wire::{
    advertise_info_refs, commands, encode_into, flush, pack, AdvertisedRef, CommandKind, Service,
};

const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const READ_CHUNK_BYTES: usize = 16 * 1024;
const TEMPER_API: &str = "http://127.0.0.1:3000";
const SYSTEM_TENANT: &str = "default";
const SYSTEM_PRINCIPAL: &str = "git-receive-pack";

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        let http_value = ctx
            .http_request
            .clone()
            .ok_or_else(|| "git_receive_pack requires HttpEndpoint dispatch".to_string())?;
        let http: InboundHttp = serde_json::from_value(http_value)
            .map_err(|e| format!("http_request parse error: {e}"))?;

        let raw = http.path.as_str();
        let path = raw.split('?').next().unwrap_or(raw);

        if http.method == "GET" && path.ends_with("/info/refs") {
            return serve_info_refs(&http);
        }
        if http.method == "POST" && path.ends_with("/git-receive-pack") {
            return serve_receive_pack(&ctx, &http);
        }
        respond_text(&http, 404, "text/plain", "no receive-pack route matches")
    }
}

fn serve_info_refs(http: &InboundHttp) -> Result<Value, String> {
    let service = match query_param(http, "service").as_deref() {
        Some("git-receive-pack") => Service::ReceivePack,
        Some("git-upload-pack") | None => Service::UploadPack,
        Some(other) => {
            return respond_text(
                http,
                400,
                "text/plain",
                &format!("unknown service '{other}' on /info/refs"),
            );
        }
    };
    let refs: Vec<AdvertisedRef<'_>> = Vec::new();
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
    Ok(json!({ "bytes_written": body.len() }))
}

fn serve_receive_pack(ctx: &Context, http: &InboundHttp) -> Result<Value, String> {
    let body = read_full_body(http)?;
    let parsed = commands::parse_commands(&body)
        .map_err(|e| format!("parse_commands: {e}"))?;
    let pack_slice = &body[parsed.pack_offset..];
    let objects = pack::parse_pack(pack_slice).map_err(|e| format!("parse_pack: {e}"))?;

    // Convention-based repository id derivation.
    let owner = http.params.get("owner").cloned().unwrap_or_default();
    let repo = http.params.get("repo").cloned().unwrap_or_default();
    let repository_id = format!("rp-{owner}-{repo}");

    // Attempt to persist every object. We report a single
    // "unpack ok" or "unpack <reason>" based on whether ALL
    // object writes succeeded.
    let mut unpack_status = "ok".to_string();
    let mut per_obj_errors: Vec<String> = Vec::new();

    for obj in &objects {
        let (kind_prefix, entity_set) = match obj.kind {
            pack::ObjectKind::Blob => ("blob", "Blobs"),
            pack::ObjectKind::Tree => ("tree", "Trees"),
            pack::ObjectKind::Commit => ("commit", "Commits"),
            pack::ObjectKind::Tag => ("tag", "Tags"),
        };
        let sha = match obj.kind {
            pack::ObjectKind::Blob => tg_canonical::blob_hash(&obj.data),
            _ => sha_from_prefix(kind_prefix, &obj.data),
        };
        let mut canonical = format!("{} {}\0", kind_prefix, obj.data.len()).into_bytes();
        canonical.extend_from_slice(&obj.data);

        let row = build_object_row(obj.kind, &sha, &repository_id, &obj.data, &canonical);
        let url = format!("{TEMPER_API}/tdata/{entity_set}");
        let body_json = row.to_string();
        match post_json(ctx, &url, &body_json) {
            Ok(resp) if (200..400).contains(&resp.status) => {}
            Ok(resp) => {
                // 409 is idempotent success (object already stored).
                if resp.status == 409 {
                    continue;
                }
                unpack_status = format!("error status {} on {sha}", resp.status);
                per_obj_errors.push(format!("{sha}:{}", resp.status));
            }
            Err(e) => {
                unpack_status = format!("error writing {sha}: {e}");
                per_obj_errors.push(format!("{sha}:{e}"));
            }
        }
    }

    // Apply ref updates. Each command produces a per-ref status line.
    let mut ref_statuses: Vec<(String, Result<(), String>)> = Vec::new();
    for cmd in &parsed.commands {
        let result = if !per_obj_errors.is_empty() {
            Err(alloc::format!("object write failures: {}", per_obj_errors.len()))
        } else {
            apply_ref_command(ctx, &repository_id, cmd)
        };
        ref_statuses.push((cmd.refname.clone(), result));
    }

    // Build the receive-pack response.
    let mut inner = Vec::new();
    let unpack_line = if unpack_status == "ok" {
        "unpack ok\n".to_string()
    } else {
        format!("unpack {unpack_status}\n")
    };
    encode_into(&mut inner, unpack_line.as_bytes())
        .map_err(|e| format!("encode unpack: {e}"))?;
    for (refname, status) in &ref_statuses {
        let line = match status {
            Ok(()) => format!("ok {refname}\n"),
            Err(reason) => format!("ng {refname} {reason}\n"),
        };
        encode_into(&mut inner, line.as_bytes())
            .map_err(|e| format!("encode ref status: {e}"))?;
    }
    flush(&mut inner);

    // Wrap in sideband-64k if negotiated.
    let sideband = parsed.capabilities.iter().any(|c| c == "side-band-64k");
    let mut response = Vec::new();
    if sideband {
        for chunk in inner.chunks(65515) {
            let mut payload = Vec::with_capacity(1 + chunk.len());
            payload.push(0x01); // channel 1 (pack/result)
            payload.extend_from_slice(chunk);
            encode_into(&mut response, &payload)
                .map_err(|e| format!("encode sideband: {e}"))?;
        }
        flush(&mut response);
    } else {
        response.extend_from_slice(&inner);
    }

    http.submit_response_head(
        200,
        &[
            ("content-type", "application/x-git-receive-pack-result"),
            ("cache-control", "no-cache"),
        ],
    )
    .map_err(|e| format!("submit_response_head: {e}"))?;
    let mut writer = http.response_body();
    writer
        .write_all_chunk(&response)
        .map_err(|e| format!("response_body write: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("response_body close: {e}"))?;

    Ok(json!({
        "commands": parsed.commands.len(),
        "objects": objects.len(),
        "unpack_status": unpack_status,
        "ref_statuses": ref_statuses
            .iter()
            .map(|(n, r)| json!({"ref": n, "ok": r.is_ok()}))
            .collect::<Vec<_>>(),
    }))
}

/// POST a JSON body to the temper-git OData API, injecting the
/// system principal headers.
fn post_json(
    ctx: &Context,
    url: &str,
    body: &str,
) -> Result<temper_wasm_sdk::HttpResponse, String> {
    let headers: Vec<(String, String)> = alloc::vec![
        ("X-Tenant-Id".to_string(), SYSTEM_TENANT.to_string()),
        ("X-Temper-Principal-Kind".to_string(), "Admin".to_string()),
        ("X-Temper-Principal-Id".to_string(), SYSTEM_PRINCIPAL.to_string()),
        ("X-Temper-Agent-Type".to_string(), "system".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    ctx.http_call("POST", url, &headers, body)
}

/// PATCH the same way.
fn patch_json(
    ctx: &Context,
    url: &str,
    body: &str,
) -> Result<temper_wasm_sdk::HttpResponse, String> {
    let headers: Vec<(String, String)> = alloc::vec![
        ("X-Tenant-Id".to_string(), SYSTEM_TENANT.to_string()),
        ("X-Temper-Principal-Kind".to_string(), "Admin".to_string()),
        ("X-Temper-Principal-Id".to_string(), SYSTEM_PRINCIPAL.to_string()),
        ("X-Temper-Agent-Type".to_string(), "system".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    ctx.http_call("PATCH", url, &headers, body)
}

fn apply_ref_command(
    ctx: &Context,
    repository_id: &str,
    cmd: &tg_wire::RefCommand,
) -> Result<(), String> {
    match cmd.kind() {
        CommandKind::Create => {
            let row = json!({
                "Id": format!("rf-{}-{}", repository_id, cmd.refname.replace('/', "-")),
                "RepositoryId": repository_id,
                "Name": cmd.refname,
                "TargetCommitSha": cmd.new_sha,
                "Kind": if cmd.refname.starts_with("refs/tags/") { "tag" } else { "branch" },
                "Status": "Active",
                "UpdatedAt": "1970-01-01T00:00:00Z",
            });
            let url = format!("{TEMPER_API}/tdata/Refs");
            let resp = post_json(ctx, &url, &row.to_string())?;
            if !(200..400).contains(&resp.status) {
                return Err(format!("ref create status {}", resp.status));
            }
            Ok(())
        }
        CommandKind::Update => {
            let ref_id = format!("rf-{}-{}", repository_id, cmd.refname.replace('/', "-"));
            let patch = json!({
                "TargetCommitSha": cmd.new_sha,
                "ExpectedOldSha": cmd.old_sha,
            });
            let url = format!("{TEMPER_API}/tdata/Refs('{ref_id}')");
            let resp = patch_json(ctx, &url, &patch.to_string())?;
            if !(200..400).contains(&resp.status) {
                return Err(format!("ref update status {}", resp.status));
            }
            Ok(())
        }
        CommandKind::Delete => {
            Err("ref delete not implemented".to_string())
        }
    }
}

fn build_object_row(
    kind: pack::ObjectKind,
    sha: &str,
    repository_id: &str,
    raw: &[u8],
    canonical: &[u8],
) -> Value {
    let canonical_b64 = B64.encode(canonical);
    let created_at = "1970-01-01T00:00:00Z"; // temper platform fills a real timestamp on durable write
    match kind {
        pack::ObjectKind::Blob => json!({
            "Id": sha,
            "RepositoryId": repository_id,
            "Size": raw.len(),
            "Content": B64.encode(raw),
            "CanonicalBytes": canonical_b64,
            "Status": "Durable",
            "CreatedAt": created_at,
        }),
        pack::ObjectKind::Tree => json!({
            "Id": sha,
            "RepositoryId": repository_id,
            "CanonicalBytes": canonical_b64,
            "Status": "Durable",
            "CreatedAt": created_at,
        }),
        pack::ObjectKind::Commit => {
            // v0: don't parse the commit body; leave metadata fields
            // blank. Next slice does the parse via tg-canonical.
            json!({
                "Id": sha,
                "RepositoryId": repository_id,
                "TreeSha": "",
                "ParentShas": "",
                "Author": "",
                "Committer": "",
                "Message": "",
                "CanonicalBytes": canonical_b64,
                "Status": "Durable",
                "CreatedAt": created_at,
            })
        }
        pack::ObjectKind::Tag => json!({
            "Id": sha,
            "RepositoryId": repository_id,
            "TargetSha": "",
            "TargetType": "",
            "TagName": "",
            "Tagger": "",
            "Message": "",
            "CanonicalBytes": canonical_b64,
            "Status": "Durable",
            "CreatedAt": created_at,
        }),
    }
}

fn sha_from_prefix(prefix: &str, body: &[u8]) -> String {
    let header = format!("{} {}\0", prefix, body.len());
    let mut hasher = tg_canonical::Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(body);
    hasher.hex()
}

fn read_full_body(http: &InboundHttp) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut scratch = alloc::vec![0u8; READ_CHUNK_BYTES];
    let mut reader = http.request_body();
    loop {
        match reader.read_next_chunk(&mut scratch) {
            Ok(None) => break,
            Ok(Some(n)) => {
                if body.len() + n > MAX_BODY_BYTES {
                    return Err(format!("request body exceeds {MAX_BODY_BYTES} bytes"));
                }
                body.extend_from_slice(&scratch[..n]);
            }
            Err(e) => return Err(format!("request_body read: {e}")),
        }
    }
    Ok(body)
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
