//! T-junction removal for finished chunk meshes (L1945: cracks are fixed in the mesher, never in the shader).
//!
//! Two interchangeable point indexes answer "which mesh vertices lie strictly inside this edge":
//! - [`Lattice`], the fast path for chunk meshes: vertices on the quarter-metre lattice go into per-axis line
//!   bitsets over the mesh's bounding box, so an axis-aligned edge is a masked bit scan and a sloped edge with
//!   lattice ends steps through its own lattice points. The few vertices off the lattice (crossings of two cut
//!   lines on a shaped face) sit in a side list sorted by x and get the exact distance test.
//! - [`Points`], the general index (hash maps and sorted lines over every vertex), used when the lattice box is
//!   too large for the bitsets (a mesh much bigger than one chunk). It is also the oracle the fast path is tested
//!   against.
//!
//! Both answer the same query with the same rule (exact equality on axis lines, [`ON_EDGE_EPS_M`] on sloped
//! edges, the [`SEAM_STEP_M`] chunk-seam rule), so the splitting pass gives the same mesh with either.
//!
//! Work the pass skips, because it cannot find anything:
//! - fan diagonals: an edge the previous or next triangle shares by vertex index (see [`run`]);
//! - on a coarse mesh (every vertex on the half-metre cell grid: no 1:2 slopes and no cut-line crossings), an
//!   edge one cell long or shorter that the seam rule does not touch. Its only interior lattice point is at a
//!   quarter-metre offset, where no vertex sits.
//!
//! Spike S5 (L2259): `cargo test --release -p hull-mesh -- --ignored bench_hull_chunk --nocapture`.
use crate::types::MeshData;
use crate::surface::FastMap;

/// Vertices lying strictly inside a segment, ordered from its start to its end.
trait EdgeQuery {
    /// Segment from vertex `ia` at `p` to vertex `ib` at `q` (indices into the positions the index was built on).
    fn on_edge(&self, ia: u32, ib: u32, p: [f32; 3], q: [f32; 3], out: &mut Vec<[f32; 3]>);
    /// Cheap pre-check: `false` only when the edge cannot hold a vertex or a seam point.
    fn may_hold(&self, _ia: u32, _ib: u32) -> bool { true }
}

/// Chunk-seam lattice step: edges in a chunk boundary plane split at every multiple of this. A quarter metre
/// is half a cell, because 1:2 slopes put vertices at cell-edge midpoints.
pub const SEAM_STEP_M: f32 = 0.25;
/// Distance within which a vertex counts as lying on a sloped edge.
pub const ON_EDGE_EPS_M: f64 = 1e-4;
/// Chunk edge in metres (32 cells of 0.5 m).
const SEAM_PLANE_M: f32 = 16.0;
/// Lattice points per metre (the seam step).
const LATTICE_PER_M: f32 = 4.0;
/// Most lattice points one bitset line holds: a box of up to 32 m per axis takes the fast path.
const LINE_BITS: i32 = 128;
/// Most seam points added to one edge. A chunk edge is 64 steps; only malformed coordinates come near this.
const MAX_SEAM_POINTS: f32 = 4096.0;

fn key(v: f32) -> u32 { if v == 0.0 { 0 } else { v.to_bits() } }

fn on_seam_plane(c: f32) -> bool { c > 0.0 && (c / SEAM_PLANE_M).fract() == 0.0 }

struct Points {
    /// Unique positions (exact equality).
    pts: Vec<[f32; 3]>,
    /// Per axis a: coordinates p[a] of the points on each axis-parallel line, grouped by line and sorted.
    lines: [Vec<f32>; 3],
    /// Per axis a: (bits of p[a + 1], bits of p[a + 2]) -> range of that line in `lines[a]`.
    groups: [FastMap<(u32, u32), (u32, u32)>; 3],
    /// Point ids bucketed by half-metre cell, for sloped-edge queries.
    cells: CellBuckets,
}

/// Point ids grouped by cell key: a dense grid over the keys' bounding box when it is small (one chunk), else a
/// sorted list searched per cell.
enum CellBuckets {
    Dense { min: [i32; 3], dim: [i32; 3], start: Vec<u32>, ids: Vec<u32> },
    Sorted(Vec<([i32; 3], u32)>),
}

/// Largest dense bucket grid (a chunk mesh spans 33^3 keys).
const DENSE_MAX: i64 = 80 * 80 * 80;

