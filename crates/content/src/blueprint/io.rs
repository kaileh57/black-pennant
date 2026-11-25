//! Unit `content-blueprint`. Canonical serialisation, hashing, loading and derived forms of blueprints.
//! DESIGN.md L2079 (hash), L2081 (limits), L2083 (unknown fields, newer formats), L2076 (paint clamps), L945.
use super::{Blueprint, BlueprintKind, ExteriorBlueprint, Header, PaintEntry, PaintUse, PartPlacement, BLUEPRINT_FORMAT, DEFAULT_LICENCE, MAX_PAINT};
use crate::catalogue::Catalogue;
use crate::cell::{full_face_mask, ArmourTier, Cell, CellGrid, CellPos, CellRow, CellRows, CellRun, CellShape, Face, Material, MAX_GRID_DIM};
use crate::error::ContentResult;
use crate::hull::{Hull, HullSource};
use crate::ids::{ContentHash, PartSlot};
use crate::kit::{canonicalize_kit, kit_hash, normalize_rot, rotate_offset, text, MAX_LINEAGE};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Canonical RON: the game serializer's fixed field order, LF line endings, no comments, `hash` field empty.
/// Byte-identical for equal blueprints on every platform. Unit: `content-blueprint`.
pub fn to_canonical_ron(bp: &Blueprint) -> String {
    let mut b = bp.clone();
    b.header.hash = ContentHash::EMPTY;
    text::to_canonical(&b)
}

/// BLAKE3 of [`to_canonical_ron`] (L2079). Unit: `content-blueprint`.
pub fn compute_hash(bp: &Blueprint) -> ContentHash { blake3_hash(to_canonical_ron(bp).as_bytes()) }

/// BLAKE3 of arbitrary bytes; the one hash primitive used for blueprints, kits, saves and join codes.
pub fn blake3_hash(bytes: &[u8]) -> ContentHash { ContentHash(*blake3::hash(bytes).as_bytes()) }

/// File text to write: canonical RON with the hash field filled in. Unit: `content-blueprint`.
pub fn to_file_text(bp: &Blueprint) -> String {
    let mut b = bp.clone();
    b.header.hash = compute_hash(bp);
    text::to_canonical(&b)
}

/// Load untrusted blueprint text: enforce [`super::MAX_RON_BYTES`] and [`super::MAX_RON_DEPTH`] before parsing;
/// read the `format` field first and refuse a newer format naming the release it needs; run migrations
/// ([`crate::migrate`]); parse with unknown fields rejected; re-apply paint clamps; recompute the hash (a
/// mismatched stored hash is recomputed, not an error). Does not run ruleset validation; callers follow with
/// [`crate::validate::validate_blueprint`]. Unit: `content-blueprint`.
///
/// Removed parts are migrated against the process-wide [`crate::catalogue::catalogue`]; see
/// [`load_blueprint_with`] to name another catalogue.
pub fn load_blueprint(ron_text: &str, file: &str) -> ContentResult<LoadedBlueprint> { load_blueprint_with(ron_text, file, crate::catalogue::catalogue()) }

