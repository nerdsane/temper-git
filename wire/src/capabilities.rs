//! Capability strings advertised on the first ref line.
//!
//! Clients parse the first pkt-line's trailing `\0capabilities\n` and
//! use those to negotiate. We advertise the minimum v1 (non-v2) set
//! needed for modern git clients to clone and push successfully.
//!
//! Deliberately no `no-thin` (we do accept thin packs), no
//! `shallow` (initial implementation serves full history), no
//! `include-tag` (client can request it via want/have v2 later).
//! Adding those is a spec change we gate behind an ADR.

/// Agent header baked into every capability block. Matches the
/// "agent=..." convention from git's own server; updating on
/// every release helps operators grep the wire.
pub const AGENT: &str = "agent=temper-git/0.1.0";

/// Capabilities we advertise on git-upload-pack (fetch/clone side).
///
/// * `multi_ack_detailed` — negotiate shared history with detailed ACKs.
/// * `no-done` — client can omit the final `done` when server has
///   definitively sent back everything it's going to.
/// * `side-band-64k` — multiplex progress / pack bytes on one socket.
/// * `thin-pack` — client can send deltas against objects we already have.
/// * `ofs-delta` — we emit OFS_DELTA pack entries (smaller than
///   REF_DELTA for objects in the same pack).
pub fn upload_pack_capabilities() -> String {
    format!("multi_ack_detailed no-done side-band-64k thin-pack ofs-delta {AGENT}")
}

/// Capabilities we advertise on git-receive-pack (push side).
///
/// * `report-status` — return per-ref status after push.
/// * `delete-refs` — allow clients to delete refs via push.
/// * `side-band-64k` — multiplex progress bytes during pack write-out.
///
/// Deliberately NOT advertised (RFC-0002 slice A):
/// * `ofs-delta` — our v0 pack parser rejects delta entries.
///   Without this capability, clients send plain objects only.
/// * `atomic` — per-ref best-effort is fine for v0.
/// * `push-options` — no options to receive yet.
pub fn receive_pack_capabilities() -> String {
    format!("report-status delete-refs side-band-64k {AGENT}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_pack_caps_contain_required() {
        let caps = upload_pack_capabilities();
        for required in ["multi_ack_detailed", "side-band-64k", "thin-pack", "ofs-delta", AGENT] {
            assert!(caps.contains(required), "missing {required} in {caps}");
        }
    }

    #[test]
    fn receive_pack_caps_contain_required() {
        let caps = receive_pack_capabilities();
        for required in ["report-status", "delete-refs", "side-band-64k", AGENT] {
            assert!(caps.contains(required), "missing {required} in {caps}");
        }
    }

    #[test]
    fn agent_string_is_well_formed() {
        assert!(AGENT.starts_with("agent="));
        // No whitespace inside the value — git tokenizes on space.
        let value = &AGENT["agent=".len()..];
        assert!(!value.contains(' '));
    }
}
