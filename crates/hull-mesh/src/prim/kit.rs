//! Unit `hm-prim` (crate-private). Assembly helpers shared by the part generators: tone setting, parameter
//! sanitising, footprint sizes and the [`Kit`] that collects pieces and anchors into a [`PartModel`].
use super::{normalize_or, MeshBuilder};
use crate::parts::{Anchor, AnchorKind, Generator, Joint, PartModel, PartPiece, PieceRole, Tone};
use crate::types::{Aabb, MeshData, CELL_M_F32};

/// Set the builder's colour from a [`Tone`] (A = paint mask) and its `custom0` channel: emission strength in x
/// (lamps 1.0, on the L1955 ladder) and the glass flag in y. Plain tones write no `custom0` (zero-padded only if
/// the piece also holds glass or a lamp).
pub(crate) fn set_tone(b: &mut MeshBuilder, t: Tone) {
    b.color = t.rgba();
    b.custom = match t {
        Tone::Glass => Some([0.0, 1.0, 0.0, 0.0]),
        Tone::Lamp => Some([1.0, 0.0, 0.0, 0.0]),
        _ => None,
    };
}

/// A builder preset to one tone.
pub(crate) fn builder(t: Tone) -> MeshBuilder { let mut b = MeshBuilder::new(); set_tone(&mut b, t); b }

/// Generator parameters made safe for direct callers of a family `build`: missing or non-finite values take the
/// documented default, values are clamped into the documented range and integer parameters are rounded.
/// `build_model` already resolves and validates; this keeps a direct call with bad data from misbehaving.
pub(crate) fn sanitize(g: Generator, params: &[f32]) -> Vec<f32> {
    g.spec().params.iter().enumerate().map(|(i, ps)| {
        let v = params.get(i).copied().filter(|v| v.is_finite()).unwrap_or(ps.default);
        let v = v.clamp(ps.min, ps.max);
        if ps.integer { libm::roundf(v) } else { v }
    }).collect()
}

/// Footprint extents in metres; a zero cell count is treated as one cell.
pub(crate) fn foot_m(footprint: [u8; 3]) -> [f32; 3] {
    [footprint[0].max(1) as f32 * CELL_M_F32, footprint[1].max(1) as f32 * CELL_M_F32, footprint[2].max(1) as f32 * CELL_M_F32]
}

/// Collects pieces and anchors; parents always precede children because a piece can only name an index that
/// already exists.
#[derive(Default)]
pub(crate) struct Kit { pieces: Vec<PartPiece>, anchors: Vec<Anchor> }
impl Kit {
    pub(crate) fn new() -> Self { Self::default() }

    /// Add a piece and return its index. A parent index that does not exist yet is dropped (root piece).
    pub(crate) fn piece(&mut self, role: PieceRole, parent: Option<u8>, pivot: [f32; 3], joint: Joint, mesh: MeshData) -> u8 {
        let parent = parent.filter(|&p| (p as usize) < self.pieces.len());
        let idx = self.pieces.len().min(u8::MAX as usize) as u8;
        if self.pieces.len() < u8::MAX as usize { self.pieces.push(PartPiece { role, parent, pivot, joint, mesh }); }
        idx
    }

    /// Add an anchor in `piece`'s frame; `dir` is normalised (+X if degenerate).
    pub(crate) fn anchor(&mut self, kind: AnchorKind, piece: u8, pos: [f32; 3], dir: [f32; 3]) {
        if (piece as usize) < self.pieces.len() {
            self.anchors.push(Anchor { kind, piece, pos, dir: normalize_or(dir, [1.0, 0.0, 0.0]) });
        }
    }

    /// Rest-pose offset of a piece's frame in part-local metres (sum of pivots up the parent chain).
    pub(crate) fn offset(pieces: &[PartPiece], mut i: usize) -> [f32; 3] {
        let mut o = [0.0f32; 3];
        let mut guard = 0;
        loop {
            let p = &pieces[i];
            o = [o[0] + p.pivot[0], o[1] + p.pivot[1], o[2] + p.pivot[2]];
            match p.parent { Some(q) if (q as usize) < i && guard < 256 => { i = q as usize; guard += 1; } _ => break }
        }
        o
    }

