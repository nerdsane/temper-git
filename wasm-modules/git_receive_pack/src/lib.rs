//! git_receive_pack — smart-HTTP receive-pack WASM integration.
//!
//! Handles the two endpoints git clients hit during push:
//!
//!   * `GET  /{owner}/{repo}.git/info/refs?service=git-receive-pack`
//!     → ref advertisement so the client can compute deltas from
//!     what the server already has.
//!   * `POST /{owner}/{repo}.git/git-receive-pack`
//!     → a pkt-line command list (old-sha new-sha refname ...)
//!     followed by a pack-v2 stream of the new objects.
//!
//! v0 scope (this slice — RFC-0002 Slice A, piece 3):
//!
//!   * GET /info/refs: static empty-repo advertisement (no ref
//!     enumeration yet — needs OData query, piece 4+).
//!   * POST /git-receive-pack:
//!     - Read request body into a bounded buffer.
//!     - Parse command list via `tg_wire::parse_commands`.
//!     - Parse pack via `tg_wire::parse_pack`.
//!     - Compute each pack object's canonical SHA-1 via
//!       `tg_canonical` and log the triple (kind, size, sha).
//!     - Emit a pkt-line "unpack ok" + per-ref "ok <ref>" response
//!       on side-band-64k channel 1.
//!
//! v0 does NOT persist objects or apply ref updates yet. That lets
//! us validate the wire path end-to-end (real `git push` against a
//! deployed pod) before adding the OData-back plumbing, which
//! needs base64-binary JSON bodies + principal header injection.
//! Per RFC-0002 Slice A piece 4, persistence lands next.

#![forbid(unsafe_code)]

extern crate alloc;

use temper_wasm_sdk::http_stream::InboundHttp;
use temper_wasm_sdk::prelude::*;
use tg_wire::{
    advertise_info_refs, commands, encode_into, flush, pack, AdvertisedRef, Service,
};

/// Cap on request-body size the guest will buffer. 64 MiB matches
/// the dispatcher's axum-side to_bytes cap.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Chunk size for the streaming read loop.
const READ_CHUNK_BYTES: usize = 16 * 1024;

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        let http_value = ctx
            .http_request
            .clone()
            .ok_or_else(|| "git_receive_pack requires HttpEndpoint dispatch".to_string())?;
        let http: InboundHttp = serde_json::from_value(http_value)
            .map_err(|e| format!("http_request parse error: {e}"))?;

        // Strip any `?query` from the path so `ends_with` checks
        // match regardless of the `service=` query string.
        let raw = http.path.as_str();
        let path = raw.split('?').next().unwrap_or(raw);

        if http.method == "GET" && path.ends_with("/info/refs") {
            return serve_info_refs(&http);
        }
        if http.method == "POST" && path.ends_with("/git-receive-pack") {
            return serve_receive_pack(&http);
        }
        respond_text(&http, 404, "text/plain", "no receive-pack route matches")
    }
}

