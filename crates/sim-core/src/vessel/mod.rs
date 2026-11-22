//! Unit `sim-core-vessel`. One vessel's physical state and how it is built from a blueprint.
//! DESIGN.md L1114 (hot data), L537 (end states), L701-705 (crew capacity), L218 (cached build for Splash),
//! L1542-1549 (fidelity levels).
//!
//! # Implementation decisions (binding for callers)
//! - [`build_prefab`] runs: `build_grid`, `parts::build_parts`, `VesselGrid::build` (part flags from the
//!   catalogue), `CompartmentSet::derive` and `link_parts`, `networks::route`, `mass::dry_mass`,
//!   `physics::greedy_cuboids`, `hydro::build_blocks`, the waterline, `CompartmentSet::set_waterline` (flood
//!   valves), the sealed-air bits and `structure::pick_root_cell`. `hash` is the header hash, or the computed
//!   content hash when the header carries none.
//! - `waterline_y` is where the hull floats at its loaded mass (fuel and rounds as placed, no flood water),
//!   read at the centre of mass station from the calm-water equilibrium; a hull that cannot float uses the
//!   blueprint's design waterline row. It is clamped into the grid.
//! - `berths` sums the catalogue's crew `Berths` parts (passenger berths are not crew). `dims` is the extent
//!   of the solid cells. `work_counts` are cuboids, buoyancy columns (one water sample each per tick), routed
//!   conduits and compartments.
//! - Status (L537): `Lost` and `Wreck` never change here. A hull is lost by capsize when its heel stays beyond
//!   `capsize_heel_deg` for `capsize_hold_s`, counted in calls (the world calls once per tick at 60 Hz), and
//!   sunk when no live cell of its top deck has its centre above the water surface (checked every call, at any
//!   heel; a hull lying on its side keeps its high deck edge dry while the capsize timer runs). When the
//!   top-deck layer has no live cell left, the highest layer below it that has one stands in. Submersibles
//!   are sunk instead when their mass exceeds the water their whole displacing volume can hold. A hull is disabled when it has
//!   struck, or when it has no working propulsor or no working engine (a design without propulsion is
//!   disabled from the start).
//! - `top_height_m` is the highest corner of any live cell above y = 0 (mean sea level), found from the
//!   column extents the grid keeps, so it costs one step per column, not per cell.
//! - Projected areas: bin `i` is the observer bearing `[10 i, 10 i + 10)` degrees clockwise from the bow
//!   (bin 9 the starboard beam), evaluated at the bin centre. Each is a silhouette (union of cells, never a
//!   sum), drawn on a 0.125 m raster per cell layer in the vessel frame. The water plane is y = 0 at the
//!   current pose (vessel) or the design waterline (prefab); a cell straddling it counts above and below in
//!   proportion by the flooding unit's cell fill law. The radar area weights each raster pixel by the
//!   material factor of the cell nearest the observer (`MaterialDef::rcs_factor` for structure,
//!   `PartDef::rcs_factor` for part cells, 1.0 when unknown).
mod areas;
mod build;
#[cfg(test)]
pub(crate) mod testkit;
#[cfg(test)]
mod tests;

