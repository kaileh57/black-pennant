//! Run-length rows to dense grid and back.
use super::{Cell, CellGrid, CellRow, CellRows, CellRun, MAX_GRID_DIM};

/// Expand rows into a dense grid sized to the bounding box of their solid cells.
///
/// Untrusted input never panics: runs of empty cells and zero-length runs are ignored, cells with a negative
/// coordinate or at or beyond [`MAX_GRID_DIM`] on any axis are clipped (validation reports out-of-range designs),
/// and where runs overlap the later one in file order wins.
pub fn from_rows(rows: &CellRows) -> CellGrid {
    let dim = MAX_GRID_DIM as i32;
    // Clipped [x0, x1) span of a run, or None when nothing of it lands in the grid.
    let span = |row: &CellRow, run: &CellRun| -> Option<(i32, i32)> {
        if run.cell.is_empty() || run.len == 0 || row.y < 0 || row.z < 0 || row.y as i32 >= dim || row.z as i32 >= dim {
            return None;
        }
        let x0 = (run.x as i32).max(0);
        let x1 = (run.x as i32 + run.len as i32).min(dim);
        (x0 < x1).then_some((x0, x1))
    };
    let mut size = [0u16; 3];
    for row in &rows.0 {
        for run in &row.runs {
            if let Some((_, x1)) = span(row, run) {
                size[0] = size[0].max(x1 as u16);
                size[1] = size[1].max(row.y as u16 + 1);
                size[2] = size[2].max(row.z as u16 + 1);
            }
        }
    }
    let mut grid = CellGrid::new(size);
    let (sx, sz) = (size[0] as usize, size[2] as usize);
    for row in &rows.0 {
        for run in &row.runs {
            if let Some((x0, x1)) = span(row, run) {
                let base = sx * (row.z as usize + sz * row.y as usize);
                grid.cells[base + x0 as usize..base + x1 as usize].fill(run.cell);
            }
        }
    }
    grid
}

/// Canonical rows: ascending (y, z), maximal runs of identical solid cells ascending by x, empty rows omitted.
/// A grid whose `cells` length disagrees with `size` only emits the cells both describe.
pub fn to_rows(grid: &CellGrid) -> CellRows {
    let (sx, sy, sz) = (grid.size[0] as usize, grid.size[1] as usize, grid.size[2] as usize);
    let mut out = Vec::new();
    if sx == 0 {
        return CellRows(out);
    }
    for y in 0..sy {
        for z in 0..sz {
            let base = sx * (z + sz * y);
            let Some(line) = grid.cells.get(base..base + sx) else { return CellRows(out) };
            let mut runs: Vec<CellRun> = Vec::new();
            let mut x = 0;
            while x < sx {
                let cell: Cell = line[x];
                let start = x;
                while x < sx && line[x] == cell {
                    x += 1;
                }
                if !cell.is_empty() {
                    runs.push(CellRun { x: start as i16, len: (x - start) as u16, cell });
                }
            }
            if !runs.is_empty() {
                out.push(CellRow { y: y as i16, z: z as i16, runs });
            }
        }
    }
    CellRows(out)
}
