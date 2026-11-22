//! Contract-owned (no work unit). Identifier newtypes used across sim-systems and on the wire.
//! Entities, part slots, sides and players come from sim-core and content; these are the ids sim-systems adds.
use content::ids::{PartSlot, PlayerSlot, SideId};
use serde::{Deserialize, Serialize};
use sim_core::EntityId;

macro_rules! idx_id {
    ($(#[$m:meta])* $name:ident, $t:ty) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub $t);
        impl $name { pub fn index(self) -> usize { self.0 as usize } }
    };
}

idx_id!(/// Track number inside one side's picture. Never reused within a sortie; merges, splits and renumbering are
    /// events (DESIGN.md L1661). Numbers start at 1; 0 is "no track".
    TrackId, u32);
idx_id!(/// Alert id, unique within a sortie, ascending by first annunciation (L1676-1687).
    AlertId, u32);
idx_id!(/// Station action instance id (boarding, insertion, photograph...), L786-797.
    ActionId, u32);
idx_id!(/// AI group inside one sortie (task group, convoy, patrol, air patrol), ascending in `SortieConfig::groups`.
    GroupId, u16);
idx_id!(/// Objective index inside one sortie: 0 is the primary, ghosts follow in briefing order.
    ObjectiveId, u8);
idx_id!(/// Chart-table proposal id in co-op (L839).
    ProposalId, u32);

impl TrackId { pub const NONE: TrackId = TrackId(0); pub fn is_none(self) -> bool { self.0 == 0 } }

/// One sensor: a sensor part on a vessel (`index` = its [`PartSlot`]) or sensor `index` of a small body's
/// `MunitionDef::sensors` list (`index` 255 = the munition's seeker).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SensorRef { pub carrier: EntityId, pub index: u16 }
impl SensorRef {
    pub const SEEKER: u16 = 255;
    pub fn part(vessel: EntityId, slot: PartSlot) -> Self { Self { carrier: vessel, index: slot.0 } }
    pub fn seeker(body: EntityId) -> Self { Self { carrier: body, index: Self::SEEKER } }
    /// Key word for RNG draws and stagger hashing: (seed, tick, sensor id, target id), L441, L412.
    pub fn key(self) -> u64 { self.carrier.to_u64() ^ ((self.index as u64) << 48) ^ 0x5EB5_0000_0000_0000 }
}

/// One weapon mount, launcher or decoy launcher on a vessel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MountRef { pub vessel: EntityId, pub slot: PartSlot }

/// Bit set of sides (bit n = `SideId(n)`), at most 8 sides per sortie.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SideMask(pub u8);
impl SideMask {
    pub const NONE: SideMask = SideMask(0);
    pub const ALL: SideMask = SideMask(0xFF);
    pub fn only(side: SideId) -> Self { SideMask(1 << (side.0 & 7)) }
    pub fn has(self, side: SideId) -> bool { self.0 & (1 << (side.0 & 7)) != 0 }
    pub fn with(self, side: SideId) -> Self { SideMask(self.0 | (1 << (side.0 & 7))) }
    pub fn without(self, side: SideId) -> Self { SideMask(self.0 & !(1 << (side.0 & 7))) }
    pub fn union(self, o: SideMask) -> Self { SideMask(self.0 | o.0) }
    pub fn is_empty(self) -> bool { self.0 == 0 }
    /// Sides in ascending id.
    pub fn iter(self) -> impl Iterator<Item = SideId> { (0u8..8).filter(move |b| self.0 & (1 << b) != 0).map(SideId) }
}

/// Bit set of player slots (bit n = `PlayerSlot(n)`), at most 8 players (sandbox, L957-963).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlayerMask(pub u8);
impl PlayerMask {
    pub fn has(self, p: PlayerSlot) -> bool { self.0 & (1 << (p.0 & 7)) != 0 }
    pub fn with(self, p: PlayerSlot) -> Self { PlayerMask(self.0 | (1 << (p.0 & 7))) }
    pub fn without(self, p: PlayerSlot) -> Self { PlayerMask(self.0 & !(1 << (p.0 & 7))) }
    pub fn count(self) -> u32 { self.0.count_ones() }
}