/// GET /info/refs?service=git-receive-pack — empty-repo advertisement.
///
/// Piece 4 (next slice) replaces this with a real ref enumeration
/// via OData: query /tdata/Refs?$filter=RepositoryId=... and emit
/// one ref line per active ref.
fn serve_info_refs(http: &InboundHttp) -> Result<Value, String> {
    let service = match query_param(http, "service")
        .as_deref()
        .unwrap_or("git-upload-pack")
    {
        "git-receive-pack" => Service::ReceivePack,
        "git-upload-pack" => Service::UploadPack,
        other => {
            return respond_text(
                http,
                400,
                "text/plain",
                &format!("unknown service '{other}' on /info/refs"),
            );
        }
    };

    let refs: alloc::vec::Vec<AdvertisedRef<'_>> = alloc::vec::Vec::new();
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

/// POST /git-receive-pack — parse the push body, verify object
/// SHAs, emit pkt-line response.
fn serve_receive_pack(http: &InboundHttp) -> Result<Value, String> {
    let body = read_full_body(http)?;

    // Parse the command list. pack bytes begin at `pack_offset`.
    let parsed = commands::parse_commands(&body)
        .map_err(|e| format!("parse_commands: {e}"))?;

    // Parse the pack.
    let pack_slice = &body[parsed.pack_offset..];
    let objects = pack::parse_pack(pack_slice)
        .map_err(|e| format!("parse_pack: {e}"))?;

    // Compute canonical SHA-1 per object. The pack stores RAW body
    // bytes (no `<kind> <len>\0` prefix); tg_canonical's hash
    // functions wrap that prefix themselves.
    let mut object_shas: alloc::vec::Vec<(pack::ObjectKind, alloc::string::String)> =
        alloc::vec::Vec::with_capacity(objects.len());
    for obj in &objects {
        let sha = match obj.kind {
            pack::ObjectKind::Blob => tg_canonical::blob_hash(&obj.data),
            pack::ObjectKind::Commit => sha_from_prefix("commit", &obj.data),
            pack::ObjectKind::Tree => sha_from_prefix("tree", &obj.data),
            pack::ObjectKind::Tag => sha_from_prefix("tag", &obj.data),
        };
        object_shas.push((obj.kind, sha));
    }

    // Emit the receive-pack response.
    //
    //   000eunpack ok\n
    //   <pkt>ok refs/heads/main\n
    //   0000
    //
    // wrapped on side-band-64k channel 1 (pack bytes / results).
    let mut response = alloc::vec::Vec::new();

    // Inner report (unwrapped).
    let mut inner = alloc::vec::Vec::new();
    encode_into(&mut inner, b"unpack ok\n")
        .map_err(|e| format!("encode unpack: {e}"))?;
    for cmd in &parsed.commands {
        let line = alloc::format!("ok {}\n", cmd.refname);
        encode_into(&mut inner, line.as_bytes())
            .map_err(|e| format!("encode ok line: {e}"))?;
    }
    flush(&mut inner);

    // Wrap inner report in sideband-64k frames (channel 1).
    let sideband = parsed.capabilities.iter().any(|c| c == "side-band-64k");
    if sideband {
        for chunk in inner.chunks(65515) {
            // pkt-line payload = 1 byte channel + chunk
            let mut payload = alloc::vec::Vec::with_capacity(1 + chunk.len());
            payload.push(0x01); // channel 1 = pack/result data
            payload.extend_from_slice(chunk);
            encode_into(&mut response, &payload)
                .map_err(|e| format!("encode sideband: {e}"))?;
        }
        flush(&mut response);
    } else {
        // No sideband: emit the report unwrapped.
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
        "command_count": parsed.commands.len(),
        "object_count": object_shas.len(),
        "object_shas": object_shas
            .iter()
            .map(|(k, s)| format!("{:?}:{}", k, s))
            .collect::<alloc::vec::Vec<_>>(),
    }))
}

/// Compute SHA-1 of `<prefix> <len>\0<body>` — the canonical git
/// object hash. tg_canonical exposes typed helpers for blob/commit/
/// tree/tag but those take typed inputs; for commit/tree/tag the
/// pack gives us raw body bytes that are already in canonical form,
/// so we just need to prepend the header and hash.
fn sha_from_prefix(prefix: &str, body: &[u8]) -> alloc::string::String {
    let header = alloc::format!("{} {}\0", prefix, body.len());
    let mut hasher = tg_canonical::Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(body);
    hasher.hex()
}

/// Read the full request body into a Vec<u8>, bounded by MAX_BODY_BYTES.
fn read_full_body(http: &InboundHttp) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
    let mut body = alloc::vec::Vec::new();
    let mut scratch = alloc::vec![0u8; READ_CHUNK_BYTES];
    let mut reader = http.request_body();
    loop {
        match reader.read_next_chunk(&mut scratch) {
            Ok(None) => break, // EOF
            Ok(Some(n)) => {
                if body.len() + n > MAX_BODY_BYTES {
                    return Err(alloc::format!(
                        "request body exceeds {} bytes",
                        MAX_BODY_BYTES
                    ));
                }
                body.extend_from_slice(&scratch[..n]);
            }
            Err(e) => return Err(alloc::format!("request_body read: {e}")),
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

fn query_param(http: &InboundHttp, key: &str) -> Option<alloc::string::String> {
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
