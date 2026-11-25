//! Unit `content-hull`. Lofted hull shell parameters and the voxelizer that turns them into cells.
//! DESIGN.md L122-134 (hull and scale), L1923 (voxelizer rule).
//!
//! The editor generates hulls: a hull is a lofted shell with 4 to 16 cross-section stations, mirrored by
//! default, or freeform cells after a one-way bake. The blueprint always stores the resulting cells as well,
//! so the simulation never runs the voxelizer.
//!
//! # Frames
//! The loft is defined in a design frame (x forward from the aft extreme, y up from the design waterline, z to
//! starboard from the centreline). [`shell_layout`] gives the whole-cell offsets to the vessel-local frame of the
//! voxelized grid; [`shell_sdf`] takes vessel-local metres in that grid's frame. The centreline is always a
//! lattice plane in the middle of the grid, and every boundary slab of a voxelized shell holds a solid cell, so
//! the grid is the blueprint's bounding box.
//!
//! # Symmetry
//! A station carries one full beam, so the lofted shell is symmetric by construction and voxelizes to an exactly
//! mirrored grid. [`HullShell::mirrored`] is the editor's symmetry setting for edits made on top of it; it does
//! not change the generated cells.
//!
//! # Paint
//! The voxelizer paints with the default paint table's indices ([`PAINT_SIDES`] and friends): the 8 USN swatches
//! in the order of the Graphics and rendering table (pale, light, haze, ocean, sea, navy, deck, black), then the
//! red-brown antifouling colour at index 8. `content::blueprint::io::default_paint_table` must keep that order.
mod defaults;
mod loft;
#[cfg(test)]
mod tests;
mod voxel;

pub use loft::{ShellField, ShellLayout};

use crate::cell::CellGrid;
use crate::enums::Axis;
use serde::{Deserialize, Serialize};

pub const MIN_STATIONS: usize = 4;
pub const MAX_STATIONS: usize = 16;
/// Default vertical spacing of decks in metres (L132).
pub const DEFAULT_DECK_SPACING_M: f64 = 2.5;

/// Inclusive range of one shell parameter; the editor's numeric fields and drag handles clamp to these, and
/// [`validate_shell`] rejects anything outside them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamRange {
    pub min: f64,
    pub max: f64,
}
impl ParamRange {
    pub const fn new(min: f64, max: f64) -> Self { Self { min, max } }
    /// True for a finite value inside the range.
    pub fn contains(self, v: f64) -> bool { v.is_finite() && v >= self.min && v <= self.max }
    /// Clamp into the range; NaN becomes `min`.
    pub fn clamp(self, v: f64) -> f64 { if v.is_nan() { self.min } else { v.clamp(self.min, self.max) } }
}

/// Station position along the hull.
pub const STATION_T: ParamRange = ParamRange::new(0.0, 1.0);
/// Station beam in metres. At least one station must reach [`MIN_MAX_BEAM_M`], stations between the ends
/// [`MIN_INTERIOR_BEAM_M`], and none may exceed
/// [`MAX_BEAM_FRACTION`] of the length.
pub const STATION_BEAM_M: ParamRange = ParamRange::new(0.0, 40.0);
pub const MIN_MAX_BEAM_M: f64 = 1.0;
/// Least beam of a station between the first and the last (two cells), so the hull never pinches in two.
pub const MIN_INTERIOR_BEAM_M: f64 = 1.0;
pub const MAX_BEAM_FRACTION: f64 = 0.6;
/// Station draft below the design waterline in metres.
pub const DRAFT_M: ParamRange = ParamRange::new(0.0, 15.0);
/// Bottom deadrise in degrees. With [`FLARE_DEG`] it keeps the chine angle convex (deadrise + flare < 90).
pub const DEADRISE_DEG: ParamRange = ParamRange::new(0.0, 40.0);
/// Topside flare in degrees; negative is tumblehome.
pub const FLARE_DEG: ParamRange = ParamRange::new(-25.0, 35.0);
/// Deck-edge height above the design waterline in metres (at least one cell of freeboard).
pub const SHEER_M: ParamRange = ParamRange::new(0.5, 25.0);
pub const BOW_RAKE_DEG: ParamRange = ParamRange::new(-30.0, 60.0);
pub const TRANSOM_RAKE_DEG: ParamRange = ParamRange::new(-30.0, 45.0);
pub const TRANSOM_WIDTH: ParamRange = ParamRange::new(0.0, 1.0);
/// Plating thickness in cells.
pub const SHELL_CELLS: ParamRange = ParamRange::new(1.0, 4.0);
/// The stem and transom planes must leave at least this fraction of the length between them at every height.
pub const MIN_RUN_FRACTION: f64 = 0.25;

