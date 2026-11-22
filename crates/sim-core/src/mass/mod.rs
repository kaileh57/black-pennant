//! Unit `sim-core-mass`. Mass properties computed by the game for every collider type; Rapier's automatic mass
//! and inertia are never trusted (DESIGN.md L1100). Includes fluids: fuel in tanks and flood water at the
//! centroid of each compartment's filled volume, so free-surface effect emerges (L634).
//!
//! Model: every live cell is a solid 0.5 m cube of uniform density at its centre (its own inertia m s^2 / 6
//! about each axis). A structural cell weighs its material's `cell_mass_kg` plus the armour mass of its tier.
//! A part's `mass_kg` is spread evenly over its footprint, and only the live footprint cells carry their share,
//! so a shot-away part loses mass with its cells. Part-body cells take armour mass from the steel row (the
//! catalogue has no part-body material row). Fluids and stores are point masses added by [`with_fluids`]: fuel
//! at [`FUEL_DENSITY_KG_PER_L`], ballast water at its current load, magazine ammunition, ready rounds on
//! mounts, ready munitions in launchers and mines on rails at the part centre, and flood water at each
//! compartment's water centroid with the catalogue's water density.
//!
//! Ballast is a fluid here, not dry mass: `parts::step_parts` pumps it during a sortie and `with_fluids` runs
//! every physics tick, while dry mass is rebuilt only when cells change. Keeping it in dry mass would leave a
//! trim or counter-list order without effect until the next hit.
use crate::compartments::CompartmentSet;
use crate::grid::VesselGrid;
use crate::math::{DMat3, DVec3};
use crate::parts::{PartRuntime, PartState};
use content::catalogue::Catalogue;
use content::cell::{ArmourTier, Material, CELL_M};
use content::part::{PartDef, PartFunction};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// Marine diesel, kg per litre.
pub const FUEL_DENSITY_KG_PER_L: f64 = 0.85;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MassProperties {
    pub mass_kg: f64,
    /// Centre of mass in vessel-local metres.
    pub com: DVec3,
    /// Inertia tensor about the centre of mass in the local frame, kg m^2.
    pub inertia: DMat3,
}
impl Default for MassProperties { fn default() -> Self { Self { mass_kg: 1.0, com: DVec3::ZERO, inertia: DMat3::IDENTITY } } }

/// Running sum of point masses about the vessel origin: mass, first moment and second moment
/// (sum of m (|r|^2 I - r r^T)) plus the cubes' own inertia.
#[derive(Clone, Copy, Debug)]
struct Accum { m: f64, first: DVec3, second: DMat3 }
impl Accum {
    fn new() -> Self { Self { m: 0.0, first: DVec3::ZERO, second: DMat3::ZERO } }
    fn point(&mut self, m: f64, r: DVec3) {
        if !(m > 0.0) || !m.is_finite() || !r.is_finite() { return; }
        self.m += m;
        self.first += r * m;
        self.second += shift(m, r);
    }
    fn cube(&mut self, m: f64, r: DVec3, side: f64) {
        if !(m > 0.0) || !m.is_finite() || !r.is_finite() { return; }
        self.point(m, r);
        self.second += DMat3::from_diagonal(DVec3::splat(m * side * side / 6.0));
    }
    /// Add a body's properties (inertia about its own centre of mass).
    fn body(&mut self, b: &MassProperties) {
        if !(b.mass_kg > 0.0) || !b.mass_kg.is_finite() || !b.com.is_finite() || !mat_finite(&b.inertia) { return; }
        self.m += b.mass_kg;
        self.first += b.com * b.mass_kg;
        self.second += b.inertia + shift(b.mass_kg, b.com);
    }
    fn finish(&self, fallback_com: DVec3) -> MassProperties {
        if !(self.m > 0.0) || !self.m.is_finite() { return MassProperties { mass_kg: 1.0, com: fallback_com, inertia: DMat3::IDENTITY }; }
        let com = self.first / self.m;
        let mut inertia = self.second - shift(self.m, com);
        // Symmetrise away rounding, and keep the tensor usable for a degenerate body (a single point).
        inertia = (inertia + inertia.transpose()) * 0.5;
        let floor = self.m * CELL_M * CELL_M / 6.0;
        for k in 0..3 { if !(inertia.col(k)[k] >= floor) { let mut c = inertia.col(k); c[k] = floor; *inertia.col_mut(k) = c; } }
        MassProperties { mass_kg: self.m, com, inertia }
    }
}

/// Parallel-axis term m (|r|^2 I - r r^T).
fn shift(m: f64, r: DVec3) -> DMat3 {
    let rr = r.dot(r);
    DMat3::from_cols(
        DVec3::new(rr - r.x * r.x, -r.y * r.x, -r.z * r.x),
        DVec3::new(-r.x * r.y, rr - r.y * r.y, -r.z * r.y),
        DVec3::new(-r.x * r.z, -r.y * r.z, rr - r.z * r.z),
    ) * m
}

fn mat_finite(m: &DMat3) -> bool { m.x_axis.is_finite() && m.y_axis.is_finite() && m.z_axis.is_finite() }

fn part_def<'c>(catalogue: &'c Catalogue, p: &PartRuntime) -> Option<&'c PartDef> { catalogue.parts.get_index(p.def.index()).map(|(_, d)| d) }

