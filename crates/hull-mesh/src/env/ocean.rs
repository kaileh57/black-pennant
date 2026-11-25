//! Unit `hm-ocean`. The ocean geometry clipmap. DESIGN.md L1895: Rust geometry clipmap; levels snap to 2s in
//! f64; morph over the outer n/10 band; skirt to the far plane. Table L1897-1901: Low 8 levels, n 63, s0 1.0 m,
//! +-4 km, 48k triangles (sag <= 7.7% of A on 8 m waves); Medium 10 levels, n 63, s0 0.5 m, +-8 km, 55k;
//! High 9 levels, n 127, s0 0.5 m, +-8 km, 222k. L1887 (Nyquist fade A x smoothstep(2s, 4s, lambda) is the only
//! allowed reduction; the shader does it from `custom0`). L1873 (camera far >= 20,000 m: the skirt reaches it).
//! Spike S1 fallback (L2053): next-lower clipmap. The water shader displaces vertices with the wave include.
//!
//! # Layout
//! Level `l` has spacing `s = s0 * 2^l` and is a grid of `q = n - 1` quads per side (62 or 126) spanning grid
//! indices `-(q/2 - 1) ..= q/2 + 1` on both axes, so a level spans `-30 s .. +32 s` for n = 63 around its
//! snapped centre. Centres snap with `floor` to multiples of `2 s` (computed in integers, `k_l = k_0 >> l`), so a
//! finer level always sits at offset 0 or `+s_coarse` per axis inside the next coarser one. The coarser ring's
//! hole is the intersection of those two footprints; the finer level therefore covers the hole exactly on one
//! side of each axis and overlaps the ring by one coarse cell on the other. That overlap strip lies entirely in
//! the finer level's fully morphed band, where its triangles coincide with the coarse ring's triangles (same
//! corners, same diagonal), so no trim mesh moves per frame and no crack opens. The cost is `2 (q - 1)` extra
//! triangles per ring over Hoppe's moving L-trim, about 1-2% (the table's Low and High counts are Hoppe's).
//!
//! # Vertex data
//! Positions are level-local metres (`x = i s`, `y = 0`, `z = j s`); the client places level `l` at
//! `snap_origins(..)[l]` minus the render origin. Normals are +Y. `custom0` = (s, w, l, skirt):
//! - `w` = 1 within 2 cells of the level's outer edge, ramping linearly to 0 at `n / 10` cells in; 0 elsewhere.
//! - Seam rule the water shader must follow: the morphed vertex is `p.xz - mod(p.xz / s, 2) * s * w` (odd grid
//!   lines slide toward the lower even line; level-local parity equals world parity because centres are
//!   multiples of 2s), and every spacing-dependent term (the Nyquist fade `smoothstep(2 s', 4 s', lambda)`) uses
//!   the effective spacing `s' = s * (1 + w)`. At w = 1 a vertex then lands exactly where the coarser level puts
//!   its own vertex and evaluates the identical wave sum, which is what hides the overlap and the T-junctions.
//! - Skirt vertices carry `(s_outer, 1, levels - 1, 1)`: never morph-snapped, faded at `2 s_outer`.
//!
//! Triangles are clockwise seen from above (front face +Y, Godot's convention) with one uniform diagonal, so a
//! fully morphed fine quad reproduces the coarse quad's triangulation.
use crate::types::{MeshData, QualityTier};

/// Clipmap parameters of one tier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipmapSpec { pub levels: u8, pub n: u16, pub s0_m: f64, pub extent_m: f64 }
impl ClipmapSpec {
    /// Table L1897-1901. Fallback uses Low.
    pub fn for_tier(tier: QualityTier) -> ClipmapSpec {
        match tier {
            QualityTier::Fallback | QualityTier::Low => ClipmapSpec { levels: 8, n: 63, s0_m: 1.0, extent_m: 4_000.0 },
            QualityTier::Medium => ClipmapSpec { levels: 10, n: 63, s0_m: 0.5, extent_m: 8_000.0 },
            QualityTier::High => ClipmapSpec { levels: 9, n: 127, s0_m: 0.5, extent_m: 8_000.0 },
        }
    }
    /// Grid spacing of level `l`: s0 * 2^l.
    pub fn spacing(&self, level: u8) -> f64 { self.s0_m * (1u64 << level) as f64 }
}

