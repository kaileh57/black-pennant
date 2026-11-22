//! The loft voxelizer (L1923): shell field to 0.5 m shaped cells, hollowed to the plating thickness, with decks,
//! bulkheads and the default paint bands (L130).
//!
//! Rules, in order:
//! 1. A cell is hull when its centre is inside the shell (`|z| <= half_breadth`), so the cell solid is the
//!    nearest-cell rounding of the shell and a flat bottom or side lying on a lattice plane never grows a slab.
//! 2. Its shape comes from the inside/outside pattern of its 8 corners through
//!    [`crate::cell::shape_from_corners`] (nearest shape by Hamming distance, ties to cube). Corners are
//!    evaluated once on the lattice and shared by the 8 cells around them.
//! 3. Hull cells within `shell_cells` of the outside (chessboard distance, so diagonal runs stay face-connected
//!    and the plating is one connected piece) are steel; deeper cells are interior air. Water can never reach
//!    interior air through a face.
//! 4. A shaped cell whose face toward interior air is not fully covered by its shape keeps that neighbour as a
//!    steel cube, so no hull face opens into the interior.
//! 5. Planes fill interior air on their layer with one-cell steel cubes; they never replace plating.
//! 6. Paint: plating below the design waterline row is antifouling, the row itself is the boot-topping band,
//!    and above it plating that faces the sky (open above, plated outboard) is deck colour; the rest is the
//!    sides colour. Decks are deck colour, bulkheads the sides colour.
//! 7. The cells must be one face-connected piece, or the simulation would see parts of the hull floating free.
//!    Sampling can leave a sliver (a stem head on a steep rake, say) touching the rest by an edge only; each such
//!    piece is joined to the largest one by the shortest run of steel cubes through empty cells.
use std::collections::VecDeque;
use super::loft::ShellField;
use super::{Hull, HullShell, InternalPlane, VoxelizedHull, PAINT_ANTIFOULING, PAINT_BOOT_TOPPING, PAINT_DECK, PAINT_SIDES};
use crate::cell::{full_face_mask, shape_from_corners, Cell, CellGrid, CellShape, Face, Material, CELL_M};
use crate::cell::ArmourTier;
use crate::enums::Axis;

/// Per-cell state while voxelizing.
const OUTSIDE: u8 = 0;
const PLATING: u8 = 1;
const INTERIOR: u8 = 2;