/// [`load_blueprint`] against a given catalogue (tests, a server running a part pack). After parsing it
/// replaces removed parts ([`crate::migrate::replace_removed_parts`]) and applies
/// [`canonicalize_blueprint`], then recomputes the hash.
pub fn load_blueprint_with(ron_text: &str, file: &str, catalogue: &Catalogue) -> ContentResult<LoadedBlueprint> {
    text::check_untrusted(ron_text)?;
    let from_format = crate::migrate::peek_format(ron_text).map_err(|e| text::rename_file(e, file))?;
    let migrated = crate::migrate::migrate_text(ron_text, crate::migrate::BLUEPRINT_MIGRATIONS, from_format, BLUEPRINT_FORMAT)?;
    let mut bp: Blueprint = crate::data::parse_ron(&migrated, file)?;
    let stored = bp.header.hash;
    let removed_parts = crate::migrate::replace_removed_parts(&mut bp, catalogue);
    canonicalize_blueprint(&mut bp);
    bp.header.hash = compute_hash(&bp);
    Ok(LoadedBlueprint { rehashed: stored != bp.header.hash, blueprint: bp, from_format, removed_parts })
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedBlueprint {
    pub blueprint: Blueprint,
    /// Format the file was written in, before migration.
    pub from_format: u32,
    /// Parts removed by migrations with no replacement; each becomes a Ship's Note (L2083).
    pub removed_parts: Vec<crate::migrate::RemovedPart>,
    /// True when the stored hash did not match and was recomputed.
    pub rehashed: bool,
}

/// Re-canonicalise a blueprint the way every load does (L2079, L2076): cell rows in canonical run-length form
/// (only when every solid cell lies inside the grid bound; an out-of-range design is left for validation to
/// report), paint clamps re-applied, rotations reduced, the lineage cut to its [`MAX_LINEAGE`] most recent
/// (last) entries, and embedded kits canonicalised and rehashed. Idempotent. Does not touch `header.hash`.
pub fn canonicalize_blueprint(bp: &mut Blueprint) {
    if let Some(rows) = normalize_rows(&bp.cells) {
        bp.cells = rows;
    }
    for p in &mut bp.paint {
        *p = clamp_paint(p);
    }
    for p in &mut bp.parts {
        p.rot = normalize_rot(p.rot);
    }
    for k in &mut bp.kits {
        k.rot = normalize_rot(k.rot);
    }
    if bp.header.lineage.len() > MAX_LINEAGE {
        let cut = bp.header.lineage.len() - MAX_LINEAGE;
        bp.header.lineage.drain(..cut);
    }
    for kit in bp.embedded_kits.values_mut() {
        canonicalize_kit(kit);
        kit.hash = kit_hash(kit);
    }
}

/// Canonical rows without building the dense grid (a tiny hostile file can describe a 384^3 box): rows grouped
/// by (y, z) ascending, later runs win where they overlap, empty runs ignored, maximal runs ascending by x.
/// Equal to `CellGrid::from_rows(rows).to_rows()` whenever it returns `Some`. `None` when a solid run leaves
/// `0..MAX_GRID_DIM` on any axis.
fn normalize_rows(rows: &CellRows) -> Option<CellRows> {
    let dim = MAX_GRID_DIM as i32;
    let mut by_row: BTreeMap<(i16, i16), Vec<&CellRun>> = BTreeMap::new();
    for row in &rows.0 {
        for run in &row.runs {
            if run.cell.is_empty() || run.len == 0 {
                continue;
            }
            let x1 = run.x as i32 + run.len as i32;
            if row.y < 0 || row.z < 0 || row.y as i32 >= dim || row.z as i32 >= dim || run.x < 0 || x1 > dim {
                return None;
            }
            by_row.entry((row.y, row.z)).or_default().push(run);
        }
    }
    let mut line = vec![Cell::EMPTY; MAX_GRID_DIM as usize];
    let mut out = Vec::with_capacity(by_row.len());
    for ((y, z), runs) in by_row {
        let mut hi = 0usize;
        for run in &runs {
            let (x0, x1) = (run.x as usize, run.x as usize + run.len as usize);
            line[x0..x1].fill(run.cell);
            hi = hi.max(x1);
        }
        let mut merged = Vec::new();
        let mut x = 0usize;
        while x < hi {
            let cell = line[x];
            let start = x;
            while x < hi && line[x] == cell {
                x += 1;
            }
            if !cell.is_empty() {
                merged.push(CellRun { x: start as i16, len: (x - start) as u16, cell });
            }
        }
        line[..hi].fill(Cell::EMPTY);
        if !merged.is_empty() {
            out.push(CellRow { y, z, runs: merged });
        }
    }
    Some(CellRows(out))
}

/// Footprint cells of a placement that land inside the cell frame (`0..MAX_GRID_DIM` on every axis).
fn placed_cells(p: &PartPlacement, catalogue: &Catalogue, out: &mut Vec<CellPos>) {
    out.clear();
    let Some(def) = catalogue.part(&p.part) else { return };
    let size = def.size;
    let dim = MAX_GRID_DIM as i32;
    for x in 0..size[0] {
        for y in 0..size[1] {
            for z in 0..size[2] {
                let o = rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, p.rot);
                let c = [p.pos.x as i32 + o.x as i32, p.pos.y as i32 + o.y as i32, p.pos.z as i32 + o.z as i32];
                if c.iter().all(|v| (0..dim).contains(v)) {
                    out.push(CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16));
                }
            }
        }
    }
}

/// Expand the blueprint's structure cells and stamp every placed part's footprint as `Material::PartBody`.
/// This dense grid is what the simulation, the gauges and hull-mesh consume. Unit: `content-blueprint`.
///
/// The grid grows to cover parts outside the structure (a mast above the deckhouse). Parts stamp in ascending
/// slot order with a cube shape; a part cell keeps the armour and paint of the structure cell it replaces.
/// Parts the catalogue does not know and footprint cells outside `0..MAX_GRID_DIM` are skipped (validation
/// reports both).
pub fn build_grid(bp: &Blueprint, catalogue: &Catalogue) -> CellGrid {
    let base = CellGrid::from_rows(&bp.cells);
    let mut stamps: Vec<Vec<CellPos>> = Vec::with_capacity(bp.parts.len());
    let mut size = base.size;
    let mut buf = Vec::new();
    for p in &bp.parts {
        placed_cells(p, catalogue, &mut buf);
        for c in &buf {
            size[0] = size[0].max(c.x as u16 + 1);
            size[1] = size[1].max(c.y as u16 + 1);
            size[2] = size[2].max(c.z as u16 + 1);
        }
        stamps.push(buf.clone());
    }
    let mut grid = if size == base.size {
        base
    } else {
        let mut g = CellGrid::new(size);
        let (bx, by, bz) = (base.size[0] as usize, base.size[1] as usize, base.size[2] as usize);
        for y in 0..by {
            for z in 0..bz {
                let src = bx * (z + bz * y);
                let dst = size[0] as usize * (z + size[2] as usize * y);
                g.cells[dst..dst + bx].copy_from_slice(&base.cells[src..src + bx]);
            }
        }
        g
    };
    for cells in &stamps {
        for &c in cells {
            let under = grid.get(c);
            let (armour, paint) = if under.is_empty() || under.material == Material::PartBody { (ArmourTier::None, 0) } else { (under.armour, under.paint) };
            grid.set(c, Cell { material: Material::PartBody, armour, paint, shape: CellShape::CUBE });
        }
    }
    grid
}

