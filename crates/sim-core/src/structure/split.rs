//! Connectivity jobs behind `step_split`: compartment recompute, seeding, and the merged multi-source fill.
use super::{Piece, SplitJob, SplitResult};
use crate::bits::BitSet;
use crate::grid::{VesselGrid, CHUNK};
use crate::vessel::Vessel;
use content::cell::{Material, CELL_M};
use content::ids::CellIndex;
use glam::DVec3;
use serde::{Deserialize, Serialize};

pub(crate) const NO_FILL: u32 = u32::MAX;
pub(crate) fn no_fill() -> u32 { NO_FILL }

const PHASE_COMPARTMENTS: u8 = 0;
const PHASE_SEEDING: u8 = 1;
const PHASE_FILL: u8 = 2;

/// Largest label buffer a keyframe may ask for (cells); real grids are far smaller.
const MAX_LABELS: usize = 1 << 23;

/// One fill of the union-find forest. Only roots (`parent == self`) carry meaningful totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Fill {
    pub parent: u32,
    /// Cells labelled.
    pub count: u32,
    /// Cells of this fill still queued in the frontier.
    pub open: u32,
    /// Lowest labelled cell index.
    pub lowest: u32,
}

/// Sparse serde for the label buffer: (length, ascending (index, label) pairs of the non-zero entries).
pub(crate) mod sparse_labels {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(v: &[u32], s: S) -> Result<S::Ok, S::Error> {
        let pairs: Vec<(u32, u32)> = v.iter().enumerate().filter(|(_, &l)| l != 0).map(|(i, &l)| (i as u32, l)).collect();
        (v.len() as u64, pairs).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u32>, D::Error> {
        let (len, pairs): (u64, Vec<(u32, u32)>) = Deserialize::deserialize(d)?;
        // A length beyond any real grid is corrupt data: keep no buffer and let the job revalidate.
        let len = if len as usize > super::MAX_LABELS { 0 } else { len as usize };
        let mut v = vec![0u32; len];
        for (i, l) in pairs { if let Some(x) = v.get_mut(i as usize) { *x = l; } }
        Ok(v)
    }
}

/// Fast live-solid test straight from the chunk bitsets, with the grid's `CellIndex` layout.
pub(crate) struct Solid<'a> {
    g: &'a VesselGrid,
    pub sx: usize,
    pub sy: usize,
    pub sz: usize,
    pub n: usize,
    cdx: usize,
    cdz: usize,
}
impl<'a> Solid<'a> {
    pub fn new(g: &'a VesselGrid) -> Self {
        let s = g.size();
        let (sx, sy, sz) = (s[0] as usize, s[1] as usize, s[2] as usize);
        let n = sx * sy * sz;
        let cd = g.chunk_dims;
        let chunks_ok = g.chunks.len() == cd[0] as usize * cd[1] as usize * cd[2] as usize;
        let n = if n == g.design.cells.len() && n == g.damage.len() && chunks_ok { n } else { 0 };
        Solid { g, sx, sy, sz, n, cdx: cd[0] as usize, cdz: cd[2] as usize }
    }
    #[inline]
    pub fn xyz(&self, i: usize) -> [usize; 3] { [i % self.sx, i / (self.sx * self.sz), (i / self.sx) % self.sz] }
    #[inline]
    fn at(&self, x: usize, y: usize, z: usize) -> bool {
        let ci = x / CHUNK + self.cdx * (z / CHUNK + self.cdz * (y / CHUNK));
        let li = x % CHUNK + CHUNK * (z % CHUNK + CHUNK * (y % CHUNK));
        match self.g.chunks.get(ci) { Some(Some(c)) => c.solid.get(li / 64).is_some_and(|w| (w >> (li % 64)) & 1 == 1), _ => false }
    }
    #[inline]
    pub fn solid(&self, i: usize) -> bool {
        if i >= self.n { return false; }
        let [x, y, z] = self.xyz(i);
        self.at(x, y, z)
    }
    /// In-bounds face neighbours of `i` in `Face::ALL` order; returns how many were written.
    #[inline]
    pub fn neighbours(&self, i: usize, out: &mut [usize; 6]) -> usize {
        let [x, y, z] = self.xyz(i);
        let layer = self.sx * self.sz;
        let mut k = 0;
        if x > 0 { out[k] = i - 1; k += 1; }
        if x + 1 < self.sx { out[k] = i + 1; k += 1; }
        if y > 0 { out[k] = i - layer; k += 1; }
        if y + 1 < self.sy { out[k] = i + layer; k += 1; }
        if z > 0 { out[k] = i - self.sx; k += 1; }
        if z + 1 < self.sz { out[k] = i + self.sx; k += 1; }
        k
    }
}