impl CellBuckets {
    fn build(pts: &[[f32; 3]]) -> Self {
        let keys: Vec<[i32; 3]> = pts.iter().map(|p| cell_key(*p)).collect();
        let mut min = [i32::MAX; 3];
        let mut max = [i32::MIN; 3];
        for k in &keys {
            for a in 0..3 {
                min[a] = min[a].min(k[a]);
                max[a] = max[a].max(k[a]);
            }
        }
        let dim = [0, 1, 2].map(|a| (max[a] as i64 - min[a] as i64 + 1).max(0));
        if !keys.is_empty() && dim[0] * dim[1] * dim[2] <= DENSE_MAX {
            let dim = dim.map(|d| d as i32);
            let slot = |k: [i32; 3]| ((k[0] - min[0]) + dim[0] * ((k[1] - min[1]) + dim[1] * (k[2] - min[2]))) as usize;
            let n = (dim[0] * dim[1] * dim[2]) as usize;
            let mut start = vec![0u32; n + 1];
            for k in &keys {
                start[slot(*k) + 1] += 1;
            }
            for i in 0..n {
                start[i + 1] += start[i];
            }
            let mut fill = start.clone();
            let mut ids = vec![0u32; keys.len()];
            for (i, k) in keys.iter().enumerate() {
                let s = &mut fill[slot(*k)];
                ids[*s as usize] = i as u32;
                *s += 1;
            }
            return CellBuckets::Dense { min, dim, start, ids };
        }
        let mut v: Vec<([i32; 3], u32)> = keys.into_iter().enumerate().map(|(i, k)| (k, i as u32)).collect();
        v.sort_unstable();
        CellBuckets::Sorted(v)
    }

    fn for_each(&self, k: [i32; 3], mut f: impl FnMut(u32)) {
        match self {
            CellBuckets::Dense { min, dim, start, ids } => {
                let l = [k[0] - min[0], k[1] - min[1], k[2] - min[2]];
                if (0..3).any(|a| l[a] < 0 || l[a] >= dim[a]) {
                    return;
                }
                let s = (l[0] + dim[0] * (l[1] + dim[1] * l[2])) as usize;
                for &id in &ids[start[s] as usize..start[s + 1] as usize] {
                    f(id);
                }
            }
            CellBuckets::Sorted(v) => {
                let s = v.partition_point(|e| e.0 < k);
                for e in v[s..].iter().take_while(|e| e.0 == k) {
                    f(e.1);
                }
            }
        }
    }
}

fn cell_key(p: [f32; 3]) -> [i32; 3] { [(p[0] * 2.0).floor() as i32, (p[1] * 2.0).floor() as i32, (p[2] * 2.0).floor() as i32] }

impl Points {
    fn build(positions: &[[f32; 3]]) -> Self {
        let mut ids: FastMap<[u32; 3], u32> = FastMap::with_capacity_and_hasher(positions.len(), Default::default());
        let mut pts = Vec::new();
        for p in positions {
            if p.iter().any(|c| !c.is_finite()) {
                continue;
            }
            ids.entry([key(p[0]), key(p[1]), key(p[2])]).or_insert_with(|| {
                pts.push([p[0] + 0.0, p[1] + 0.0, p[2] + 0.0]);
                (pts.len() - 1) as u32
            });
        }
        let mut lines: [Vec<f32>; 3] = Default::default();
        let mut groups: [FastMap<(u32, u32), (u32, u32)>; 3] = Default::default();
        for a in 0..3 {
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let mut l: Vec<(u32, u32, f32)> = pts.iter().map(|p| (key(p[b]), key(p[c]), p[a])).collect();
            l.sort_unstable_by(|x, y| (x.0, x.1).cmp(&(y.0, y.1)).then(x.2.total_cmp(&y.2)));
            groups[a].reserve(l.len());
            let mut i = 0;
            while i < l.len() {
                let mut j = i + 1;
                while j < l.len() && (l[j].0, l[j].1) == (l[i].0, l[i].1) {
                    j += 1;
                }
                // Lines holding a single point cannot hold a point inside an edge (the edge's own ends are on it).
                if j - i > 2 {
                    groups[a].insert((l[i].0, l[i].1), (i as u32, j as u32));
                }
                i = j;
            }
            lines[a] = l.into_iter().map(|e| e.2).collect();
        }
        let cells = CellBuckets::build(&pts);
        Self { pts, lines, groups, cells }
    }

}