pub(crate) fn voxelize_shell(shell: &HullShell, hull: &Hull) -> VoxelizedHull {
    let Some(field) = ShellField::new(shell) else {
        return VoxelizedHull { grid: CellGrid::default(), design_waterline_y: hull.design_waterline_y.max(0) };
    };
    let lay = field.layout();
    let [sx, sy, sz] = lay.size.map(|v| v as usize);
    let waterline = (hull.design_waterline_y as i32).clamp(0, sy as i32);
    if sx == 0 || sy == 0 || sz == 0 {
        return VoxelizedHull { grid: CellGrid::default(), design_waterline_y: waterline as i16 };
    }
    let idx = |x: usize, y: usize, z: usize| x + sx * (z + sz * y);
    let half = lay.half_width_cells as f64;

    // Corner lattice: half-breadth per (x, y) corner column, then inside bits per corner.
    let (cx, cy, cz) = (sx + 1, sy + 1, sz + 1);
    let mut corner_hb = vec![0.0f64; cx * cy];
    for j in 0..cy {
        let y = (j as f64 - lay.waterline_y as f64) * CELL_M;
        for i in 0..cx {
            let x = (i as f64 + lay.x_offset as f64) * CELL_M;
            corner_hb[i + cx * j] = field.half_breadth(x, y);
        }
    }
    let corner_abs_z: Vec<f64> = (0..cz).map(|k| ((k as f64 - half) * CELL_M).abs()).collect();
    let centre_abs_z: Vec<f64> = (0..sz).map(|k| ((k as f64 + 0.5 - half) * CELL_M).abs()).collect();
    let corner_in = |i: usize, j: usize, k: usize| corner_abs_z[k] <= corner_hb[i + cx * j];

    // Rule 1 and 2: hull cells and their shapes.
    let mut state = vec![OUTSIDE; sx * sy * sz];
    let mut shapes = vec![CellShape::CUBE; sx * sy * sz];
    for y in 0..sy {
        for x in 0..sx {
            let hb = field.centre_hb[x + sx * y];
            for z in 0..sz {
                if centre_abs_z[z] > hb {
                    continue;
                }
                let i = idx(x, y, z);
                state[i] = INTERIOR;
                let mut pattern = 0u8;
                for bit in 0..8u8 {
                    let (dx, dy, dz) = ((bit & 1) as usize, ((bit >> 1) & 1) as usize, ((bit >> 2) & 1) as usize);
                    if corner_in(x + dx, y + dy, z + dz) {
                        pattern |= 1 << bit;
                    }
                }
                shapes[i] = if pattern == 0xFF { CellShape::CUBE } else { shape_from_corners(pattern).unwrap_or(CellShape::CUBE) };
            }
        }
    }

    // Rule 3: chessboard dilation of the outside (out of grid counts as outside) by the plating thickness.
    let r = shell.shell_cells.max(1) as usize;
    let mut near_out: Vec<bool> = state.iter().map(|&s| s == OUTSIDE).collect();
    for axis in 0..3 {
        near_out = dilate(&near_out, [sx, sy, sz], axis, r);
    }
    for (s, &n) in state.iter_mut().zip(&near_out) {
        if *s == INTERIOR && n {
            *s = PLATING;
        }
    }

    // Rule 4: no shaped plating cell may expose a partial face to interior air.
    let dims = [sx as i32, sy as i32, sz as i32];
    let neighbour = |x: usize, y: usize, z: usize, f: Face| -> Option<usize> {
        let (dx, dy, dz) = f.offset();
        let (nx, ny, nz) = (x as i32 + dx as i32, y as i32 + dy as i32, z as i32 + dz as i32);
        if nx < 0 || ny < 0 || nz < 0 || nx >= dims[0] || ny >= dims[1] || nz >= dims[2] {
            return None;
        }
        Some(idx(nx as usize, ny as usize, nz as usize))
    };
    let mut seal = Vec::new();
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let i = idx(x, y, z);
                if state[i] != PLATING || shapes[i].is_cube() {
                    continue;
                }
                let full = full_face_mask(shapes[i]);
                for (b, f) in Face::ALL.iter().enumerate() {
                    if full & (1 << b) != 0 {
                        continue;
                    }
                    if let Some(n) = neighbour(x, y, z, *f) {
                        if state[n] == INTERIOR {
                            seal.push(n);
                        }
                    }
                }
            }
        }
    }
    for n in seal {
        state[n] = PLATING;
        shapes[n] = CellShape::CUBE;
    }

    // Rule 6 for plating, and the cells themselves.
    let mut grid = CellGrid::new(lay.size);
    let steel = |paint: u8, shape: CellShape| Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape };
    let is_out = |n: Option<usize>| n.is_none_or(|n| state[n] == OUTSIDE);
    let centre_z = lay.half_width_cells as usize;
    for y in 0..sy {
        for z in 0..sz {
            let outboard = if z >= centre_z { Face::ZPos } else { Face::ZNeg };
            for x in 0..sx {
                let i = idx(x, y, z);
                if state[i] != PLATING {
                    continue;
                }
                let paint = if (y as i32) < waterline {
                    PAINT_ANTIFOULING
                } else if y as i32 == waterline {
                    PAINT_BOOT_TOPPING
                } else if is_out(neighbour(x, y, z, Face::YPos)) && !is_out(neighbour(x, y, z, outboard)) {
                    PAINT_DECK
                } else {
                    PAINT_SIDES
                };
                grid.cells[i] = steel(paint, shapes[i]);
            }
        }
    }

    // Rule 5: planes through interior air.
    for plane in &hull.planes {
        insert_plane(&mut grid, &state, [sx, sy, sz], plane);
    }
    // Rule 7.
    bridge_pieces(&mut grid);
    VoxelizedHull { grid, design_waterline_y: waterline as i16 }
}

/// Fill the interior-air cells of one plane's layer with steel. A layer outside the grid holds no cells.
fn insert_plane(grid: &mut CellGrid, state: &[u8], [sx, sy, sz]: [usize; 3], plane: &InternalPlane) {
    let (paint, extent) = match plane.axis {
        Axis::Y => (PAINT_DECK, sy),
        Axis::X => (PAINT_SIDES, sx),
        Axis::Z => (PAINT_SIDES, sz),
    };
    if plane.offset < 0 || plane.offset as usize >= extent {
        return;
    }
    let o = plane.offset as usize;
    let cell = Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape: CellShape::CUBE };
    let (xs, ys, zs) = match plane.axis {
        Axis::X => (o..o + 1, 0..sy, 0..sz),
        Axis::Y => (0..sx, o..o + 1, 0..sz),
        Axis::Z => (0..sx, 0..sy, o..o + 1),
    };
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                let i = x + sx * (z + sz * y);
                if state[i] == INTERIOR && grid.cells[i].is_empty() {
                    grid.cells[i] = cell;
                }
            }
        }
    }
}