/// Frontier entries pack a cell's coordinates (9 bits each; grids are at most 384 cells a side) so the fill
/// never divides to find a cell's neighbours.
const _: () = assert!(content::cell::MAX_GRID_DIM as usize <= 511);
#[inline]
fn pack(x: usize, y: usize, z: usize) -> CellIndex { CellIndex(x as u32 | (y as u32) << 9 | (z as u32) << 18) }
#[inline]
fn unpack(c: CellIndex) -> [usize; 3] { [(c.0 & 511) as usize, (c.0 >> 9 & 511) as usize, (c.0 >> 18 & 511) as usize] }

/// Liveness from the design and damage arrays: a cell is live when designed and not at bucket 8 (every removal
/// sets bucket 8, see the grid unit). Avoids the chunk lookup in the fill's inner loop.
struct Live<'a> { cells: &'a [content::cell::Cell], damage: &'a [u8], sx: usize, sy: usize, sz: usize }
impl<'a> Live<'a> {
    fn new(g: &'a VesselGrid, s: &Solid) -> Self {
        if s.n == 0 { return Live { cells: &[], damage: &[], sx: 0, sy: 0, sz: 0 }; }
        Live { cells: &g.design.cells, damage: &g.damage, sx: s.sx, sy: s.sy, sz: s.sz }
    }
    #[inline]
    fn live(&self, i: usize) -> bool {
        self.cells.get(i).is_some_and(|c| !c.is_empty()) && self.damage.get(i).is_some_and(|&b| b < content::cell::BUCKET_DESTROYED)
    }
    #[inline]
    fn index(&self, [x, y, z]: [usize; 3]) -> Option<usize> { (x < self.sx && y < self.sy && z < self.sz).then(|| x + self.sx * (z + self.sz * y)) }
}

/// Cells destroyed since the job started (they wait in `pending_destroyed`). The job still treats them as
/// solid, so it answers for the grid it started on: a cut that lands mid-job is never half-seen, and the
/// precondition of the next job (the hull was one body before that job's cells were destroyed) holds. `None`
/// when there are none.
fn late_cells(vessel: &Vessel, job: &SplitJob, n: usize) -> Option<BitSet> {
    let mut late: Option<BitSet> = None;
    for c in &vessel.pending_destroyed {
        let i = c.index();
        if i >= n || vessel.grid.design.cells.get(i).is_none_or(|x| x.is_empty()) || job.destroyed.binary_search(c).is_ok() { continue; }
        late.get_or_insert_with(|| BitSet::new(n)).set(i, true);
    }
    late
}

#[inline]
fn is_late(late: &Option<BitSet>, i: usize) -> bool { late.as_ref().is_some_and(|b| b.get(i)) }

/// Debris threshold: the signature of `step_split` carries no tuning table.
fn min_piece_cells() -> usize { content::tuning::StructureTuning::default().min_piece_cells as usize }

/// Union-find root of `x` with path compression; `None` on a corrupt forest (out of range or a cycle).
fn find(fills: &mut [Fill], x: u32) -> Option<u32> {
    let mut root = x;
    let mut steps = 0usize;
    loop {
        let p = fills.get(root as usize)?.parent;
        if p == root { break; }
        root = p;
        steps += 1;
        if steps > fills.len() { return None; }
    }
    let mut c = x;
    while c != root {
        let p = fills[c as usize].parent;
        fills[c as usize].parent = root;
        c = p;
    }
    Some(root)
}