impl EdgeQuery for Points {
    /// Positions strictly inside segment p->q, ordered from p to q, appended to `out` (cleared first).
    fn on_edge(&self, _ia: u32, _ib: u32, p: [f32; 3], q: [f32; 3], out: &mut Vec<[f32; 3]>) {
        out.clear();
        let same = [p[0] == q[0], p[1] == q[1], p[2] == q[2]];
        let n_same = same.iter().filter(|s| **s).count();
        if n_same == 3 {
            return;
        }
        if n_same == 2 {
            let a = same.iter().position(|s| !*s).unwrap_or(0);
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let (lo, hi) = if p[a] < q[a] { (p[a], q[a]) } else { (q[a], p[a]) };
            let mk = |v: f32| { let mut r = p; r[a] = v; r };
            if let Some(&(s, e)) = self.groups[a].get(&(key(p[b]), key(p[c]))) {
                let line = &self.lines[a][s as usize..e as usize];
                let start = line.partition_point(|&v| v <= lo);
                for &v in line[start..].iter().take_while(|&&v| v < hi) {
                    out.push(mk(v));
                }
            }
            if (on_seam_plane(p[b]) || on_seam_plane(p[c])) && (hi - lo) / SEAM_STEP_M <= MAX_SEAM_POINTS {
                let mut k = (lo / SEAM_STEP_M).floor() + 1.0;
                loop {
                    let v = k * SEAM_STEP_M;
                    if !(v < hi) {
                        break;
                    }
                    if v > lo {
                        out.push(mk(v));
                    }
                    k += 1.0;
                }
                out.sort_by(|x, y| x[a].total_cmp(&y[a]));
                out.dedup();
            }
            if p[a] > q[a] {
                out.reverse();
            }
            return;
        }
        // Sloped edge (shaped faces only, inside one cell): candidates from the half-metre cells its box
        // touches, then a distance test. The box corners are lattice points, so no tolerance is needed for the
        // cell range; a longer edge (not produced by this crate) scans every cell its box covers.
        let eps = ON_EDGE_EPS_M;
        let lo = cell_key([p[0].min(q[0]), p[1].min(q[1]), p[2].min(q[2])]);
        let hi = cell_key([p[0].max(q[0]), p[1].max(q[1]), p[2].max(q[2])]);
        if (0..3).any(|k| hi[k] - lo[k] > 64) {
            return;
        }
        let d = [q[0] as f64 - p[0] as f64, q[1] as f64 - p[1] as f64, q[2] as f64 - p[2] as f64];
        let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        let len = len2.sqrt();
        let bmin = [0, 1, 2].map(|k| p[k].min(q[k]) as f64 - eps);
        let bmax = [0, 1, 2].map(|k| p[k].max(q[k]) as f64 + eps);
        let mut hits: Vec<(f64, [f32; 3])> = Vec::new();
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    self.cells.for_each([x, y, z], |id| {
                        let r = self.pts[id as usize];
                        if (0..3).any(|k| (r[k] as f64) < bmin[k] || (r[k] as f64) > bmax[k]) {
                            return;
                        }
                        let w = [r[0] as f64 - p[0] as f64, r[1] as f64 - p[1] as f64, r[2] as f64 - p[2] as f64];
                        // Strictly inside, away from both ends by more than the tolerance: eps < t * len < len - eps.
                        let dot = w[0] * d[0] + w[1] * d[1] + w[2] * d[2];
                        if dot <= eps * len || dot >= len2 - eps * len {
                            return;
                        }
                        let cr = [w[1] * d[2] - w[2] * d[1], w[2] * d[0] - w[0] * d[2], w[0] * d[1] - w[1] * d[0]];
                        if cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2] <= eps * eps * len2 {
                            hits.push((dot, r));
                        }
                    });
                }
            }
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.extend(hits.into_iter().map(|h| h.1));
    }
}

/// Lattice coordinate (quarter metres) of a metre coordinate, when it lies on the lattice.
#[inline]
fn lattice(c: f32) -> Option<i32> {
    let q = c * LATTICE_PER_M;
    (q.fract() == 0.0 && q.abs() <= 16_777_216.0).then_some(q as i32)
}

#[inline]
fn lattice3(p: [f32; 3]) -> Option<[i32; 3]> { Some([lattice(p[0])?, lattice(p[1])?, lattice(p[2])?]) }

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Exact sloped-edge test shared by both indexes: `r` lies within the tolerance of segment p->q and strictly
/// inside it, away from both ends by more than the tolerance. Returns the projection `(r - p) . (q - p)`.
#[inline]
fn on_sloped(p: [f32; 3], d: [f64; 3], len2: f64, len: f64, r: [f32; 3]) -> Option<f64> {
    let eps = ON_EDGE_EPS_M;
    let w = [r[0] as f64 - p[0] as f64, r[1] as f64 - p[1] as f64, r[2] as f64 - p[2] as f64];
    let dot = w[0] * d[0] + w[1] * d[1] + w[2] * d[2];
    if dot <= eps * len || dot >= len2 - eps * len {
        return None;
    }
    let cr = [w[1] * d[2] - w[2] * d[1], w[2] * d[0] - w[0] * d[2], w[0] * d[1] - w[1] * d[0]];
    (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2] <= eps * eps * len2).then_some(dot)
}

/// Fast point index for one chunk mesh (see the module docs).
struct Lattice {
    /// Minimum lattice coordinate of the box and its extent per axis (each at most [`LINE_BITS`]).
    min: [i32; 3],
    dim: [i32; 3],
    /// Per axis a: one bitset per line along a, indexed `(r[b] - min[b]) + dim[b] * (r[c] - min[c])` with
    /// b = a + 1, c = a + 2 (mod 3); bit `r[a] - min[a]` set where a vertex sits.
    lines: [Vec<u128>; 3],
    /// Unique off-lattice vertices, sorted by x.
    side: Vec<[f32; 3]>,
    /// Lattice coordinate of every vertex, [`OFF_LATTICE`] where it has none.
    lat: Vec<[i32; 3]>,
    /// Every vertex is on the lattice at even coordinates (the half-metre cell grid): no 1:2 slope midpoints
    /// and no cut-line crossings. Then a one-cell edge, whose only interior lattice point is odd, holds nothing
    /// unless the seam rule adds points to it.
    coarse: bool,
}

/// `Lattice::lat` entry of a vertex off the lattice (or not finite).
const OFF_LATTICE: [i32; 3] = [i32::MIN; 3];
/// Lattice units between chunk seam planes (16 m). A power of two.
const SEAM_PLANE_L: i32 = 64;
const _: () = assert!(SEAM_PLANE_L.count_ones() == 1);