/// Dilate a boolean volume by `r` cells along one axis; positions beyond the grid count as set.
fn dilate(src: &[bool], [sx, sy, sz]: [usize; 3], axis: usize, r: usize) -> Vec<bool> {
    let n = [sx, sy, sz][axis];
    let stride = match axis {
        0 => 1,
        1 => sx * sz,
        _ => sx,
    };
    let mut out = vec![false; src.len()];
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let i = x + sx * (z + sz * y);
                let p = [x, y, z][axis];
                if p < r || p + r >= n {
                    out[i] = true;
                    continue;
                }
                let base = i - p * stride;
                out[i] = (p - r..=p + r).any(|q| src[base + q * stride]);
            }
        }
    }
    out
}

/// Face neighbours of linear index `i` in a grid of `size`, in [`Face::ALL`] order.
fn face_neighbours(i: usize, [sx, sy, sz]: [usize; 3]) -> impl Iterator<Item = usize> {
    let (x, z, y) = (i % sx, (i / sx) % sz, i / (sx * sz));
    let plane = sx * sz;
    [
        (x > 0).then(|| i - 1),
        (x + 1 < sx).then(|| i + 1),
        (y > 0).then(|| i - plane),
        (y + 1 < sy).then(|| i + plane),
        (z > 0).then(|| i - sx),
        (z + 1 < sz).then(|| i + sx),
    ]
    .into_iter()
    .flatten()
}

/// Join every face-connected piece of solid cells to the largest by the shortest path of steel cubes through
/// empty cells, mirrored across the centreline. Deterministic: pieces are labelled and searched in index order.
fn bridge_pieces(grid: &mut CellGrid) {
    let size = grid.size.map(|v| v as usize);
    let n = grid.cells.len();
    // Each pass joins at least one piece, so the pass count is bounded by the piece count.
    for _ in 0..n {
        // Label pieces.
        let mut label = vec![u32::MAX; n];
        let mut sizes: Vec<usize> = Vec::new();
        let mut queue = VecDeque::new();
        for start in 0..n {
            if label[start] != u32::MAX || grid.cells[start].is_empty() {
                continue;
            }
            let id = sizes.len() as u32;
            label[start] = id;
            queue.push_back(start);
            let mut count = 0;
            while let Some(i) = queue.pop_front() {
                count += 1;
                for j in face_neighbours(i, size) {
                    if label[j] == u32::MAX && !grid.cells[j].is_empty() {
                        label[j] = id;
                        queue.push_back(j);
                    }
                }
            }
            sizes.push(count);
        }
        if sizes.len() <= 1 {
            return;
        }
        let main = sizes.iter().enumerate().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0))).map(|(k, _)| k as u32).unwrap_or(0);
        // Breadth-first from the main piece through empty cells until another piece is touched.
        const ROOT: usize = usize::MAX;
        let mut parent = vec![usize::MAX - 1; n];
        queue.clear();
        for i in 0..n {
            if label[i] == main {
                parent[i] = ROOT;
                queue.push_back(i);
            }
        }
        let mut joined = false;
        'search: while let Some(i) = queue.pop_front() {
            for j in face_neighbours(i, size) {
                if label[j] != u32::MAX && label[j] != main {
                    // Fill the empty cells on the path back to the main piece with the reached piece's paint.
                    let fill = Cell { shape: CellShape::CUBE, ..grid.cells[j] };
                    let steel = Cell { material: Material::Steel, armour: ArmourTier::None, ..fill };
                    let mut k = i;
                    while label[k] == u32::MAX {
                        // Fill the mirror cell too, so the hull stays exactly symmetric.
                        let (x, z, y) = (k % size[0], (k / size[0]) % size[2], k / (size[0] * size[2]));
                        let m = x + size[0] * ((size[2] - 1 - z) + size[2] * y);
                        grid.cells[k] = steel;
                        if grid.cells[m].is_empty() {
                            grid.cells[m] = steel;
                        }
                        k = parent[k];
                    }
                    joined = true;
                    break 'search;
                }
                if label[j] == u32::MAX && parent[j] == usize::MAX - 1 {
                    parent[j] = i;
                    queue.push_back(j);
                }
            }
        }
        if !joined {
            return;
        }
    }
}
