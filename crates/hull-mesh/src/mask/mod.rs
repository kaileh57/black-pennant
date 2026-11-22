//! Unit `hm-mask`. The hull mask layer the water shader reads to hide water inside hulls and draw contact foam.
//! DESIGN.md L1909: up to 8 hulls within 500 m; an RGBA16F layer per hull (dry-column bottom, top, waterline
//! SDF), 512x128 texels in hull XZ at 0.5 m, a 4 MB Texture2DArray plus world-to-hull mat4s. Water discards
//! inside the column. Rebuild on cell change, at most every 100 ms. Flooded compartments draw their own water.
//! L1907 (contact foam reads the waterline SDF).
//!
//! Texel (u, v) covers hull-local x = u * 0.5 m (bow +X), z = (v - 64) * 0.5 m + centreline (starboard +Z), so
//! a 90 m beam-centred hull fits; hulls longer than 256 m are a caller error. Values are vessel-local metres.
//!
//! # Rules
//! - Texels are cell columns: texel (u, v) is cell column x = u, z = v - 64 + size_z / 2 (integer division), so
//!   [`HullMask::offset_z_m`] is `(size_z / 2) * 0.5` m. Columns outside the grid, or cells outside the texture
//!   (beams over 64 m), have no hull.
//! - Dry cells: a solid cell (removed cells are not solid), and air inside the hull. On a live vessel (a non-empty
//!   sealed-air slice) air inside the hull is sealed air; air of a breached or entered compartment is open. A view
//!   with no sealed slice (exterior-only forms, editor designs) carries no compartment state, so an empty cell
//!   with solid cells below and above it in its column counts as dry.
//! - A column's dry interior is the run of consecutive dry cells that holds the design waterline row, else the
//!   run nearest to it (ties to the upper run). R is its bottom face, G its top face, in metres. A breach below
//!   the waterline therefore lowers G or raises R past the waterline, and the sea shows inside.
//! - The waterline outline is the waterline row's solid cells plus the air they enclose in that slice (found by
//!   a flood fill from the texture border). B is the signed Euclidean distance from the texel centre to that
//!   outline in metres, from an exact distance transform: negative inside, `+65504` (the largest half) when the
//!   row holds no hull.
//! - "Large" is the largest finite half, 65504, so the shader never meets infinities.
use crate::chunk::{validate_input, Cells, MeshInput};
use crate::types::CELL_M_F32;

pub const MASK_W: usize = 512;
pub const MASK_H: usize = 128;
pub const MASK_TEXEL_M: f64 = 0.5;
/// Hulls masked at once and their range (L1909).
pub const MAX_MASKED_HULLS: usize = 8;
pub const MASK_RANGE_M: f64 = 500.0;
pub const MASK_REBUILD_MIN_S: f64 = 0.1;
/// The "large" value of empty columns and hull-less outlines: the largest finite binary16 value.
pub const MASK_LARGE: f32 = 65_504.0;

/// One hull's mask layer: RGBA16F as raw IEEE half bits, row-major (u fastest), `MASK_W * MASK_H` texels.
/// R: lowest y of the column's dry interior (hull bottom where the column is enclosed), G: highest y of it,
/// B: signed XZ distance to the hull outline at the design waterline (negative inside), A: 1 where the column
/// holds hull, else 0. Columns with no hull: R = +large, G = -large, A = 0.
#[derive(Clone, Debug, PartialEq)]
pub struct HullMask {
    pub texels: Vec<[u16; 4]>,
    /// Vessel-local z in metres where texel row v = 64 begins: row v spans `offset_z_m + (v - 64) * 0.5` to
    /// `offset_z_m + (v - 63) * 0.5`. It is `(size_z / 2) * 0.5` with integer division, the cell boundary on or
    /// just below the centreline, so texels line up with cells (an odd beam sits 0.25 m off centre).
    pub offset_z_m: f32,
}