/// Can light (and the outside-air flood) cross this cell's face `f`? Air and glass are clear; a shaped cell is
/// open on every face its solid does not fully cover.
fn open_face(c: Cell, f: Face) -> bool {
    c.is_empty() || c.material == Material::Glass || (c.material != Material::PartBody && !c.shape.is_cube() && full_face_mask(c.shape) & (1 << f as u8) == 0)
}
fn opposite(f: Face) -> Face {
    match f {
        Face::XNeg => Face::XPos,
        Face::XPos => Face::XNeg,
        Face::YNeg => Face::YPos,
        Face::YPos => Face::YNeg,
        Face::ZNeg => Face::ZPos,
        Face::ZPos => Face::ZNeg,
    }
}

/// Cells of `grid` with a face visible from outside its bounding box: flood outside air through air, glass and
/// the open faces of shaped cells, and mark every cell a flooded cell looks at through an open face.
fn exterior_seen(grid: &CellGrid) -> Vec<bool> {
    let s = [grid.size[0] as usize, grid.size[1] as usize, grid.size[2] as usize];
    let p = [s[0] + 2, s[1] + 2, s[2] + 2];
    let pidx = |x: usize, y: usize, z: usize| x + p[0] * (z + p[2] * y);
    let inner = |x: usize, y: usize, z: usize| -> Option<usize> {
        if x == 0 || y == 0 || z == 0 || x > s[0] || y > s[1] || z > s[2] {
            return None;
        }
        Some((x - 1) + s[0] * ((z - 1) + s[2] * (y - 1)))
    };
    let cell_at = |x: usize, y: usize, z: usize| -> Cell { inner(x, y, z).and_then(|i| grid.cells.get(i).copied()).unwrap_or(Cell::EMPTY) };
    let mut visited = vec![false; p[0] * p[1] * p[2]];
    let mut seen = vec![false; grid.cells.len()];
    let mut stack = vec![(0usize, 0usize, 0usize)];
    visited[0] = true;
    while let Some((x, y, z)) = stack.pop() {
        let a = cell_at(x, y, z);
        for f in Face::ALL {
            if !open_face(a, f) {
                continue;
            }
            let (dx, dy, dz) = f.offset();
            let (nx, ny, nz) = (x as isize + dx as isize, y as isize + dy as isize, z as isize + dz as isize);
            if nx < 0 || ny < 0 || nz < 0 || nx as usize >= p[0] || ny as usize >= p[1] || nz as usize >= p[2] {
                continue;
            }
            let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
            let b = cell_at(nx, ny, nz);
            if let Some(i) = inner(nx, ny, nz) {
                if !b.is_empty() && i < seen.len() {
                    seen[i] = true;
                }
            }
            let i = pidx(nx, ny, nz);
            if visited[i] || !open_face(b, opposite(f)) {
                continue;
            }
            visited[i] = true;
            if b.is_empty() || b.material == Material::Glass {
                stack.push((nx, ny, nz));
            } else {
                // A shaped cell: what lies behind its open faces is visible, but outside air does not flow on
                // through it, so a faceted hull never exposes the compartments inside it.
                for g in Face::ALL {
                    if !open_face(b, g) {
                        continue;
                    }
                    let (gx, gy, gz) = g.offset();
                    let (qx, qy, qz) = (nx as isize + gx as isize, ny as isize + gy as isize, nz as isize + gz as isize);
                    if qx < 1 || qy < 1 || qz < 1 {
                        continue;
                    }
                    if let Some(j) = inner(qx as usize, qy as usize, qz as usize) {
                        if !cell_at(qx as usize, qy as usize, qz as usize).is_empty() {
                            seen[j] = true;
                        }
                    }
                }
            }
        }
    }
    seen
}

/// Derive the exterior-only render blueprint for opposing PvP teams (L945): keep cells reachable from outside
/// the bounding box through air (plus glass), and weather-deck parts. Unit: `content-blueprint`.
///
/// The result keeps the full grid's size and cell frame, so cell indices match the damage stream. Kept cells
/// are structure cells with a face visible from outside (through air, glass or a shaped cell's open face, which
/// is what hull-mesh draws). Armour tiers are stripped, because armour layout is what the full blueprint must
/// not reveal. Parts are kept when any footprint cell is visible from outside (mounts, launchers, masts,
/// propulsors), with their load, label and kit membership removed. Markings on hidden cells are dropped.
pub fn exterior_only(bp: &Blueprint, catalogue: &Catalogue) -> ExteriorBlueprint {
    let grid = build_grid(bp, catalogue);
    let seen = exterior_seen(&grid);
    let mut ext = CellGrid::new(grid.size);
    for (i, c) in grid.cells.iter().enumerate() {
        if seen[i] && !c.is_empty() && c.material != Material::PartBody {
            ext.cells[i] = Cell { armour: ArmourTier::None, ..*c };
        }
    }
    let visible = |p: CellPos| grid.index(p).is_some_and(|i| seen[i.index()]);
    let mut parts = Vec::new();
    let mut buf = Vec::new();
    for (slot, p) in bp.parts.iter().enumerate() {
        placed_cells(p, catalogue, &mut buf);
        if buf.iter().any(|c| visible(*c)) {
            parts.push((PartSlot(slot as u16), PartPlacement { part: p.part.clone(), pos: p.pos, rot: p.rot, kit: None, label: None, load: None }));
        }
    }
    let markings = bp.markings.iter().filter(|m| visible(m.cell) && !ext.get(m.cell).is_empty()).cloned().collect();
    ExteriorBlueprint {
        source_hash: compute_hash(bp),
        name: bp.header.name.clone(),
        size: grid.size,
        cells: ext.to_rows(),
        paint: bp.paint.iter().map(clamp_paint).collect(),
        markings,
        parts,
    }
}