use crate::compartments::CompartmentSet;
use crate::crew::CrewState;
use crate::entity::EntityId;
use crate::grid::VesselGrid;
use crate::hydro::BuoyancyBlocks;
use crate::mass::MassProperties;
use crate::math::{Aabb, Kinematics, Pose};
use crate::networks::Networks;
use crate::parts::{PartRuntime, VesselControls};
use crate::physics::{BodyHandle, CuboidLocal};
use content::blueprint::Blueprint;
use content::catalogue::Catalogue;
use content::ids::{CellIndex, ClassId, ContentHash, PlayerSlot, SideId};
use content::save::HullDamage;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Simulation fidelity of a hull, set by sim-systems each tick from tracking state and the overload ladder
/// (L1542-1549). sim-core only obeys it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fidelity {
    /// Rapier body, buoyancy blocks, every tick.
    #[default] Full,
    /// Full physics at 30 Hz (hulls beyond 10 km of every player-side hull).
    Full30,
    /// Kinematic motion along its ordered course with analytic buoyancy; damage still applies.
    Kinematic,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LossCause { #[default] Sunk, Capsized, CookOff, Scuttled }

/// Vessel end states (L537).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VesselStatus {
    #[default] Active,
    /// No propulsion part works, or colours struck (set by sim-systems via [`crate::world::World::set_struck`]).
    Disabled,
    Lost(LossCause),
    /// A split-off piece or a lost hull still afloat: buoyancy and compartments only.
    Wreck,
}

/// Everything derived from a blueprint that does not depend on where or when it spawns. Built once per design
/// save on a background worker and cached, so Splash only swaps worlds (L218). Cloneable and `Send`.
#[derive(Clone, Debug, PartialEq)]
pub struct VesselPrefab {
    pub hash: ContentHash,
    pub name: String,
    pub grid: VesselGrid,
    pub parts: Vec<PartRuntime>,
    pub compartments: CompartmentSet,
    pub networks: Networks,
    pub dry_mass: MassProperties,
    /// Greedy-merged cuboid compound for the collider (L1100).
    pub cuboids: Vec<CuboidLocal>,
    pub blocks: BuoyancyBlocks,
    /// Keel cell nearest the design centre of mass; fixed at spawn (L682).
    pub root_cell: CellIndex,
    pub berths: u16,
    /// Design waterline height in vessel-local metres.
    pub waterline_y: f64,
    /// Length, height, beam in metres.
    pub dims: [f64; 3],
    /// Deterministic work counts for Complexity points (L973): cuboids, buoyancy samples, conduit segments,
    /// compartments.
    pub work_counts: [u32; 4],
}

/// Build a prefab: expand the grid, build parts, derive compartments, route networks, compute dry mass, merge
/// cuboids, build buoyancy blocks, pick the root cell. Deterministic; same blueprint gives an equal prefab.
/// Unit: `sim-core-vessel`.
pub fn build_prefab(bp: &Blueprint, catalogue: &Catalogue) -> VesselPrefab { build::build(bp, catalogue) }

/// How to place a prefab in a world.
#[derive(Clone, Debug, PartialEq)]
pub struct VesselSpawn {
    pub prefab: Arc<VesselPrefab>,
    pub side: SideId,
    /// Player who commands it; `None` for AI hulls.
    pub player: Option<PlayerSlot>,
    pub class: Option<ClassId>,
    /// Pose of the grid origin; `y` is ignored and the hull is set at its static equilibrium draft.
    pub pose: Pose,
    pub speed_mps: f64,
    /// Persistent damage and loads carried in from the theatre.
    pub damage: Option<HullDamage>,
    pub fuel_fraction: f64,
    pub ammo_fraction: f64,
    pub fidelity: Fidelity,
    /// Act 3 AI-only submarine (`EnemyClassDef::submersible`).
    pub submersible: bool,
}

