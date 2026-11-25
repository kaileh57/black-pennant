//! Unit `sim-core-armour` (hard: the penetration rules are exact and gate-checked).
//! Armour, penetration, shell stepping and HE bursts over the cell grid. DESIGN.md L563-582.
//!
//! Shell stepping, while penetration remains:
//! 1. Air cells cost 0. An unarmoured solid cell costs 2 mm. An armour cell costs its tier thickness divided by
//!    cos(obliquity), using the shaped cell's surface normal; obliquity over 70 degrees ricochets and deletes
//!    the shell.
//! 2. Each solid cell crossed takes the band's K buckets of path damage.
//! 3. The fuse fires after the band's delay, counted in solid cells after the first solid cell hit. If an
//!    armour cell's effective thickness exceeds the remaining penetration, the shell stops and bursts at that
//!    cell's outer face.
//! 4. A non-penetrating hit that reached at least 50% of the needed thickness spalls: the cell directly behind
//!    the armour takes 1 bucket.
//! 5. HE: every cell with Euclidean centre distance d <= r takes ceil(B * (1 - d / (r + 1))) buckets. An intact
//!    armour cell on the straight line from the burst to a target cell blocks HE to that cell. Armour cells
//!    themselves take floor(B / 2) at the centre with the same falloff.
//!
//! Bucket results are integers; scaled values round to nearest, minimum 1 (L582).
//!
//! # Implementation decisions
//! - Obliquity of a cube armour cell is measured against the face the shell actually enters through (the
//!   traversal's entry face); shaped cells use `content::cell::armour_normal`. A shell meeting a plate
//!   therefore pays `tier / cos(obliquity)` once per layer of plate it crosses: stepping sideways inside a
//!   contiguous run of cube armour (entering the next armour cell through a face on a different axis than
//!   the one that was struck) costs nothing more, because the traversal visits about `1 + tan(obliquity)`
//!   cells per layer and charging each would multiply the plate's resistance. A sideways step into a
//!   thicker tier than the run has paid for is charged as a fresh strike (against the face it enters), so a
//!   thin strake never lets a shell into a heavy belt for free. Path damage still lands on every solid cell
//!   crossed. Shaped armour cells are always charged.
//! - The fuse fires in the solid cell that brings the count past the delay, i.e. the (delay + 1)-th solid
//!   cell crossed; HE is centred on that cell. A shell whose penetration runs out in unarmoured structure
//!   bursts in the cell where it ran out (outcome `Penetrated`). A shell that leaves the grid unfused is
//!   `PassedThrough`.
//! - "Bursts at the outer face" centres HE on the cell in front of the stopping armour cell (the cell the
//!   shell came from, which may lie outside the grid), so the intact plate shields what is behind it.
//! - Spall: past the stopping cell and any armour directly behind it along the shell's path, the first cell
//!   takes 1 bucket if it is solid.
//! - "Intact" armour for HE blocking means damage level Intact (bucket 0 or 1), counting the path damage of
//!   the same hit, so a shell that holed a plate lets its own burst through the hole. The burst cell and the
//!   target cell never block.
//! - Armour cells take `round(floor(B / 2) * (1 - d / (r + 1)))`, minimum 1 (0 only when floor(B / 2) is 0).
//! - `rounds == 2` doubles every entry, clamped to 8 (identical to applying the result twice).
//! - Warheads: `Penetrate` uses the autocannon band's path K, and bursts (rather than vanishing) on a
//!   ricochet or when stopped. `Proximity` against a hull steps exactly like an autocannon shell at its 1 km
//!   penetration (L590-591: "the 20 to 40 mm band's values against hulls"); if its path meets no solid cell it
//!   bursts at the solid cell nearest `entry` with that band's B and r. `Contact` bursts at the first solid
//!   cell along `dir` (or the nearest solid cell if the path meets none). `Fuse::None` does nothing.
//! - Cook-off: [`he_burst`] with `b == COOKOFF_B` destroys every live cell within r (bucket 8), with no
//!   falloff and no armour blocking (L666).
use crate::grid::VesselGrid;
use crate::projectiles::ShellHit;
use content::cell::{armour_normal, ArmourTier, CellPos, Face, BUCKET_DESTROYED};
use content::enums::Axis;
use content::ids::CellIndex;
use content::part::weapons::{BandDef, Fuse, RICOCHET_DEG, UNARMOURED_CELL_MM};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// `b` sentinel for [`he_burst`]: destroy every live cell within the radius (magazine cook-off, L666).
pub const COOKOFF_B: u8 = u8::MAX;