/// Canonical RON of an exterior-only blueprint, by the same rules as [`to_canonical_ron`]. Added by the net
/// contract: one rule for the exterior hash on both ends. Unit: `content-blueprint`.
pub fn exterior_canonical_ron(ext: &ExteriorBlueprint) -> String { text::to_canonical(ext) }

/// BLAKE3 of [`exterior_canonical_ron`]: the hash `EntityIntro::blueprint` carries for the other PvP team,
/// `sim_systems::sim::interest::ExteriorIndex::exterior_hash` stores, and net streams on the blueprint lane.
pub fn exterior_hash(ext: &ExteriorBlueprint) -> ContentHash { blake3_hash(exterior_canonical_ron(ext).as_bytes()) }

// ---------------------------------------------------------------------------------------------------------
// Paint: OKLab (Ottosson 2020) in f64 with libm, so a clamp gives the same bits on every platform; the clamped
// values feed the content hash.

/// Hull paint limits: linear Y and OKLCH chroma (L130, L2076).
pub const HULL_MAX_Y: f64 = 0.6;
pub const HULL_MAX_CHROMA: f64 = 0.06;
/// Marking limits (L130, L2076).
pub const MARKING_MAX_Y: f64 = 0.7;
pub const MARKING_MAX_CHROMA: f64 = 0.12;
/// Slack when deciding an entry is already inside its limits; far below anything visible, and above the f32
/// rounding of a clamped value, so clamping is idempotent.
const TOL_Y: f64 = 1e-5;
const TOL_C: f64 = 1e-4;

fn luminance(c: [f64; 3]) -> f64 { 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2] }

fn lin_to_oklab(c: [f64; 3]) -> [f64; 3] {
    let l = 0.412_221_470_8 * c[0] + 0.536_332_536_3 * c[1] + 0.051_445_992_9 * c[2];
    let m = 0.211_903_498_2 * c[0] + 0.680_699_545_1 * c[1] + 0.107_396_956_6 * c[2];
    let s = 0.088_302_461_9 * c[0] + 0.281_718_837_6 * c[1] + 0.629_978_700_5 * c[2];
    let (l, m, s) = (libm::cbrt(l), libm::cbrt(m), libm::cbrt(s));
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

fn oklab_to_lin(lab: [f64; 3]) -> [f64; 3] {
    let l = lab[0] + 0.396_337_777_4 * lab[1] + 0.215_803_757_3 * lab[2];
    let m = lab[0] - 0.105_561_345_8 * lab[1] - 0.063_854_172_8 * lab[2];
    let s = lab[0] - 0.089_484_177_5 * lab[1] - 1.291_485_548_0 * lab[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ]
}

fn chroma(lab: [f64; 3]) -> f64 { libm::sqrt(lab[1] * lab[1] + lab[2] * lab[2]) }

fn limits(usage: PaintUse) -> (f64, f64) {
    match usage {
        PaintUse::Hull => (HULL_MAX_Y, HULL_MAX_CHROMA),
        PaintUse::Marking => (MARKING_MAX_Y, MARKING_MAX_CHROMA),
    }
}

fn within(rgb: [f64; 3], max_y: f64, max_c: f64) -> bool {
    rgb.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) && luminance(rgb) <= max_y + TOL_Y && chroma(lin_to_oklab(rgb)) <= max_c + TOL_C
}

fn clip(c: [f64; 3]) -> [f64; 3] { c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }) }

/// Largest OKLab lightness at or below `l_hi` whose colour (with the given a, b) has luminance at most `max_y`.
fn fit_lightness(l_hi: f64, a: f64, b: f64, max_y: f64) -> f64 {
    if luminance(clip(oklab_to_lin([l_hi, a, b]))) <= max_y {
        return l_hi;
    }
    let (mut lo, mut hi) = (0.0, l_hi);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if luminance(clip(oklab_to_lin([mid, a, b]))) <= max_y {
            lo = mid
        } else {
            hi = mid
        }
    }
    lo
}

/// Clamp a linear RGB colour to `max_y` and `max_c`, chroma first (keeping hue), then lightness (keeping chroma
/// and hue), then the sRGB gamut. Returns colours already inside the limits unchanged.
fn clamp_rgb(rgb: [f64; 3], max_y: f64, max_c: f64) -> [f64; 3] {
    if within(rgb, max_y, max_c) {
        return rgb;
    }
    let lab = lin_to_oklab(clip(rgb));
    let c = chroma(lab);
    let mut scale = if c > max_c && c > 0.0 { max_c / c } else { 1.0 };
    for _ in 0..12 {
        let (a, b) = (lab[1] * scale, lab[2] * scale);
        let l = fit_lightness(lab[0], a, b, max_y);
        let out = clip(oklab_to_lin([l, a, b]));
        if within(out, max_y, max_c) {
            return out;
        }
        // Gamut clipping moved the colour outside a limit: give up some chroma and try again.
        scale *= 0.5;
    }
    let y = luminance(clip(rgb)).min(max_y);
    [y, y, y]
}