#[derive(Clone, Debug)]
pub struct Vessel {
    pub id: EntityId,
    pub side: SideId,
    pub player: Option<PlayerSlot>,
    pub class: Option<ClassId>,
    pub blueprint: ContentHash,
    pub name: String,
    pub status: VesselStatus,
    pub fidelity: Fidelity,
    pub grid: VesselGrid,
    pub parts: Vec<PartRuntime>,
    pub compartments: CompartmentSet,
    pub networks: Networks,
    pub crew: CrewState,
    pub controls: VesselControls,
    pub dry_mass: MassProperties,
    /// Mass with fluids, refreshed each tick the hull runs full physics.
    pub mass: MassProperties,
    pub body: BodyHandle,
    pub cuboids: Vec<CuboidLocal>,
    pub blocks: BuoyancyBlocks,
    /// Pose of the grid origin and body velocities, copied out of the physics world after each step.
    pub kin: Kinematics,
    /// World bounds for the projectile broadphase, refreshed each tick.
    pub bounds: Aabb,
    pub root_cell: CellIndex,
    pub dims: [f64; 3],
    pub waterline_y: f64,
    /// Tick the vessel last fired a gun or launched (visual night rule, L436) and whether it is burning.
    pub last_fired_tick: u64,
    /// Ticks spent heeled beyond 90 degrees (capsize needs 10 s, L537).
    pub capsize_ticks: u32,
    /// Colours struck (Combine surrender rule; owner decision pending, L537).
    pub struck: bool,
    /// Cells destroyed and not yet processed by splitting and compartment recompute, ascending (L682).
    pub pending_destroyed: Vec<CellIndex>,
    /// Entity this piece split from, for wrecks.
    pub parent: Option<EntityId>,
    pub spawn_tick: u64,
    /// Act 3 AI-only submarine: holds `controls.depth_m`; the sunk test (no top-deck cell above water) is
    /// replaced by "flooded beyond reserve buoyancy" for such hulls.
    pub submersible: bool,
    /// Tonnage at spawn in tonnes, for salvage and "share of enemy tonnage disabled" (L227, L686).
    pub tonnage_t: f64,
}

impl Vessel {
    /// Heel (roll) and trim (pitch) in radians from the current pose. Unit: `sim-core-vessel`.
    pub fn heel_trim(&self) -> (f64, f64) { crate::hydro::heel_trim(self.kin.pose.rot) }
    /// Speed over ground along the bow in m/s. Unit: `sim-core-vessel`.
    pub fn forward_speed(&self) -> f64 {
        let rot = self.kin.pose.rot;
        if !(rot.is_finite() && self.kin.lin_vel.is_finite()) { return 0.0; }
        let v = self.kin.lin_vel.dot((rot * glam::DVec3::X).normalize_or(glam::DVec3::X));
        if v.is_finite() { v } else { 0.0 }
    }
    /// World position of a vessel-local point in metres.
    pub fn to_world(&self, local: glam::DVec3) -> glam::DVec3 { self.kin.pose.to_world(local) }
    /// Highest live cell above the water plane in metres (radar horizon target height, L425).
    /// Unit: `sim-core-vessel`.
    pub fn top_height_m(&self) -> f64 { areas::top_height(&self.grid, &self.kin.pose) }
    /// Total propulsion shaft power now in kW (acoustic noise and infrared signature inputs, L453, L524).
    /// Unit: `sim-core-vessel`.
    pub fn propulsion_kw(&self) -> f64 {
        let mut kw = 0.0;
        for p in &self.parts {
            if let crate::parts::PartState::Propulsor { shaft_kw, .. } = p.state { if shaft_kw.is_finite() { kw += shaft_kw.abs(); } }
        }
        kw
    }
    pub fn is_burning(&self) -> bool { self.compartments.compartments.iter().any(|c| c.fire > 0.0) }
    /// Fraction of original solid cells lost.
    pub fn cells_lost_fraction(&self) -> f64 { if self.grid.original_count == 0 { 0.0 } else { 1.0 - self.grid.solid_count as f64 / self.grid.original_count as f64 } }
    /// Re-evaluate Active / Disabled / Lost from parts, pose and flooding (L537). Returns the new status when it
    /// changed. Unit: `sim-core-vessel`.
    pub fn evaluate_status(&mut self, water_height_at: &dyn Fn(f64, f64) -> f64, tuning: &content::tuning::StabilityTuning) -> Option<VesselStatus> {
        if matches!(self.status, VesselStatus::Lost(_) | VesselStatus::Wreck) { return None; }
        let limit = if tuning.capsize_heel_deg.is_finite() && tuning.capsize_heel_deg > 0.0 { tuning.capsize_heel_deg.min(180.0) } else { 90.0 }.to_radians();
        let (heel, _) = self.heel_trim();
        self.capsize_ticks = if heel.abs() > limit { self.capsize_ticks.saturating_add(1) } else { 0 };
        let hold_s = if tuning.capsize_hold_s.is_finite() && tuning.capsize_hold_s >= 0.0 { tuning.capsize_hold_s.min(1.0e6) } else { 10.0 };
        let hold = (libm::round(hold_s * crate::time::TICK_HZ as f64) as u32).max(1);
        let next = if self.capsize_ticks >= hold {
            VesselStatus::Lost(LossCause::Capsized)
        } else if self.is_sunk(water_height_at) {
            VesselStatus::Lost(LossCause::Sunk)
        } else if self.struck || !self.has_propulsion() {
            VesselStatus::Disabled
        } else {
            VesselStatus::Active
        };
        if next == self.status { return None; }
        self.status = next;
        Some(next)
    }