/// Merge two roots into the lower index; returns the new root.
fn union(job: &mut SplitJob, a: u32, b: u32) -> u32 {
    let (r, o) = (a.min(b), a.max(b));
    let fo = job.fills[o as usize];
    let fr = job.fills[r as usize];
    if fr.open > 0 && fo.open > 0 { job.active = job.active.saturating_sub(1); }
    job.fills[o as usize].parent = r;
    let m = &mut job.fills[r as usize];
    m.count = m.count.saturating_add(fo.count);
    m.open = m.open.saturating_add(fo.open);
    m.lowest = m.lowest.min(fo.lowest);
    if job.root_fill == o { job.root_fill = r; }
    r
}

/// Job state that a keyframe or a changed grid could have made inconsistent.
fn job_consistent(job: &SplitJob, n: usize, dims: [usize; 3]) -> bool {
    if !job.in_progress { return true; }
    if job.phase > PHASE_FILL || job.destroyed.iter().any(|c| c.index() >= n) { return false; }
    if job.phase == PHASE_COMPARTMENTS { return true; }
    let words_ok = job.visited.len == n && job.visited.words.len() == n.div_ceil(64);
    words_ok && job.labels.len() == n && job.head as usize <= job.frontier.len()
        && job.frontier.iter().all(|&c| { let [x, y, z] = unpack(c); x < dims[0] && y < dims[1] && z < dims[2] }) && job.seeds.iter().all(|c| c.index() < n)
        && job.fills.iter().all(|f| (f.parent as usize) < job.fills.len())
        && (job.root_fill == NO_FILL || (job.root_fill as usize) < job.fills.len())
}

/// Forget the job; its cells go back to the pending list so a fresh job redoes them.
fn requeue(vessel: &mut Vessel, job: &mut SplitJob) {
    let mut cells = std::mem::take(&mut job.destroyed);
    cells.extend_from_slice(&vessel.pending_destroyed);
    cells.sort_unstable();
    cells.dedup();
    vessel.pending_destroyed = cells;
    *job = SplitJob::default();
}

/// Clear per-job state, keeping the zeroed label buffer.
fn reset(job: &mut SplitJob) {
    let mut labels = std::mem::take(&mut job.labels);
    for i in job.visited.iter_ones() { if let Some(l) = labels.get_mut(i) { *l = 0; } }
    *job = SplitJob { labels, ..SplitJob::default() };
}

pub(crate) fn step(vessel: &mut Vessel, job: &mut SplitJob, budget: u32) -> SplitResult {
    let mut res = SplitResult::default();
    let n = Solid::new(&vessel.grid).n;
    let dims = { let s = Solid::new(&vessel.grid); [s.sx, s.sy, s.sz] };
    if !job_consistent(job, n, dims) { requeue(vessel, job); }
    let mut used: u32 = 0;
    'jobs: while used < budget {
        if !job.in_progress {
            if vessel.pending_destroyed.is_empty() { break; }
            start(vessel, job, n);
        }
        if job.phase == PHASE_COMPARTMENTS {
            let (v, remains) = vessel.compartments.update_after_damage(&mut vessel.grid, &job.destroyed, budget - used);
            used = used.saturating_add(v.max(1));
            if remains { continue; }
            begin_seeding(vessel, job, n);
            used = used.saturating_add(job.destroyed.len() as u32);
        }
        if job.phase == PHASE_SEEDING {
            seed(vessel, job, budget, &mut used);
            if job.phase == PHASE_SEEDING { continue; }
        }
        let outcome = fill(vessel, job, budget, &mut used);
        match outcome {
            Progress::Running => continue,
            Progress::Corrupt => { requeue(vessel, job); continue; }
            Progress::Finished => {}
        }
        let (pieces, reroot) = finish(vessel, job);
        reset(job);
        if let Some(r) = reroot { vessel.root_cell = r; res.rerooted = Some(r); }
        let mut wreck = false;
        for p in pieces {
            if p.debris {
                let (_, w) = super::carve::detach(vessel, &p.cells, false);
                used = used.saturating_add(w);
            } else {
                wreck = true;
            }
            res.pieces.push(p);
        }
        if wreck { break 'jobs; }
    }
    res.pieces.sort_by_key(|p| p.cells.first().copied());
    res.cells_visited = used;
    res.unfinished = job.in_progress || !vessel.pending_destroyed.is_empty();
    res
}

enum Progress { Running, Finished, Corrupt }