/// Buffers kept between calls on one meshing thread, so a chunk does not allocate and fault in 200 KB.
#[derive(Default)]
struct Buffers {
    lines: [Vec<u128>; 3],
    lat: Vec<[i32; 3]>,
}

thread_local! {
    /// The index list the pass reads from, swapped with the mesh's own each call, so a reused mesh allocates
    /// nothing here either.
    static SPARE_INDICES: std::cell::Cell<Vec<u32>> = const { std::cell::Cell::new(Vec::new()) };
}

thread_local! {
    static BUFFERS: std::cell::Cell<Buffers> = Default::default();
}

impl Lattice {
    /// `None` when the lattice vertices span more than [`LINE_BITS`] points on some axis. `lines` are reused
    /// buffers (any content).
    fn build(positions: &[[f32; 3]], bufs: Buffers) -> Option<Self> {
        let Buffers { mut lines, mut lat } = bufs;
        lat.clear();
        let mut min = [i32::MAX; 3];
        let mut max = [i32::MIN; 3];
        let mut side: Vec<[f32; 3]> = Vec::new();
        let (mut odd, mut off) = (0i32, 0usize);
        lat.reserve(positions.len());
        for p in positions {
            // Same test as `lattice3`, branch-light: the truncated value converts back exactly.
            let q = [p[0] * LATTICE_PER_M, p[1] * LATTICE_PER_M, p[2] * LATTICE_PER_M];
            let r = [q[0] as i32, q[1] as i32, q[2] as i32];
            let exact = (r[0] as f32 == q[0]) & (r[1] as f32 == q[1]) & (r[2] as f32 == q[2]);
            let in_range = (r[0].unsigned_abs() <= 1 << 24) & (r[1].unsigned_abs() <= 1 << 24) & (r[2].unsigned_abs() <= 1 << 24);
            if exact && in_range {
                for a in 0..3 {
                    min[a] = min[a].min(r[a]);
                    max[a] = max[a].max(r[a]);
                }
                odd |= r[0] | r[1] | r[2];
                lat.push(r);
            } else {
                off += 1;
                if p.iter().all(|c| c.is_finite()) {
                    side.push([p[0] + 0.0, p[1] + 0.0, p[2] + 0.0]);
                }
                lat.push(OFF_LATTICE);
            }
        }
        if min[0] > max[0] {
            min = [0; 3];
            max = [-1; 3];
        }
        let coarse = off == 0 && odd & 1 == 0;
        let dim = [0, 1, 2].map(|a| max[a] as i64 - min[a] as i64 + 1);
        if dim.iter().any(|&d| d > LINE_BITS as i64) {
            return None;
        }
        let dim = dim.map(|d| d as i32);
        for (a, l) in lines.iter_mut().enumerate() {
            l.clear();
            l.resize((dim[(a + 1) % 3] * dim[(a + 2) % 3]) as usize, 0);
        }
        for r in &lat {
            if *r == OFF_LATTICE {
                continue;
            }
            let l = [r[0] - min[0], r[1] - min[1], r[2] - min[2]];
            for a in 0..3 {
                let (b, c) = ((a + 1) % 3, (a + 2) % 3);
                lines[a][(l[b] + dim[b] * l[c]) as usize] |= 1u128 << l[a];
            }
        }
        side.sort_unstable_by(|x, y| x[0].total_cmp(&y[0]).then(x[1].total_cmp(&y[1])).then(x[2].total_cmp(&y[2])));
        side.dedup();
        Some(Self { min, dim, lines, side, lat, coarse })
    }

    /// Hand the buffers back for the next call.
    fn into_buffers(self) -> Buffers { Buffers { lines: self.lines, lat: self.lat } }

    /// Side-list vertices exactly on the axis-a line through `p` with `lo < r[a] < hi`, appended to `out`.
    /// Returns whether any was added.
    fn side_on_line(&self, a: usize, p: [f32; 3], lo: f32, hi: f32, out: &mut Vec<[f32; 3]>) -> bool {
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        let (xlo, xhi) = if a == 0 { (lo as f64, hi as f64) } else { (p[0] as f64, p[0] as f64) };
        let mut any = false;
        for r in self.side_in(xlo, xhi) {
            if r[b] == p[b] && r[c] == p[c] && r[a] > lo && r[a] < hi {
                let mut v = p;
                v[a] = r[a];
                out.push(v);
                any = true;
            }
        }
        any
    }

