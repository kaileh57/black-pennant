//! Silhouette areas for the signature stage, the radar-horizon height and the top-deck sunk test.
use super::ProjectedAreas;
use crate::grid::VesselGrid;
use crate::math::{DQuat, DVec3, Pose};
use crate::parts::PartRuntime;
use content::catalogue::Catalogue;
use content::cell::{CellPos, Material, CELL_M};

/// Raster pixel width along the view plane, metres.
const PIXEL_M: f64 = CELL_M / 4.0;

/// World up in the body frame and the world height of the body origin, sanitised.
fn frame(pose: &Pose) -> (DVec3, f64) {
    let rot = if pose.rot.is_finite() && pose.rot.length_squared() > 1e-12 { pose.rot.normalize() } else { DQuat::IDENTITY };
    let up = (rot.inverse() * DVec3::Y).normalize_or(DVec3::Y);
    (up, if pose.pos.y.is_finite() { pose.pos.y } else { 0.0 })
}

#[inline]
fn centre(x: usize, y: usize, z: usize) -> DVec3 { DVec3::new(x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5) * CELL_M }

fn dims(grid: &VesselGrid) -> Option<[usize; 3]> {
    let s = grid.size();
    let d = [s[0] as usize, s[1] as usize, s[2] as usize];
    let n = d[0] * d[1] * d[2];
    (n > 0 && grid.design.cells.len() == n && grid.damage.len() == n).then_some(d)
}

/// Highest corner of any live cell, in metres above y = 0, from the per-column solid extents. 0 for a grid
/// with no live cell.
pub(super) fn top_height(grid: &VesselGrid, pose: &Pose) -> f64 {
    let Some([sx, _, sz]) = dims(grid) else { return 0.0 };
    let (up, base) = frame(pose);
    let reach = 0.5 * CELL_M * (up.x.abs() + up.y.abs() + up.z.abs());
    let mut best = f64::NEG_INFINITY;
    if grid.yline.len() == sx * sz {
        for z in 0..sz {
            for x in 0..sx {
                let [lo, hi] = grid.yline[x + sx * z];
                if lo > hi { continue; }
                let y = if up.y >= 0.0 { hi } else { lo } as usize;
                best = best.max(up.dot(centre(x, y, z)));
            }
        }
    } else {
        for c in grid.solid_cells() {
            let p = grid.pos(c);
            best = best.max(up.dot(DVec3::from(p.centre_m())));
        }
    }
    let h = base + best + reach;
    if h.is_finite() { h } else { 0.0 }
}

/// True when no live cell of the top deck has its centre above the water surface (L537). When the top-deck
/// layer has lost every cell, the highest live layer below it stands in (and any live layer above it when
/// nothing below is left). Costs one step per row while the highest deck cell is clear of the water.
pub(super) fn top_deck_submerged(grid: &VesselGrid, top_deck_y: i16, pose: &Pose, water: &dyn Fn(f64, f64) -> f64) -> bool {
    let Some([sx, sy, sz]) = dims(grid) else { return true };
    let rows_ok = grid.xline.len() == sy * sz;
    let row = |y: usize, z: usize| -> Option<(usize, usize)> {
        if rows_ok {
            let [lo, hi] = grid.xline[z + sz * y];
            (lo <= hi).then_some((lo as usize, hi as usize))
        } else {
            let lo = (0..sx).find(|&x| grid.is_solid(CellPos::new(x as i16, y as i16, z as i16)))?;
            let hi = (0..sx).rev().find(|&x| grid.is_solid(CellPos::new(x as i16, y as i16, z as i16)))?;
            Some((lo, hi))
        }
    };
    let layer_live = |y: usize| (0..sz).any(|z| row(y, z).is_some());
    let y0 = (top_deck_y.max(0) as usize).min(sy - 1);
    let Some(layer) = (0..=y0).rev().find(|&y| layer_live(y)).or_else(|| (y0 + 1..sy).find(|&y| layer_live(y))) else { return true };
    let (up, base) = frame(pose);
    let rot = if pose.rot.is_finite() && pose.rot.length_squared() > 1e-12 { pose.rot.normalize() } else { DQuat::IDENTITY };
    let pos = if pose.pos.is_finite() { pose.pos } else { DVec3::ZERO };
    let above = |x: usize, z: usize| -> bool {
        let p = centre(x, layer, z);
        let w = pos + rot * p;
        let s = water(w.x, w.z);
        let s = if s.is_finite() { s } else { 0.0 };
        base + up.dot(p) > s
    };
    // The highest live cell of a row is one of its two ends along X; test the highest of those first.
    let mut best: Option<(f64, usize, usize)> = None;
    for z in 0..sz {
        let Some((lo, hi)) = row(layer, z) else { continue };
        let x = if up.x >= 0.0 { hi } else { lo };
        let h = up.dot(centre(x, layer, z));
        if best.is_none_or(|b| h > b.0) { best = Some((h, x, z)); }
    }
    let Some((_, bx, bz)) = best else { return true };
    if above(bx, bz) { return false; }
    // Awash: every live deck cell, highest first.
    let mut cells: Vec<(f64, usize, usize)> = Vec::new();
    for z in 0..sz {
        let Some((lo, hi)) = row(layer, z) else { continue };
        for x in lo..=hi {
            if grid.is_solid(CellPos::new(x as i16, layer as i16, z as i16)) { cells.push((up.dot(centre(x, layer, z)), x, z)); }
        }
    }
    cells.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.2.cmp(&b.2)).then(a.1.cmp(&b.1)));
    !cells.iter().any(|&(_, x, z)| above(x, z))
}