fn start(vessel: &mut Vessel, job: &mut SplitJob, n: usize) {
    let mut cells = std::mem::take(&mut vessel.pending_destroyed);
    cells.retain(|c| c.index() < n);
    cells.sort_unstable();
    cells.dedup();
    let labels = std::mem::take(&mut job.labels);
    *job = SplitJob { labels, destroyed: cells, in_progress: true, phase: PHASE_COMPARTMENTS, root_fill: NO_FILL, ..SplitJob::default() };
}

fn begin_seeding(vessel: &Vessel, job: &mut SplitJob, n: usize) {
    let s = Solid::new(&vessel.grid);
    let late = late_cells(vessel, job, s.n);
    let mut seeds = Vec::new();
    let mut nb = [0usize; 6];
    for c in &job.destroyed {
        let i = c.index();
        if i >= s.n { continue; }
        let k = s.neighbours(i, &mut nb);
        for &q in &nb[..k] { if s.solid(q) || is_late(&late, q) { seeds.push(CellIndex(q as u32)); } }
    }
    seeds.sort_unstable();
    seeds.dedup();
    job.seeds = seeds;
    if job.labels.len() != n { job.labels = vec![0; n]; }
    job.visited = BitSet::new(n);
    job.frontier.clear();
    job.head = 0;
    job.fills.clear();
    job.active = 0;
    job.done.clear();
    job.done_max = 0;
    job.root_alive = s.solid(vessel.root_cell.index()) || is_late(&late, vessel.root_cell.index());
    job.root_fill = NO_FILL;
    job.phase = PHASE_SEEDING;
}

/// Start one fill per seed not already labelled, within the budget.
fn seed(vessel: &Vessel, job: &mut SplitJob, budget: u32, used: &mut u32) {
    let s = Solid::new(&vessel.grid);
    let late = late_cells(vessel, job, s.n);
    let root = vessel.root_cell.index();
    let mut k = 0;
    while k < job.seeds.len() && *used < budget {
        let c = job.seeds[k].index();
        k += 1;
        *used = used.saturating_add(1);
        if c >= job.labels.len() || job.labels[c] != 0 || !(s.solid(c) || is_late(&late, c)) { continue; }
        let id = job.fills.len() as u32;
        job.fills.push(Fill { parent: id, count: 1, open: 1, lowest: c as u32 });
        job.labels[c] = id + 1;
        job.visited.set(c, true);
        let [x, y, z] = s.xyz(c);
        job.frontier.push(pack(x, y, z));
        job.active += 1;
        if c == root && job.root_alive { job.root_fill = id; }
    }
    job.seeds.drain(..k);
    if job.seeds.is_empty() { job.phase = PHASE_FILL; }
}

/// The one fill still growing when exactly one is.
fn active_one(job: &mut SplitJob, dims: [usize; 3]) -> Option<u32> {
    let mut h = job.head as usize;
    while let Some(&c) = job.frontier.get(h) {
        let [x, y, z] = unpack(c);
        if x >= dims[0] || y >= dims[1] || z >= dims[2] { return None; }
        let l = *job.labels.get(x + dims[0] * (z + dims[2] * y))?;
        if l == 0 { return None; }
        let r = find(&mut job.fills, l - 1)?;
        if job.fills[r as usize].open > 0 { return Some(r); }
        h += 1;
    }
    None
}

fn finished(job: &mut SplitJob, dims: [usize; 3]) -> bool {
    match job.active {
        0 => true,
        1 => {
            let Some(a) = active_one(job, dims) else { return true };
            if job.root_alive {
                if job.root_fill == NO_FILL { return true; }
                find(&mut job.fills, job.root_fill) == Some(a)
            } else {
                job.fills[a as usize].count > job.done_max
            }
        }
        _ => false,
    }
}