    /// Side-list vertices on sloped segment p->q (the shared tolerance test), appended to `out`.
    fn side_on_sloped(&self, p: [f32; 3], q: [f32; 3], out: &mut Vec<[f32; 3]>) -> bool {
        if self.side.is_empty() {
            return false;
        }
        let eps = ON_EDGE_EPS_M;
        let d = [q[0] as f64 - p[0] as f64, q[1] as f64 - p[1] as f64, q[2] as f64 - p[2] as f64];
        let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        let len = len2.sqrt();
        let bmin = [0, 1, 2].map(|k| p[k].min(q[k]) as f64 - eps);
        let bmax = [0, 1, 2].map(|k| p[k].max(q[k]) as f64 + eps);
        let mut any = false;
        for r in self.side_in(bmin[0], bmax[0]) {
            if (1..3).any(|k| (r[k] as f64) < bmin[k] || (r[k] as f64) > bmax[k]) {
                continue;
            }
            if on_sloped(p, d, len2, len, *r).is_some() {
                out.push(*r);
                any = true;
            }
        }
        any
    }

    /// Both ends on the lattice: integer arithmetic only, except for side-list vertices.
    fn on_lattice_edge(&self, la: [i32; 3], lb: [i32; 3], p: [f32; 3], q: [f32; 3], out: &mut Vec<[f32; 3]>) {
        let d = [lb[0] - la[0], lb[1] - la[1], lb[2] - la[2]];
        let moving = (d[0] != 0) as u8 + (d[1] != 0) as u8 + (d[2] != 0) as u8;
        if moving == 0 {
            return;
        }
        if moving == 1 {
            let a = if d[0] != 0 { 0 } else if d[1] != 0 { 1 } else { 2 };
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let (lo, hi) = if d[a] > 0 { (la[a], lb[a]) } else { (lb[a], la[a]) };
            let mk = |k: i32| { let mut r = p; r[a] = k as f32 * SEAM_STEP_M; r };
            let seam = |g: i32| g > 0 && g % SEAM_PLANE_L == 0;
            if (seam(la[b]) || seam(la[c])) && (hi - lo) as f32 <= MAX_SEAM_POINTS {
                // Chunk seam: every lattice point, so both chunks split their boundary edges alike.
                out.extend((lo + 1..hi).map(mk));
            } else {
                self.line_points(a, la[b], la[c], lo as i64 + 1, hi as i64 - 1, |k| out.push(mk(k)));
            }
            let (flo, fhi) = if p[a] < q[a] { (p[a], q[a]) } else { (q[a], p[a]) };
            if self.side_on_line(a, p, flo, fhi, out) {
                out.sort_by(|x, y| x[a].total_cmp(&y[a]));
                out.dedup();
            }
            if d[a] < 0 {
                out.reverse();
            }
            return;
        }
        // Sloped: lattice points on the segment are exactly la + k * step for 0 < k < g.
        let g = gcd(gcd(d[0].unsigned_abs(), d[1].unsigned_abs()), d[2].unsigned_abs()) as i32;
        if g > 1 {
            let step = [d[0] / g, d[1] / g, d[2] / g];
            for k in 1..g {
                let r = [la[0] + k * step[0], la[1] + k * step[1], la[2] + k * step[2]];
                if self.present(r) {
                    out.push(r.map(|v| v as f32 * SEAM_STEP_M));
                }
            }
        }
        if self.side_on_sloped(p, q, out) {
            let dd = [q[0] as f64 - p[0] as f64, q[1] as f64 - p[1] as f64, q[2] as f64 - p[2] as f64];
            let t = |r: &[f32; 3]| (r[0] as f64 - p[0] as f64) * dd[0] + (r[1] as f64 - p[1] as f64) * dd[1] + (r[2] as f64 - p[2] as f64) * dd[2];
            out.sort_by(|x, y| t(x).total_cmp(&t(y)));
        }
    }

    /// A vertex sits at lattice point `r`.
    #[inline]
    fn present(&self, r: [i32; 3]) -> bool {
        let l = [r[0] - self.min[0], r[1] - self.min[1], r[2] - self.min[2]];
        if (0..3).any(|a| l[a] < 0 || l[a] >= self.dim[a]) {
            return false;
        }
        (self.lines[0][(l[1] + self.dim[1] * l[2]) as usize] >> l[0]) & 1 == 1
    }

    /// Side-list vertices whose x lies in `lo..=hi`.
    fn side_in(&self, lo: f64, hi: f64) -> &[[f32; 3]] {
        if self.side.is_empty() {
            return &[];
        }
        let s = self.side.partition_point(|r| (r[0] as f64) < lo);
        let e = self.side.partition_point(|r| (r[0] as f64) <= hi);
        &self.side[s..e.max(s)]
    }

    /// Lattice vertices on the axis-a line through lattice (rb, rc), with lattice a-coordinate in `lo..=hi`.
    fn line_points(&self, a: usize, rb: i32, rc: i32, lo: i64, hi: i64, mut f: impl FnMut(i32)) {
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        let (lb, lc) = (rb - self.min[b], rc - self.min[c]);
        if lb < 0 || lb >= self.dim[b] || lc < 0 || lc >= self.dim[c] {
            return;
        }
        let lo = (lo - self.min[a] as i64).max(0);
        let hi = (hi - self.min[a] as i64).min(self.dim[a] as i64 - 1);
        if lo > hi {
            return;
        }
        let mut bits = self.lines[a][(lb + self.dim[b] * lc) as usize] >> lo;
        if hi - lo < 127 {
            bits &= (1u128 << (hi - lo + 1)) - 1;
        }
        while bits != 0 {
            let k = bits.trailing_zeros() as i64;
            f((lo + k) as i32 + self.min[a]);
            bits &= bits - 1;
        }
    }
}