/// Buckets to add to one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellDamage { pub cell: CellIndex, pub buckets: u8 }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HitOutcome { Ricochet, Stopped, Spalled, Penetrated, PassedThrough }

/// Result of one shell or warhead against one grid. `damage` is ascending by cell with one entry per cell
/// (path and HE damage to the same cell are summed).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HitResult {
    pub outcome: Option<HitOutcome>,
    pub damage: Vec<CellDamage>,
    /// Cell where the charge burst, if it did.
    pub burst: Option<CellIndex>,
    /// True when the hit penetrated or burst with HE: only such hits may ignite a fire (L662).
    pub can_ignite: bool,
    /// Cells visited (work units).
    pub work_units: u32,
}

/// Pending buckets per cell index.
type DamageMap = BTreeMap<u32, u16>;

fn add(map: &mut DamageMap, i: CellIndex, b: u16) {
    if b > 0 { *map.entry(i.0).or_insert(0) += b; }
}

/// Cell coordinates that may lie outside the grid (a burst in front of an outer plate).
type Coord = [i32; 3];

fn coord(p: CellPos) -> Coord { [p.x as i32, p.y as i32, p.z as i32] }

fn grid_dims(grid: &VesselGrid) -> Option<[i32; 3]> {
    let s = grid.size();
    let n = s[0] as usize * s[1] as usize * s[2] as usize;
    if s.contains(&0) || grid.design.cells.len() != n || grid.damage.len() != n { return None; }
    Some([s[0] as i32, s[1] as i32, s[2] as i32])
}

fn to_pos(c: Coord, dims: [i32; 3]) -> Option<CellPos> {
    if (0..3).all(|a| c[a] >= 0 && c[a] < dims[a]) { Some(CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16)) } else { None }
}

fn armour_of(grid: &VesselGrid, p: CellPos) -> Option<ArmourTier> {
    if !grid.is_solid(p) { return None; }
    let t = grid.cell_type(p).armour;
    if t == ArmourTier::None { None } else { Some(t) }
}

fn cos_ricochet() -> f64 { libm::cos(RICOCHET_DEG.to_radians()) }

fn dominant_axis(n: [f64; 3]) -> Axis {
    let (ax, ay, az) = (n[0].abs(), n[1].abs(), n[2].abs());
    if ax >= ay && ax >= az { Axis::X } else if ay >= az { Axis::Y } else { Axis::Z }
}

fn face_normal(f: Face) -> [f64; 3] {
    let (x, y, z) = f.offset();
    [x as f64, y as f64, z as f64]
}

fn unit(dir: [f64; 3]) -> Option<[f64; 3]> {
    if !dir.iter().all(|v| v.is_finite()) { return None; }
    let l = libm::sqrt(dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]);
    if l.is_nan() || l <= 1e-12 { return None; }
    Some([dir[0] / l, dir[1] / l, dir[2] / l])
}

/// Parameters of one shell-like walk through the grid.
#[derive(Clone, Copy)]
struct Walk { pen_mm: f64, delay: u8, path_k: u8, burst_on_ricochet: bool }

/// Where and how a walk ended.
#[derive(Clone, Copy, Default)]
struct WalkEnd {
    outcome: Option<HitOutcome>,
    /// HE centre (possibly outside the grid) and the cell reported as the burst cell.
    burst: Option<(Coord, CellIndex)>,
    solid_hits: u32,
    visited: u32,
}