/// Default paint table indices the voxelizer uses (see the module doc): sides `hull.haze` (5-H Haze Gray),
/// decks `hull.deck` (20-B Deck Blue, four swatch steps darker), the boot-topping band `hull.black`, and the
/// red-brown antifouling colour below it.
pub const PAINT_SIDES: u8 = 2;
pub const PAINT_DECK: u8 = 6;
pub const PAINT_BOOT_TOPPING: u8 = 7;
pub const PAINT_ANTIFOULING: u8 = 8;

/// One cross-section station of the lofted shell. Every handle also has a numeric field (L124).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Station {
    /// Position along the hull, 0.0 = transom, 1.0 = stem.
    pub t: f64,
    /// Full beam at the deck edge in metres.
    pub beam_m: f64,
    /// Depth of the keel below the design waterline in metres.
    pub draft_m: f64,
    /// Bottom deadrise angle in degrees (0 = flat bottom).
    pub deadrise_deg: f64,
    /// Topside flare in degrees outward from vertical (negative = tumblehome).
    pub flare_deg: f64,
    /// Height of the deck edge above the design waterline in metres (the sheer line samples this).
    pub sheer_m: f64,
}

/// Lofted shell parameters. The seven drag handles are beam, draft, deadrise, flare, sheer, bow rake and transom.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HullShell {
    /// Length overall in metres. Player modes allow 10 to 90 m, enemy-class mode up to 150 m (L126).
    pub length_m: f64,
    /// 4 to 16 stations ascending by `t`.
    pub stations: Vec<Station>,
    /// Stem rake in degrees forward from vertical.
    pub bow_rake_deg: f64,
    /// Transom rake in degrees aft from vertical.
    pub transom_rake_deg: f64,
    /// Transom width as a fraction of the aftmost station's beam, 0.0 (double-ended) to 1.0.
    pub transom_width: f64,
    /// Mirrored about the centreline. Default true.
    pub mirrored: bool,
    /// Shell plating thickness in cells (1 = 0.5 m). Interior is air until decks and bulkheads are cut.
    pub shell_cells: u8,
}

/// A deck or bulkhead: a plane dragged through the hull that cuts it into compartments (L132).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternalPlane {
    /// `Y` = deck, `X` = transverse bulkhead, `Z` = longitudinal bulkhead.
    pub axis: Axis,
    /// Cell coordinate of the plane along `axis`.
    pub offset: i16,
    /// False for a non-watertight flat (a routing surface that does not bound compartments).
    pub watertight: bool,
}

/// How the hull's cells came to be (L2074).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HullSource {
    /// Cells regenerate from these parameters whenever a handle moves.
    Lofted(HullShell),
    /// Baked to freeform cells; parameters are gone for good (L134).
    Freeform,
}

/// Hull section of a blueprint: shell source plus decks and bulkheads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hull {
    pub source: HullSource,
    pub planes: Vec<InternalPlane>,
    /// Y cell coordinate of the design waterline, set by the editor from the Float gauge. The boot-topping band
    /// and antifouling paint follow it (L130).
    pub design_waterline_y: i16,
}