    /// At least one working propulsor and one working engine.
    fn has_propulsion(&self) -> bool {
        use crate::parts::PartState;
        let prop = self.parts.iter().any(|p| p.works() && matches!(p.state, PartState::Propulsor { .. }));
        let engine = self.parts.iter().any(|p| p.works() && matches!(p.state, PartState::Engine { .. }));
        prop && engine
    }

    /// The sunk test (L537); submersibles use the reserve-buoyancy rule.
    pub(crate) fn is_sunk(&self, water_height_at: &dyn Fn(f64, f64) -> f64) -> bool {
        if self.submersible {
            let cap = self.blocks.total_volume_m3() * crate::hydro::SEA_WATER_DENSITY;
            return self.mass.mass_kg.is_finite() && cap.is_finite() && self.mass.mass_kg > cap;
        }
        areas::top_deck_submerged(&self.grid, self.compartments.top_deck_y, &self.kin.pose, water_height_at)
    }
}

/// Projected areas for the Signature stage (L404, L427, L436, L454): 36 bins of 10 degrees of aspect, each
/// the projected area perpendicular to the line of sight in m^2, above water with material factors (radar),
/// above water plain (visual) and submerged (active sonar). Computed from live cells at the current draft.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedAreas { pub rcs_m2: [f32; 36], pub above_m2: [f32; 36], pub submerged_m2: [f32; 36], pub top_height_m: f32 }

/// Compute projected areas. Cost is bounded by the cell count; sim-systems calls it at spawn and after damage
/// changes the silhouette, never per tick. Unit: `sim-core-vessel`.
pub fn projected_areas(vessel: &Vessel, catalogue: &Catalogue) -> ProjectedAreas {
    let rot = if vessel.kin.pose.rot.is_finite() { vessel.kin.pose.rot } else { glam::DQuat::IDENTITY };
    let up = (rot.inverse() * glam::DVec3::Y).normalize_or(glam::DVec3::Y);
    let base = if vessel.kin.pose.pos.y.is_finite() { vessel.kin.pose.pos.y } else { 0.0 };
    // A cell is under water when base + up . p < 0.
    areas::compute(&vessel.grid, &vessel.parts, catalogue, up, -base, vessel.top_height_m())
}
/// Same from a prefab at its design draft, for the editor's signature view. Unit: `sim-core-vessel`.
pub fn projected_areas_prefab(prefab: &VesselPrefab, catalogue: &Catalogue) -> ProjectedAreas {
    let wl = if prefab.waterline_y.is_finite() { prefab.waterline_y } else { 0.0 };
    let top = areas::top_height(&prefab.grid, &Pose { pos: glam::DVec3::new(0.0, -wl, 0.0), rot: glam::DQuat::IDENTITY });
    areas::compute(&prefab.grid, &prefab.parts, catalogue, glam::DVec3::Y, wl, top)
}