/// Build the mask from the current cells (removed cells and breached compartments open the column). Unit: `hm-mask`.
///
/// A malformed view (see `chunk::validate_input`) yields a mask with no hull.
pub fn build_hull_mask(input: &MeshInput<'_>) -> HullMask {
    let size = input.view.size;
    let half_z = (size[2] / 2) as i32;
    let offset_z_m = half_z as f32 * CELL_M_F32;
    let empty_texel = [f32_to_f16_bits(MASK_LARGE), f32_to_f16_bits(-MASK_LARGE), f32_to_f16_bits(MASK_LARGE), 0];
    let mut texels = vec![empty_texel; MASK_W * MASK_H];
    if validate_input(input).is_err() {
        return HullMask { texels, offset_z_m };
    }
    let g = Cells::new(input.view);
    let live = !input.view.sealed_air.is_empty();
    let wl = input.design_waterline_y as i32;
    let one = f32_to_f16_bits(1.0);
    let mut col = Vec::with_capacity(g.s[1].max(0) as usize);
    // Waterline slice occupancy per texel, for the outline.
    let mut wl_solid = vec![false; MASK_W * MASK_H];
    for v in 0..MASK_H {
        let z = v as i32 - 64 + half_z;
        if z < 0 || z >= g.s[2] {
            continue;
        }
        for u in 0..MASK_W.min(g.s[0].max(0) as usize) {
            let x = u as i32;
            col.clear();
            let mut any = false;
            for y in 0..g.s[1] {
                let Some(i) = g.index(x, y, z) else { break };
                let c = if g.solid(i) {
                    any = true;
                    if y == wl {
                        wl_solid[u + MASK_W * v] = true;
                    }
                    Dry::Yes
                } else if g.has_material(i) {
                    // Removed cell: always open.
                    Dry::No
                } else if live {
                    if g.sealed(i) { Dry::Yes } else { Dry::No }
                } else {
                    Dry::IfBetween
                };
                col.push(c);
            }
            if !any {
                continue;
            }
            if let Some((lo, hi)) = dry_run(&mut col, wl) {
                let t = &mut texels[u + MASK_W * v];
                t[0] = f32_to_f16_bits(lo as f32 * CELL_M_F32);
                t[1] = f32_to_f16_bits((hi + 1) as f32 * CELL_M_F32);
                t[3] = one;
            }
        }
    }
    let sdf = waterline_sdf(&wl_solid);
    for (t, d) in texels.iter_mut().zip(sdf) {
        t[2] = f32_to_f16_bits(d);
    }
    HullMask { texels, offset_z_m }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dry {
    Yes,
    No,
    /// Air of a view without compartment state: dry when the column has dry cells below and above it.
    IfBetween,
}

/// Resolve `IfBetween` cells, then pick the dry run holding row `wl` or nearest to it (ties to the upper run).
/// Returns the run's lowest and highest row.
fn dry_run(col: &mut [Dry], wl: i32) -> Option<(i32, i32)> {
    let n = col.len();
    let mut y = 0;
    while y < n {
        if col[y] != Dry::IfBetween {
            y += 1;
            continue;
        }
        let start = y;
        while y < n && col[y] == Dry::IfBetween {
            y += 1;
        }
        let bounded = start > 0 && col[start - 1] == Dry::Yes && y < n && col[y] == Dry::Yes;
        for c in &mut col[start..y] {
            *c = if bounded { Dry::Yes } else { Dry::No };
        }
    }
    let mut best: Option<(i32, i32, i32)> = None;
    let mut y = 0;
    while y < n {
        if col[y] != Dry::Yes {
            y += 1;
            continue;
        }
        let lo = y as i32;
        while y < n && col[y] == Dry::Yes {
            y += 1;
        }
        let hi = y as i32 - 1;
        let gap = if wl < lo { lo - wl } else if wl > hi { wl - hi } else { 0 };
        // Runs come bottom-up, so `<=` lets an equally near upper run win the tie.
        if best.is_none_or(|(_, _, g)| gap <= g) {
            best = Some((lo, hi, gap));
        }
    }
    best.map(|(lo, hi, _)| (lo, hi))
}

/// Signed distance in metres from each texel centre to the waterline outline: the solid texels plus the air they
/// enclose. Negative inside; `MASK_LARGE` everywhere when nothing is inside.
fn waterline_sdf(solid: &[bool]) -> Vec<f32> {
    let n = MASK_W * MASK_H;
    // Flood the outside from every non-solid border texel through non-solid texels (4-connected).
    let mut outside = vec![false; n];
    let mut stack = Vec::new();
    for u in 0..MASK_W {
        for v in [0, MASK_H - 1] {
            stack.push(u + MASK_W * v);
        }
    }
    for v in 0..MASK_H {
        for u in [0, MASK_W - 1] {
            stack.push(u + MASK_W * v);
        }
    }
    while let Some(i) = stack.pop() {
        if outside[i] || solid[i] {
            continue;
        }
        outside[i] = true;
        let (u, v) = (i % MASK_W, i / MASK_W);
        if u > 0 { stack.push(i - 1); }
        if u + 1 < MASK_W { stack.push(i + 1); }
        if v > 0 { stack.push(i - MASK_W); }
        if v + 1 < MASK_H { stack.push(i + MASK_W); }
    }
    if outside.iter().all(|&o| o) {
        return vec![MASK_LARGE; n];
    }
    // Squared distances (in texels) to the nearest inside texel and to the nearest outside texel.
    let to_inside = edt(&outside.iter().map(|&o| !o).collect::<Vec<_>>());
    let to_outside = edt(&outside);
    let texel = MASK_TEXEL_M;
    (0..n)
        .map(|i| {
            // Centre-to-centre distance less half a texel puts the zero crossing on the texel boundary.
            let d = if outside[i] { libm::sqrt(to_inside[i]) - 0.5 } else { -(libm::sqrt(to_outside[i]) - 0.5) };
            ((d * texel) as f32).clamp(-MASK_LARGE, MASK_LARGE)
        })
        .collect()
}

/// Squared Euclidean distance transform (Felzenszwalb and Huttenlocher) of the `MASK_W x MASK_H` grid to the
/// texels where `feature` is set. Texels with no feature anywhere get a huge finite value.
fn edt(feature: &[bool]) -> Vec<f64> {
    // Far above any real squared distance (512^2 + 128^2), small enough that adding q^2 stays exact.
    const FAR: f64 = 1e12;
    let mut grid: Vec<f64> = feature.iter().map(|&f| if f { 0.0 } else { FAR }).collect();
    let mut f = Vec::new();
    let mut d = Vec::new();
    let mut scratch = Edt1d::default();
    // Columns (along v), then rows (along u).
    for u in 0..MASK_W {
        f.clear();
        f.extend((0..MASK_H).map(|v| grid[u + MASK_W * v]));
        scratch.run(&f, &mut d);
        for v in 0..MASK_H {
            grid[u + MASK_W * v] = d[v];
        }
    }
    for v in 0..MASK_H {
        let row = &mut grid[MASK_W * v..MASK_W * (v + 1)];
        f.clear();
        f.extend_from_slice(row);
        scratch.run(&f, &mut d);
        row.copy_from_slice(&d);
    }
    grid
}

#[derive(Default)]
struct Edt1d {
    v: Vec<usize>,
    z: Vec<f64>,
}

impl Edt1d {
    /// Lower envelope of parabolas: `d[q] = min_p (q - p)^2 + f[p]`.
    fn run(&mut self, f: &[f64], d: &mut Vec<f64>) {
        let n = f.len();
        d.clear();
        d.resize(n, 0.0);
        if n == 0 {
            return;
        }
        self.v.clear();
        self.v.resize(n, 0);
        self.z.clear();
        self.z.resize(n + 1, 0.0);
        let mut k = 0usize;
        self.v[0] = 0;
        self.z[0] = f64::NEG_INFINITY;
        self.z[1] = f64::INFINITY;
        let sq = |x: usize| (x * x) as f64;
        let meet = |q: usize, p: usize| ((f[q] + sq(q)) - (f[p] + sq(p))) / (2.0 * (q as f64 - p as f64));
        for q in 1..n {
            let mut s = meet(q, self.v[k]);
            // z[0] is minus infinity and every s is finite, so k never drops below zero.
            while s <= self.z[k] {
                k -= 1;
                s = meet(q, self.v[k]);
            }
            k += 1;
            self.v[k] = q;
            self.z[k] = s;
            self.z[k + 1] = f64::INFINITY;
        }
        k = 0;
        for (q, out) in d.iter_mut().enumerate() {
            while self.z[k + 1] < q as f64 {
                k += 1;
            }
            let p = self.v[k];
            let dq = q as f64 - p as f64;
            *out = dq * dq + f[p];
        }
    }
}

/// f32 to IEEE 754 binary16 bits, round to nearest even. Unit: `hm-mask`.
///
/// Overflow rounds to infinity as IEEE does (65520 and up); NaN stays a quiet NaN with its sign.
pub fn f32_to_f16_bits(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xFF) as i32;
    let mant = x & 0x007F_FFFF;
    if exp == 0xFF {
        return if mant == 0 { sign | 0x7C00 } else { sign | 0x7E00 | (mant >> 13) as u16 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1F {
        return sign | 0x7C00;
    }
    if e <= 0 {
        // Subnormal half (or zero). Below half the smallest subnormal everything rounds to zero.
        if e < -10 {
            return sign;
        }
        let m = mant | 0x0080_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let rem = m & ((1u32 << shift) - 1);
        let mut r = m >> shift;
        if rem > half || (rem == half && r & 1 == 1) {
            r += 1;
        }
        // A carry into bit 10 is exactly the smallest normal's encoding.
        return sign | r as u16;
    }
    let mut h = ((e as u32) << 10) | (mant >> 13);
    let rem = mant & 0x1FFF;
    if rem > 0x1000 || (rem == 0x1000 && h & 1 == 1) {
        // A carry out of the mantissa bumps the exponent, up to infinity at the top: both are correct.
        h += 1;
    }
    sign | h as u16
}

#[cfg(test)]
mod tests;