/// Clamp a paint entry in OKLCH, chroma first: hull Y <= 0.6 and C <= 0.06; markings Y <= 0.7 and C <= 0.12
/// (L2076). Applied on pick and on load. Unit: `content-blueprint`.
///
/// An entry already inside its limits comes back bit-identical, so clamping is idempotent and a loaded file
/// keeps its hash. Non-finite or out-of-range channels (a hand-edited file) are sanitised first.
pub fn clamp_paint(entry: &PaintEntry) -> PaintEntry {
    let (max_y, max_c) = limits(entry.usage);
    let rgb = entry.rgb.map(|v| v as f64);
    if within(rgb, max_y, max_c) {
        return entry.clone();
    }
    let out = clamp_rgb(rgb, max_y, max_c);
    PaintEntry { token: entry.token.clone(), rgb: out.map(|v| v as f32), usage: entry.usage }
}

/// World colour tokens (L1788); the only place world colours are written.
const WORLD_TOKENS: &str = include_str!("../../../../game/art/tokens/world_tokens.json");

#[derive(serde::Deserialize)]
struct TokenFile {
    hull: Vec<TokenRow>,
    antifouling: TokenRow,
}
#[derive(serde::Deserialize)]
struct TokenRow {
    token: String,
    y: f64,
    /// Linear sRGB, when the token export carries it.
    #[serde(default, alias = "linear")]
    linear_rgb: Vec<f64>,
    /// OKLCH (L, C, h degrees), when the token export carries it.
    #[serde(default)]
    oklch: Vec<f64>,
}

/// Antifouling hue when the token file gives only its Y: red-brown, OKLCH hue 40 degrees at the hull chroma cap.
const ANTIFOULING_HUE_DEG: f64 = 40.0;
/// Spec values (L1790-1799, L1801) used only if the committed token file cannot be read.
const SPEC_SWATCHES: [(&str, f64); 8] = [("hull.pale", 0.55), ("hull.light", 0.35), ("hull.haze", 0.28), ("hull.ocean", 0.17), ("hull.sea", 0.11), ("hull.navy", 0.09), ("hull.deck", 0.05), ("hull.black", 0.03)];
const SPEC_ANTIFOULING: (&str, f64) = ("hull.antifouling", 0.08);

/// Index of the antifouling entry in [`default_paint_table`]; the eight swatches come first, palest first.
pub const ANTIFOULING_PAINT: u8 = 8;
/// Entries in [`default_paint_table`]; up to `MAX_PAINT - DEFAULT_PAINT_COUNT` more are picked.
pub const DEFAULT_PAINT_COUNT: usize = 9;

/// Colour at OKLCH (C, h) whose luminance is `y`.
fn colour_at_y(y: f64, c: f64, hue_deg: f64) -> [f64; 3] {
    let h = hue_deg.to_radians();
    let (a, b) = (c * libm::cos(h), c * libm::sin(h));
    let l = fit_lightness(1.0, a, b, y);
    clip(oklab_to_lin([l, a, b]))
}

fn row_colour(row: &TokenRow, fallback_hue: Option<f64>) -> [f64; 3] {
    if let [r, g, b] = row.linear_rgb[..] {
        return [r, g, b];
    }
    if let [l, c, h] = row.oklch[..] {
        let hr = h.to_radians();
        return clip(oklab_to_lin([l, c * libm::cos(hr), c * libm::sin(hr)]));
    }
    match fallback_hue {
        Some(hue) => colour_at_y(row.y, HULL_MAX_CHROMA, hue),
        None => [row.y; 3],
    }
}

fn parse_tokens() -> Option<(Vec<TokenRow>, TokenRow)> {
    let t = ron::from_str::<ron::Value>(WORLD_TOKENS).ok()?.into_rust::<TokenFile>().ok()?;
    (t.hull.len() == 8).then_some((t.hull, t.antifouling))
}

fn token_rows() -> (Vec<TokenRow>, TokenRow) {
    parse_tokens().unwrap_or_else(|| {
        let row = |(token, y): (&str, f64)| TokenRow { token: token.to_string(), y, linear_rgb: Vec::new(), oklch: Vec::new() };
        (SPEC_SWATCHES.into_iter().map(row).collect(), row(SPEC_ANTIFOULING))
    })
}

/// The default paint table: 8 USN swatches then the red-brown antifouling colour (L130).
/// Values come from `res://art/tokens/world_tokens.json` via the data unit. Unit: `content-blueprint`.
///
/// Each entry is the token's linear RGB (or its OKLCH) when the token file carries one; until then a swatch is
/// the neutral grey of its Y (the spec's table values are neutral) and antifouling is red-brown at its Y. Every
/// entry passes [`clamp_paint`] unchanged.
pub fn default_paint_table() -> Vec<PaintEntry> {
    static TABLE: OnceLock<Vec<PaintEntry>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let (hull, anti) = token_rows();
            let entry = |row: &TokenRow, hue: Option<f64>| clamp_paint(&PaintEntry { token: Some(row.token.clone()), rgb: row_colour(row, hue).map(|v| v as f32), usage: PaintUse::Hull });
            let mut out: Vec<PaintEntry> = hull.iter().map(|r| entry(r, None)).collect();
            out.push(entry(&anti, Some(ANTIFOULING_HUE_DEG)));
            out.truncate(MAX_PAINT);
            out
        })
        .clone()
}

