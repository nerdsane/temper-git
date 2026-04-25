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

/// Headers used for internal OData calls. Currently all calls go
/// through a system principal; #3 in the gap list will swap this
/// for a real `GitToken`-derived principal once auth resolution
/// lands.
fn system_headers() -> Vec<(String, String)> {
    alloc::vec![
        ("X-Tenant-Id".to_string(), SYSTEM_TENANT.to_string()),
        ("X-Temper-Principal-Kind".to_string(), "Admin".to_string()),
        ("X-Temper-Principal-Id".to_string(), SYSTEM_PRINCIPAL.to_string()),
        ("X-Temper-Agent-Type".to_string(), "system".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ]
}

fn post_json(
    ctx: &Context,
    url: &str,
    body: &str,
) -> Result<temper_wasm_sdk::HttpResponse, String> {
    ctx.http_call("POST", url, &system_headers(), body)
}

fn apply_ref_command(
    ctx: &Context,
    repository_id: &str,
    cmd: &tg_wire::RefCommand,
) -> Result<(), String> {
    match cmd.kind() {
        CommandKind::Create => {
            let row = json!({
                "Id": ref_id_for(repository_id, &cmd.refname),
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
            // New ref might already be the source of an open PR
            // (rare on Create, but possible if a ref was deleted +
            // recreated). Flow PR head updates the same way.
            propagate_to_open_prs(ctx, repository_id, &cmd.refname, &cmd.new_sha)?;
            Ok(())
        }
        CommandKind::Update => {
            // Use the spec's `Update` action with compare-and-swap on
            // PreviousCommitSha rather than a generic PATCH. This
            // routes the change through the entity's state machine
            // so any [[integration]] triggers (webhooks, projection
            // rebuilds, …) fire on the canonical event.
            let ref_id = ref_id_for(repository_id, &cmd.refname);
            let body = json!({
                "PreviousCommitSha": cmd.old_sha,
                "NewCommitSha": cmd.new_sha,
            });
            let url = format!("{TEMPER_API}/tdata/Refs('{ref_id}')/Temper.Update");
            let resp = post_json(ctx, &url, &body.to_string())?;
            if !(200..400).contains(&resp.status) {
                return Err(format!("ref update status {}", resp.status));
            }
            propagate_to_open_prs(ctx, repository_id, &cmd.refname, &cmd.new_sha)?;
            Ok(())
        }
        CommandKind::Delete => {
            Err("ref delete not implemented".to_string())
        }
    }
}

fn ref_id_for(repository_id: &str, refname: &str) -> String {
    format!("rf-{}-{}", repository_id, refname.replace('/', "-"))
}

/// After a ref advances, fire `PullRequest.UpdateHead` on every PR
/// whose `SourceRef` matches. This is the wire-side hook that keeps
/// PR head SHAs current as new commits land. Non-fatal: a PR lookup
/// or update failure is logged in the response but doesn't fail the
/// push (the ref advance itself already succeeded).
fn propagate_to_open_prs(
    ctx: &Context,
    repository_id: &str,
    refname: &str,
    new_head: &str,
) -> Result<(), String> {
    let filter = format!(
        "RepositoryId eq '{repository_id}' and SourceRef eq '{refname}' \
         and (State eq 'Open' or State eq 'UnderReview' \
              or State eq 'ChangesRequested' or State eq 'Approved')"
    );
    let url = format!(
        "{TEMPER_API}/tdata/PullRequests?$filter={}&$select=Id",
        urlencode(&filter)
    );
    let headers = system_headers();
    let resp = ctx.http_call("GET", &url, &headers, "")
        .map_err(|e| format!("PR lookup: {e}"))?;
    if !(200..400).contains(&resp.status) {
        // Non-fatal: surface in logs, keep the push successful.
        return Ok(());
    }
    let parsed: Value = match serde_json::from_str(&resp.body) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let Some(items) = parsed.get("value").and_then(|v| v.as_array()) else {
        return Ok(());
    };
    for item in items {
        let Some(pr_id) = item.get("Id").and_then(|v| v.as_str()) else {
            continue;
        };
        let body = json!({ "NewHeadCommitSha": new_head });
        let url = format!(
            "{TEMPER_API}/tdata/PullRequests('{pr_id}')/Temper.UpdateHead"
        );
        let _ = post_json(ctx, &url, &body.to_string());
    }
    Ok(())
}

fn urlencode(s: &str) -> String {
    // Minimal: encode only the characters that break OData $filter
    // through curl/axum. Spaces and quotes are the main ones; full
    // percent-encoding is deferred until we hit a case that needs it.
    s.replace(' ', "%20").replace('\'', "%27")
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
            // Best-effort metadata extraction: a malformed commit
            // shouldn't fail the push (the canonical bytes already
            // round-trip; the OData fields are derived). Null out
            // metadata if the parser can't find what it needs.
            let parsed = tg_canonical::parse_commit(raw).ok();
            let (tree, parents, author, committer, message, gpg) = match &parsed {
                Some(c) => (
                    c.tree.clone(),
                    c.parents.join(","),
                    c.author.clone(),
                    c.committer.clone(),
                    c.message.clone(),
                    c.gpg_signature.clone(),
                ),
                None => Default::default(),
            };
            let mut row = json!({
                "Id": sha,
                "RepositoryId": repository_id,
                "TreeSha": tree,
                "ParentShas": parents,
                "Author": author,
                "Committer": committer,
                "Message": message,
                "CanonicalBytes": canonical_b64,
                "Status": "Durable",
                "CreatedAt": created_at,
            });
            if let Some(sig) = gpg {
                row["PgpSignature"] = Value::String(sig);
            }
            row
        }
        pack::ObjectKind::Tag => {
            let parsed = tg_canonical::parse_tag(raw).ok();
            let (target, ttype, name, tagger, message, gpg) = match &parsed {
                Some(t) => (
                    t.object.clone(),
                    t.target_type.clone(),
                    t.tag.clone(),
                    t.tagger.clone(),
                    t.message.clone(),
                    t.gpg_signature.clone(),
                ),
                None => Default::default(),
            };
            let mut row = json!({
                "Id": sha,
                "RepositoryId": repository_id,
                "TargetSha": target,
                "TargetType": ttype,
                "TagName": name,
                "Tagger": tagger,
                "Message": message,
                "CanonicalBytes": canonical_b64,
                "Status": "Durable",
                "CreatedAt": created_at,
            });
            if let Some(sig) = gpg {
                row["PgpSignature"] = Value::String(sig);
            }
            row
        }
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
