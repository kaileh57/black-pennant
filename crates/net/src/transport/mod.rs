//! Contract-owned (no work unit). The internal Transport trait (L982-990): message-level connections with
//! the [`Channel`] reliability classes. renet2 1.0.0 provides channels and fragmentation above every backend
//! and sits behind these traits because of its single-maintainer history.
//!
//! | Backend | Built | Used for | File (unit) |
//! | In-memory (renet2_netcode memory sockets) | always | single-player, the host's own client | `netcode.rs` (`net-transport`) |
//! | UDP (renet2_netcode native socket) | always | listen hosts, LAN, direct IP, dedicated, CI bot matches, soak | `netcode.rs` (`net-transport`) |
//! | Steam Networking Sockets + SDR (renet2_steam) | `steam` feature | Steam invites and lobbies (phase 2) | `steam.rs` (`net-steam`) |
//!
//! The link conditioner (`conditioner.rs`, unit `net-conditioner`) wraps the renet2_netcode socket of either
//! netcode backend, so renet2's own RTT and loss measurements see the conditioned link (the drop-rule and cap
//! gates read them). A listen host serves the in-memory transport for its own client and, once the player
//! chooses to host, a UDP transport as well: `ServerSession` multiplexes a list of boxed transports.
//!
//! Client ids: on UDP the renet2 client id is the install's random 64-bit id (L1003); on the in-memory backend
//! it is the small id the pair was created with. Sessions identify players by the id in [`crate::msg::Hello`],
//! never by the transport id.
pub mod conditioner;
pub mod netcode;
#[cfg(feature = "steam")]
pub mod steam;

use crate::channel::Channel;
use crate::error::NetError;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportKind { Memory, Udp, Steam }

/// Link measurements renet2 keeps per connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkStats { pub rtt_ms: f32, pub loss: f32, pub sent_kbps: f32, pub recv_kbps: f32 }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisconnectCause {
    /// The remote end closed the connection.
    Remote,
    TimedOut,
    /// This end disconnected it.
    Local,
    /// A reliable channel overflowed or the transport failed; renet2's reason, for the log.
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerTransportEvent { Connected { client: u64 }, Disconnected { client: u64, cause: DisconnectCause } }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState { Connecting, Connected, Disconnected(DisconnectCause) }

/// Host side of one backend.
pub trait ServerTransport: Send {
    fn kind(&self) -> TransportKind;
    /// Receive and process packets, advance renet2 by `dt`. `unix_now` feeds netcode token expiry.
    fn update(&mut self, dt: Duration, unix_now: Duration) -> Result<(), NetError>;
    fn poll_event(&mut self) -> Option<ServerTransportEvent>;
    fn clients(&self) -> Vec<u64>;
    fn receive(&mut self, client: u64, channel: Channel) -> Option<Vec<u8>>;
    /// A reliable renet2 channel that overflows disconnects the client: check before every reliable send.
    fn can_send(&self, client: u64, channel: Channel, bytes: usize) -> bool;
    fn send(&mut self, client: u64, channel: Channel, bytes: Vec<u8>);
    /// Send queued packets now.
    fn flush(&mut self);
    fn stats(&self, client: u64) -> Option<LinkStats>;
    fn client_addr(&self, client: u64) -> Option<SocketAddr>;
    fn disconnect(&mut self, client: u64);
    fn disconnect_all(&mut self);
    fn set_max_clients(&mut self, n: usize);
    /// Addresses this backend listens on (empty for the in-memory backend).
    fn local_addrs(&self) -> Vec<SocketAddr>;
}

/// Client side of one backend.
pub trait ClientTransport: Send {
    fn kind(&self) -> TransportKind;
    fn update(&mut self, dt: Duration, unix_now: Duration) -> Result<(), NetError>;
    fn state(&self) -> ConnState;
    fn receive(&mut self, channel: Channel) -> Option<Vec<u8>>;
    fn can_send(&self, channel: Channel, bytes: usize) -> bool;
    fn send(&mut self, channel: Channel, bytes: Vec<u8>);
    fn flush(&mut self) -> Result<(), NetError>;
    fn stats(&self) -> LinkStats;
    /// Time since the last packet from the host (co-op host loss after 10 s, L1005).
    fn time_since_last_packet(&self) -> Duration;
    fn disconnect(&mut self);
}