/// Grow the fills breadth first until the job's end condition holds or the budget runs out.
fn fill(vessel: &Vessel, job: &mut SplitJob, budget: u32, used: &mut u32) -> Progress {
    let s = Solid::new(&vessel.grid);
    let lv = Live::new(&vessel.grid, &s);
    let dims = [lv.sx, lv.sy, lv.sz];
    let layer = lv.sx * lv.sz;
    let root = vessel.root_cell.index();
    let late = late_cells(vessel, job, lv.cells.len());
    let solid = |i: usize| lv.live(i) || is_late(&late, i);
    let mut nb = [(0usize, CellIndex(0)); 6];
    loop {
        if job.active <= 1 && finished(job, dims) { return Progress::Finished; }
        if *used >= budget { return Progress::Running; }
        let h = job.head as usize;
        let Some(&cell) = job.frontier.get(h) else { return Progress::Finished };
        job.head += 1;
        *used = used.saturating_add(1);
        let [x, y, z] = unpack(cell);
        let Some(c) = lv.index([x, y, z]) else { return Progress::Corrupt };
        let Some(&lab) = job.labels.get(c) else { return Progress::Corrupt };
        if lab == 0 { return Progress::Corrupt; }
        let Some(mut f) = find(&mut job.fills, lab - 1) else { return Progress::Corrupt };
        // Every labelled cell was solid when the job started, and the job works on that grid.
        {
            let mut k = 0;
            if x > 0 { nb[k] = (c - 1, pack(x - 1, y, z)); k += 1; }
            if x + 1 < lv.sx { nb[k] = (c + 1, pack(x + 1, y, z)); k += 1; }
            if y > 0 { nb[k] = (c - layer, pack(x, y - 1, z)); k += 1; }
            if y + 1 < lv.sy { nb[k] = (c + layer, pack(x, y + 1, z)); k += 1; }
            if z > 0 { nb[k] = (c - lv.sx, pack(x, y, z - 1)); k += 1; }
            if z + 1 < lv.sz { nb[k] = (c + lv.sx, pack(x, y, z + 1)); k += 1; }
            for &(q, packed) in &nb[..k] {
                if !solid(q) { continue; }
                let lq = job.labels[q];
                if lq == 0 {
                    job.labels[q] = f + 1;
                    job.visited.words[q / 64] |= 1 << (q % 64);
                    let fr = &mut job.fills[f as usize];
                    fr.count = fr.count.saturating_add(1);
                    fr.open = fr.open.saturating_add(1);
                    fr.lowest = fr.lowest.min(q as u32);
                    job.frontier.push(packed);
                    if q == root && job.root_alive && job.root_fill == NO_FILL { job.root_fill = f; }
                } else {
                    let Some(g) = find(&mut job.fills, lq - 1) else { return Progress::Corrupt };
                    if g != f { f = union(job, f, g); }
                }
            }
        }
        let fr = &mut job.fills[f as usize];
        fr.open = fr.open.saturating_sub(1);
        if fr.open == 0 {
            job.active = job.active.saturating_sub(1);
            job.done.push(f);
            job.done_max = job.done_max.max(fr.count);
        }
        // Drop the consumed queue prefix now and then so a long job does not hold it.
        if job.head as usize >= 4096 && job.head as usize * 2 >= job.frontier.len() {
            job.frontier.drain(..job.head as usize);
            job.head = 0;
        }
    }
}

