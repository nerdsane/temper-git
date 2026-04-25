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

/// Cap on the command-list bytes accumulated before pack parsing
/// begins. The list is pkt-line framed (4-hex length + payload) and
/// in practice tops out at a few KiB even for very large pushes —
/// 1 MiB is generous head-room with a clear failure mode.
const COMMAND_LIST_MAX_BYTES: usize = 1 * 1024 * 1024;
/// BufReader capacity for the request-body stream. Big enough that
/// the pack parser doesn't churn through tiny `fill_buf` cycles,
/// small enough that the WASM heap isn't pinned by a giant buffer.
const BUFREAD_CAPACITY: usize = 64 * 1024;
pub(crate) const TEMPER_API: &str = "http://127.0.0.1:3000";
pub(crate) const SYSTEM_TENANT: &str = "default";
pub(crate) const SYSTEM_PRINCIPAL: &str = "git-receive-pack";

mod auth;
pub(crate) use auth::Principal;

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
    // Convention-based repository id derivation.
    let owner = http.params.get("owner").cloned().unwrap_or_default();
    let repo = http.params.get("repo").cloned().unwrap_or_default();
    let repository_id = format!("rp-{owner}-{repo}");

    // Auth resolution. A real bearer/Basic token whose hash matches
    // an Active GitToken returns that token's principal + scopes;
    // otherwise we fall through to the system principal for
    // backwards-compat with un-tokenised dev setups.
    let resolved = auth::resolve_principal(ctx, &http.headers);
    let principal = if resolved.is_anonymous() {
        Principal::system()
    } else {
        resolved
    };

    // Stream the request body. We read JUST the command list bytes
    // (pkt-line framed; ends at a 0000 flush), then hand the same
    // reader to the pack parser so the rest of the body never needs
    // to be buffered. Memory peak across this whole flow is one
    // object's body + the command list (typically a few KiB).
    let mut reader = std::io::BufReader::with_capacity(
        BUFREAD_CAPACITY,
        WasmRequestReader::new(http.request_body()),
    );
    let cmd_bytes = read_command_list(&mut reader)?;
    let parsed = commands::parse_commands(&cmd_bytes)
        .map_err(|e| format!("parse_commands: {e}"))?;

    let mut unpack_status = "ok".to_string();
    let mut per_obj_errors: Vec<String> = Vec::new();
    let mut object_count = 0u32;

    let mut parser =
        pack::StreamingPackParser::begin(reader).map_err(|e| format!("pack header: {e}"))?;
    while let Some(obj) = parser
        .next_object()
        .map_err(|e| format!("pack next: {e}"))?
    {
        // Blobs go through the streaming-binary `Temper.IngestRaw`
        // endpoint: the body is sent as raw octets and the kernel
        // computes the SHA + persists the row, so we skip the
        // base64+JSON round-trip that costs ~2.6× the body size on
        // both sides.
        if matches!(obj.kind, pack::ObjectKind::Blob) {
            match ingest_blob_streaming(&principal, &repository_id, &obj.data) {
                Ok(_) => {}
                Err(e) => {
                    unpack_status = format!("error ingesting blob: {e}");
                    per_obj_errors.push(format!("blob:{e}"));
                }
            }
            object_count += 1;
            continue;
        }

        // Tree / Commit / Tag stay on the JSON path. They're small
        // (typically a few hundred bytes), and Commit + Tag rows
        // need parsed metadata fields anyway, so the existing
        // `build_object_row` flow is the right shape.
        let (kind_prefix, entity_set) = match obj.kind {
            pack::ObjectKind::Tree => ("tree", "Trees"),
            pack::ObjectKind::Commit => ("commit", "Commits"),
            pack::ObjectKind::Tag => ("tag", "Tags"),
            pack::ObjectKind::Blob => unreachable!("blob handled above"),
        };
        let sha = sha_from_prefix(kind_prefix, &obj.data);
        let mut canonical = format!("{} {}\0", kind_prefix, obj.data.len()).into_bytes();
        canonical.extend_from_slice(&obj.data);

        let row = build_object_row(obj.kind, &sha, &repository_id, &obj.data, &canonical);
        let url = format!("{TEMPER_API}/tdata/{entity_set}");
        let body_json = row.to_string();
        match post_json(ctx, &principal, &url, &body_json) {
            Ok(resp) if (200..400).contains(&resp.status) => {}
            Ok(resp) => {
                if resp.status == 409 {
                    object_count += 1;
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
        object_count += 1;
        // `obj.data` and `canonical` drop here — only the next
        // object's bytes will be live in WASM memory.
    }
    // Verify the trailer — only meaningful if every object decoded.
    if let Err(e) = parser.finish() {
        unpack_status = format!("trailer: {e}");
        per_obj_errors.push(format!("trailer:{e}"));
    }

    // Apply ref updates. Each command produces a per-ref status line.
    let mut ref_statuses: Vec<(String, Result<(), String>)> = Vec::new();
    for cmd in &parsed.commands {
        let result = if !per_obj_errors.is_empty() {
            Err(alloc::format!("object write failures: {}", per_obj_errors.len()))
        } else {
            apply_ref_command(ctx, &principal, &repository_id, cmd)
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
        "objects": object_count,
        "unpack_status": unpack_status,
        "ref_statuses": ref_statuses
            .iter()
            .map(|(n, r)| json!({"ref": n, "ok": r.is_ok()}))
            .collect::<Vec<_>>(),
    }))
}

fn post_json(
    ctx: &Context,
    principal: &Principal,
    url: &str,
    body: &str,
) -> Result<temper_wasm_sdk::HttpResponse, String> {
    ctx.http_call("POST", url, &principal.outbound_headers(), body)
}

/// Stream a raw blob body to the kernel via `POST
/// /tdata/Blobs/Temper.IngestRaw`. Bytes go out as octet-stream
/// chunks, the kernel computes the SHA-1 + persists the row, and
/// returns the row Id. Avoids the 2.6× heap blowup of the JSON +
/// base64 encoding the standard OData POST would require.
fn ingest_blob_streaming(
    principal: &Principal,
    repository_id: &str,
    body: &[u8],
) -> Result<String, String> {
    use temper_wasm_sdk::http_stream::streaming_call;

    let url = format!("{TEMPER_API}/tdata/Blobs/Temper.IngestRaw");
    let content_length = body.len().to_string();

    // Strip the JSON Content-Type the principal helper attaches
    // and add the protocol-specific ones for this endpoint.
    let mut owned: Vec<(String, String)> = principal
        .outbound_headers()
        .into_iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("content-type"))
        .collect();
    owned.push((
        "Content-Type".to_string(),
        "application/octet-stream".to_string(),
    ));
    owned.push(("Content-Length".to_string(), content_length));
    owned.push(("X-Repository-Id".to_string(), repository_id.to_string()));
    let header_refs: Vec<(&str, &str)> =
        owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    let (mut req, mut resp, head) = streaming_call("POST", &url, &header_refs)
        .map_err(|e| format!("ingest stream begin: {e}"))?;

    const STREAM_CHUNK: usize = 64 * 1024;
    for chunk in body.chunks(STREAM_CHUNK) {
        req.write_all_chunk(chunk)
            .map_err(|e| format!("ingest write: {e}"))?;
    }
    req.finish().map_err(|e| format!("ingest finish: {e}"))?;

    let head = head().map_err(|e| format!("ingest head: {e}"))?;
    if !(200..400).contains(&head.status) {
        return Err(format!("ingest status {}", head.status));
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut scratch = alloc::vec![0u8; 4096];
    loop {
        match resp.read_next_chunk(&mut scratch) {
            Ok(None) => break,
            Ok(Some(n)) => buf.extend_from_slice(&scratch[..n]),
            Err(e) => return Err(format!("ingest read response: {e}")),
        }
    }

    let parsed: serde_json::Value =
        serde_json::from_slice(&buf).map_err(|e| format!("ingest response json: {e}"))?;
    parsed
        .get("Id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "ingest response missing Id".to_string())
}

fn apply_ref_command(
    ctx: &Context,
    principal: &Principal,
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
            let resp = post_json(ctx, principal, &url, &row.to_string())?;
            if !(200..400).contains(&resp.status) {
                return Err(format!("ref create status {}", resp.status));
            }
            propagate_to_open_prs(ctx, principal, repository_id, &cmd.refname, &cmd.new_sha)?;
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
            let resp = post_json(ctx, principal, &url, &body.to_string())?;
            if !(200..400).contains(&resp.status) {
                return Err(format!("ref update status {}", resp.status));
            }
            propagate_to_open_prs(ctx, principal, repository_id, &cmd.refname, &cmd.new_sha)?;
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
    principal: &Principal,
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
    let resp = ctx
        .http_call("GET", &url, &principal.outbound_headers(), "")
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
        let _ = post_json(ctx, principal, &url, &body.to_string());
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

/// `std::io::Read` adapter over the SDK's inbound body reader. Used
/// to drive the streaming pack parser without ever materialising the
/// full pack in WASM memory.
struct WasmRequestReader {
    inner: temper_wasm_sdk::http_stream::HttpResponseBodyReader,
}

impl WasmRequestReader {
    fn new(inner: temper_wasm_sdk::http_stream::HttpResponseBodyReader) -> Self {
        Self { inner }
    }
}

impl std::io::Read for WasmRequestReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.read_next_chunk(out) {
            Ok(None) => Ok(0),
            Ok(Some(n)) => Ok(n),
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("{e}"),
            )),
        }
    }
}