/// Index in [`default_paint_table`] of a world token such as `hull.deck` or `hull.black`.
pub fn default_paint_index(token: &str) -> Option<u8> { default_paint_table().iter().position(|p| p.token.as_deref() == Some(token)).map(|i| i as u8) }

/// An empty design around a default shell of the given length. Unit: `content-blueprint`.
///
/// The length is clamped to 10..=150 m (enemy-class mode allows 150; the editor sets `kind` and validation
/// enforces the player limit); a non-finite length gives the 10 m minimum. The shell comes from
/// [`crate::hull::default_shell`] and its cells from [`crate::hull::voxelize`]; the paint table is
/// [`default_paint_table`] and every other section starts empty or at its defaults.
pub fn new_blueprint(name: &str, length_m: f64) -> Blueprint {
    let length = if length_m.is_finite() { length_m.clamp(crate::cell::MIN_PLAYER_HULL_M, crate::cell::MAX_ENEMY_HULL_M) } else { crate::cell::MIN_PLAYER_HULL_M };
    let shell = crate::hull::default_shell(length);
    // The design waterline sits on the shell's draft datum, as `default_hull` puts it: at row 0 nothing is below
    // it, so no underwater part (a rudder, a waterjet) could be placed on a new design.
    let waterline = crate::hull::shell_layout(&shell).waterline_y;
    let mut hull = Hull { source: HullSource::Lofted(shell), planes: Vec::new(), design_waterline_y: waterline };
    let vox = crate::hull::voxelize(&hull);
    hull.design_waterline_y = vox.design_waterline_y;
    let mut bp = Blueprint {
        header: Header {
            format: BLUEPRINT_FORMAT,
            game_version: env!("CARGO_PKG_VERSION").to_string(),
            hash: ContentHash::EMPTY,
            kind: BlueprintKind::Ship,
            name: name.trim().to_string(),
            name_key: None,
            author: None,
            licence: DEFAULT_LICENCE.to_string(),
            lineage: Vec::new(),
        },
        hull,
        cells: vox.grid.to_rows(),
        paint: default_paint_table(),
        markings: Vec::new(),
        parts: Vec::new(),
        kits: Vec::new(),
        embedded_kits: Default::default(),
        officers: Default::default(),
        rules: Vec::new(),
        networks: Default::default(),
        graph: Default::default(),
    };
    canonicalize_blueprint(&mut bp);
    bp.header.hash = compute_hash(&bp);
    bp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::ShapeKind;
    use crate::error::ContentError;
    use crate::ids::PartId;
    use crate::kit::PartRot;

    const BP: &str = include_str!("../../../../blueprints/examples/minimal.bp.ron");

    fn part(id: &str, size: [u8; 3]) -> crate::part::PartDef {
        ron::from_str(&format!("(id: \"{id}\", name: \"n\", description: \"d\", category: Hull, tier: T1, size: ({}, {}, {}), mass_kg: 1.0, cost: 1, function: Structure)", size[0], size[1], size[2])).unwrap()
    }
    fn cat() -> Catalogue {
        let mut c = Catalogue::default();
        for p in [part("mast", [1, 3, 1]), part("box", [2, 1, 2])] {
            c.parts.insert(p.id.clone(), p);
        }
        c
    }
    fn example() -> Blueprint { load_blueprint_with(BP, "minimal.bp.ron", &cat()).unwrap().blueprint }
    fn place(id: &str, pos: CellPos) -> PartPlacement { PartPlacement { part: PartId::new(id), pos, rot: PartRot::default(), kit: None, label: Some("Gun 2".into()), load: Some(0.5) } }

    #[test]
    fn file_text_round_trips_and_is_canonical() {
        let loaded = load_blueprint_with(BP, "minimal.bp.ron", &cat()).unwrap();
        assert!(loaded.rehashed, "the example stores an empty hash");
        assert_eq!(loaded.from_format, 1);
        let mut bp = loaded.blueprint;
        bp.header.name_key = Some(crate::ids::TextKey::new("ship.raft"));
        bp.header.author = Some("A \"quoted\" author".into());
        bp.parts.push(place("box", CellPos::new(1, 1, 0)));
        bp.header.hash = compute_hash(&bp);
        let text = to_file_text(&bp);
        assert!(!text.contains('\r'));
        assert!(text.ends_with(")\n"));
        assert!(text.contains("name_key: Some(\"ship.raft\")"));
        // One line per cell row and per part.
        assert_eq!(text.lines().filter(|l| l.trim_start().starts_with("(y: ")).count(), bp.cells.0.len());
        assert_eq!(text.lines().filter(|l| l.trim_start().starts_with("(part: ")).count(), 1);
        let again = load_blueprint_with(&text, "again", &cat()).unwrap();
        assert!(!again.rehashed);
        assert_eq!(again.blueprint, bp);
        assert_eq!(to_file_text(&again.blueprint), text);
        assert_eq!(to_canonical_ron(&bp), text.replace(&bp.header.hash.to_hex(), ""));
    }

    #[test]
    fn hand_edits_are_recanonicalised() {
        // Split runs, rows out of order, a stale hash: the same design and hash as the canonical file.
        let edited = BP
            .replace("(y: 0, z: 0, runs: [(x: 0, len: 8,", "(y: 0, z: 1, runs: [(x: 0, len: 8, cell: (material: Steel, armour: None, paint: 0, shape: 0))]),\n(y: 0, z: 0, runs: [(x: 4, len: 4, cell: (material: Steel, armour: None, paint: 0, shape: 0)), (x: 0, len: 4,")
            .replacen("hash: \"\"", &format!("hash: \"{}\"", "ab".repeat(32)), 1);
        let a = load_blueprint_with(&edited, "edited", &cat()).unwrap();
        assert!(a.rehashed);
        assert_eq!(a.blueprint, example());
    }

    #[test]
    fn untrusted_limits_and_formats() {
        let big = format!("{BP}{}", " ".repeat(crate::blueprint::MAX_RON_BYTES));
        assert!(matches!(load_blueprint_with(&big, "f", &cat()), Err(ContentError::Limit(_))));
        let deep = BP.replace("planes: [],", &format!("planes: {}{},", "[".repeat(130), "]".repeat(130)));
        assert!(matches!(load_blueprint_with(&deep, "f", &cat()), Err(ContentError::Limit(_))));
        let newer = BP.replacen("format: 1", "format: 2", 1);
        match load_blueprint_with(&newer, "f", &cat()) {
            Err(ContentError::FormatTooNew { found: 2, supported: 1, needs_release }) => assert!(!needs_release.is_empty()),
            other => panic!("{other:?}"),
        }
        let unknown = BP.replace("rules: [],", "rules: [], cheats: true,");
        assert!(matches!(load_blueprint_with(&unknown, "f", &cat()), Err(ContentError::Parse { .. })));
        assert!(load_blueprint_with("(header: (format: 1))", "f", &cat()).is_err());
    }

    #[test]
    fn normalize_rows_matches_the_dense_round_trip() {
        let c = |m: Material, paint: u8| Cell { material: m, armour: ArmourTier::None, paint, shape: CellShape::CUBE };
        let rows = CellRows(vec![
            CellRow { y: 2, z: 1, runs: vec![CellRun { x: 3, len: 5, cell: c(Material::Steel, 1) }, CellRun { x: 5, len: 1, cell: c(Material::Glass, 0) }, CellRun { x: 0, len: 2, cell: Cell::EMPTY }] },
            CellRow { y: 0, z: 0, runs: vec![CellRun { x: 1, len: 2, cell: c(Material::Steel, 0) }, CellRun { x: 3, len: 2, cell: c(Material::Steel, 0) }, CellRun { x: 9, len: 0, cell: c(Material::Steel, 0) }] },
            CellRow { y: 2, z: 1, runs: vec![CellRun { x: 7, len: 3, cell: c(Material::Aluminium, 0) }] },
        ]);
        assert_eq!(normalize_rows(&rows).unwrap(), CellGrid::from_rows(&rows).to_rows());
        let far = CellRows(vec![CellRow { y: 0, z: 0, runs: vec![CellRun { x: 380, len: 10, cell: c(Material::Steel, 0) }] }]);
        assert_eq!(normalize_rows(&far), None);
        let neg = CellRows(vec![CellRow { y: -1, z: 0, runs: vec![CellRun { x: 0, len: 1, cell: c(Material::Steel, 0) }] }]);
        assert_eq!(normalize_rows(&neg), None);
    }

    fn entry(rgb: [f32; 3], usage: PaintUse) -> PaintEntry { PaintEntry { token: None, rgb, usage } }

    #[test]
    fn paint_clamps_are_idempotent_and_inside_limits() {
        let samples = [[1.0, 0.0, 0.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [0.9, 0.2, 0.6], [0.05, 0.02, 0.01], [0.0, 0.0, 0.0], [2.0, -1.0, 0.5], [f32::NAN, f32::INFINITY, 0.3], [0.3, 0.3, 0.3]];
        for rgb in samples {
            for usage in [PaintUse::Hull, PaintUse::Marking] {
                let (max_y, max_c) = limits(usage);
                let once = clamp_paint(&entry(rgb, usage));
                let c = once.rgb.map(|v| v as f64);
                assert!(within(c, max_y, max_c), "{rgb:?} {usage:?} -> {c:?}");
                let twice = clamp_paint(&once);
                assert_eq!(once.rgb.map(f32::to_bits), twice.rgb.map(f32::to_bits));
            }
        }
        // Inside the limits: untouched, bit for bit.
        let ok = entry([0.2, 0.21, 0.23], PaintUse::Hull);
        assert_eq!(clamp_paint(&ok), ok);
        // Chroma first: a saturated red keeps its hue family (red stays the largest channel).
        let red = clamp_paint(&entry([0.8, 0.05, 0.05], PaintUse::Hull)).rgb;
        assert!(red[0] > red[1] && red[0] > red[2]);
        // Marking limits are looser than hull limits.
        let bright = [0.9, 0.9, 0.2];
        assert!(luminance(clamp_paint(&entry(bright, PaintUse::Marking)).rgb.map(|v| v as f64)) > luminance(clamp_paint(&entry(bright, PaintUse::Hull)).rgb.map(|v| v as f64)));
    }

    #[test]
    fn default_paint_table_comes_from_the_tokens() {
        assert!(parse_tokens().is_some(), "world_tokens.json no longer parses; default paint would fall back to the spec table");
        let t = default_paint_table();
        assert_eq!(t.len(), DEFAULT_PAINT_COUNT);
        assert_eq!(t[0].token.as_deref(), Some("hull.pale"));
        assert_eq!(default_paint_index("hull.black"), Some(7));
        assert_eq!(t[ANTIFOULING_PAINT as usize].token.as_deref(), Some("hull.antifouling"));
        for (e, y) in t.iter().zip([0.55, 0.35, 0.28, 0.17, 0.11, 0.09, 0.05, 0.03, 0.08]) {
            assert_eq!(&clamp_paint(e), e, "default entries are clamp fixed points");
            assert!((luminance(e.rgb.map(|v| v as f64)) - y).abs() < 2e-3, "{e:?}");
            assert_eq!(e.usage, PaintUse::Hull);
        }
        let af = t[8].rgb;
        assert!(af[0] > af[1] && af[1] > af[2], "antifouling is red-brown: {af:?}");
    }

    #[test]
    fn build_grid_stamps_parts_and_grows() {
        let mut bp = example();
        let armoured = Cell { material: Material::Steel, armour: ArmourTier::Mm15, paint: 3, shape: CellShape::CUBE };
        let mut g = CellGrid::from_rows(&bp.cells);
        g.set(CellPos::new(2, 0, 1), armoured);
        bp.cells = g.to_rows();
        bp.parts = vec![place("mast", CellPos::new(0, 1, 0)), place("box", CellPos::new(2, 0, 0)), place("ghost", CellPos::new(5, 0, 0)), place("mast", CellPos::new(-3, 0, 0))];
        let grid = build_grid(&bp, &cat());
        assert_eq!(grid.size, [8, 4, 2], "the mast grows the grid upwards");
        for y in 1..4 {
            assert_eq!(grid.get(CellPos::new(0, y, 0)).material, Material::PartBody);
        }
        let under = grid.get(CellPos::new(2, 0, 1));
        assert_eq!((under.material, under.armour, under.paint), (Material::PartBody, ArmourTier::Mm15, 3));
        assert_eq!(grid.get(CellPos::new(5, 0, 0)).material, Material::Steel, "unknown parts are skipped");
        assert_eq!(grid.get(CellPos::new(7, 0, 1)).material, Material::Steel);
    }

    /// A closed steel box 6 x 4 x 4 with a part inside, a mast on top and a glass window.
    fn boxed() -> Blueprint {
        let mut bp = example();
        let steel = Cell { material: Material::Steel, armour: ArmourTier::Mm40, paint: 0, shape: CellShape::CUBE };
        let mut g = CellGrid::new([6, 4, 4]);
        for x in 0..6 {
            for y in 0..4 {
                for z in 0..4 {
                    let shell = x == 0 || x == 5 || y == 0 || y == 3 || z == 0 || z == 3;
                    if shell {
                        g.set(CellPos::new(x, y, z), steel);
                    }
                }
            }
        }
        bp.cells = g.to_rows();
        bp.parts = vec![place("box", CellPos::new(1, 1, 1)), place("mast", CellPos::new(2, 4, 1))];
        bp
    }

    #[test]
    fn exterior_hides_the_inside() {
        let bp = boxed();
        let ext = exterior_only(&bp, &cat());
        let grid = build_grid(&bp, &cat());
        assert_eq!(ext.size, grid.size);
        assert_eq!(ext.source_hash, compute_hash(&bp));
        let cells = CellGrid::from_rows(&ext.cells);
        // Every shell cell faces outside except the deck cell under the mast; none is armoured any more.
        assert_eq!(cells.solid_count(), 6 * 4 * 4 - 4 * 2 * 2 - 1);
        assert!(cells.get(CellPos::new(2, 3, 1)).is_empty());
        assert!(cells.cells.iter().all(|c| c.armour == ArmourTier::None));
        let slots: Vec<u16> = ext.parts.iter().map(|(s, _)| s.0).collect();
        assert_eq!(slots, [1], "only the mast shows");
        assert_eq!((ext.parts[0].1.label.clone(), ext.parts[0].1.load), (None, None));

        // A window in the side shows what is behind it.
        let mut windowed = boxed();
        let mut g = CellGrid::from_rows(&windowed.cells);
        g.set(CellPos::new(1, 1, 0), Cell { material: Material::Glass, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE });
        windowed.cells = g.to_rows();
        let ext = exterior_only(&windowed, &cat());
        assert_eq!(ext.parts.iter().map(|(s, _)| s.0).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(exterior_hash(&ext), blake3_hash(exterior_canonical_ron(&ext).as_bytes()));
    }

    #[test]
    fn shaped_cells_show_neighbours_but_do_not_leak() {
        // A corner cell (no full face) in the shell: what touches it is visible, but the inside stays hidden.
        let mut bp = boxed();
        let mut g = CellGrid::from_rows(&bp.cells);
        g.set(CellPos::new(2, 0, 2), Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::new(ShapeKind::Corner, 0) });
        bp.cells = g.to_rows();
        let ext = exterior_only(&bp, &cat());
        let parts: Vec<u16> = ext.parts.iter().map(|(s, _)| s.0).collect();
        assert_eq!(parts, [0, 1], "the part right behind the open corner shows");
        // Cells two steps inside are never reached.
        let mut deep = boxed();
        let mut g = CellGrid::from_rows(&deep.cells);
        g.set(CellPos::new(4, 0, 2), Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::new(ShapeKind::Corner, 0) });
        deep.cells = g.to_rows();
        deep.parts = vec![place("box", CellPos::new(1, 2, 1))];
        let ext = exterior_only(&deep, &cat());
        assert!(ext.parts.is_empty());
    }
}