impl EdgeQuery for Lattice {
    #[inline]
    fn may_hold(&self, ia: u32, ib: u32) -> bool {
        if !self.coarse {
            return true;
        }
        let (Some(la), Some(lb)) = (self.lat.get(ia as usize), self.lat.get(ib as usize)) else { return true };
        let d = [lb[0] - la[0], lb[1] - la[1], lb[2] - la[2]];
        if d[0].abs().max(d[1].abs()).max(d[2].abs()) > 2 {
            return true;
        }
        // One cell or less: only the seam rule can add a point, and only to an axis-aligned edge.
        let seam = |k: usize| (d[k] == 0) & (la[k] > 0) & (la[k] & (SEAM_PLANE_L - 1) == 0);
        let moving = (d[0] != 0) as u8 + (d[1] != 0) as u8 + (d[2] != 0) as u8;
        (moving == 1) & (seam(0) | seam(1) | seam(2))
    }

    fn on_edge(&self, ia: u32, ib: u32, p: [f32; 3], q: [f32; 3], out: &mut Vec<[f32; 3]>) {
        out.clear();
        let (la, lb) = (self.lat.get(ia as usize).copied().unwrap_or(OFF_LATTICE), self.lat.get(ib as usize).copied().unwrap_or(OFF_LATTICE));
        if la != OFF_LATTICE && lb != OFF_LATTICE {
            return self.on_lattice_edge(la, lb, p, q, out);
        }
        // An end off the lattice: the general float form of the same rules.
        let same = [p[0] == q[0], p[1] == q[1], p[2] == q[2]];
        let n_same = same.iter().filter(|s| **s).count();
        if n_same == 3 {
            return;
        }
        if n_same == 2 {
            let a = same.iter().position(|s| !*s).unwrap_or(0);
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let (lo, hi) = if p[a] < q[a] { (p[a], q[a]) } else { (q[a], p[a]) };
            let mk = |v: f32| { let mut r = p; r[a] = v; r };
            // Lattice a-coordinates strictly inside (lo, hi).
            let ilo = (lo * LATTICE_PER_M).floor() as i64 + 1;
            let ihi = (hi * LATTICE_PER_M).ceil() as i64 - 1;
            let seam = on_seam_plane(p[b]) || on_seam_plane(p[c]);
            let mut sorted = true;
            if seam && (hi - lo) / SEAM_STEP_M <= MAX_SEAM_POINTS {
                for k in ilo..=ihi {
                    out.push(mk(k as f32 * SEAM_STEP_M));
                }
            } else if let (Some(rb), Some(rc)) = (lattice(p[b]), lattice(p[c])) {
                self.line_points(a, rb, rc, ilo, ihi, |k| out.push(mk(k as f32 * SEAM_STEP_M)));
            }
            for r in self.side_in(if a == 0 { lo as f64 } else { p[0] as f64 }, if a == 0 { hi as f64 } else { p[0] as f64 }) {
                if r[b] == p[b] && r[c] == p[c] && r[a] > lo && r[a] < hi {
                    out.push(mk(r[a]));
                    sorted = false;
                }
            }
            if !sorted {
                out.sort_by(|x, y| x[a].total_cmp(&y[a]));
                out.dedup();
            }
            if p[a] > q[a] {
                out.reverse();
            }
            return;
        }
        // Sloped edge.
        let eps = ON_EDGE_EPS_M;
        let d = [q[0] as f64 - p[0] as f64, q[1] as f64 - p[1] as f64, q[2] as f64 - p[2] as f64];
        let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        let len = len2.sqrt();
        let mut sorted = true;
        match (lattice3(p), lattice3(q)) {
            (Some(lp), Some(lq)) => {
                // Lattice points on the segment are exactly p + k * step for 0 < k < g.
                let dl = [lq[0] - lp[0], lq[1] - lp[1], lq[2] - lp[2]];
                let g = gcd(gcd(dl[0].unsigned_abs(), dl[1].unsigned_abs()), dl[2].unsigned_abs()) as i32;
                if g > 1 {
                    let step = [dl[0] / g, dl[1] / g, dl[2] / g];
                    for k in 1..g {
                        let r = [lp[0] + k * step[0], lp[1] + k * step[1], lp[2] + k * step[2]];
                        if self.present(r) {
                            out.push(r.map(|v| v as f32 * SEAM_STEP_M));
                        }
                    }
                }
            }
            _ => {
                // An end off the lattice: every lattice point in the edge's box, within the tolerance.
                let lo = [0, 1, 2].map(|k| ((p[k].min(q[k]) as f64 - eps) * LATTICE_PER_M as f64).ceil() as i64);
                let hi = [0, 1, 2].map(|k| ((p[k].max(q[k]) as f64 + eps) * LATTICE_PER_M as f64).floor() as i64);
                let volume: i64 = (0..3).map(|k| (hi[k] - lo[k] + 1).max(0)).product();
                if volume <= 4096 {
                    for x in lo[0]..=hi[0] {
                        for y in lo[1]..=hi[1] {
                            self.line_points(2, x as i32, y as i32, lo[2], hi[2], |z| {
                                let r = [x as f32 * SEAM_STEP_M, y as f32 * SEAM_STEP_M, z as f32 * SEAM_STEP_M];
                                if on_sloped(p, d, len2, len, r).is_some() {
                                    out.push(r);
                                }
                            });
                        }
                    }
                    sorted = out.len() < 2;
                }
            }
        }
        let bmin = [0, 1, 2].map(|k| p[k].min(q[k]) as f64 - eps);
        let bmax = [0, 1, 2].map(|k| p[k].max(q[k]) as f64 + eps);
        for r in self.side_in(bmin[0], bmax[0]) {
            if (1..3).any(|k| (r[k] as f64) < bmin[k] || (r[k] as f64) > bmax[k]) {
                continue;
            }
            if on_sloped(p, d, len2, len, *r).is_some() {
                out.push(*r);
                sorted = false;
            }
        }
        if !sorted {
            let t = |r: &[f32; 3]| (r[0] as f64 - p[0] as f64) * d[0] + (r[1] as f64 - p[1] as f64) * d[1] + (r[2] as f64 - p[2] as f64) * d[2];
            out.sort_by(|x, y| t(x).total_cmp(&t(y)));
        }
    }
}

