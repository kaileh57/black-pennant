//! net: replication, transports, hosting and sessions. No Godot dependency.
//! DESIGN.md "Multiplayer" (L897-1050), "Crate layout" (L1060-1074): replication schema, `Transport` trait,
//! in-memory and UDP (renet2_netcode) backends, port mapping, join codes; the Steam backend only with `steam`.
//!
//! One machine runs the only full simulation; every other machine sends inputs and draws what it receives
//! (L899). Single-player is the same program with a [`session::ServerSession`] in-process over the in-memory
//! transport, so every campaign session exercises this crate. The model is server-authoritative state sync:
//! no lockstep, no rollback (L903-908).
//!
//! # Data flow
//! Host: [`session::ServerSession`] owns the `sim_systems::Sim` of the running sortie, steps it at the tick
//! rate inside the caller's 3-worker rayon pool, routes `OutEvent`s to clients by audience ([`events`]),
//! builds one `Snapshot` per player at 30 Hz with `Sim::snapshot(viewer)` and encodes it per client against
//! that client's acknowledged baseline ([`delta`]) inside that client's adaptive byte budget ([`priority`],
//! [`quality`]). Interest management and anti-wallhack are the same filter (L924): the Sim already cuts each
//! viewer's snapshot to what its side has sensed; net only ever sends a client its own viewer's snapshot and
//! the events whose `Audience` admits it.
//! Client: [`session::ClientSession`] sends commands (reliable) and inputs (unreliable, each packet repeating
//! the last 4, L1424), decodes snapshots and events, and feeds [`interp::SnapshotBuffer`], which yields an
//! interpolated `Snapshot` at the render time (4 intervals = 133 ms behind by default, L914).
//!
//! # Module map
//! One file or directory is one work unit; the unit id is in each module's `//!` header.
//! Contract-owned files (no unit; change only through the orchestrator): this file, `error.rs`, `channel.rs`,
//! `msg.rs`, `clock.rs`, `schema/mod.rs`, `transport/mod.rs`, `session/mod.rs`. In unit-owned files the public
//! types and signatures are frozen contract: owners fill `todo!()` bodies, add private helpers or extra files
//! inside their own directory, and may add or change `pub(crate)` and private fields of structs defined in
//! their own unit's files (public fields and every signature stay as written).
//!
//! | Unit | Files | Scope |
//! | `net-schema` | `schema/{bits,quant,replicate,codec}.rs` | bit packing, quantisation, the one replication schema, message codec |
//! | `net-delta` (hard) | `delta/` | per-client snapshot delta encoding and client decoding |
//! | `net-priority` | `priority/` | priority accumulator, per-item update rates |
//! | `net-events` | `events/` | audience routing, lane to channel, projectile share |
//! | `net-interp` (hard) | `interp/` | snapshot buffer, interpolation delay, corrections, client shell integration |
//! | `net-quality` | `quality/` | adaptive cap, drop rule, saturation, host load meter, connection log |
//! | `net-transport` | `transport/netcode.rs` | memory and UDP backends on renet2 and renet2_netcode |
//! | `net-conditioner` | `transport/conditioner.rs` | link conditioner socket wrappers |
//! | `net-steam` (phase 2) | `transport/steam.rs` | Steam backend, `steam` feature only |
//! | `net-join` | `join/` | lobby secret, join codes, lobby key, netcode key and connect tokens, client identity |
//! | `net-portmap` | `portmap/` | UPnP IGD, NAT-PMP and PCP mapping, double NAT, interfaces, VPN, mock gateways |
//! | `net-lobby` | `lobby/` | membership, admission, host commands, lobby state messages |
//! | `net-blueprints` | `blueprints/` | content-addressed cache, chunked transfer, entitlement, server validation |
//! | `net-server-session` (hard) | `session/server.rs`, `session/local.rs` | the host loop |
//! | `net-client-session` (hard) | `session/client.rs` | the client loop |
//! | `net-coop` | `coop/` | checkpoints, succession, re-host, save copies, campaign relay |
//! | `net-replay` | `replay/` | replay recording in the session, PvP release, replay store, wire stream capture |
//!
//! Later-phase work, to schedule last: `net-steam` (phase 2); the 1.0 player limits (4-player co-op, 3v3 and
//! 4v4, escort) are data in `content::ruleset::max_players` and need no net change; the direction-shot
//! rewind (L951) is conditional on the aim-bot trigger and lives in sim-systems (`input::PoseHistory`), not
//! here; the 20 Hz non-own-vessel fallback and salvo batching (L935) are switches in [`priority`] and
//! [`events`], off unless the bandwidth gate fails.
//!
//! # Rules
//! - No renet2 type appears in a public signature outside `transport/`: renet2 sits behind the trait because
//!   of its single-maintainer history (L982).
//! - net never reads a wall clock except through [`clock::Clock`]; tests drive time by hand.
//! - Every player-facing string is a `TextKey` or a typed enum; net produces no English text.
//! - Steam code compiles only with the `steam` feature (CLAUDE.md hard rule).
#![allow(dead_code, unused_variables, unused_imports)]

pub mod blueprints;
pub mod channel;
pub mod clock;
pub mod coop;
pub mod delta;
pub mod error;
pub mod events;
pub mod interp;
pub mod join;
pub mod lobby;
pub mod msg;
pub mod portmap;
pub mod priority;
pub mod quality;
pub mod replay;
pub mod schema;
pub mod session;
pub mod transport;

pub use error::{JoinRefusal, NetError};
pub use session::client::ClientSession;
pub use session::server::ServerSession;
pub use sim_systems::PROTOCOL_VERSION;

/// Default UDP port (L994); the dedicated server config and the host panel can change it.
pub const DEFAULT_PORT: u16 = content::settings::DEFAULT_PORT;

/// renet2_netcode protocol id. Constant across protocol versions on purpose: a version mismatch must connect
/// far enough to exchange [`msg::Hello`] so the refusal can name both versions (L1003). Changing this value
/// would make old and new builds time out silently instead.
pub const NETCODE_PROTOCOL_ID: u64 = 0x4250_4E4E_4554_0001; // "BPNNET" v1, frozen

/// Wire format version of everything after the hello header (message codec and snapshot schema). Bumped with
/// `PROTOCOL_VERSION`; kept separate so the hello header can stay frozen forever.
pub const WIRE_VERSION: u32 = PROTOCOL_VERSION;
