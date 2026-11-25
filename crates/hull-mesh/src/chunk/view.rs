//! Panic-free accessors over a `CellGridView`, the class mapping and the Bare (exposed interior) rule.
use super::class;
use crate::types::MaterialClass;
use content::cell::{CellGridView, CellShape, Material, BUCKET_DESTROYED, MAX_GRID_DIM};

pub(crate) const DIRS: [[i32; 3]; 6] = [[-1, 0, 0], [1, 0, 0], [0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1]];

/// Read-only cell access with bounds checks everywhere: a short or mismatched slice reads as defaults.
pub(crate) struct Cells<'a> {
    pub v: CellGridView<'a>,
    pub s: [i32; 3],
    pub len: usize,
}

impl<'a> Cells<'a> {
    pub fn new(v: CellGridView<'a>) -> Self {
        let s = [v.size[0] as i32, v.size[1] as i32, v.size[2] as i32];
        Self { v, s, len: v.size[0] as usize * v.size[1] as usize * v.size[2] as usize }
    }

    /// The view is meshable: size within bounds and every slice the right length (see `validate_input`).
    pub fn well_formed(&self) -> bool {
        let v = &self.v;
        v.size.iter().all(|&d| d <= MAX_GRID_DIM)
            && v.cells.len() == self.len
            && (v.damage.is_empty() || v.damage.len() == self.len)
            && (v.scorch.is_empty() || v.scorch.len() == self.len)
            && (v.sealed_air.is_empty() || v.sealed_air.len() == self.len.div_ceil(64))
    }

    #[inline]
    pub fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        if x < 0 || y < 0 || z < 0 || x >= self.s[0] || y >= self.s[1] || z >= self.s[2] {
            return None;
        }
        Some(x as usize + self.s[0] as usize * (z as usize + self.s[2] as usize * y as usize))
    }

    /// Cell position of a linear index, `None` past the end.
    pub fn pos(&self, i: usize) -> Option<[i32; 3]> {
        if i >= self.len {
            return None;
        }
        let (sx, sz) = (self.s[0] as usize, self.s[2] as usize);
        Some([(i % sx) as i32, (i / (sx * sz)) as i32, ((i / sx) % sz) as i32])
    }

    #[inline]
    pub fn material(&self, i: usize) -> Material { self.v.cells.get(i).map_or(Material::Empty, |c| c.material) }
    #[inline]
    pub fn bucket(&self, i: usize) -> u8 { self.v.damage.get(i).copied().unwrap_or(0) }
    #[inline]
    pub fn sealed(&self, i: usize) -> bool { self.v.sealed_air.get(i / 64).is_some_and(|w| (w >> (i % 64)) & 1 == 1) }
    #[inline]
    pub fn scorch(&self, i: usize) -> u8 { self.v.scorch.get(i).copied().unwrap_or(0) }
    /// Material present in the blueprint (destroyed cells included).
    #[inline]
    pub fn has_material(&self, i: usize) -> bool { self.material(i) != Material::Empty }
    /// A cell that exists now: material present and not destroyed.
    #[inline]
    pub fn solid(&self, i: usize) -> bool { self.has_material(i) && self.bucket(i) < BUCKET_DESTROYED }

    /// Class-buffer value of a cell; `None` (outside the grid) is exposed air.
    pub fn class(&self, i: Option<usize>) -> u16 {
        let Some(i) = i else { return class::EXPOSED_AIR };
        let Some(c) = self.v.cells.get(i) else { return class::EXPOSED_AIR };
        if c.material == Material::Empty {
            return if self.sealed(i) { class::SEALED_AIR } else { class::EXPOSED_AIR };
        }
        if self.bucket(i) >= BUCKET_DESTROYED {
            return class::EXPOSED_AIR;
        }
        if !c.shape.is_cube() {
            // Canonical byte: invalid orientations 24..=31 read as 0 (as `orientation_matrix` does), so equal
            // solids always carry equal class values.
            let o = c.shape.orientation();
            let canon = CellShape::new(c.shape.kind(), if o < 24 { o } else { 0 });
            return class::SHAPED_BASE | canon.0 as u16;
        }
        if c.material == Material::Glass { class::GLASS } else { class::OPAQUE }
    }

    /// An empty-material cell from which a straight run of empty-material cells along `d` leaves the grid.
    fn ray_escapes(&self, p: [i32; 3], d: [i32; 3]) -> bool {
        let mut q = p;
        loop {
            q = [q[0] + d[0], q[1] + d[1], q[2] + d[2]];
            match self.index(q[0], q[1], q[2]) {
                None => return true,
                Some(i) if self.has_material(i) => return false,
                Some(_) => {}
            }
        }
    }

    /// True when an empty-material cell is boxed in by blueprint material along all six axis rays: air that was
    /// inside the hull in the intact design. Depends on materials only, never on damage or sealed state, so the
    /// dirty-chunk rules stay local. `first` is tried first (the outward direction usually escapes at once).
    pub fn enclosed(&self, p: [i32; 3], first: usize) -> bool {
        if self.ray_escapes(p, DIRS[first]) {
            return false;
        }
        (0..6).filter(|&k| k != first).all(|k| !self.ray_escapes(p, DIRS[k]))
    }
}