/// Appends split vertices to the mesh in place and collects the new index list.
///
/// Inserted vertices are not shared between triangles: the only triangles that would share one are the two
/// sides of a fan diagonal, which is never split (see [`run`]).
struct Builder {
    indices: Vec<u32>,
}

impl Builder {
    /// A new vertex at `p` with the attributes (normal, colour, custom) of original vertex `like`. An attribute
    /// array that is not one entry per vertex (empty, or malformed input) is left as it is.
    fn vertex(&mut self, mesh: &mut MeshData, p: [f32; 3], like: u32) -> u32 {
        let l = like as usize;
        let n = mesh.positions.len();
        if mesh.normals.len() == n {
            if let Some(&x) = mesh.normals.get(l) { mesh.normals.push(x); }
        }
        if mesh.colors.len() == n {
            if let Some(&x) = mesh.colors.get(l) { mesh.colors.push(x); }
        }
        if mesh.custom0.len() == n {
            if let Some(&x) = mesh.custom0.get(l) { mesh.custom0.push(x); }
        }
        mesh.positions.push(p);
        n as u32
    }

    /// Emit triangle (a, b, c) whose edges a->b, b->c, c->a carry the given interior vertices in order.
    fn split(&mut self, t: [u32; 3], e: [&[u32]; 3]) {
        let Some(k) = (0..3).find(|&k| !e[k].is_empty()) else {
            self.indices.extend_from_slice(&t);
            return;
        };
        // Rotate so the split edge is a->b.
        let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
        let (ab, bc, ca) = (e[k], e[(k + 1) % 3], e[(k + 2) % 3]);
        let n = ab.len();
        // Sub-triangles (u, w, c) along the chain a, ab.., b: edges u->w (clear), w->c (the last one holds b->c),
        // c->u (the first one holds c->a).
        for i in 0..=n {
            let u = if i == 0 { a } else { ab[i - 1] };
            let w = if i == n { b } else { ab[i] };
            let first_bc: &[u32] = if i == n { bc } else { &[] };
            let first_ca: &[u32] = if i == 0 { ca } else { &[] };
            if first_bc.is_empty() && first_ca.is_empty() {
                self.indices.extend_from_slice(&[u, w, c]);
            } else {
                self.split([u, w, c], [&[], first_bc, first_ca]);
            }
        }
    }
}

/// Edge `k` of triangle `t` is shared, by vertex index and in the opposite direction, with triangle `o`.
#[inline]
fn shares_edge(t: &[u32], k: usize, o: Option<&[u32]>) -> bool {
    let Some(o) = o else { return false };
    let (a, b) = (t[k], t[(k + 1) % 3]);
    (0..3).any(|j| o[j] == b && o[(j + 1) % 3] == a)
}

/// Bit k set where edge k of `t` is edge j of `o` reversed, plus the matching bits of `o`'s edges.
#[inline]
fn shared_edges(t: [u32; 3], o: [u32; 3]) -> (u8, u8) {
    // eq[k][j]: t[k] == o[j], branch-free; edge k of t (t[k] -> t[k+1]) is edge j of o reversed when
    // t[k + 1] == o[j] and t[k] == o[j + 1].
    let eq = |k: usize, j: usize| (t[k] == o[j]) as u8;
    let (mut mt, mut mo) = (0u8, 0u8);
    for k in 0..3 {
        for j in 0..3 {
            let hit = eq((k + 1) % 3, j) & eq(k, (j + 1) % 3);
            mt |= hit << k;
            mo |= hit << j;
        }
    }
    (mt, mo)
}