/// Step a shell through the grid from `entry` along unit `dir`, accumulating path and spall damage.
fn walk(grid: &VesselGrid, entry: [f64; 3], dir: [f64; 3], w: Walk, dmg: &mut DamageMap) -> WalkEnd {
    let mut end = WalkEnd::default();
    let Some(dims) = grid_dims(grid) else { return end };
    let reach = (dims[0] + dims[1] + dims[2] + 2) as f64;
    // Start half a cell back so the first solid cell's entry face is the one actually crossed.
    let from = [entry[0] - dir[0] * 0.5, entry[1] - dir[1] * 0.5, entry[2] - dir[2] * 0.5];
    let to = [entry[0] + dir[0] * reach, entry[1] + dir[1] * reach, entry[2] + dir[2] * reach];
    let cos_ric = cos_ricochet();
    let mut remaining = if w.pen_mm.is_finite() { w.pen_mm.max(0.0) } else { 0.0 };
    // Axis struck and thickest tier paid in the current contiguous run of armour.
    let mut run_axis: Option<Axis> = None;
    let mut run_tier_mm = 0.0f64;
    let mut seeking_spall = false;
    let mut done = false;
    grid.traverse(from, to, &mut |p, face| {
        end.visited = end.visited.saturating_add(1);
        let Some(idx) = grid.index(p) else { return true };
        if seeking_spall {
            if armour_of(grid, p).is_some() { return true; }
            if grid.is_solid(p) { add(dmg, idx, 1); }
            return false;
        }
        if !grid.is_solid(p) {
            run_axis = None;
            run_tier_mm = 0.0;
            return true;
        }
        let ty = grid.cell_type(p);
        if ty.armour != ArmourTier::None {
            let cube = ty.shape.is_cube();
            let tier_mm = ty.armour.thickness_mm();
            let lateral = cube && tier_mm <= run_tier_mm && run_axis.is_some_and(|a| a != face.axis());
            if !lateral {
                let (n, axis) = if cube { (face_normal(face), face.axis()) } else {
                    let n = armour_normal(ty.shape, dir);
                    (n, dominant_axis(n))
                };
                let cos = (n[0] * dir[0] + n[1] * dir[1] + n[2] * dir[2]).abs().min(1.0);
                let outer = { let (dx, dy, dz) = face.offset(); [p.x as i32 + dx as i32, p.y as i32 + dy as i32, p.z as i32 + dz as i32] };
                if cos < cos_ric {
                    end.outcome = Some(HitOutcome::Ricochet);
                    if w.burst_on_ricochet { end.burst = Some((outer, idx)); }
                    done = true;
                    return false;
                }
                let eff = tier_mm / cos;
                if eff > remaining {
                    end.burst = Some((outer, idx));
                    done = true;
                    if remaining >= 0.5 * eff {
                        end.outcome = Some(HitOutcome::Spalled);
                        seeking_spall = true;
                        return true;
                    }
                    end.outcome = Some(HitOutcome::Stopped);
                    return false;
                }
                remaining -= eff;
                run_axis = Some(axis);
                run_tier_mm = run_tier_mm.max(tier_mm);
            }
        } else {
            run_axis = None;
            run_tier_mm = 0.0;
            remaining -= UNARMOURED_CELL_MM;
        }
        add(dmg, idx, w.path_k as u16);
        end.solid_hits += 1;
        if end.solid_hits > w.delay as u32 || remaining <= 0.0 {
            end.outcome = Some(HitOutcome::Penetrated);
            end.burst = Some((coord(p), idx));
            done = true;
            return false;
        }
        true
    });
    if !done && end.visited > 0 { end.outcome = Some(HitOutcome::PassedThrough); }
    end
}

