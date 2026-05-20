//! git_receive_pack — smart-HTTP receive-pack WASM integration.
//!
//! Handles the two endpoints git clients hit during push:
//!
//!   * `GET  /{owner}/{repo}.git/info/refs?service=git-receive-pack`
//!     → ref advertisement.
//!   * `POST /{owner}/{repo}.git/git-receive-pack`
//!     → pkt-line command list + pack-v2 stream.
//!
//! For `POST /git-receive-pack`, this module is now only the Git wire
//! adapter. It reads the streamed body, parses the receive-pack command list,
//! buffers the raw pack bytes, and returns typed parameters for the
//! kernel-owned HttpEndpoint action bridge. The bridge invokes the
//! spec-defined `Repository.IngestPack` action; WASM does not dispatch Temper
//! actions or fan out object/ref writes.
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
use std::io::Read;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use temper_wasm_sdk::http_stream::InboundHttp;
use temper_wasm_sdk::prelude::*;
use tg_wire::{AdvertisedRef, CommandKind, Service, advertise_info_refs, commands};

/// Cap on the command-list bytes accumulated before pack parsing
/// begins. The list is pkt-line framed (4-hex length + payload) and
/// in practice tops out at a few KiB even for very large pushes —
/// 1 MiB is generous head-room with a clear failure mode.
const COMMAND_LIST_MAX_BYTES: usize = 1 * 1024 * 1024;
/// BufReader capacity for the request-body stream.
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
            return serve_info_refs(&ctx, &http);
        }
        if http.method == "POST" && path.ends_with("/git-receive-pack") {
            return serve_receive_pack(&ctx, &http);
        }
        respond_text(&http, 404, "text/plain", "no receive-pack route matches")
    }
}

fn serve_info_refs(ctx: &Context, http: &InboundHttp) -> Result<Value, String> {
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

    let owner = http.params.get("owner").cloned().unwrap_or_default();
    let repo = http.params.get("repo").cloned().unwrap_or_default();
    let repository_id = format!("rp-{owner}-{repo}");
    let principal = effective_principal(ctx, &http.headers);
    let api_base = temper_api_from_headers(&http.headers);
    let refs_rows = fetch_refs_for_repo(ctx, &principal, &repository_id, &api_base)?;

    let owned: Vec<(String, String)> = refs_rows
        .into_iter()
        .filter(|r| r.status == "Active" && r.name != "HEAD")
        .map(|r| (r.target_sha, r.name))
        .collect();
    let refs: Vec<AdvertisedRef<'_>> = owned
        .iter()
        .map(|(sha, name)| AdvertisedRef {
            sha: sha.as_str(),
            name: name.as_str(),
        })
        .collect();

    let body =
        advertise_info_refs(service, &refs).map_err(|e| format!("advertise_info_refs: {e}"))?;
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

fn serve_receive_pack(_ctx: &Context, http: &InboundHttp) -> Result<Value, String> {
    // Convention-based repository id derivation.
    let owner = http.params.get("owner").cloned().unwrap_or_default();
    let repo = http.params.get("repo").cloned().unwrap_or_default();
    let repository_id = format!("rp-{owner}-{repo}");

    // Stream the request body. We read the command list bytes
    // (pkt-line framed; ends at a 0000 flush), then buffer the raw
    // pack bytes as action input. Repository.IngestPack's spec-triggered
    // parser integration owns object verification and sub-write emission.
    let mut reader = std::io::BufReader::with_capacity(
        BUFREAD_CAPACITY,
        WasmRequestReader::new(http.request_body()),
    );
    let cmd_bytes = read_command_list(&mut reader)?;
    let parsed =
        commands::parse_commands(&cmd_bytes).map_err(|e| format!("parse_commands: {e}"))?;

    let mut pack_bytes = Vec::new();
    reader
        .read_to_end(&mut pack_bytes)
        .map_err(|e| format!("read pack bytes: {e}"))?;

    let needs_pack = parsed
        .commands
        .iter()
        .any(|cmd| cmd.kind() != CommandKind::Delete);
    if needs_pack && pack_bytes.is_empty() {
        return Err("receive-pack command list requires pack bytes".to_string());
    }

    let ref_updates: Vec<Value> = parsed
        .commands
        .iter()
        .map(|cmd| {
            json!({
                "Name": cmd.refname,
                "PreviousCommitSha": cmd.old_sha,
                "NewCommitSha": cmd.new_sha,
            })
        })
        .collect();
    let refs: Vec<String> = parsed
        .commands
        .iter()
        .map(|cmd| cmd.refname.clone())
        .collect();
    let sideband = parsed.capabilities.iter().any(|c| c == "side-band-64k");

    let mut action_params = json!({
        "RefUpdates": ref_updates,
        "ClientRequestId": receive_pack_client_request_id(&repository_id, &cmd_bytes, &pack_bytes),
    });
    if !pack_bytes.is_empty() {
        action_params["PackBytes"] = Value::String(B64.encode(&pack_bytes));
    }

    Ok(json!({
        "action_params": action_params,
        "git_receive_pack": {
            "refs": refs,
            "sideband": sideband,
            "commands": parsed.commands.len(),
            "pack_bytes": pack_bytes.len(),
            "repository_id": repository_id,
        },
    }))
}

fn receive_pack_client_request_id(
    repository_id: &str,
    command_bytes: &[u8],
    pack_bytes: &[u8],
) -> String {
    let mut hasher = tg_canonical::Sha1::new();
    hasher.update(repository_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(command_bytes);
    hasher.update(b"\0");
    hasher.update(pack_bytes);
    format!("git-receive-pack:{}", hasher.hex())
}

fn effective_principal(ctx: &Context, headers: &[(String, String)]) -> Principal {
    let resolved = auth::resolve_principal(ctx, headers);
    if resolved.is_anonymous() {
        Principal::system()
    } else {
        resolved
    }
}

struct RefRow {
    name: String,
    target_sha: String,
    status: String,
}

fn fetch_refs_for_repo(
    ctx: &Context,
    principal: &Principal,
    repository_id: &str,
    api_base: &str,
) -> Result<Vec<RefRow>, String> {
    let url = format!("{api_base}/tdata/Refs");
    let resp = ctx
        .http_call("GET", &url, &principal.outbound_headers(), "")
        .map_err(|e| format!("fetch refs: {e}"))?;
    if !(200..400).contains(&resp.status) {
        return Err(format!("fetch refs status {}", resp.status));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&resp.body).map_err(|e| format!("refs parse: {e}"))?;
    let items = parsed
        .get("value")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut rows = Vec::with_capacity(items.len());
    for row in items {
        let fields = row
            .get("fields")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
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
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

fn temper_api_from_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| format!("http://{v}"))
        .unwrap_or_else(|| TEMPER_API.to_string())
}

/// `std::io::Read` adapter over the SDK's inbound body reader. Used
/// to read the streamed receive-pack body.
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
        let len_str =
            core::str::from_utf8(&len_buf).map_err(|e| format!("pkt length not ASCII: {e}"))?;
        let pkt_len =
            usize::from_str_radix(len_str, 16).map_err(|e| format!("pkt length not hex: {e}"))?;
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