/// Result of voxelizing a shell.
#[derive(Clone, Debug, PartialEq)]
pub struct VoxelizedHull {
    pub grid: CellGrid,
    /// Y cell coordinate of the design waterline in `grid`.
    pub design_waterline_y: i16,
}

/// Check station count, ordering and parameter ranges, and the length limit for the mode.
/// `max_length_m` is 90 for player modes and 150 for enemy-class mode. Unit: `content-hull`.
/// A NaN `max_length_m` means the player limit; other values clamp to 10..=150.
pub fn validate_shell(shell: &HullShell, max_length_m: f64) -> Result<(), ShellError> { loft::validate(shell, max_length_m) }

/// Signed distance to the lofted shell surface at a vessel-local point in metres (negative inside).
/// Deterministic, libm only. The voxelizer and the editor's handle gizmos both use it. Unit: `content-hull`.
/// The frame is the grid [`voxelize`] produces for this shell ([`shell_layout`]). An invalid shell (see
/// [`validate_shell`] at 150 m) has no surface and returns `f64::MAX` everywhere. Each call prepares the shell
/// (about a millisecond at 90 m); for many queries build a [`ShellField`] once.
pub fn shell_sdf(shell: &HullShell, p: [f64; 3]) -> f64 {
    match ShellField::new(shell) {
        Some(field) => field.sdf(p),
        None => f64::MAX,
    }
}

/// Grid size and frame offsets of the shell's voxelization; all zero for an invalid shell.
pub fn shell_layout(shell: &HullShell) -> ShellLayout { ShellField::new(shell).map(|f| f.layout()).unwrap_or_default() }

/// Voxelize a shell to 0.5 m cells: sample the SDF at each boundary cell's 8 corners and map the pattern with
/// [`crate::cell::shape_from_corners`]; interior beyond `shell_cells` is left empty; then insert the planes as
/// one-cell-thick steel. Paint: default sides swatch, decks at least 3 swatch steps darker, a 1-cell boot-topping
/// band at the design waterline and antifouling below it (L130). Same input always gives the same grid.
/// Must finish a 90 m hull well inside the 100 ms gauge budget (L177). Unit: `content-hull`.
///
/// The band follows `hull.design_waterline_y`, clamped into the grid, and the clamped row is returned. A
/// `Freeform` hull has no parameters to regenerate from, and an invalid shell (see [`validate_shell`] at 150 m)
/// has no surface: both give an empty grid, and the caller keeps the blueprint's stored cells.
pub fn voxelize(hull: &Hull) -> VoxelizedHull {
    match &hull.source {
        HullSource::Lofted(shell) => voxel::voxelize_shell(shell, hull),
        HullSource::Freeform => VoxelizedHull { grid: CellGrid::default(), design_waterline_y: hull.design_waterline_y.max(0) },
    }
}

/// A default shell of the given length, afloat and plausible, for "new design". Unit: `content-hull`.
/// The length clamps to 10..=150 m (NaN gives 10 m).
pub fn default_shell(length_m: f64) -> HullShell { defaults::default_shell(length_m) }

/// A complete hull for "new design": [`default_shell`], default decks and bulkheads ([`default_planes`]) and the
/// design waterline on the shell's draft datum.
pub fn default_hull(length_m: f64) -> Hull { defaults::default_hull(length_m) }

/// Default planes for a shell: watertight decks [`DEFAULT_DECK_SPACING_M`] apart below the lowest deck edge and
/// evenly spaced watertight transverse bulkheads, in the shell's grid coordinates. Empty for an invalid shell.
pub fn default_planes(shell: &HullShell) -> Vec<InternalPlane> { defaults::default_planes(shell) }

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ShellError {
    #[error("station count {0} outside 4..=16")]
    StationCount(usize),
    #[error("stations not ascending in t")]
    StationOrder,
    #[error("length {length_m} m outside {min_m}..={max_m} m")]
    Length { length_m: f64, min_m: f64, max_m: f64 },
    #[error("parameter `{0}` out of range")]
    Param(&'static str),
}