fn armour_mass(catalogue: &Catalogue, material: Material, armour: ArmourTier) -> f64 {
    if armour == ArmourTier::None { return 0.0; }
    let row = catalogue.materials.iter().find(|m| m.material == material).or_else(|| catalogue.materials.iter().find(|m| m.material == Material::Steel));
    row.map_or(0.0, |r| r.armour_mass_kg.get(armour as usize - 1).copied().unwrap_or(0.0)).max(0.0)
}

/// Dry structure and parts only (cells, armour, part masses). Ballast water, like fuel and ammunition, is a
/// load and comes from [`with_fluids`]. Sums run serially in ascending cell index, then ascending part slot.
/// Unit: `sim-core-mass`.
///
/// An empty or inert grid gives mass 1 kg at the grid centre with unit inertia, so a body built from it stays
/// valid.
pub fn dry_mass(grid: &VesselGrid, parts: &[PartRuntime], catalogue: &Catalogue) -> MassProperties {
    let size = grid.size();
    let centre = DVec3::new(size[0] as f64, size[1] as f64, size[2] as f64) * (0.5 * CELL_M);
    let n = grid.design.cells.len();
    let mut acc = Accum::new();
    if n != size.iter().map(|&s| s as usize).product::<usize>() { return acc.finish(centre); }
    // Per-cell share of each part's mass, by owning part slot.
    let mut share: std::collections::BTreeMap<u16, f64> = std::collections::BTreeMap::new();
    for p in parts {
        let Some(def) = part_def(catalogue, p) else { continue };
        let cells = p.cells.len();
        if cells > 0 && def.mass_kg.is_finite() && def.mass_kg > 0.0 { share.insert(p.slot.0, def.mass_kg / cells as f64); }
    }
    let side = CELL_M;
    for i in grid.solid_cells() {
        let idx = i.index();
        if idx >= n { continue; }
        let pos = grid.pos(i);
        let cell = grid.design.cells[idx];
        let r = DVec3::from(pos.centre_m());
        let mut m = armour_mass(catalogue, cell.material, cell.armour);
        match cell.material {
            Material::Empty => continue,
            Material::PartBody => {
                if let Some(s) = grid.cell_type(pos).part { m += share.get(&s.0).copied().unwrap_or(0.0); }
            }
            mat => { m += catalogue.materials.iter().find(|d| d.material == mat).map_or(0.0, |d| d.cell_mass_kg.max(0.0)); }
        }
        acc.cube(m, r, side);
    }
    // Parts whose footprint cells are not part-body cells of the grid (or unowned there) still carry their
    // mass at their live footprint cells.
    let mut order: Vec<&PartRuntime> = parts.iter().collect();
    order.sort_by_key(|p| p.slot);
    for p in order {
        let Some(&each) = share.get(&p.slot.0) else { continue };
        for c in &p.cells {
            if c.index() >= n { continue; }
            let pos = grid.pos(*c);
            if !grid.is_solid(pos) { continue; }
            let owned = grid.design.cells[c.index()].material == Material::PartBody && grid.cell_type(pos).part == Some(p.slot);
            if !owned { acc.cube(each, DVec3::from(pos.centre_m()), side); }
        }
    }
    acc.finish(centre)
}

/// Add fluids to dry mass: fuel, ballast water and ammunition by part state, flood water per compartment at
/// the centroid of its filled volume. Called every tick for full-physics hulls; must be cheap (no cell loops).
/// Unit: `sim-core-mass`.
pub fn with_fluids(dry: &MassProperties, parts: &[PartRuntime], compartments: &CompartmentSet, catalogue: &Catalogue) -> MassProperties {
    let mut acc = Accum::new();
    acc.body(dry);
    for p in parts {
        let def = part_def(catalogue, p);
        let m = match (&p.state, def.map(|d| &d.function)) {
            (PartState::FuelTank { litres }, _) => litres * FUEL_DENSITY_KG_PER_L,
            (PartState::Ballast { kg }, Some(PartFunction::Ballast { max_kg })) => kg.clamp(0.0, max_kg.max(0.0)),
            (PartState::Magazine { ammo_kg, .. }, _) => *ammo_kg,
            (PartState::Mount(ms), Some(PartFunction::Gun(g))) => ms.ready_rounds as f64 * g.shell_kg,
            (PartState::Launcher { ready, .. }, Some(PartFunction::Launcher(l))) => catalogue.munition(&l.munition).map_or(0.0, |m| *ready as f64 * m.mass_kg),
            (PartState::MineRail { mines, .. }, Some(PartFunction::MineRail { mine, .. })) => catalogue.munition(mine).map_or(0.0, |m| *mines as f64 * m.mass_kg),
            _ => 0.0,
        };
        acc.point(m, p.centre);
    }
    let rho = catalogue.tuning.buoyancy.water_density;
    let rho = if rho.is_finite() && rho > 0.0 { rho } else { 1025.0 };
    for c in &compartments.compartments {
        if !(c.water_m3 > 0.0) { continue; }
        let at = if c.water_centroid.is_finite() { c.water_centroid } else { c.centroid };
        acc.point(c.water_m3 * rho, at);
    }
    acc.finish(if dry.com.is_finite() { dry.com } else { DVec3::ZERO })
}

/// Combine two bodies' properties (parallel-axis theorem). Unit: `sim-core-mass`.
pub fn combine(a: &MassProperties, b: &MassProperties) -> MassProperties {
    let mut acc = Accum::new();
    acc.body(a);
    acc.body(b);
    let fallback = if a.com.is_finite() { a.com } else { DVec3::ZERO };
    acc.finish(fallback)
}
