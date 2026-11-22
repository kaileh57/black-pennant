//! Contract-owned types plus unit `content-blueprint` (the functions in `io.rs`). The blueprint file format.
//! DESIGN.md L2067-2087 (format, hash, limits, migrations), L128-134 (cells, paint), L943-945 (network use).
//!
//! Field order in [`Blueprint`] is the file order the spec lists: header, hull, cells, paint table, then parts
//! and kits with socket bindings, then tuning, rules tables, graphs and Luau script sources (L2073-2077).
//! Every type in the hashed canonical form uses `deny_unknown_fields` (the loader never silently drops unknown
//! fields, L2083) and ordered maps only.
pub mod io;

use crate::cell::{CellPos, CellRows};
use crate::graph::{GraphOverrides, OfficerSettings, Rule};
use crate::hull::Hull;
use crate::ids::{ContentHash, KitId, KitSlot, PartId, PartSlot};
use crate::kit::{KitSettings, LineageEntry, PartRot};
use crate::network::NetworkEdits;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Current blueprint format; starts at 1 (L2073).
pub const BLUEPRINT_FORMAT: u32 = 1;
/// Untrusted-input limits, checked before parsing (L2081).
pub const MAX_RON_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RON_DEPTH: usize = 128;
/// The server refuses a blueprint over this compressed size (L945).
pub const MAX_COMPRESSED_BYTES: usize = 8 * 1024 * 1024;
/// Paint table size: 4-bit swatch index per cell (L130, L2076).
pub const MAX_PAINT: usize = 16;
pub const DEFAULT_LICENCE: &str = "CC-BY-4.0";

/// Ship or enemy class; decides the length limit a loader enforces (L126).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlueprintKind { #[default] Ship, EnemyClass }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub format: u32,
    /// Game version that wrote the file.
    pub game_version: String,
    /// BLAKE3 over the canonical RON with this field empty (L2079).
    pub hash: ContentHash,
    pub kind: BlueprintKind,
    /// Player-entered ship name (not a translation key). Stock blueprints also set `name_key`.
    pub name: String,
    pub name_key: Option<crate::ids::TextKey>,
    pub author: Option<String>,
    /// SPDX id; recorded and shown, not enforced (L2073).
    pub licence: String,
    /// 32 most recent ancestors.
    pub lineage: Vec<LineageEntry>,
}

/// What a paint entry colours; picks which clamp applies (L130, L2076).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaintUse { #[default] Hull, Marking }

/// One paint table entry in linear sRGB. New blueprints start with the 8 USN swatches and the antifouling
/// colour; up to 7 more are picked. Loaders re-apply the clamps: hull Y <= 0.6 and OKLCH chroma <= 0.06,
/// markings Y <= 0.7 and chroma <= 0.12 (L2076).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaintEntry {
    /// World token name for stock swatches (e.g. `hull.haze_grey`), `None` for picked colours.
    pub token: Option<String>,
    pub rgb: [f32; 3],
    pub usage: PaintUse,
}

/// Stripes, flags and hull numbers (MSDF text), L130.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marking { pub kind: MarkingKind, pub cell: CellPos, pub face: crate::cell::Face, pub paint: u8, pub size_cells: u8, pub text: String }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkingKind { Stripe, Flag, HullNumber }

/// One placed part. Its index in [`Blueprint::parts`] is its [`PartSlot`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartPlacement {
    pub part: PartId,
    /// Minimum-corner cell of the rotated footprint.
    pub pos: CellPos,
    pub rot: PartRot,
    /// The kit instance this part belongs to, if any.
    pub kit: Option<KitSlot>,
    /// Player label ("Gun 2"); `None` = generated from category and order.
    pub label: Option<String>,
    /// Adjustable load: fuel fraction for tanks, kilograms for ballast, rounds fraction for magazines.
    pub load: Option<f64>,
}

/// Binding of one kit socket to the structure it attached to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocketBinding { pub socket: String, pub cell: CellPos }

/// One placed kit. Updating the kit definition offers to migrate every placed copy (L140).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KitPlacement {
    pub kit: KitId,
    pub version: u32,
    pub pos: CellPos,
    pub rot: PartRot,
    pub mirrored: bool,
    pub sockets: Vec<SocketBinding>,
    pub settings: KitSettings,
    /// Slots of the parts this instance placed, in the kit's part order.
    pub parts: Vec<PartSlot>,
}

/// A kit definition embedded in the blueprint so a shared ship carries the player kits it uses.
pub type EmbeddedKits = BTreeMap<KitId, crate::kit::KitDef>;

/// A ship design. Canonical RON of this struct is what gets hashed, shared and sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blueprint {
    pub header: Header,
    pub hull: Hull,
    pub cells: CellRows,
    pub paint: Vec<PaintEntry>,
    pub markings: Vec<Marking>,
    pub parts: Vec<PartPlacement>,
    pub kits: Vec<KitPlacement>,
    pub embedded_kits: EmbeddedKits,
    /// L0 and L1 settings, doctrine and tuning.
    pub officers: OfficerSettings,
    /// L2 rules table, at most 32 (L336).
    pub rules: Vec<Rule>,
    pub networks: NetworkEdits,
    /// L3 graph overrides and L4 script sources.
    pub graph: GraphOverrides,
}

/// Exterior-only render blueprint the server derives for the other team in PvP: hull shell, visible
/// superstructure and mounts; no internals, settings or graphs. Damage events to that team reference these
/// cells only (L945).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExteriorBlueprint {
    pub source_hash: ContentHash,
    pub name: String,
    pub size: [u16; 3],
    pub cells: CellRows,
    pub paint: Vec<PaintEntry>,
    pub markings: Vec<Marking>,
    /// Weather-deck parts only (mounts, launchers, masts), with their original slots.
    pub parts: Vec<(PartSlot, PartPlacement)>,
}

/// Either kind of shareable file; a share string's content says which (L2093).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Shareable { Ship(Box<Blueprint>), Kit(Box<crate::kit::KitDef>) }
