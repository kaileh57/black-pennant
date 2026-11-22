//! Unit `hm-scenery`. Cloud clusters and island meshes.
//! DESIGN.md L1915: clouds are faceted opaque low-subdivision icosphere clusters with per-vertex wrap lighting,
//! drawn in the main pass, fogged, never blurred; layout from the server seed, drift from the synced wind
//! offset; no volumetric clouds. L1917: islands are flat-shaded triangles (8 m near, 32 m far) with token colour
//! bands and the same fog. L1779 (depth layers: far band of islands, cloud masses and fronts), G5 (L1813: no
//! 90 degree sector of uniform horizon; fix with a seeded front).
//!
//! Hull-mesh does not depend on sim-core: islands sample a height function the client passes in
//! (`sim_core::terrain::Terrain::height_at`).
//!
//! # Cloud vertex data (what `cloud.gdshader` reads)
//! - `COLOR` is white (RGBA 1). Clouds take their value from light and fog only; no per-face tint (L1815 bans).
//! - `CUSTOM0.x` is the wrap term `w` in `0.2..=0.8`, fixed per vertex and independent of the sun (the mesh is
//!   static): the shader lights a vertex with `max((dot(N, L) + w) / (1 + w), 0)`. Larger `w` lets light wrap
//!   further round the facet. It grows with height in the cluster and with distance from the cluster's axis,
//!   so sunlit tops and thin outer puffs read soft and bright while the dense core and the base stay darker.
//! - `CUSTOM0.y` is the height fraction in the cluster, 0 at the base to 1 at the top, for a flat grey base.
//! - `CUSTOM0.z` and `.w` are 0 (reserved).
//!
//! Cluster-local frame: origin at the centre of the cloud base, +Y up; the base sits at y = 0 and nothing hangs
//! more than a few percent of the size below it. `size_m` is the horizontal diameter of a cumulus or stratus
//! sheet and the length (along X) of a storm front.
//!
//! # Layout
//! [`cloud_layout`] places a fixed horizon ring of six masses (one of them the seeded front, L1813 G5) at
//! 85-100% of `radius_m`, 60 degrees apart with at most 10 degrees of jitter, so the largest bearing gap seen
//! from the map centre stays under 90 degrees at every cover. Interior clusters scale with cover and area
//! (one per 9 km^2 at full cover, at most 160); storms add two or three fronts inside the map. Heights are
//! cloud-base altitudes in metres above the sea.
//!
//! Every random choice comes from a counter hash of (seed, stream, index), so a seed gives the same clouds on
//! every machine. Trig is libm.

use crate::prim::{MeshBuilder, Xform};
use crate::types::MeshData;

/// Kind of cloud mass; sizes and stacking differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CloudKind { Cumulus, StratusSheet, StormFront }

// ---------------------------------------------------------------------------------------------------------
// Seeded hash RNG (counter based, no state shared between streams).

fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Draws from `hash(seed, stream, counter)`.
struct Rng { key: u64, n: u64 }
impl Rng {
    fn new(seed: u64, stream: u64) -> Self { Rng { key: mix64(seed ^ mix64(stream.wrapping_mul(0xD6E8_FEB8_6659_FD93))), n: 0 } }
    fn next(&mut self) -> u64 { self.n = self.n.wrapping_add(1); mix64(self.key ^ mix64(self.n)) }
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 { (self.next() >> 11) as f64 * (1.0 / (1u64 << 53) as f64) }
    fn range(&mut self, lo: f64, hi: f64) -> f64 { lo + (hi - lo) * self.unit() }
    fn rangef(&mut self, lo: f32, hi: f32) -> f32 { self.range(lo as f64, hi as f64) as f32 }
    /// Integer in lo..=hi.
    fn int(&mut self, lo: u32, hi: u32) -> u32 { lo + (self.next() % (hi.saturating_sub(lo) as u64 + 1)) as u32 }
}

// ---------------------------------------------------------------------------------------------------------
// Cloud clusters

/// One puff: centre, radii (x, y, z), yaw, icosphere subdivision.
struct Puff { c: [f32; 3], r: [f32; 3], yaw: f32, subdiv: u8 }

fn default_size(kind: CloudKind) -> f32 {
    match kind { CloudKind::Cumulus => 600.0, CloudKind::StratusSheet => 3_000.0, CloudKind::StormFront => 10_000.0 }
}