/// Smallest camera far plane the skirt reaches (L1873).
pub const MIN_FAR_M: f64 = 20_000.0;
/// Most levels a spec may ask for; more are clamped (2^24 x s0 is far beyond any far plane).
const MAX_LEVELS: u8 = 24;
/// Smallest grid (quads per side) a malformed spec is raised to.
const MIN_QUADS: i64 = 14;
/// Width of the fully morphed outer strip, in the level's own cells: the one coarse cell the finer level may
/// overlap its coarser ring by.
const FULL_MORPH_CELLS: f64 = 2.0;
/// The skirt reaches this many far-plane distances (the view-axis far plane seen at the screen corners).
const FAR_REACH: f64 = 3.0;
/// Rings of the skirt between the outer level and the far plane.
const SKIRT_RINGS: usize = 4;

/// The sanitised geometry every function here works from, so a malformed public `ClipmapSpec` never panics.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Grid {
    levels: u8,
    /// Quads per side, `q mod 4 == 2`.
    q: i64,
    s0: f64,
    /// Morph band width in cells (n / 10, kept between the full-morph strip and the ring width).
    band: f64,
}
impl Grid {
    fn of(spec: &ClipmapSpec) -> Grid {
        let levels = spec.levels.clamp(1, MAX_LEVELS);
        let raw = (spec.n as i64 - 1).max(MIN_QUADS);
        let q = ((raw - 2) / 4) * 4 + 2;
        let s0 = if spec.s0_m.is_finite() && spec.s0_m > 0.0 { spec.s0_m } else { 1.0 };
        let half = (q / 2 + 1) as f64; // ring width in cells is half of this
        let band = (spec.n as f64 / 10.0).max(FULL_MORPH_CELLS + 1.0).min(half / 2.0 - 1.0);
        Grid { levels, q, s0, band }
    }
    /// Lowest grid index of a level (both axes).
    fn lo(&self) -> i64 { -(self.q / 2 - 1) }
    /// Highest grid index of a level (both axes).
    fn hi(&self) -> i64 { self.q / 2 + 1 }
    /// Hole of a ring in its own grid indices, `[hole_lo, hole_hi]`: the finer level's guaranteed footprint.
    fn hole(&self) -> (i64, i64) { let b = self.hi(); (-(b - 4) / 2, b / 2) }
    fn spacing(&self, level: u8) -> f64 { self.s0 * (1u64 << level.min(MAX_LEVELS)) as f64 }
    /// Morph weight of a vertex at grid index (i, j).
    fn morph(&self, i: i64, j: i64) -> f32 {
        let d = (i - self.lo()).min(self.hi() - i).min(j - self.lo()).min(self.hi() - j) as f64;
        if d <= FULL_MORPH_CELLS { return 1.0; }
        (1.0 - (d - FULL_MORPH_CELLS) / (self.band - FULL_MORPH_CELLS)).clamp(0.0, 1.0) as f32
    }
    fn level_triangles(&self, level: u8) -> usize {
        let full = (self.q * self.q) as usize;
        let (a, b) = self.hole();
        let hole = ((b - a) * (b - a)) as usize;
        2 * if level == 0 { full } else { full - hole }
    }
}

/// Push the two triangles of quad (v00, v10, v01, v11) (x right, z down the page), clockwise from +Y.
fn quad(indices: &mut Vec<u32>, v00: u32, v10: u32, v01: u32, v11: u32) {
    indices.extend_from_slice(&[v00, v10, v01, v10, v11, v01]);
}