/// Split every triangle of `mesh` at the points `points` finds on its edges.
///
/// An edge that the previous or next triangle in the index list shares by vertex index (the inner diagonals of
/// every fanned quad and polygon) is not queried: both sides of it already use the same two vertices, and in a
/// voxel surface no other face's vertex lies inside a face's own polygon, so nothing can sit on it (a test checks
/// this on random shaped grids). Seam-plane points are not needed there either: the neighbouring chunk's
/// vertices on a seam plane lie on polygon boundaries, never inside one.
fn run<Q: EdgeQuery>(mesh: &mut MeshData, points: &Q) {
    let mut out = SPARE_INDICES.take();
    out.clear();
    out.reserve(mesh.indices.len() + mesh.indices.len() / 2);
    let src = std::mem::take(&mut mesh.indices);
    let nverts = mesh.positions.len();
    let tri = |j: usize| [src[3 * j], src[3 * j + 1], src[3 * j + 2]];
    let ntris = src.len() / 3;
    let mut b = Builder { indices: out };
    // Room for the usual number of split vertices, so the attribute arrays do not reallocate mid-pass.
    let extra = nverts / 3;
    mesh.positions.reserve(extra);
    if mesh.normals.len() == nverts { mesh.normals.reserve(extra); }
    if mesh.colors.len() == nverts { mesh.colors.reserve(extra); }
    if mesh.custom0.len() == nverts { mesh.custom0.reserve(extra); }
    let mut scratch = Vec::new();
    let mut edges: [Vec<u32>; 3] = Default::default();
    // Edges of the current triangle shared with the previous one.
    let mut shared_prev = 0u8;
    for j in 0..ntris {
        let t = tri(j);
        let (shared_next, next_from_us) = if j + 1 < ntris { shared_edges(t, tri(j + 1)) } else { (0, 0) };
        let shared = shared_prev | shared_next;
        shared_prev = next_from_us;
        if t.iter().any(|&i| i as usize >= nverts) {
            continue;
        }
        let p = [mesh.positions[t[0] as usize], mesh.positions[t[1] as usize], mesh.positions[t[2] as usize]];
        let degenerate = p[0] == p[1] || p[1] == p[2] || p[0] == p[2] || p.iter().flatten().any(|c| !c.is_finite());
        if degenerate {
            b.indices.extend_from_slice(&t);
            continue;
        }
        for k in 0..3 {
            edges[k].clear();
            if shared & (1 << k) != 0 || !points.may_hold(t[k], t[(k + 1) % 3]) {
                continue;
            }
            points.on_edge(t[k], t[(k + 1) % 3], p[k], p[(k + 1) % 3], &mut scratch);
            for &r in &scratch {
                let v = b.vertex(mesh, r, t[0]);
                edges[k].push(v);
            }
        }
        b.split(t, [&edges[0], &edges[1], &edges[2]]);
    }
    mesh.indices = b.indices;
    SPARE_INDICES.set(src);
}

pub(super) fn fix(mesh: &mut MeshData) {
    if mesh.indices.len() < 3 {
        return;
    }
    match Lattice::build(&mesh.positions, BUFFERS.take()) {
        Some(l) => {
            run(mesh, &l);
            BUFFERS.set(l.into_buffers());
        }
        None => run(mesh, &Points::build(&mesh.positions)),
    }
}

/// The general index on every mesh: the oracle the fast path is pinned to in tests.
#[cfg(test)]
pub(crate) fn fix_with_general_index(mesh: &mut MeshData) {
    if mesh.indices.len() < 3 {
        return;
    }
    let points = Points::build(&mesh.positions);
    run(mesh, &points);
}

/// Mesh vertices lying strictly inside an edge that `run` skips as shared (the claim it relies on is that there
/// are none). Seam-rule points are not vertices and are not counted.
#[cfg(test)]
pub(crate) fn vertices_inside_shared_edges(mesh: &MeshData) -> usize {
    let points = Points::build(&mesh.positions);
    let verts: std::collections::BTreeSet<[u32; 3]> = mesh.positions.iter().map(|p| p.map(key)).collect();
    let tris: Vec<&[u32]> = mesh.indices.chunks_exact(3).collect();
    let mut found = Vec::new();
    let mut hits = 0;
    for (j, t) in tris.iter().enumerate() {
        let (prev, next) = (j.checked_sub(1).map(|i| tris[i]), tris.get(j + 1).copied());
        for k in 0..3 {
            if shares_edge(t, k, prev) || shares_edge(t, k, next) {
                points.on_edge(t[k], t[(k + 1) % 3], mesh.positions[t[k] as usize], mesh.positions[t[(k + 1) % 3] as usize], &mut found);
                hits += found.iter().filter(|r| verts.contains(&r.map(key))).count();
            }
        }
    }
    hits
}

/// Whether the fast path takes this mesh.
#[cfg(test)]
pub(crate) fn takes_fast_path(mesh: &MeshData) -> bool { Lattice::build(&mesh.positions, Default::default()).is_some() }

/// Whether the fast path prunes one-cell edges on this mesh (every vertex on the half-metre grid).
#[cfg(test)]
pub(crate) fn takes_coarse_path(mesh: &MeshData) -> bool { Lattice::build(&mesh.positions, Default::default()).is_some_and(|l| l.coarse) }