/// A puff of horizontal radius `r` centred at height `cy`, its underside flattened so it never hangs more than
/// `0.03 * r` below the cluster base.
fn base_puff(x: f32, cy: f32, z: f32, r: f32, yaw: f32, subdiv: u8) -> Puff {
    let ry = r.min(cy + 0.03 * r).max(r * 0.35);
    Puff { c: [x, cy, z], r: [r, ry, r], yaw, subdiv }
}

fn cumulus(rng: &mut Rng, d: f32) -> Vec<Puff> {
    let mut v = Vec::new();
    let r0 = d * 0.27;
    v.push(base_puff(0.0, r0 * 0.6, 0.0, r0, rng.rangef(0.0, core::f32::consts::TAU), 1));
    let n = rng.int(5, 8);
    let a0 = rng.rangef(0.0, core::f32::consts::TAU);
    for i in 0..n {
        let a = a0 + core::f32::consts::TAU * i as f32 / n as f32 + rng.rangef(-0.3, 0.3);
        let r = d * rng.rangef(0.13, 0.2);
        let dist = (d * rng.rangef(0.18, 0.3)).min(d * 0.5 - r);
        let cy = r * rng.rangef(0.45, 0.8);
        let sub = if r > d * 0.17 { 1 } else { 0 };
        v.push(base_puff(dist * libm::cosf(a), cy, dist * libm::sinf(a), r, rng.rangef(0.0, core::f32::consts::TAU), sub));
    }
    // Crown: one to three puffs sunk into the main puff's shoulders, offset from the axis so the top is a
    // lumpy cauliflower. A crown sitting high on the axis reads as a head on a body (a snowman) from afar.
    let crowns = rng.int(1, 3);
    for _ in 0..crowns {
        let a = rng.rangef(0.0, core::f32::consts::TAU);
        let r = d * rng.rangef(0.13, 0.18);
        let off = d * rng.rangef(0.06, 0.16);
        let cy = r0 * 0.95 + r * rng.rangef(0.0, 0.3);
        v.push(Puff { c: [off * libm::cosf(a), cy, off * libm::sinf(a)], r: [r * 1.15, r * 0.85, r * 1.15], yaw: rng.rangef(0.0, core::f32::consts::TAU), subdiv: 1 });
    }
    v
}

fn stratus(rng: &mut Rng, d: f32) -> Vec<Puff> {
    let mut v = Vec::new();
    let t = d * 0.06;
    let n = rng.int(10, 15);
    for i in 0..n {
        // Stratified over an ellipse (aspect 1 : 0.6) so the sheet has no holes in the middle.
        let a = core::f32::consts::TAU * (i as f32 + rng.rangef(0.0, 0.8)) / n as f32;
        let k = (i as f32 + 0.5) / n as f32;
        let rad = 0.42 * libm::sqrtf(k.max(0.05)) * rng.rangef(0.6, 1.0);
        let rh = d * rng.rangef(0.12, 0.2);
        let x = (d * rad * libm::cosf(a)).clamp(-d * 0.5 + rh, d * 0.5 - rh);
        let z = (d * 0.6 * rad * libm::sinf(a)).clamp(-d * 0.3 + rh * 0.6, d * 0.3 - rh * 0.6);
        let ry = t * rng.rangef(0.35, 0.6);
        v.push(Puff { c: [x, ry * 0.9, z], r: [rh, ry, rh * 0.6], yaw: rng.rangef(-0.4, 0.4), subdiv: 0 });
    }
    v
}

