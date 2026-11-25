//! Contract-owned (no work unit). Reliability classes. renet2 channels sit under these ids; the renet2
//! `ChannelConfig` list is built in `transport/netcode.rs` from [`Channel::spec`]. DESIGN.md L920 (damage on a
//! reliable sequenced stream), L924 (snapshots by priority), L926 (projectile spawn and impact events), L943
//! (blueprints on a low-priority reliable lane), L1424 (inputs repeat the last 4 ticks, so unreliable).
//!
//! renet2 serves channels in list order each update, so the order of [`Channel::SERVER`] and
//! [`Channel::CLIENT`] is the priority order. A reliable renet2 channel that runs out of memory disconnects the
//! client, so every sender on a reliable channel checks `can_send` first and the bulk lane self-throttles.
use serde::{Deserialize, Serialize};
use sim_systems::protocol::event::Lane;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Channel {
    /// Session control both ways: hello, welcome, refusals, lobby, commands, theatre, connection panel.
    Session = 0,
    /// Server to client: events of `Lane::Reliable` and `Lane::Log`.
    Events = 1,
    /// Server to client: `EntityRevealed`, `EntityHidden` and every `Lane::Damage` event (cell damage, splits),
    /// on one ordered stream so damage never arrives before the reveal of its vessel.
    Entity = 2,
    /// Server to client: projectile spawns, impacts, splashes (`Lane::Projectile`).
    Projectile = 3,
    /// Unreliable: server to client snapshot frames; client to server input packets.
    Unreliable = 4,
    /// Low-priority reliable both ways: blueprint chunks, checkpoints, save copies, replay release.
    Bulk = 5,
}

/// Delivery class of a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery { Unreliable, ReliableOrdered }

/// What the transport needs to configure one channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelSpec { pub channel: Channel, pub delivery: Delivery, pub max_memory_bytes: usize, pub resend_ms: u32 }

impl Channel {
    /// Server-to-client channels in priority order.
    pub const SERVER: [Channel; 6] = [Channel::Session, Channel::Events, Channel::Entity, Channel::Projectile, Channel::Unreliable, Channel::Bulk];
    /// Client-to-server channels in priority order.
    pub const CLIENT: [Channel; 3] = [Channel::Session, Channel::Unreliable, Channel::Bulk];

    pub fn id(self) -> u8 { self as u8 }
    pub fn from_id(id: u8) -> Option<Channel> {
        Some(match id { 0 => Channel::Session, 1 => Channel::Events, 2 => Channel::Entity, 3 => Channel::Projectile, 4 => Channel::Unreliable, 5 => Channel::Bulk, _ => return None })
    }
    /// Channel for a simulation event. Interest events (reveal and hide) go with damage; see [`Channel::Entity`].
    pub fn for_lane(lane: Lane) -> Channel {
        match lane { Lane::Reliable | Lane::Log => Channel::Events, Lane::Damage => Channel::Entity, Lane::Projectile => Channel::Projectile }
    }
    /// Delivery class and memory limits. Real code; the numbers are transport tuning, not spec values: reliable
    /// limits are sized for a 30-minute worst case burst (2,000 shells, 44 hulls of damage) with headroom.
    pub fn spec(self) -> ChannelSpec {
        let (delivery, max_memory_bytes, resend_ms) = match self {
            Channel::Session => (Delivery::ReliableOrdered, 4 << 20, 100),
            Channel::Events => (Delivery::ReliableOrdered, 4 << 20, 100),
            Channel::Entity => (Delivery::ReliableOrdered, 8 << 20, 100),
            Channel::Projectile => (Delivery::ReliableOrdered, 8 << 20, 100),
            Channel::Unreliable => (Delivery::Unreliable, 1 << 20, 0),
            Channel::Bulk => (Delivery::ReliableOrdered, 16 << 20, 200),
        };
        ChannelSpec { channel: self, delivery, max_memory_bytes, resend_ms }
    }
}