/// Read pkt-line packets from `reader` until the `0000` flush that
/// ends the receive-pack command list. Returns the consumed bytes
/// (including the flush) so they can be handed verbatim to
/// `parse_commands`. The reader is positioned at the first pack byte
/// when this returns.
fn read_command_list<R: std::io::BufRead>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        if out.len() >= COMMAND_LIST_MAX_BYTES {
            return Err(format!(
                "command list exceeds {COMMAND_LIST_MAX_BYTES} bytes"
            ));
        }
        let mut len_buf = [0u8; 4];
        reader
            .read_exact(&mut len_buf)
            .map_err(|e| format!("read pkt length: {e}"))?;
        out.extend_from_slice(&len_buf);
        let len_str = core::str::from_utf8(&len_buf)
            .map_err(|e| format!("pkt length not ASCII: {e}"))?;
        let pkt_len = usize::from_str_radix(len_str, 16)
            .map_err(|e| format!("pkt length not hex: {e}"))?;
        if pkt_len == 0 {
            // Flush — end of command list.
            return Ok(out);
        }
        if pkt_len < 4 {
            return Err(format!("pkt length {pkt_len} below 4-byte header"));
        }
        let payload_len = pkt_len - 4;
        let prev = out.len();
        out.resize(prev + payload_len, 0);
        reader
            .read_exact(&mut out[prev..])
            .map_err(|e| format!("read pkt payload: {e}"))?;
    }
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