/// HE from a centre that may lie outside the grid. `pending` holds damage already dealt by the same hit (it
/// decides which armour is still intact); HE buckets go to `out`. Returns cells examined plus line steps.
fn burst_at(grid: &VesselGrid, c: Coord, b: u8, r_cells: u8, pending: &DamageMap, out: &mut DamageMap) -> u32 {
    let Some(dims) = grid_dims(grid) else { return 0 };
    if b == 0 { return 0; }
    let r = r_cells as i32;
    let destroy_all = b == COOKOFF_B;
    let lo = [(c[0] - r).max(0), (c[1] - r).max(0), (c[2] - r).max(0)];
    let hi = [(c[0] + r).min(dims[0] - 1), (c[1] + r).min(dims[1] - 1), (c[2] + r).min(dims[2] - 1)];
    if (0..3).any(|a| lo[a] > hi[a]) { return 0; }
    // Line tests only matter when some armour lies inside the burst box.
    let mut any_armour = false;
    if !destroy_all && grid.types.iter().any(|t| t.armour != ArmourTier::None) {
        'scan: for y in lo[1]..=hi[1] { for z in lo[2]..=hi[2] { for x in lo[0]..=hi[0] {
            if armour_of(grid, CellPos::new(x as i16, y as i16, z as i16)).is_some() { any_armour = true; break 'scan; }
        } } }
    }
    let mut work = 0u32;
    let rf = r as f64 + 1.0;
    for y in lo[1]..=hi[1] {
        for z in lo[2]..=hi[2] {
            for x in lo[0]..=hi[0] {
                let (dx, dy, dz) = (x - c[0], y - c[1], z - c[2]);
                let d2 = dx * dx + dy * dy + dz * dz;
                if d2 > r * r { continue; }
                let p = CellPos::new(x as i16, y as i16, z as i16);
                if !grid.is_solid(p) { continue; }
                let Some(idx) = grid.index(p) else { continue };
                work = work.saturating_add(1);
                let buckets: u16 = if destroy_all { BUCKET_DESTROYED as u16 } else {
                    let d = libm::sqrt(d2 as f64);
                    let frac = (rf - d) / rf;
                    if armour_of(grid, p).is_some() {
                        let base = (b / 2) as f64;
                        if base == 0.0 { 0 } else { libm::round(base * frac).max(1.0) as u16 }
                    } else {
                        libm::ceil(b as f64 * frac - 1e-9).max(1.0) as u16
                    }
                };
                if buckets == 0 { continue; }
                if any_armour && d2 > 0 && blocked(grid, c, [x, y, z], pending, &mut work) { continue; }
                add(out, idx, buckets);
            }
        }
    }
    work
}

/// True when an intact armour cell lies strictly between the burst centre and the target on the line
/// between their centres (the shared Amanatides-Woo traversal).
fn blocked(grid: &VesselGrid, c: Coord, t: Coord, pending: &DamageMap, work: &mut u32) -> bool {
    let from = [c[0] as f64 + 0.5, c[1] as f64 + 0.5, c[2] as f64 + 0.5];
    let to = [t[0] as f64 + 0.5, t[1] as f64 + 0.5, t[2] as f64 + 0.5];
    let mut hit = false;
    grid.traverse(from, to, &mut |p, _| {
        *work = work.saturating_add(1);
        let q = coord(p);
        if q == t { return false; }
        if q == c { return true; }
        if armour_of(grid, p).is_some() {
            if let Some(i) = grid.index(p) {
                let bucket = grid.bucket(i) as u16 + pending.get(&i.0).copied().unwrap_or(0);
                if bucket <= 1 { hit = true; return false; }
            }
        }
        true
    });
    hit
}

/// Live solid cell whose centre is nearest a point in cell units; ties go to the lowest index.
fn nearest_solid(grid: &VesselGrid, at: [f64; 3]) -> Option<(CellIndex, CellPos, u32)> {
    grid_dims(grid)?;
    if !at.iter().all(|v| v.is_finite()) { return None; }
    let cells = grid.solid_cells();
    let work = cells.len() as u32;
    let mut best: Option<(f64, CellIndex)> = None;
    for i in cells {
        let p = grid.pos(i);
        let d = [p.x as f64 + 0.5 - at[0], p.y as f64 + 0.5 - at[1], p.z as f64 + 0.5 - at[2]];
        let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        if best.is_none_or(|(b, _)| d2 < b) { best = Some((d2, i)); }
    }
    best.map(|(_, i)| (i, grid.pos(i), work))
}

