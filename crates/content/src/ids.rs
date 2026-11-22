//! Contract-owned (no work unit). Identifier newtypes shared by every crate.
//!
//! String keys are the stable form used in RON files, saves and blueprints.
//! Dense `*Idx` values are assigned by [`crate::catalogue::Catalogue`] at load
//! (ascending key order, so they are deterministic for one build) and are what
//! the simulation and the wire protocol carry. DESIGN.md L1106-1112.
use serde::{Deserialize, Serialize};

macro_rules! key_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { f.write_str(&self.0) }
        }
    };
}
macro_rules! idx_id {
    ($(#[$m:meta])* $name:ident, $t:ty) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub $t);
        impl $name { pub fn index(self) -> usize { self.0 as usize } }
    };
}

key_id!(/// Catalogue part key, e.g. `gun.76mm.single`.
    PartId);
key_id!(/// Kit key, e.g. `kit.gun76.twin`.
    KitId);
key_id!(/// Munition or small-body key (missile, torpedo, mine, depth charge, drone, aircraft).
    MunitionId);
key_id!(/// Combine enemy class key. Every class is a blueprint (L756).
    ClassId);
key_id!(/// Fleet (task group, convoy, patrol) template key.
    FleetTemplateId);
key_id!(/// Operation key: one of the 11 templates, a story operation, an ace duel or a capstone.
    OperationId);
key_id!(/// Ace key (7 named Combine captains, L774).
    AceId);
key_id!(/// Doctrine card key (about 30 authored cards, L772).
    CardId);
key_id!(/// Theatre region key (4 regions, L735).
    RegionId);
key_id!(/// Theatre graph node key (48 nodes, L741).
    TheatreNodeId);
key_id!(/// Difficulty preset key: `easy`, `normal`, `hard`.
    DifficultyId);
key_id!(/// Translation key. The Godot client resolves it with `tr()`; English lives in `game/i18n/*.csv`.
    /// No crate outside gdclient ever holds player-facing English text.
    TextKey);
key_id!(/// Procedural mesh generator key understood by hull-mesh (weapons, masts, detail set, L1233).
    ModelId);
key_id!(/// Hand-written SVG icon key resolved by the client.
    IconId);

idx_id!(/// Dense catalogue index of a part type. Wire and sim form of [`PartId`].
    PartIdx, u16);
idx_id!(/// Dense catalogue index of a munition or small-body type.
    MunitionIdx, u16);
idx_id!(/// Index of a part placement inside one blueprint ("ascending part index", L198). Also the mount id.
    PartSlot, u16);
idx_id!(/// Index of a placed kit inside one blueprint.
    KitSlot, u16);
idx_id!(/// Compartment id inside one vessel; ascending order is the fixed processing order (L630).
    CompartmentId, u16);
idx_id!(/// Linear index of a cell in a vessel's dense grid (see [`crate::cell::CellGrid::index`]).
    /// This is the "cell index" of the replicated damage stream (L561).
    CellIndex, u32);
idx_id!(/// Control-graph node id; ties in topological order break by ascending id (L369).
    GraphNodeId, u32);
idx_id!(/// Player slot in a lobby or campaign (0 = host).
    PlayerSlot, u8);
idx_id!(/// Side (team) in one sortie. 0 is always the LANTERN / first player side.
    SideId, u8);

/// BLAKE3 content hash of canonical RON (L2079). Addresses blueprints on disk and on the network.
/// Text formats (RON) write it as 64 lowercase hex digits, or an empty string when unset; binary formats
/// write the 32 raw bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash(pub [u8; 32]);
impl ContentHash {
    pub const EMPTY: ContentHash = ContentHash([0; 32]);
    pub fn is_empty(&self) -> bool { self.0 == [0; 32] }
    pub fn to_hex(&self) -> String {
        if self.is_empty() { return String::new(); }
        let mut s = String::with_capacity(64);
        for b in self.0 { s.push(char::from_digit((b >> 4) as u32, 16).unwrap()); s.push(char::from_digit((b & 15) as u32, 16).unwrap()); }
        s
    }
    pub fn from_hex(s: &str) -> Option<ContentHash> {
        if s.is_empty() { return Some(ContentHash::EMPTY); }
        if s.len() != 64 || !s.is_ascii() { return None; }
        let mut out = [0u8; 32];
        for (i, pair) in s.as_bytes().chunks(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16)?; let lo = (pair[1] as char).to_digit(16)?;
            out[i] = (hi * 16 + lo) as u8;
        }
        Some(ContentHash(out))
    }
}
impl Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() { s.serialize_str(&self.to_hex()) } else { self.0.serialize(s) }
    }
}
impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            let s = String::deserialize(d)?;
            ContentHash::from_hex(&s).ok_or_else(|| serde::de::Error::custom("content hash must be 64 hex digits or empty"))
        } else { Ok(ContentHash(<[u8; 32]>::deserialize(d)?)) }
    }
}
