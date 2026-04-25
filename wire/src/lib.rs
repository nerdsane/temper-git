//! tg-wire — smart-HTTP git wire protocol primitives.
//!
//! Pure Rust, no WASM SDK, no temper deps. Host-testable against the
//! real `git` binary via integration tests in `tests/`. When the
//! WASM integrations (`git_upload_pack`, `git_receive_pack`) are
//! scaffolded, they depend on this crate for all wire-format logic
//! and stay thin — just SDK bindings + DB access.
//!
//! Scope so far:
//!   * pkt-line framing (gitprotocol-pack(5))
//!   * smart-HTTP /info/refs advertisement (upload-pack & receive-pack)
//!
//! Next:
//!   * upload-pack v2 want/have negotiation
//!   * pack-v2 emission (depends on canonical + gzip/zlib)
//!   * receive-pack pack parsing + ref-update application
//!
//! Discipline: TigerStyle inherited from canonical/.

#![forbid(unsafe_code)]

pub mod advertise;
pub mod capabilities;
pub mod commands;
pub mod pack;
pub mod pkt_line;
pub mod sideband;

pub use advertise::{advertise_info_refs, AdvertisedRef, Service, ZERO_SHA};
pub use capabilities::{receive_pack_capabilities, upload_pack_capabilities, AGENT};
pub use commands::{parse_commands, CommandKind, CommandsError, ParsedCommands, RefCommand};
pub use pack::{emit_pack, parse_pack, ObjectKind, PackEmitter, PackError, PackObject};
pub use pkt_line::{encode, encode_into, flush, PktLineError, MAX_PAYLOAD};
pub use sideband::{SidebandWriter, CHANNEL_ERROR, CHANNEL_PACK, CHANNEL_PROGRESS};