/// First live solid cell along a ray from `entry`.
fn first_solid(grid: &VesselGrid, entry: [f64; 3], dir: [f64; 3]) -> (Option<(CellIndex, CellPos)>, u32) {
    let Some(dims) = grid_dims(grid) else { return (None, 0) };
    let reach = (dims[0] + dims[1] + dims[2] + 2) as f64;
    let from = [entry[0] - dir[0] * 0.5, entry[1] - dir[1] * 0.5, entry[2] - dir[2] * 0.5];
    let to = [entry[0] + dir[0] * reach, entry[1] + dir[1] * reach, entry[2] + dir[2] * reach];
    let mut found = None;
    let mut work = 0u32;
    grid.traverse(from, to, &mut |p, _| {
        work = work.saturating_add(1);
        if grid.is_solid(p) { found = grid.index(p).map(|i| (i, p)); false } else { true }
    });
    (found, work)
}

/// Combine path and HE damage into the sorted output, doubling for merged rounds.
fn finish(path: DamageMap, he: DamageMap, rounds: u8, outcome: Option<HitOutcome>, burst: Option<CellIndex>, work: u32) -> HitResult {
    let mut total = path;
    for (k, v) in he { *total.entry(k).or_insert(0) += v; }
    let mult: u16 = if rounds >= 2 { 2 } else { 1 };
    let damage: Vec<CellDamage> = total
        .into_iter()
        .filter(|&(_, v)| v > 0)
        .map(|(k, v)| CellDamage { cell: CellIndex(k), buckets: (v.saturating_mul(mult)).min(BUCKET_DESTROYED as u16) as u8 })
        .collect();
    let can_ignite = burst.is_some()
        || outcome == Some(HitOutcome::Penetrated)
        || (outcome == Some(HitOutcome::PassedThrough) && !damage.is_empty());
    HitResult { outcome, damage, burst, can_ignite, work_units: work }
}

/// Run a walk and its burst.
fn walk_and_burst(grid: &VesselGrid, entry: [f64; 3], dir: [f64; 3], w: Walk, he_b: u8, he_r: u8, rounds: u8) -> (HitResult, WalkEnd) {
    let mut path = DamageMap::new();
    let end = walk(grid, entry, dir, w, &mut path);
    let mut he = DamageMap::new();
    let mut work = end.visited;
    if let Some((c, _)) = end.burst { work = work.saturating_add(burst_at(grid, c, he_b, he_r, &path, &mut he)); }
    (finish(path, he, rounds, end.outcome, end.burst.map(|b| b.1), work), end)
}

/// Resolve one shell hit by the rules above; `hit.rounds == 2` applies the result twice. Reads the grid only;
/// the caller applies `damage` through the damage unit. Unit: `sim-core-armour`.
pub fn resolve_shell(grid: &VesselGrid, hit: &ShellHit, band: &BandDef) -> HitResult {
    let Some(dir) = unit(hit.dir) else { return HitResult::default() };
    if !hit.entry.iter().all(|v| v.is_finite()) { return HitResult::default(); }
    let pen = crate::projectiles::penetration_mm(band, hit.speed_mps, hit.v_1km);
    let w = Walk { pen_mm: pen, delay: band.fuse_delay_cells, path_k: band.path_k, burst_on_ricochet: false };
    walk_and_burst(grid, hit.entry, dir, w, band.he_b, band.he_r_cells, hit.rounds).0
}