/// One silhouette cell of a layer: plan position, the fraction of it above the water and its radar factor.
#[derive(Clone, Copy)]
struct Sil { x: f64, z: f64, above: f32, rcs: f32 }

/// Projected areas of the live cells; the water plane is `up . p = c` in the body frame. A cell straddling the
/// plane counts above and below in proportion, by the flooding unit's cell fill law, so the areas follow the
/// draft continuously.
pub(super) fn compute(grid: &VesselGrid, parts: &[PartRuntime], cat: &Catalogue, up: DVec3, c: f64, top: f64) -> ProjectedAreas {
    let mut out = ProjectedAreas { rcs_m2: [0.0; 36], above_m2: [0.0; 36], submerged_m2: [0.0; 36], top_height_m: if top.is_finite() { top as f32 } else { 0.0 } };
    let Some([sx, _, sz]) = dims(grid) else { return out };
    let up = if up.is_finite() { up.normalize_or(DVec3::Y) } else { DVec3::Y };
    let c = if c.is_finite() { c } else { 0.0 };
    let h = (CELL_M * (up.x.abs() + up.y.abs() + up.z.abs())).max(CELL_M);
    let factor = |v: f32| if v.is_finite() && v >= 0.0 { v } else { 1.0 };
    let material = |m: Material| cat.materials.iter().find(|d| d.material == m).map_or(1.0, |d| factor(d.rcs_factor));
    let part_rcs = |slot: content::ids::PartSlot| {
        let p = parts.get(slot.index()).filter(|p| p.slot == slot).or_else(|| parts.iter().find(|p| p.slot == slot));
        p.and_then(|p| cat.parts.get_index(p.def.index())).map_or(1.0, |(_, d)| factor(d.rcs_factor))
    };
    let above_of = |p: CellPos| -> f32 { (1.0 - ((c - up.dot(DVec3::from(p.centre_m()))) / h + 0.5).clamp(0.0, 1.0)) as f32 };

    // Silhouette cells by layer: a cell whose four horizontal neighbours are live and stand exactly as far out
    // of the water is covered by them in every horizontal view, so it is left out.
    let mut cells: Vec<Sil> = Vec::new();
    let mut layers: Vec<(usize, usize)> = Vec::new();
    let mut layer_y = i32::MIN;
    for i in grid.solid_cells() {
        let p = grid.pos(i);
        let above = above_of(p);
        let hidden = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().all(|&(dx, dz)| {
            let q = CellPos::new(p.x + dx, p.y, p.z + dz);
            grid.is_solid(q) && above_of(q) == above
        });
        if hidden { continue; }
        let t = grid.cell_type(p);
        let rcs = match (t.material, t.part) {
            (Material::PartBody, Some(s)) => part_rcs(s),
            (m, _) => material(m),
        };
        if p.y as i32 != layer_y {
            layer_y = p.y as i32;
            layers.push((cells.len(), cells.len()));
        }
        let ctr = p.centre_m();
        cells.push(Sil { x: ctr[0], z: ctr[2], above, rcs });
        if let Some(l) = layers.last_mut() { l.1 = cells.len(); }
    }
    if cells.is_empty() { return out; }

    let (ex, ez) = (sx as f64 * CELL_M, sz as f64 * CELL_M);
    let pixel_m2 = PIXEL_M * CELL_M;
    let mut near_t: Vec<f64> = Vec::new();
    let mut near_f: Vec<f32> = Vec::new();
    let mut far_t: Vec<f64> = Vec::new();
    let mut far_f: Vec<f32> = Vec::new();
    let mut up_w: Vec<f32> = Vec::new();
    let mut down_w: Vec<f32> = Vec::new();
    for k in 0..18usize {
        let a = ((10 * k + 5) as f64).to_radians();
        let (sa, ca) = libm::sincos(a);
        // Toward the observer d = (ca, sa) in (x, z); across the view e = (-sa, ca).
        let w = CELL_M * (ca.abs() + sa.abs());
        let along = |x: f64, z: f64| -sa * x + ca * z;
        let corners = [along(0.0, 0.0), along(ex, 0.0), along(0.0, ez), along(ex, ez)];
        let smin = corners.iter().copied().fold(f64::INFINITY, f64::min);
        let smax = corners.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let np = (libm::ceil((smax - smin) / PIXEL_M) as usize).saturating_add(2);
        near_t.clear(); near_t.resize(np, f64::NEG_INFINITY);
        far_t.clear(); far_t.resize(np, f64::INFINITY);
        near_f.clear(); near_f.resize(np, 0.0);
        far_f.clear(); far_f.resize(np, 0.0);
        up_w.clear(); up_w.resize(np, 0.0);
        down_w.clear(); down_w.resize(np, 0.0);
        let (mut above_px, mut sub_px, mut rcs_near, mut rcs_far) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for &(s0, s1) in &layers {
            let (mut lo_i, mut hi_i) = (usize::MAX, 0usize);
            for cell in &cells[s0..s1] {
                let s = along(cell.x, cell.z) - smin;
                let t = ca * cell.x + sa * cell.z;
                let i0 = libm::ceil((s - 0.5 * w) / PIXEL_M - 0.5).max(0.0) as usize;
                let i1 = libm::floor((s + 0.5 * w) / PIXEL_M - 0.5);
                if !(i1 >= 0.0) { continue; }
                let i1 = (i1 as usize).min(np - 1);
                if i1 < i0 { continue; }
                lo_i = lo_i.min(i0);
                hi_i = hi_i.max(i1);
                let below = 1.0 - cell.above;
                for i in i0..=i1 {
                    if cell.above > 0.0 {
                        if cell.above > up_w[i] { up_w[i] = cell.above; }
                        if t > near_t[i] { near_t[i] = t; near_f[i] = cell.rcs; }
                        if t < far_t[i] { far_t[i] = t; far_f[i] = cell.rcs; }
                    }
                    if below > down_w[i] { down_w[i] = below; }
                }
            }
            if lo_i > hi_i { continue; }
            for i in lo_i..=hi_i {
                let wu = up_w[i] as f64;
                above_px += wu;
                rcs_near += wu * near_f[i] as f64;
                rcs_far += wu * far_f[i] as f64;
                sub_px += down_w[i] as f64;
                up_w[i] = 0.0;
                down_w[i] = 0.0;
                near_t[i] = f64::NEG_INFINITY;
                far_t[i] = f64::INFINITY;
            }
        }
        let (above, sub) = ((above_px * pixel_m2) as f32, (sub_px * pixel_m2) as f32);
        out.above_m2[k] = above;
        out.above_m2[k + 18] = above;
        out.submerged_m2[k] = sub;
        out.submerged_m2[k + 18] = sub;
        out.rcs_m2[k] = (rcs_near * pixel_m2) as f32;
        out.rcs_m2[k + 18] = (rcs_far * pixel_m2) as f32;
    }
    out
}