/// One clipmap level's mesh in level-local metres (centred on the level's snapped origin, y = 0). `custom0` per
/// vertex: x = level spacing s, y = morph factor weight base (0 inside, ramps to 1 across the outer n/10 band),
/// z = level index, w = 1 on skirt vertices. Level 0 is a filled square; others are rings. Unit: `hm-ocean`.
///
/// A level at or beyond `spec.levels` is empty. See the module docs for the seam rule.
pub fn level_mesh(spec: &ClipmapSpec, level: u8) -> MeshData {
    let g = Grid::of(spec);
    let mut m = MeshData::new();
    if level >= g.levels { return m; }
    let s = g.spacing(level);
    let (lo, hi) = (g.lo(), g.hi());
    let (h0, h1) = g.hole();
    let ring = level > 0;
    let side = (hi - lo + 1) as usize;
    // Vertex strictly inside the hole: unused by a ring.
    let inside = |i: i64, j: i64| ring && i > h0 && i < h1 && j > h0 && j < h1;
    let mut index = vec![u32::MAX; side * side];
    let cap = side * side;
    m.positions.reserve(cap);
    m.normals.reserve(cap);
    m.custom0.reserve(cap);
    for j in lo..=hi {
        for i in lo..=hi {
            if inside(i, j) { continue; }
            index[(i - lo) as usize + side * (j - lo) as usize] = m.positions.len() as u32;
            m.positions.push([(i as f64 * s) as f32, 0.0, (j as f64 * s) as f32]);
            m.normals.push([0.0, 1.0, 0.0]);
            m.custom0.push([s as f32, g.morph(i, j), level as f32, 0.0]);
        }
    }
    m.indices.reserve(3 * g.level_triangles(level));
    let at = |i: i64, j: i64| index[(i - lo) as usize + side * (j - lo) as usize];
    for j in lo..hi {
        for i in lo..hi {
            if ring && i >= h0 && i < h1 && j >= h0 && j < h1 { continue; }
            quad(&mut m.indices, at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
        }
    }
    m
}

/// The skirt from the outermost ring to `far_m` (>= 20,000 m). Unit: `hm-ocean`.
///
/// In the outermost level's local frame (place it at `snap_origins(..)[levels - 1]`). Its inner loop is exactly
/// the outer level's even boundary vertices (where that level is fully morphed), and its outer loop is a square
/// reaching `far_m` (raised to [`MIN_FAR_M`], non-finite treated as the minimum) plus the camera's possible
/// offset from the snapped centre, times [`FAR_REACH`] because the far clip is a plane and the screen corners see
/// past `far_m`, so the camera never sees past the water. Rings between are spaced
/// geometrically.
pub fn skirt_mesh(spec: &ClipmapSpec, far_m: f64) -> MeshData {
    let g = Grid::of(spec);
    let outer_level = g.levels - 1;
    let s = g.spacing(outer_level);
    let far = if far_m.is_finite() { far_m.clamp(MIN_FAR_M, 1.0e7) } else { MIN_FAR_M };
    let (lo, hi) = (g.lo(), g.hi());
    // Even boundary indices, walked once around the square.
    let mut inner: Vec<[f64; 2]> = Vec::new();
    let mut i = lo;
    while i < hi { inner.push([i as f64, lo as f64]); i += 2; }
    let mut j = lo;
    while j < hi { inner.push([hi as f64, j as f64]); j += 2; }
    let mut i = hi;
    while i > lo { inner.push([i as f64, hi as f64]); i -= 2; }
    let mut j = hi;
    while j > lo { inner.push([lo as f64, j as f64]); j -= 2; }
    let centre = (lo + hi) as f64 / 2.0;
    let inner_half = (hi - lo) as f64 / 2.0 * s;
    // The far clip is a plane, so water at angle theta off the view axis stays visible to far / cos(theta): about
    // 2.1 x far at the corner of a 21:9 screen. Reach 3 x far (overshoot is clipped for free), plus the camera's
    // offset from the square's centre (within 2s of the snapped centre per axis, which is s off the square's).
    let outer_half = FAR_REACH * far + 4.0 * s;
    let ratio = (outer_half / inner_half).max(1.0);
    let mut m = MeshData::new();
    let loop_len = inner.len();
    for r in 0..=SKIRT_RINGS {
        let scale = libm::pow(ratio, r as f64 / SKIRT_RINGS as f64);
        let scale = if r == 0 { 1.0 } else { scale };
        for p in &inner {
            let x = (centre + (p[0] - centre) * scale) * s;
            let z = (centre + (p[1] - centre) * scale) * s;
            m.positions.push([x as f32, 0.0, z as f32]);
            m.normals.push([0.0, 1.0, 0.0]);
            m.custom0.push([s as f32, 1.0, outer_level as f32, 1.0]);
        }
    }
    for r in 0..SKIRT_RINGS {
        for k in 0..loop_len {
            let k1 = (k + 1) % loop_len;
            let (a, b) = ((r * loop_len + k) as u32, (r * loop_len + k1) as u32);
            let (c, d) = (((r + 1) * loop_len + k1) as u32, ((r + 1) * loop_len + k) as u32);
            for tri in [[a, b, c], [a, c, d]] {
                let (p0, p1, p2) = (m.positions[tri[0] as usize], m.positions[tri[1] as usize], m.positions[tri[2] as usize]);
                // Front normal (c - a) x (b - a) must point up: its y is (c-a).z (b-a).x - (c-a).x (b-a).z.
                let ny = (p2[2] - p0[2]) * (p1[0] - p0[0]) - (p2[0] - p0[0]) * (p1[2] - p0[2]);
                if ny >= 0.0 { m.indices.extend_from_slice(&tri); } else { m.indices.extend_from_slice(&[tri[0], tri[2], tri[1]]); }
            }
        }
    }
    m
}

/// Snapped centre of every level for a camera at world (x, z) in f64: each level snaps to multiples of
/// 2 x its spacing (L1895). The client subtracts the render origin before narrowing to f32. Unit: `hm-ocean`.
///
/// Snapping is `floor` in integer grid units of level 0 (`k_l = k_0 >> l`), which guarantees the nesting the
/// ring holes rely on: each finer centre minus the next coarser one is 0 or the coarser spacing, per axis. A
/// non-finite camera coordinate is treated as 0.
pub fn snap_origins(spec: &ClipmapSpec, camera_x: f64, camera_z: f64) -> Vec<[f64; 2]> {
    let g = Grid::of(spec);
    let k0 = |c: f64| -> i64 {
        let c = if c.is_finite() { c } else { 0.0 };
        libm::floor(c / (2.0 * g.s0)).clamp(-4.0e15, 4.0e15) as i64
    };
    let (kx, kz) = (k0(camera_x), k0(camera_z));
    (0..g.levels)
        .map(|l| {
            let step = 2.0 * g.spacing(l);
            [(kx >> l) as f64 * step, (kz >> l) as f64 * step]
        })
        .collect()
}

/// Total triangles of a tier's clipmap without the skirt (tests against the table). Unit: `hm-ocean`.
pub fn triangle_count(spec: &ClipmapSpec) -> usize {
    let g = Grid::of(spec);
    (0..g.levels).map(|l| g.level_triangles(l)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiers() -> [(QualityTier, ClipmapSpec); 3] {
        [QualityTier::Low, QualityTier::Medium, QualityTier::High].map(|t| (t, ClipmapSpec::for_tier(t)))
    }

    fn up_facing(m: &MeshData) {
        for t in m.indices.chunks(3) {
            let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
            let u = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let v = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            assert!(n[1] > 0.0 && n[0] == 0.0 && n[2] == 0.0, "triangle not facing +Y: {a:?} {b:?} {c:?}");
        }
    }

    #[test]
    fn triangle_counts_match_the_table() {
        // Low and High match the table (Hoppe's counts) within the static overlap strip's ~2%.
        let low = triangle_count(&ClipmapSpec::for_tier(QualityTier::Low));
        let high = triangle_count(&ClipmapSpec::for_tier(QualityTier::High));
        assert_eq!(low, 48_904);
        assert_eq!(high, 224_264);
        assert!((low as f64 / 48_000.0 - 1.0).abs() < 0.02, "{low}");
        assert!((high as f64 / 222_000.0 - 1.0).abs() < 0.02, "{high}");
        // Medium: 10 levels of n = 63 is 60,680 here (59,582 with Hoppe's trim); the table's 55k matches neither
        // and is reported to the owner as a spec discrepancy rather than hidden by a tolerance.
        assert_eq!(triangle_count(&ClipmapSpec::for_tier(QualityTier::Medium)), 60_680);
        assert_eq!(ClipmapSpec::for_tier(QualityTier::Fallback), ClipmapSpec::for_tier(QualityTier::Low));
    }

    #[test]
    fn meshes_match_the_count_and_face_up() {
        for (_, spec) in tiers() {
            let mut total = 0;
            for l in 0..spec.levels {
                let m = level_mesh(&spec, l);
                assert_eq!(m.normals.len(), m.positions.len());
                assert_eq!(m.custom0.len(), m.positions.len());
                assert!(m.colors.is_empty());
                assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
                if l == 0 { assert_eq!(m.positions.len(), (spec.n as usize).pow(2)); }
                total += m.triangle_count();
            }
            assert_eq!(total, triangle_count(&spec));
            assert!(level_mesh(&spec, spec.levels).is_empty());
        }
        let spec = ClipmapSpec::for_tier(QualityTier::Low);
        up_facing(&level_mesh(&spec, 0));
        up_facing(&level_mesh(&spec, 3));
        up_facing(&skirt_mesh(&spec, 20_000.0));
    }

    #[test]
    fn extents_match_the_table() {
        for (_, spec) in tiers() {
            let outer = level_mesh(&spec, spec.levels - 1).aabb();
            let half = (outer.max[0] - outer.min[0]) as f64 / 2.0;
            assert!((half / spec.extent_m - 1.0).abs() < 0.03, "half extent {half} vs {}", spec.extent_m);
        }
    }

    #[test]
    fn low_sag_on_8_m_waves() {
        // Linear interpolation of a sine over spacing s sags by A (1 - cos(pi s / lambda)) at worst.
        let s = ClipmapSpec::for_tier(QualityTier::Low).s0_m;
        let sag = 1.0 - libm::cos(core::f64::consts::PI * s / 8.0);
        assert!(sag <= 0.077, "{sag}");
    }

    #[test]
    fn snapping_nests_every_level() {
        let spec = ClipmapSpec::for_tier(QualityTier::Medium);
        let cams = [(0.0, 0.0), (0.3, -0.3), (1234.567, -9876.5), (-0.0001, 7.99), (49_999.9, -50_000.1), (f64::NAN, 3.0)];
        for (x, z) in cams {
            let o = snap_origins(&spec, x, z);
            assert_eq!(o.len(), spec.levels as usize);
            for l in 0..spec.levels as usize {
                let s = spec.spacing(l as u8);
                for a in 0..2 {
                    assert_eq!(libm::fmod(o[l][a], 2.0 * s), 0.0);
                    let cam = if a == 0 { if x.is_finite() { x } else { 0.0 } } else { z };
                    assert!(cam - o[l][a] >= 0.0 && cam - o[l][a] < 2.0 * s, "level {l} centre {} cam {cam}", o[l][a]);
                    if l > 0 {
                        let d = o[l - 1][a] - o[l][a];
                        assert!(d == 0.0 || d == s, "level {l} offset {d}");
                    }
                }
            }
        }
    }

    /// For both possible offsets of a finer level inside its coarser ring, the finer footprint covers the hole
    /// with no gap, overlaps it only inside its fully morphed band, and its fully morphed boundary lies on the
    /// coarse grid.
    #[test]
    fn rings_nest_without_gaps() {
        for (_, spec) in tiers() {
            let g = Grid::of(&spec);
            let (h0, h1) = g.hole();
            // Fine footprint in coarse cells at offset 0 and 1.
            for off in [0i64, 1] {
                let f0 = g.lo() / 2 + off;
                let f1 = g.hi() / 2 + off;
                assert!(f0 <= h0 && f1 >= h1, "gap: fine {f0}..{f1} hole {h0}..{h1}");
                // Overlap is at most one coarse cell = FULL_MORPH_CELLS fine cells.
                assert!(h0 - f0 <= 1 && f1 - h1 <= 1 && (h0 - f0) + (f1 - h1) == 1);
            }
            assert_eq!(g.lo() % 2, 0);
            assert_eq!(g.hi() % 2, 0);
            // Every vertex within the overlap strip is fully morphed; the hole's own edge is unmorphed.
            let m = level_mesh(&spec, 0);
            let s0 = g.s0 as f32;
            for (p, c) in m.positions.iter().zip(&m.custom0) {
                let (i, j) = ((p[0] / s0).round() as i64, (p[2] / s0).round() as i64);
                let d = (i - g.lo()).min(g.hi() - i).min(j - g.lo()).min(g.hi() - j);
                if d <= 2 { assert_eq!(c[1], 1.0); }
                if d as f64 >= g.band { assert_eq!(c[1], 0.0); }
            }
            let ring = level_mesh(&spec, 1);
            let s1 = g.spacing(1) as f32;
            for (p, c) in ring.positions.iter().zip(&ring.custom0) {
                let (i, j) = ((p[0] / s1).round() as i64, (p[2] / s1).round() as i64);
                if (i == h0 || i == h1 || j == h0 || j == h1) && i >= h0 && i <= h1 && j >= h0 && j <= h1 { assert_eq!(c[1], 0.0); }
                assert_eq!(c[0], s1);
                assert_eq!(c[2], 1.0);
            }
        }
    }

    /// A fully morphed fine quad reproduces the coarse quad's two corners and diagonal exactly.
    #[test]
    fn morph_reproduces_coarse_triangles() {
        let morph = |x: f32, s: f32| x - (x / s).rem_euclid(2.0) * s;
        let s = 0.5f32;
        // Fine quad (1,1) of the coarse cell [0, 2s]^2 maps to the whole coarse quad with the same diagonal.
        let fine = [[s, s], [2.0 * s, s], [s, 2.0 * s], [2.0 * s, 2.0 * s]].map(|p| [morph(p[0], s), morph(p[1], s)]);
        assert_eq!(fine, [[0.0, 0.0], [2.0 * s, 0.0], [0.0, 2.0 * s], [2.0 * s, 2.0 * s]]);
        // Negative coordinates slide to the lower even line too.
        assert_eq!(morph(-29.0 * s, s), -30.0 * s);
        assert_eq!(morph(-30.0 * s, s), -30.0 * s);
    }

    #[test]
    fn skirt_joins_the_outer_level_and_reaches_far() {
        for (_, spec) in tiers() {
            let g = Grid::of(&spec);
            let outer = level_mesh(&spec, spec.levels - 1);
            let skirt = skirt_mesh(&spec, 25_000.0);
            let loop_len = skirt.positions.len() / (SKIRT_RINGS + 1);
            assert_eq!(loop_len as i64, 2 * g.q);
            for p in &skirt.positions[..loop_len] {
                let k = outer.positions.iter().position(|q| q == p).expect("skirt vertex not on the outer level");
                assert_eq!(outer.custom0[k][1], 1.0, "skirt joins an unmorphed vertex");
            }
            // Any point at far / cos(60 deg) = 2 far from any camera in the snap cell lies inside the skirt.
            let b = skirt.aabb();
            let s = g.spacing(g.levels - 1) as f32;
            for cam in [[0.0f32, 0.0], [1.99 * s, 0.0], [0.0, 1.99 * s], [1.99 * s, 1.99 * s]] {
                for k in 0..16 {
                    let a = core::f32::consts::TAU * k as f32 / 16.0;
                    let p = [cam[0] + 50_000.0 * libm::cosf(a), cam[1] + 50_000.0 * libm::sinf(a)];
                    assert!(p[0] > b.min[0] && p[0] < b.max[0] && p[1] > b.min[2] && p[1] < b.max[2], "{p:?} outside {b:?}");
                }
            }
            assert!(skirt.custom0.iter().all(|c| c[1] == 1.0 && c[3] == 1.0));
            assert_eq!(skirt.triangle_count(), 2 * SKIRT_RINGS * loop_len);
            up_facing(&skirt);
            // A too-near or NaN far plane is raised to the minimum.
            let small = skirt_mesh(&spec, f64::NAN).aabb();
            assert!(small.max[0] as f64 >= MIN_FAR_M);
        }
    }

    #[test]
    fn malformed_specs_do_not_panic() {
        let bad = [
            ClipmapSpec { levels: 0, n: 0, s0_m: f64::NAN, extent_m: 0.0 },
            ClipmapSpec { levels: 255, n: 64, s0_m: -1.0, extent_m: 0.0 },
            ClipmapSpec { levels: 3, n: 17, s0_m: 2.0, extent_m: 0.0 },
        ];
        for spec in bad {
            let g = Grid::of(&spec);
            assert_eq!(g.q % 4, 2);
            for l in 0..g.levels.min(3) {
                let m = level_mesh(&spec, l);
                assert_eq!(m.triangle_count(), g.level_triangles(l));
            }
            assert_eq!(snap_origins(&spec, f64::INFINITY, -1e300).len(), g.levels as usize);
            assert!(!skirt_mesh(&spec, -5.0).is_empty());
            let _ = triangle_count(&spec);
        }
    }
}