/// Resolve a missile, torpedo, mine or depth-charge warhead entering or bursting against a grid: `Penetrate`
/// fuses step like a shell with a fixed armour budget, `Contact` bursts at the first solid cell, and
/// `Proximity`, `Influence` and `Depth` burst at the solid cell nearest `entry`. Then HE with B and r.
/// Unit: `sim-core-armour`.
///
/// See the module notes: `Proximity` with a usable `dir` first steps like an autocannon shell (L590-591).
pub fn resolve_warhead(grid: &VesselGrid, entry: [f64; 3], dir: [f64; 3], fuse: Fuse, he_b: u8, he_r_cells: u8, autocannon_band: &BandDef) -> HitResult {
    let dir_u = unit(dir);
    let entry_ok = entry.iter().all(|v| v.is_finite());
    let nearest_burst = |b: u8, r: u8, outcome: Option<HitOutcome>, work0: u32| -> HitResult {
        let Some((i, p, w)) = nearest_solid(grid, entry) else { return HitResult { work_units: work0, ..HitResult::default() } };
        let mut he = DamageMap::new();
        let w2 = burst_at(grid, coord(p), b, r, &DamageMap::new(), &mut he);
        finish(DamageMap::new(), he, 1, outcome, Some(i), work0.saturating_add(w).saturating_add(w2))
    };
    if !entry_ok { return HitResult::default(); }
    match fuse {
        Fuse::None => HitResult::default(),
        Fuse::Penetrate { armour_mm, delay_cells } => {
            let Some(d) = dir_u else { return nearest_burst(he_b, he_r_cells, Some(HitOutcome::Stopped), 0) };
            let w = Walk { pen_mm: armour_mm, delay: delay_cells, path_k: autocannon_band.path_k, burst_on_ricochet: true };
            let (res, end) = walk_and_burst(grid, entry, d, w, he_b, he_r_cells, 1);
            if end.solid_hits == 0 && end.burst.is_none() { return nearest_burst(he_b, he_r_cells, Some(HitOutcome::Stopped), res.work_units); }
            res
        }
        Fuse::Proximity { .. } => {
            let band = autocannon_band;
            let Some(d) = dir_u else { return nearest_burst(band.he_b, band.he_r_cells, Some(HitOutcome::Stopped), 0) };
            let w = Walk { pen_mm: band.p_1km_mm, delay: band.fuse_delay_cells, path_k: band.path_k, burst_on_ricochet: true };
            let (res, end) = walk_and_burst(grid, entry, d, w, band.he_b, band.he_r_cells, 1);
            if end.solid_hits == 0 && end.burst.is_none() { return nearest_burst(band.he_b, band.he_r_cells, Some(HitOutcome::Stopped), res.work_units); }
            res
        }
        Fuse::Contact => {
            let (found, work) = match dir_u { Some(d) => first_solid(grid, entry, d), None => (None, 0) };
            match found {
                Some((i, p)) => {
                    let mut he = DamageMap::new();
                    let w2 = burst_at(grid, coord(p), he_b, he_r_cells, &DamageMap::new(), &mut he);
                    finish(DamageMap::new(), he, 1, Some(HitOutcome::Stopped), Some(i), work.saturating_add(w2))
                }
                None => nearest_burst(he_b, he_r_cells, Some(HitOutcome::Stopped), work),
            }
        }
        Fuse::Influence { .. } | Fuse::Depth { .. } => nearest_burst(he_b, he_r_cells, None, 0),
    }
}

/// HE burst centred on a cell (also used for magazine cook-off with every cell in radius destroyed, L666).
/// Unit: `sim-core-armour`.
///
/// Appends one entry per damaged cell to `out`, ascending by cell, and returns the work units. `b ==`
/// [`COOKOFF_B`] destroys every live cell within `r_cells` (bucket 8, no falloff, no armour blocking). An
/// invalid centre adds nothing.
pub fn he_burst(grid: &VesselGrid, centre: CellIndex, b: u8, r_cells: u8, out: &mut Vec<CellDamage>) -> u32 {
    if grid_dims(grid).is_none() || centre.index() >= grid.design.cells.len() { return 0; }
    let p = grid.pos(centre);
    let mut he = DamageMap::new();
    let work = burst_at(grid, coord(p), b, r_cells, &DamageMap::new(), &mut he);
    out.extend(he.into_iter().filter(|&(_, v)| v > 0).map(|(k, v)| CellDamage { cell: CellIndex(k), buckets: v.min(BUCKET_DESTROYED as u16) as u8 }));
    work
}
