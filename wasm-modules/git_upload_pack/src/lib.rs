//! git_upload_pack — smart-HTTP upload-pack WASM integration.
//!
//! Handles the two endpoints git clients hit during fetch/clone:
//!
//!   * `GET  /{owner}/{repo}.git/info/refs?service=git-upload-pack`
//!     → advertisement (ref list + capabilities, wrapped in pkt-line).
//!   * `POST /{owner}/{repo}.git/git-upload-pack`
//!     → want/have negotiation + pack-v2 emission.
//!
//! v0 scope (this module): the `info/refs` advertisement only, and
//! for now only the empty-repo and first-commit cases. Want/have
//! negotiation + pack emission are the next slice — they require
//! pulling refs + reachable commits/trees/blobs from the per-repo
//! libSQL DB over OData, plus pack serialization, which is a lot
//! more code.
//!
//! Why ship the advertisement alone: it unblocks `git ls-remote`
//! end-to-end against temper-git, which validates the dispatcher
//! + SDK + wire framing on the critical path. Clone comes next.

#![forbid(unsafe_code)]

extern crate alloc;

use temper_wasm_sdk::http_stream::InboundHttp;
use temper_wasm_sdk::prelude::*;
use tg_wire::{advertise_info_refs, AdvertisedRef, Service};

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        let http_value = ctx
            .http_request
            .clone()
            .ok_or_else(|| "git_upload_pack requires HttpEndpoint dispatch (http_request missing)".to_string())?;
        let http: InboundHttp = serde_json::from_value(http_value)
            .map_err(|e| format!("http_request parse error: {e}"))?;

        let path = http.path.as_str();

        // --- Route 1: info/refs advertisement ------------------------
        if http.method == "GET" && path.ends_with("/info/refs") {
            return serve_info_refs(&http, path);
        }

        // --- Route 2: upload-pack POST (stub in v0) ------------------
        if http.method == "POST" && path.ends_with("/git-upload-pack") {
            return respond_text(
                &http,
                501,
                "application/x-git-upload-pack-result",
                "upload-pack negotiation + pack emission pending (next slice)",
            );
        }

        // Unrecognised path/method within the integration's prefix.
        respond_text(&http, 404, "text/plain", "no upload-pack route matches")
    }
}

fn serve_info_refs(http: &InboundHttp, _path: &str) -> Result<Value, String> {
    // Peek at the `service` query param — advertisements branch on
    // whether the client asked for upload-pack or receive-pack.
    let service = query_param(http, "service")
        .unwrap_or_else(|| "git-upload-pack".to_string());
    let service = match service.as_str() {
        "git-upload-pack" => Service::UploadPack,
        "git-receive-pack" => Service::ReceivePack,
        other => {
            return respond_text(
                http,
                400,
                "text/plain",
                &format!("unknown service '{other}' on /info/refs"),
            );
        }
    };

    // v0: empty-repo advertisement — no real refs yet (requires OData
    // lookup against the per-repo libSQL, next slice).
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

/// Minimal `?key=value` extractor over the path's query string.
/// The InboundHttp.path field carries the full request path
/// (including query). We split once on `?` and look for the key.
fn query_param(http: &InboundHttp, key: &str) -> Option<alloc::string::String> {
    // Prefer an explicit `?service=...` appended by the dispatcher;
    // fall back to scanning headers won't help (query lives on path).
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