fn storm_front(rng: &mut Rng, l: f32) -> Vec<Puff> {
    let mut v = Vec::new();
    // Base shelf: a row of big flattened puffs along X.
    let k = rng.int(8, 11);
    let step = l / k as f32;
    let depth = l * 0.09;
    for i in 0..k {
        let x = -l * 0.5 + step * (i as f32 + 0.5) + rng.rangef(-0.15, 0.15) * step;
        let r = step * rng.rangef(0.7, 0.85);
        let z = rng.rangef(-0.3, 0.3) * depth;
        v.push(Puff { c: [x, r * 0.45, z], r: [r, r * 0.5, depth.max(r * 0.6)], yaw: rng.rangef(-0.15, 0.15), subdiv: 1 });
    }
    // Towers: stacked puffs climbing out of the shelf; the tallest carries the anvil.
    let towers = rng.int(3, 5);
    let mut top = (0.0f32, 0.0f32, 0.0f32);
    for _ in 0..towers {
        let x = rng.rangef(-0.38, 0.38) * l;
        let z = rng.rangef(-0.2, 0.2) * depth;
        let r = l * rng.rangef(0.05, 0.075);
        let levels = rng.int(2, 3);
        let mut y = step * 0.5 + r * 0.6;
        for lv in 0..levels {
            // Each level sinks well into the one below and sits off its axis, so a tower billows upward;
            // balls stacked on their tops read as a snowman at the horizon.
            let rr = r * (1.0 - 0.15 * lv as f32);
            v.push(Puff { c: [x + rng.rangef(-0.35, 0.35) * rr, y, z + rng.rangef(-0.2, 0.2) * rr], r: [rr * 1.1, rr * 0.85, rr], yaw: rng.rangef(0.0, core::f32::consts::TAU), subdiv: 1 });
            if y > top.1 { top = (x, y + rr * 0.7, z); }
            y += rr * 0.8;
        }
    }
    // Anvil: wide, thin and sheared downwind (+X) at the top of the tallest tower.
    let ra = l * 0.16;
    v.push(Puff { c: [top.0 + ra * 0.35, top.1 + ra * 0.12, top.2], r: [ra, ra * 0.16, ra * 0.55], yaw: rng.rangef(-0.1, 0.1), subdiv: 1 });
    // Ragged scud in front of the shelf.
    let scud = rng.int(5, 8);
    for _ in 0..scud {
        let r = step * rng.rangef(0.2, 0.35);
        let x = rng.rangef(-0.45, 0.45) * l;
        v.push(Puff { c: [x, r * 0.3, depth * rng.rangef(0.7, 1.0)], r: [r * 1.4, r * 0.35, r], yaw: rng.rangef(0.0, core::f32::consts::TAU), subdiv: 0 });
    }
    v
}

/// A seeded cluster of faceted icospheres (subdivision 0 or 1) in cluster-local metres. `custom0.x` carries a
/// per-vertex wrap-lighting term the cloud shader uses. Deterministic per seed. Unit: `hm-scenery`.
///
/// See the module notes for the frame, `size_m` and the exact `custom0` meaning. A non-finite or non-positive
/// `size_m` takes the kind's typical size (600 m cumulus, 3 km sheet, 10 km front); sizes clamp to 10..=50,000 m.
/// Triangle counts: about 300-700 (cumulus), 200-300 (sheet), 1,600-2,600 (front).
pub fn cloud_cluster(kind: CloudKind, seed: u64, size_m: f32) -> MeshData {
    let size = if size_m.is_finite() && size_m > 0.0 { size_m.clamp(10.0, 50_000.0) } else { default_size(kind) };
    let mut rng = Rng::new(seed, kind as u64 + 1);
    let puffs = match kind {
        CloudKind::Cumulus => cumulus(&mut rng, size),
        CloudKind::StratusSheet => stratus(&mut rng, size),
        CloudKind::StormFront => storm_front(&mut rng, size),
    };
    let mut b = MeshBuilder::new();
    b.color = [1.0; 4];
    for p in &puffs {
        let x = Xform::translation(p.c).then(&Xform::rot_y(p.yaw)).then(&Xform::scale(p.r));
        b.with(x, |b| b.icosphere([0.0; 3], 1.0, p.subdiv.min(1)));
    }
    let mut m = b.finish();
    add_wrap_terms(&mut m);
    m
}

/// Fill `custom0` with the wrap term and height fraction (module notes).
fn add_wrap_terms(m: &mut MeshData) {
    let bb = m.aabb();
    if bb.is_empty() { m.custom0.clear(); return; }
    let top = bb.max[1].max(1e-3);
    let cx = 0.5 * (bb.min[0] + bb.max[0]);
    let cz = 0.5 * (bb.min[2] + bb.max[2]);
    let half = [0.5 * (bb.max[0] - bb.min[0]).max(1e-3), 0.5 * (bb.max[2] - bb.min[2]).max(1e-3)];
    m.custom0 = m.positions.iter().map(|p| {
        let h = (p[1] / top).clamp(0.0, 1.0);
        let (ex, ez) = ((p[0] - cx) / half[0], (p[2] - cz) / half[1]);
        let edge = libm::sqrtf(ex * ex + ez * ez).clamp(0.0, 1.0);
        let w = (0.2 + 0.4 * h + 0.2 * edge).clamp(0.2, 0.8);
        [w, h, 0.0, 0.0]
    }).collect();
}