    pub(crate) fn finish(self) -> PartModel {
        let mut aabb = Aabb::EMPTY;
        for i in 0..self.pieces.len() {
            let o = Self::offset(&self.pieces, i);
            for p in &self.pieces[i].mesh.positions { aabb.include([p[0] + o[0], p[1] + o[1], p[2] + o[2]]); }
        }
        if aabb.is_empty() { aabb = Aabb::default(); }
        PartModel { pieces: self.pieces, anchors: self.anchors, aabb }
    }
}

/// The painted block filling the footprint, drawn when a family builder receives a generator from another
/// family (a caller error; `build_model` never does this).
pub(crate) fn footprint_block(footprint: [u8; 3]) -> PartModel {
    let f = foot_m(footprint);
    let mut b = builder(Tone::Paint);
    b.cuboid([0.0; 3], f);
    let mut kit = Kit::new();
    kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.finish()
}

/// A flat `sides`-gon disc at `c` in the YZ plane facing +X (bores, tube mouths, lenses).
pub(crate) fn disc_x(b: &mut MeshBuilder, c: [f32; 3], r: f32, sides: u8) {
    let n = sides.clamp(3, 32) as usize;
    let start = core::f32::consts::FRAC_PI_2 + core::f32::consts::PI / n as f32;
    let pts: Vec<[f32; 3]> = (0..n).map(|i| {
        let a = start + core::f32::consts::TAU * i as f32 / n as f32;
        [c[0], c[1] + r * libm::sinf(a), c[2] + r * libm::cosf(a)]
    }).collect();
    b.face(&pts, [1.0, 0.0, 0.0]);
}

/// Ring of `n` points on an axis-aligned ellipse in the XZ plane at height `y` (for lofts), starting at angle
/// `start` (radians from +X toward +Z).
pub(crate) fn ellipse_ring(cx: f32, y: f32, cz: f32, rx: f32, rz: f32, n: usize, start: f32) -> Vec<[f32; 3]> {
    (0..n.max(3)).map(|i| {
        let a = start + core::f32::consts::TAU * i as f32 / n.max(3) as f32;
        [cx + rx * libm::cosf(a), y, cz + rz * libm::sinf(a)]
    }).collect()
}

/// Ring of `n` points on a circle in the YZ plane at `x` (for lofts along X: sonar domes, bodies).
pub(crate) fn ring_x(x: f32, cy: f32, cz: f32, ry: f32, rz: f32, n: usize) -> Vec<[f32; 3]> {
    let n = n.max(3);
    let start = core::f32::consts::FRAC_PI_2 + core::f32::consts::PI / n as f32;
    (0..n).map(|i| {
        let a = start + core::f32::consts::TAU * i as f32 / n as f32;
        [x, cy + ry * libm::sinf(a), cz + rz * libm::cosf(a)]
    }).collect()
}

/// Vertical plate of thickness `t` standing on the plan segment `p0`-`p1` (x, z) from `y0` to `y1`: shields,
/// cheeks, panels.
pub(crate) fn vplate(b: &mut MeshBuilder, p0: [f32; 2], p1: [f32; 2], y0: f32, y1: f32, t: f32) {
    let (dx, dz) = (p1[0] - p0[0], p1[1] - p0[1]);
    let l = (dx * dx + dz * dz).sqrt();
    if l.is_nan() || l <= 1e-6 { return; }
    let (nx, nz) = (-dz / l * t * 0.5, dx / l * t * 0.5);
    let quad = |y: f32| vec![[p0[0] + nx, y, p0[1] + nz], [p1[0] + nx, y, p1[1] + nz], [p1[0] - nx, y, p1[1] - nz], [p0[0] - nx, y, p0[1] - nz]];
    b.loft(&[quad(y0.min(y1)), quad(y0.max(y1))], true, true);
}
