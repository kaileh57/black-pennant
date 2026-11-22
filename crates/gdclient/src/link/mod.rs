//! Contract-owned (no work unit). Implementations of the session seams in [`crate::seams`].
//! - [`net`] (unit `gd-link-net`): both seams over the `net` crate. `NetLink` wraps a `net::ClientSession`,
//!   `NetHost` wraps a `net::ServerSession`; `loopback` joins them with net's in-memory transport for
//!   single-player and for a host's own client, so every local session runs the multiplayer path (L27,
//!   L899); `join_udp` connects a client outbound; hosting over UDP, port mapping and join codes are here
//!   too. This is the production path for every session kind.
//! - [`channel`] (unit `gd-link-channel`): an in-process stand-in for the same two seams that skips wire
//!   encoding, for the first playable sortie and for tests while the hard `net` units are in progress, plus
//!   the two pieces every non-net link shares: the triple buffer and the [`channel::Presenter`] (net's own
//!   snapshot buffer and shell tracker fed with whole snapshots).
//! - [`replay`] (unit `gd-link-replay`): a `ClientLink` that plays a recorded replay through the
//!   `Presenter`.
//!
//! Which pair a local session uses is one switch, [`LOCAL_BACKEND`]; nothing outside this module can tell
//! the two apart, because both hand out the same `seams` types.
//!
//! # Threads
//! | Object | Thread | Why |
//! | `net::ServerSession` / the stand-in host | clock thread (`session::host`), stepping inside the 3-worker sim pool | owns the `Sim` |
//! | `net::ClientSession` / `Presenter` | main thread | `sample()` is per rendered frame; `update()` never blocks |
//! | `net::portmap::PortMapper` | its own background thread, polled from the main thread | UPnP and NAT-PMP may take 3 s |
//! The in-memory transport (or the stand-in's channels and triple buffer) is the hand-off between the clock
//! thread and the main thread that L1088 asks for: snapshots and events one way, a command queue the other.
pub mod channel;
pub mod net;
pub mod replay;

/// Which implementation backs local sessions (single-player, Splash, tutorial, benchmark, a host's own end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalBackend {
    /// `net::session::local::local_pair` through [`net::loopback`]. Required for release builds (L27).
    Net,
    /// [`channel::pair`]. Cannot host other players and records no replay.
    Channel,
}

/// The backend `BpSession` uses for local sessions: `Net`, so single-player runs the same server over a
/// loopback transport and exercises the multiplayer path (L27, L899), and offline sorties record replays.
/// `Channel` remains for tests. Hosting and joining always use `Net`.
pub const LOCAL_BACKEND: LocalBackend = LocalBackend::Net;