/// Memo of `Cells::enclosed` over a box of cells (0 unknown, 1 open, 2 enclosed).
pub(crate) struct EnclosureCache {
    origin: [i32; 3],
    edge: i32,
    state: Vec<u8>,
}

impl EnclosureCache {
    pub fn new(origin: [i32; 3], edge: i32) -> Self { Self { origin, edge, state: vec![0; (edge * edge * edge) as usize] } }
    fn slot(&self, p: [i32; 3]) -> Option<usize> {
        let l = [p[0] - self.origin[0], p[1] - self.origin[1], p[2] - self.origin[2]];
        if l.iter().any(|&c| c < 0 || c >= self.edge) {
            return None;
        }
        Some((l[0] + self.edge * (l[1] + self.edge * l[2])) as usize)
    }
}

fn enclosed_cached(g: &Cells<'_>, p: [i32; 3], first: usize, cache: &mut Option<&mut EnclosureCache>) -> bool {
    if let Some(c) = cache.as_deref_mut() {
        if let Some(s) = c.slot(p) {
            match c.state[s] {
                1 => return false,
                2 => return true,
                _ => {
                    let e = g.enclosed(p, first);
                    c.state[s] = if e { 2 } else { 1 };
                    return e;
                }
            }
        }
    }
    g.enclosed(p, first)
}

/// Material class of the cell at `p` (index `i`).
///
/// Bare rule (L1941, "exposed interiors show primer"): a solid cell is Bare when no face neighbour was outside
/// air in the intact design and at least one face neighbour is now exposed through damage. "Outside air in the
/// intact design" is a neighbour outside the grid or an empty-material cell that is not enclosed (some axis ray
/// leaves the grid through empty cells). "Exposed through damage" is a destroyed neighbour, or (live vessels
/// only, recognised by a non-empty sealed-air slice) an unsealed empty neighbour that is enclosed, i.e. air of a
/// breached or entered compartment. Editor views carry no damage and no sealed slice, so they never show primer.
pub(crate) fn classify(g: &Cells<'_>, p: [i32; 3], i: usize, mut cache: Option<&mut EnclosureCache>) -> MaterialClass {
    if !g.solid(i) {
        return MaterialClass::Paint;
    }
    if g.material(i) == Material::Glass {
        return MaterialClass::Glass;
    }
    let live = !g.v.sealed_air.is_empty();
    let mut nb: [Option<usize>; 6] = [None; 6];
    let mut candidate = false;
    for (k, d) in DIRS.iter().enumerate() {
        match g.index(p[0] + d[0], p[1] + d[1], p[2] + d[2]) {
            // Touches the grid boundary: that face was outside air in the intact design.
            None => return MaterialClass::Paint,
            Some(n) => {
                nb[k] = Some(n);
                if g.has_material(n) {
                    candidate |= g.bucket(n) >= BUCKET_DESTROYED;
                } else {
                    candidate |= live && !g.sealed(n);
                }
            }
        }
    }
    if !candidate {
        return MaterialClass::Paint;
    }
    let mut newly_exposed = false;
    for (k, d) in DIRS.iter().enumerate() {
        let Some(n) = nb[k] else { continue };
        if g.has_material(n) {
            newly_exposed |= g.bucket(n) >= BUCKET_DESTROYED;
            continue;
        }
        let q = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
        if !enclosed_cached(g, q, k, &mut cache) {
            return MaterialClass::Paint;
        }
        newly_exposed |= live && !g.sealed(n);
    }
    if newly_exposed { MaterialClass::Bare } else { MaterialClass::Paint }
}

/// Stateless 5-bit wear seed of a cell (L1940: weathering seeded per ship and cell).
pub(crate) fn wear_bits(seed: u64, cell: usize) -> u8 {
    fn mix(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    (mix(seed ^ mix(cell as u64)) >> 59) as u8
}