/// Decide which fill keeps the vessel id and gather the pieces. Returns pieces ascending by lowest cell index
/// and the new root when the vessel id moved to another body.
fn finish(vessel: &Vessel, job: &mut SplitJob) -> (Vec<Piece>, Option<CellIndex>) {
    let s = Solid::new(&vessel.grid);
    // Finished roots (a root merged after finishing is dropped by the root test).
    let mut done: Vec<u32> = Vec::new();
    for k in 0..job.done.len() {
        let x = job.done[k];
        if find(&mut job.fills, x) == Some(x) && job.fills[x as usize].open == 0 { done.push(x); }
    }
    done.sort_unstable();
    done.dedup();
    let largest = |job: &SplitJob, set: &[u32]| -> Option<u32> {
        set.iter().copied().max_by(|&a, &b| {
            let (fa, fb) = (job.fills[a as usize], job.fills[b as usize]);
            fa.count.cmp(&fb.count).then(fb.lowest.cmp(&fa.lowest))
        })
    };
    let lv = Live::new(&vessel.grid, &s);
    let active = if job.active == 1 { active_one(job, [lv.sx, lv.sy, lv.sz]) } else { None };
    let root_fill = if job.root_fill == NO_FILL { None } else { find(&mut job.fills, job.root_fill) };
    let mut reroot = false;
    let main: Option<u32> = if job.root_alive {
        match (root_fill, active) {
            (Some(r), _) => Some(r),
            (None, Some(a)) => Some(a),
            (None, None) => {
                // Every touched body is cut off from a live root it never met: the largest keeps the id only
                // when it outweighs everything else the vessel still holds.
                let total: u64 = done.iter().map(|&d| job.fills[d as usize].count as u64).sum();
                let late = late_cells(vessel, job, s.n).map_or(0, |b| b.words.iter().map(|w| w.count_ones() as u64).sum::<u64>());
                match largest(job, &done) {
                    Some(l) => {
                        let rest = (vessel.grid.solid_count as u64 + late).saturating_sub(total);
                        if job.fills[l as usize].count as u64 > rest { reroot = true; Some(l) } else { None }
                    }
                    None => None,
                }
            }
        }
    } else {
        reroot = true;
        active.or_else(|| largest(job, &done))
    };
    let piece_roots: Vec<u32> = done.iter().copied().filter(|&d| Some(d) != main).collect();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut in_piece = BitSet::default();
    if !piece_roots.is_empty() {
        let mut slot = vec![u32::MAX; job.fills.len()];
        for (i, &r) in piece_roots.iter().enumerate() { slot[r as usize] = i as u32; }
        let mut cells: Vec<Vec<CellIndex>> = vec![Vec::new(); piece_roots.len()];
        let visited: Vec<usize> = job.visited.iter_ones().collect();
        for i in visited {
            if !lv.live(i) { continue; }
            let Some(&l) = job.labels.get(i) else { continue };
            if l == 0 { continue; }
            let Some(r) = find(&mut job.fills, l - 1) else { continue };
            let k = slot[r as usize];
            if k != u32::MAX { cells[k as usize].push(CellIndex(i as u32)); }
        }
        let min = min_piece_cells();
        in_piece = BitSet::new(s.n);
        for c in cells.into_iter().filter(|c| !c.is_empty()) {
            for x in &c { in_piece.set(x.index(), true); }
            pieces.push(Piece { debris: c.len() < min, cells: c });
        }
        pieces.sort_by_key(|p| p.cells[0]);
    }
    let new_root = if reroot { pick_root(&vessel.grid, vessel.dry_mass.com, Some(&in_piece)) } else { None };
    (pieces, new_root)
}

/// Keel cell nearest `com` in plan among live cells not in `exclude` (see `pick_root_cell`).
pub(crate) fn pick_root(grid: &VesselGrid, com: DVec3, exclude: Option<&BitSet>) -> Option<CellIndex> {
    let s = Solid::new(grid);
    if s.n == 0 { return None; }
    let centre = DVec3::new(s.sx as f64, s.sy as f64, s.sz as f64) * (0.5 * CELL_M);
    let com = if com.is_finite() { com } else { centre };
    let cols = s.sx * s.sz;
    let mut lowest = vec![u32::MAX; cols];
    // Ascending index is ascending y, so the first live cell met in a column is its lowest.
    for c in grid.solid_cells() {
        let i = c.index();
        if exclude.is_some_and(|e| e.get(i)) { continue; }
        let [x, _, z] = s.xyz(i);
        let col = x + s.sx * z;
        if lowest[col] == u32::MAX { lowest[col] = i as u32; }
    }
    // Columns whose bottom is equipment (a sonar dome, a keel cooler) are not keel unless nothing else is.
    let structural = |c: &u32| *c != u32::MAX && grid.design.cells[*c as usize].material != Material::PartBody;
    let any_structural = lowest.iter().any(structural);
    let mut best: Option<(f64, usize, u32)> = None;
    for &c in lowest.iter().filter(|c| if any_structural { structural(c) } else { **c != u32::MAX }) {
        let [x, y, z] = s.xyz(c as usize);
        let dx = (x as f64 + 0.5) * CELL_M - com.x;
        let dz = (z as f64 + 0.5) * CELL_M - com.z;
        let key = (dx * dx + dz * dz, y, c);
        let better = match best {
            None => true,
            Some(b) => key.0.total_cmp(&b.0).then(key.1.cmp(&b.1)).then(key.2.cmp(&b.2)).is_lt(),
        };
        if better { best = Some(key); }
    }
    best.map(|b| CellIndex(b.2))
}
