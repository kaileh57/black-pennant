//! Unit `content-kit`. Kits: versioned sub-assemblies with named sockets, a settings sheet and their own graph
//! fragment. DESIGN.md L140 (kits), L2069 (`.kit.ron`), L2087 (lineage, forked officer kits).
//! Stock kits are authored by the `data-kits-*` units as `blueprints/kits/*.kit.ron`.
use crate::cell::CellPos;
use crate::error::ContentResult;
use crate::graph::{ControlGraph, OfficerSettings};
use crate::ids::{ContentHash, KitId, PartId, TextKey};
use crate::part::PartTag;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod rot;
pub(crate) mod text;
#[cfg(test)]
mod tests;

pub use rot::{kit_part_placements, kit_sockets_at, normalize_rot, part_rot_from_matrix, part_rot_matrix, part_rot_orientation, rotated_size, PlacedKitPart, PlacedSocket, TILT_COUNT, YAW_COUNT};

/// Current kit file format. Every change ships a migration n -> n+1 (L2083).
pub const KIT_FORMAT: u32 = 1;

/// Rotation of a placed part or kit: quarter turns about vertical (R / Shift+R) and tilt (T), L244.
/// `tilt` (modulo 6) picks which way the part stands, then `yaw` (modulo 4) turns it about +Y, one turn carrying
/// +X to -Z; the 24 reduced values name the 24 cube rotations once each. See [`part_rot_matrix`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartRot { pub yaw: u8, pub tilt: u8 }

/// One ancestor in a lineage: hash plus author display name (32 most recent, L2073). Lineages are oldest first: a
/// fork appends its parent, and loaders keep the last [`MAX_LINEAGE`] entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineageEntry { pub hash: ContentHash, pub author: String }
pub const MAX_LINEAGE: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KitPart { pub part: PartId, pub offset: CellPos, pub rot: PartRot }

/// A named socket: where the kit attaches or what it expects (deck face, magazine space below, mast top).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Socket { pub name: String, pub offset: CellPos, pub face: crate::cell::Face }

/// One row of the settings sheet (an L1 slider scoped to the kit).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KitSetting { pub key: String, pub label: TextKey, pub min: f64, pub max: f64, pub default: f64 }

/// What kind of kit this is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KitKind {
    /// Parts plus wiring (twin 76 mm mount + magazine + hoist; radar mast + director).
    #[default] Assembly,
    /// A forked officer: no parts, only graph and settings. Others see it as an L0/L1 preset (L2087).
    Officer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KitDef {
    pub format: u32,
    pub id: KitId,
    pub version: u32,
    pub hash: ContentHash,
    /// Stock kits use `name_key`; player kits use `name` (player text is never a translation key).
    pub name: String,
    pub name_key: Option<TextKey>,
    pub description_key: Option<TextKey>,
    pub author: Option<String>,
    /// SPDX id, default `CC-BY-4.0` (L2073).
    pub licence: String,
    pub lineage: Vec<LineageEntry>,
    pub kind: KitKind,
    pub parts: Vec<KitPart>,
    pub sockets: Vec<Socket>,
    pub settings: Vec<KitSetting>,
    /// The kit's own graph fragment; node part references are indices into `parts`.
    pub graph: ControlGraph,
    /// Officer kits: the preset and sliders it presents.
    pub officer: Option<OfficerSettings>,
    /// Constraint tags this kit can satisfy, for the Quartermaster's curated lists (L801-807).
    pub tags: Vec<PartTag>,
    /// Tutorial and tray ordering hint.
    pub tray_order: u16,
}

/// Parse one `.kit.ron`, applying migrations up to [`KIT_FORMAT`]. Unit: `content-kit`.
///
/// The text is untrusted: the size and nesting limits are checked first, unknown fields are rejected, rotations
/// are reduced, the lineage is cut to its [`MAX_LINEAGE`] most recent entries, and `hash` is recomputed (a stale
/// stored hash is not an error, as for blueprints, L2079).
pub fn parse_kit(ron_text: &str, file: &str) -> ContentResult<KitDef> { parse_kit_checked(ron_text, file).map(|(kit, _)| kit) }

/// [`parse_kit`] plus the hash the text carried, when that hash matches the text (see
/// [`text::verify_text_hash`]). Share-string decoding refuses a kit whose carried hash is missing or wrong.
pub(crate) fn parse_kit_checked(ron_text: &str, file: &str) -> ContentResult<(KitDef, Option<ContentHash>)> {
    text::check_untrusted(ron_text)?;
    let from = crate::migrate::peek_format(ron_text).map_err(|e| text::rename_file(e, file))?;
    let carried = text::verify_text_hash(ron_text);
    let migrated = crate::migrate::migrate_text(ron_text, crate::migrate::KIT_MIGRATIONS, from, KIT_FORMAT)?;
    let mut kit: KitDef = crate::data::parse_ron(&migrated, file)?;
    canonicalize_kit(&mut kit);
    kit.hash = kit_hash(&kit);
    Ok((kit, carried))
}

/// Normalise a kit the way every loader does: reduced rotations and at most [`MAX_LINEAGE`] ancestors.
pub fn canonicalize_kit(kit: &mut KitDef) {
    for p in &mut kit.parts {
        p.rot = normalize_rot(p.rot);
    }
    if kit.lineage.len() > MAX_LINEAGE {
        kit.lineage.drain(..kit.lineage.len() - MAX_LINEAGE);
    }
}

/// Canonical RON text of a kit (same rules as blueprints, L2079). Unit: `content-kit`.
/// The `hash` field is written empty; this is the text [`kit_hash`] hashes.
pub fn kit_to_canonical_ron(kit: &KitDef) -> String {
    let mut k = kit.clone();
    k.hash = ContentHash::EMPTY;
    text::to_canonical(&k)
}

/// BLAKE3 of [`kit_to_canonical_ron`]: the kit's content address.
pub fn kit_hash(kit: &KitDef) -> ContentHash { crate::blueprint::io::blake3_hash(kit_to_canonical_ron(kit).as_bytes()) }

/// File text to write for a kit: canonical RON with the hash field filled in.
pub fn kit_to_file_text(kit: &KitDef) -> String {
    let mut k = kit.clone();
    k.hash = kit_hash(kit);
    text::to_canonical(&k)
}

/// Footprint cells of a kit at a placement, for ghost preview and collision checks. Unit: `content-kit`.
/// `pos` is the minimum corner of the placed footprint; `mirrored` reflects the kit across its own z centre
/// plane before `rot` turns it (see [`kit_part_placements`]). Sorted ascending, no duplicates; parts the
/// catalogue does not know are skipped.
pub fn kit_footprint(kit: &KitDef, catalogue: &crate::catalogue::Catalogue, pos: CellPos, rot: PartRot, mirrored: bool) -> Vec<CellPos> { rot::kit_footprint(kit, catalogue, pos, rot, mirrored) }
/// Rotate a part-local cell offset by a part rotation. Shared by kits, placements and the simulation.
/// Unit: `content-kit`.
///
/// `offset` is a cell of the unrotated `size` box; the result is the same cell in the rotated box, measured from
/// the rotated box's minimum corner, so `pos + rotate_offset(..)` over the whole box is the placed footprint.
/// Offsets outside the box map by the same affine rule; results saturate at the `i16` range.
pub fn rotate_offset(offset: CellPos, size: [u8; 3], rot: PartRot) -> CellPos { rot::rotate_offset(offset, size, rot) }
/// Captured settings values for a placed kit, keyed like [`KitSetting::key`].
pub type KitSettings = BTreeMap<String, f64>;