// ---------------------------------------------------------------------------------------------------------
// Cloud layout

/// Horizon masses in the G5 ring.
const HORIZON_MASSES: u32 = 6;
/// Interior clusters at full cover per square metre (one per 9 km^2) and the cap.
const INTERIOR_PER_M2: f64 = 1.0 / 9.0e6;
const INTERIOR_MAX: f64 = 160.0;

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 { let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0); t * t * (3.0 - 2.0 * t) }

/// Seeded cloud layout for a sortie: (kind, seed, size, position relative to the map centre) for each cluster,
/// density from cloud cover 0..=1, including a front for storm and for G5 coverage. Unit: `hm-scenery`.
///
/// Positions are metres: x and z relative to the map centre, y the cloud-base altitude. Cover is clamped to
/// 0..=1 (NaN = 0); `radius_m` is clamped to 2 km..=400 km (non-finite = 20 km). Order is fixed: horizon ring,
/// storm fronts, interior clusters. The interior count never falls as cover rises.
///
/// The G5 guarantee (no 90 degree gap between horizon masses) holds as seen from the map centre and only when
/// the ring is visible, so the client passes a `radius_m` inside the fog visibility V (or re-centres the layout
/// on the camera with the wind drift).
pub fn cloud_layout(seed: u64, cloud_cover: f64, storm: bool, radius_m: f64) -> Vec<(CloudKind, u64, f32, [f64; 3])> {
    let cover = if cloud_cover.is_finite() { cloud_cover.clamp(0.0, 1.0) } else { 0.0 };
    let r = if radius_m.is_finite() { radius_m.clamp(2_000.0, 400_000.0) } else { 20_000.0 };
    let mut out = Vec::new();
    let mut idx: u64 = 0;
    let mut push = |out: &mut Vec<_>, kind: CloudKind, size: f64, pos: [f64; 3]| {
        idx += 1;
        out.push((kind, mix64(seed ^ idx.wrapping_mul(0xA24B_AED4_963E_E407)), size as f32, pos));
    };
    let polar = |brg: f64, d: f64, y: f64| [d * libm::cos(brg), y, d * libm::sin(brg)];
    let deg = core::f64::consts::PI / 180.0;

    // G5 horizon ring: the seeded front first, then banks every 60 degrees.
    let mut rng = Rng::new(seed, 101);
    let b0 = rng.range(0.0, 360.0);
    for k in 0..HORIZON_MASSES {
        let brg = (b0 + 60.0 * k as f64 + rng.range(-10.0, 10.0)) * deg;
        let d = r * rng.range(0.85, 1.0);
        if k == 0 {
            let size = (r * if storm { 0.5 } else { 0.25 }).clamp(4_000.0, 40_000.0);
            push(&mut out, CloudKind::StormFront, size, polar(brg, d, rng.range(300.0, 600.0)));
        } else if cover > 0.7 {
            let size = (r * 0.15).clamp(2_000.0, 20_000.0) * rng.range(0.8, 1.2);
            push(&mut out, CloudKind::StratusSheet, size, polar(brg, d, rng.range(1_800.0, 3_000.0)));
        } else {
            // Broad heaps sitting low on the horizon (10 to 20 degrees wide at the ring's range), so the far
            // depth layer reads as a band of cloud masses rather than a scatter of distant puffs.
            let size = rng.range(3_500.0, 6_000.0) * (0.8 + 0.4 * cover);
            push(&mut out, CloudKind::Cumulus, size, polar(brg, d, rng.range(250.0, 600.0)));
        }
    }

    // Storm fronts crossing the map.
    if storm {
        let mut rng = Rng::new(seed, 202);
        let n = rng.int(2, 3);
        for _ in 0..n {
            let brg = rng.range(0.0, core::f64::consts::TAU);
            let size = (r * rng.range(0.3, 0.5)).clamp(6_000.0, 40_000.0);
            push(&mut out, CloudKind::StormFront, size, polar(brg, r * rng.range(0.3, 0.7), rng.range(300.0, 600.0)));
        }
    }

    // Interior clusters. Each draws from its own index so raising cover only appends clusters.
    let area = core::f64::consts::PI * r * r;
    let n = libm::round(cover * (area * INTERIOR_PER_M2).min(INTERIOR_MAX)) as u32;
    let p_sheet = smoothstep(0.4, 0.9, cover);
    for i in 0..n {
        let mut rng = Rng::new(seed ^ (i as u64 + 1).wrapping_mul(0x9FB2_1C65_1E98_DF25), 303);
        let brg = rng.range(0.0, core::f64::consts::TAU);
        let d = r * 0.9 * libm::sqrt(rng.unit());
        if rng.unit() < p_sheet {
            push(&mut out, CloudKind::StratusSheet, rng.range(2_000.0, 5_000.0), polar(brg, d, rng.range(1_800.0, 3_200.0)));
        } else {
            let size = rng.range(300.0, 900.0) * (0.7 + 0.6 * cover);
            push(&mut out, CloudKind::Cumulus, size, polar(brg, d, rng.range(600.0, 1_400.0)));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Islands

/// Height-band colours for islands, linear RGB, lowest band first (from the world tokens; the client passes them).
#[derive(Clone, Debug, PartialEq)]
pub struct IslandBands { pub band_top_m: Vec<f32>, pub colors: Vec<[f32; 3]> }

/// Most lattice steps along one axis; a larger square widens the step instead (bad input never allocates more
/// than about 515^2 height samples).
const MAX_STEPS: f64 = 512.0;
/// Heights are clamped to this range; non-finite samples count as just below the drop line.
const H_MIN: f64 = -500.0;
const H_MAX: f64 = 10_000.0;
const H_NAN: f64 = -2.0;
const DROP_BELOW_M: f64 = -1.0;

/// Lattice coordinates (local to the origin) on one axis: `lo`, every world multiple of `e` strictly inside,
/// and `hi`. Points closer than 1% of a step to the previous one are merged.
fn lattice(lo: f64, hi: f64, e: f64, o: f64) -> Vec<f64> {
    let mut v = vec![lo];
    let k0 = libm::ceil((o + lo) / e);
    let k1 = libm::floor((o + hi) / e);
    let mut k = k0;
    while k <= k1 && v.len() < MAX_STEPS as usize + 2 {
        let c = k * e - o;
        if c > v[v.len() - 1] + 0.01 * e && c < hi - 0.01 * e { v.push(c); }
        k += 1.0;
    }
    v.push(hi);
    v
}

fn band_color(bands: &IslandBands, h: f64) -> [f32; 4] {
    let clean = |c: [f32; 3]| { let f = |x: f32| if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 }; [f(c[0]), f(c[1]), f(c[2]), 1.0] };
    let n = bands.band_top_m.len().min(bands.colors.len());
    for i in 0..n {
        let top = bands.band_top_m[i];
        if top.is_nan() { continue; }
        if h <= top as f64 { return clean(bands.colors[i]); }
    }
    match bands.colors.last() { Some(&c) => clean(c), None => [0.15, 0.15, 0.15, 1.0] }
}

/// Flat-shaded island mesh over the square `[min, max]` (world metres relative to `origin`), triangle edge
/// `edge_m` (8 near, 32 far), sampling `height(x, z)` in world metres; drops triangles fully below y = -1 m.
/// Vertex colour by height band. Unit: `hm-scenery`.
///
/// Coordinates: `min` and `max` are (x, z) offsets from `origin`; a lattice point at local (x, z) samples
/// `height(origin[0] + x, origin[1] + z)` and is emitted at local `[x, h, z]`, so positions stay small in f32
/// (floating origin, L1831). The lattice sits on world multiples of `edge_m` plus the square's own edges, so
/// tiles that share an edge, and an 8 m tile inside a 32 m tile's grid, agree at shared points. Each lattice
/// cell splits along the diagonal whose ends differ least in height (keeps ridges and gullies sharp). One colour
/// per triangle from its mean height: the first band whose top is at or above it, the last colour above every
/// top; alpha 1. `custom0` is empty. Bad input never panics: `min > max` is swapped, a non-finite or tiny
/// `edge_m` takes 8 m (at least 0.5 m), at most 512 steps per axis, extra band entries are ignored.
pub fn island_mesh(height: &dyn Fn(f64, f64) -> f64, origin: [f64; 2], min: [f64; 2], max: [f64; 2], edge_m: f64, bands: &IslandBands) -> MeshData {
    let mut m = MeshData::new();
    if !(origin.iter().chain(min.iter()).chain(max.iter()).all(|v| v.is_finite())) { return m; }
    let (x0, x1) = (min[0].min(max[0]), min[0].max(max[0]));
    let (z0, z1) = (min[1].min(max[1]), min[1].max(max[1]));
    if x1 - x0 < 1e-3 || z1 - z0 < 1e-3 { return m; }
    let e = if edge_m.is_finite() && edge_m > 0.0 { edge_m.max(0.5) } else { 8.0 };
    let e = e.max((x1 - x0) / MAX_STEPS).max((z1 - z0) / MAX_STEPS);
    let xs = lattice(x0, x1, e, origin[0]);
    let zs = lattice(z0, z1, e, origin[1]);
    let (nx, nz) = (xs.len(), zs.len());
    let mut hs = Vec::with_capacity(nx * nz);
    for &z in &zs {
        for &x in &xs {
            let h = height(origin[0] + x, origin[1] + z);
            hs.push(if h.is_finite() { h.clamp(H_MIN, H_MAX) } else { H_NAN });
        }
    }
    let at = |i: usize, j: usize| [xs[i], hs[j * nx + i], zs[j]];
    for j in 0..nz - 1 {
        for i in 0..nx - 1 {
            let (p00, p10, p01, p11) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
            let tris = if (p00[1] - p11[1]).abs() <= (p10[1] - p01[1]).abs() { [[p00, p10, p11], [p00, p11, p01]] } else { [[p00, p10, p01], [p10, p11, p01]] };
            for t in tris { push_tri(&mut m, t, bands); }
        }
    }
    m
}

/// Append one up-facing triangle (corners in front winding: `(c - a) x (b - a)` points up) unless it lies fully
/// below the drop line.
fn push_tri(m: &mut MeshData, t: [[f64; 3]; 3], bands: &IslandBands) {
    if t.iter().all(|p| p[1] < DROP_BELOW_M) { return; }
    let p: [[f32; 3]; 3] = t.map(|q| [q[0] as f32, q[1] as f32, q[2] as f32]);
    let (u, v) = ([p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]], [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]]);
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let l = libm::sqrtf(n[0] * n[0] + n[1] * n[1] + n[2] * n[2]);
    if l.is_nan() || l <= 1e-9 || !l.is_finite() { return; }
    let n = [n[0] / l, n[1] / l, n[2] / l];
    let col = band_color(bands, (t[0][1] + t[1][1] + t[2][1]) / 3.0);
    let base = m.positions.len() as u32;
    for q in p { m.positions.push(q); m.normals.push(n); m.colors.push(col); }
    m.indices.extend_from_slice(&[base, base + 1, base + 2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::tests::check_consistent;

    fn bearing_gap_deg(layout: &[(CloudKind, u64, f32, [f64; 3])], r: f64) -> f64 {
        let mut b: Vec<f64> = layout.iter().filter(|c| (c.3[0] * c.3[0] + c.3[2] * c.3[2]).sqrt() > 0.8 * r)
            .map(|c| libm::atan2(c.3[2], c.3[0]).to_degrees().rem_euclid(360.0)).collect();
        b.sort_by(|a, c| a.partial_cmp(c).unwrap());
        let mut gap = 360.0 - b[b.len() - 1] + b[0];
        for w in b.windows(2) { gap = gap.max(w[1] - w[0]); }
        gap
    }

    #[test]
    fn clusters_are_white_with_wrap_terms_and_deterministic() {
        for kind in [CloudKind::Cumulus, CloudKind::StratusSheet, CloudKind::StormFront] {
            for seed in [0u64, 1, 77, u64::MAX] {
                let m = cloud_cluster(kind, seed, default_size(kind));
                check_consistent(&m);
                assert!(!m.is_empty());
                assert_eq!(m.custom0.len(), m.positions.len());
                assert!(m.colors.iter().all(|c| *c == [1.0; 4]));
                assert!(m.custom0.iter().all(|c| (0.2..=0.8).contains(&c[0]) && (0.0..=1.0).contains(&c[1])));
                assert_eq!(m, cloud_cluster(kind, seed, default_size(kind)));
                // Base at y = 0, nothing far below it; horizontal size about as asked.
                let bb = m.aabb();
                let s = default_size(kind);
                assert!(bb.min[1] > -0.05 * s, "{kind:?} hangs {}", bb.min[1]);
                assert!(bb.max[0] - bb.min[0] <= s * 1.15 && bb.max[0] - bb.min[0] >= s * 0.6, "{kind:?} width {}", bb.max[0] - bb.min[0]);
                let tris = m.triangle_count();
                assert!(tris <= 3_000, "{kind:?} {tris} triangles");
            }
            assert_ne!(cloud_cluster(kind, 1, 500.0), cloud_cluster(kind, 2, 500.0));
        }
        // Tops wrap more light than bases.
        let m = cloud_cluster(CloudKind::Cumulus, 5, 600.0);
        let (lo, hi): (Vec<_>, Vec<_>) = m.custom0.iter().partition(|c| c[1] < 0.5);
        let avg = |v: &[&[f32; 4]]| v.iter().map(|c| c[0]).sum::<f32>() / v.len() as f32;
        assert!(avg(&hi) > avg(&lo));
    }

    #[test]
    fn junk_cluster_sizes_fall_back() {
        for s in [f32::NAN, -5.0, 0.0, f32::INFINITY] {
            let m = cloud_cluster(CloudKind::Cumulus, 3, s);
            check_consistent(&m);
            assert!(!m.is_empty());
        }
        check_consistent(&cloud_cluster(CloudKind::StormFront, 3, 1e12));
    }

    #[test]
    fn layout_keeps_every_quarter_of_the_horizon_broken() {
        for seed in 0..200u64 {
            for cover in [0.0, 0.5, 1.0] {
                for storm in [false, true] {
                    let r = 25_000.0;
                    let l = cloud_layout(seed, cover, storm, r);
                    assert!(bearing_gap_deg(&l, r) < 90.0, "seed {seed} cover {cover} storm {storm}");
                    assert!(l.iter().any(|c| c.0 == CloudKind::StormFront), "a seeded front is always present");
                    if storm { assert!(l.iter().filter(|c| c.0 == CloudKind::StormFront).count() >= 3); }
                }
            }
        }
    }

    #[test]
    fn layout_density_follows_cover_and_is_deterministic() {
        let mut last = 0;
        for i in 0..=10 {
            let n = cloud_layout(9, i as f64 / 10.0, false, 20_000.0).len();
            assert!(n >= last);
            last = n;
        }
        assert!(cloud_layout(9, 1.0, false, 20_000.0).len() > cloud_layout(9, 0.0, false, 20_000.0).len() + 50);
        assert_eq!(cloud_layout(4, 0.6, true, 30_000.0), cloud_layout(4, 0.6, true, 30_000.0));
        assert_ne!(cloud_layout(4, 0.6, true, 30_000.0), cloud_layout(5, 0.6, true, 30_000.0));
        for (r, c) in [(f64::NAN, f64::NAN), (-1.0, 2.0), (1e12, -3.0)] {
            let l = cloud_layout(1, c, false, r);
            assert!(l.len() >= HORIZON_MASSES as usize && l.len() <= 200);
            assert!(l.iter().all(|e| e.2.is_finite() && e.2 > 0.0 && e.3.iter().all(|v| v.is_finite())));
        }
    }

    fn bands() -> IslandBands {
        IslandBands { band_top_m: vec![3.0, 15.0, 30.0], colors: vec![[0.24, 0.19, 0.13], [0.07, 0.09, 0.04], [0.03, 0.06, 0.03], [0.21, 0.22, 0.24]] }
    }

    #[test]
    fn islands_are_flat_shaded_up_facing_and_drop_the_sea_bed() {
        let hill = |x: f64, z: f64| 40.0 - (x * x + z * z).sqrt() * 0.5;
        let m = island_mesh(&hill, [1000.0, -500.0], [-1000.0 - 120.0, 500.0 - 120.0], [-1000.0 + 120.0, 500.0 + 120.0], 8.0, &bands());
        check_consistent(&m);
        assert!(m.custom0.is_empty());
        assert!(m.normals.iter().all(|n| n[1] > 0.0));
        // Cone of radius 82 m above y = -1 m: nothing fully below survives, and the cone is all there.
        for t in m.indices.chunks(3) { assert!(t.iter().any(|&i| m.positions[i as usize][1] >= -1.0)); }
        let bb = m.aabb();
        assert!(bb.max[1] > 39.0 && bb.max[1] <= 40.0);
        assert!(bb.min[0] < -1_080.0 && bb.max[0] > -920.0 && bb.min[0] > -1_120.0);
        // The summit is in the top band's colour, the shore in the first.
        let top = m.positions.iter().position(|p| p[1] > 39.0).unwrap();
        assert_eq!(m.colors[top], [0.21, 0.22, 0.24, 1.0]);
        assert!(m.colors.contains(&[0.24, 0.19, 0.13, 1.0]));
        // A flat sea bed yields nothing.
        assert!(island_mesh(&|_, _| -30.0, [0.0; 2], [0.0; 2], [500.0; 2], 32.0, &bands()).is_empty());
    }

    #[test]
    fn island_lattice_is_world_anchored_and_shared_between_tiles() {
        let f = |x: f64, z: f64| 35.0 + libm::sin(x * 0.01) * 20.0 + libm::cos(z * 0.013) * 10.0;
        let a = island_mesh(&f, [100.0, 100.0], [0.0, 0.0], [64.0, 64.0], 8.0, &bands());
        let b = island_mesh(&f, [100.0, 100.0], [64.0, 0.0], [128.0, 64.0], 8.0, &bands());
        let edge = |m: &MeshData| { let mut v: Vec<(i64, i64)> = m.positions.iter().filter(|p| (p[0] - 64.0).abs() < 1e-3).map(|p| ((p[2] * 1e3) as i64, (p[1] * 1e3) as i64)).collect(); v.sort(); v.dedup(); v };
        assert_eq!(edge(&a), edge(&b));
        // Interior lattice points lie on world multiples of the edge.
        let c = island_mesh(&f, [3.0, 5.0], [0.0, 0.0], [40.0, 40.0], 8.0, &bands());
        for p in &c.positions {
            let wx = p[0] as f64 + 3.0;
            let on = (wx / 8.0 - libm::round(wx / 8.0)).abs() < 1e-3 || p[0].abs() < 1e-3 || (p[0] - 40.0).abs() < 1e-3;
            assert!(on, "x {}", p[0]);
        }
        // A 32 m tile's points are a subset of the 8 m lattice.
        let far = island_mesh(&f, [100.0, 100.0], [0.0, 0.0], [64.0, 64.0], 32.0, &bands());
        assert!(far.triangle_count() * 5 < a.triangle_count());
    }

    #[test]
    fn island_input_junk_never_panics() {
        let calls = core::cell::Cell::new(0usize);
        let f = |x: f64, _z: f64| { calls.set(calls.get() + 1); if x > 50.0 { f64::NAN } else { 10.0 } };
        let empty = IslandBands { band_top_m: vec![], colors: vec![] };
        let m = island_mesh(&f, [0.0; 2], [100.0, 100.0], [0.0, 0.0], f64::NAN, &empty);
        check_consistent(&m);
        assert!(m.colors.iter().all(|c| *c == [0.15, 0.15, 0.15, 1.0]));
        // Huge squares and tiny edges stay bounded.
        calls.set(0);
        let g = |_x: f64, _z: f64| { calls.set(calls.get() + 1); -30.0 };
        assert!(island_mesh(&g, [0.0; 2], [0.0; 2], [1e7, 1e7], 1e-9, &bands()).is_empty());
        assert!(calls.get() <= 515 * 515, "{} samples", calls.get());
        assert!(island_mesh(&|_, _| 1.0, [f64::NAN, 0.0], [0.0; 2], [10.0; 2], 8.0, &bands()).is_empty());
        assert!(island_mesh(&|_, _| 1.0, [0.0; 2], [5.0; 2], [5.0; 2], 8.0, &bands()).is_empty());
    }
}
