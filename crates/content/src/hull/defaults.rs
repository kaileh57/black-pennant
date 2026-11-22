//! The "new design" shell, decks and bulkheads.
//!
//! Proportions follow real small combatants and scale with length: length-to-beam runs from 3.2 at 10 m (a
//! patrol boat) to 8.2 at 90 m (a destroyer) and 9.5 at 150 m (a light cruiser); beam-to-draft from 4.0 to 3.2;
//! freeboard amidships is 0.7 m plus 4% of length. Seven stations give a transom stern, a full midbody, a raked
//! stem with a sharp entry, and a sheer that rises toward the bow. Small hulls get more deadrise aft (a planing V).
use super::{Hull, HullShell, HullSource, InternalPlane, ShellField, Station, DEFAULT_DECK_SPACING_M};
use crate::cell::{CELL_M, MAX_ENEMY_HULL_M, MIN_PLAYER_HULL_M};
use crate::enums::Axis;

/// (t, beam fraction, draft fraction, deadrise deg, flare deg, sheer fraction of the amidships freeboard).
/// The stem station's beam is replaced by a fixed stem bar width.
const STATIONS: [(f64, f64, f64, f64, f64, f64); 7] = [
    (0.00, 0.84, 0.60, 4.0, 4.0, 0.92),
    (0.15, 0.95, 0.88, 6.0, 6.0, 0.90),
    (0.35, 1.00, 1.00, 8.0, 8.0, 0.92),
    (0.55, 0.99, 1.00, 10.0, 10.0, 1.00),
    (0.75, 0.86, 0.95, 16.0, 14.0, 1.12),
    (0.90, 0.56, 0.82, 24.0, 20.0, 1.30),
    (1.00, 0.00, 0.50, 30.0, 24.0, 1.45),
];
/// Width of the stem bar at the foremost station, metres (just over two cells, so the stem reaches the bow).
const STEM_BEAM_M: f64 = 0.6;
/// Extra aft deadrise for the smallest hulls, degrees at t = 0, fading to zero at the stem and at 90 m.
const PLANING_DEADRISE_DEG: f64 = 10.0;
/// Internal decks stop this far above the deepest keel point (room for a double bottom and tanks).
const LOWEST_DECK_ABOVE_KEEL_M: f64 = 1.5;
/// One watertight transverse bulkhead per this many metres of length, at least 1 and at most 10.
const BULKHEAD_SPACING_M: f64 = 12.0;

pub(crate) fn default_shell(length_m: f64) -> HullShell {
    let l = if length_m.is_finite() { length_m.clamp(MIN_PLAYER_HULL_M, MAX_ENEMY_HULL_M) } else { MIN_PLAYER_HULL_M };
    let u = ((l - 10.0) / 80.0).clamp(0.0, 1.0);
    let l_over_b = if l <= 90.0 { 3.2 + 5.0 * u } else { 8.2 + 1.3 * (l - 90.0) / 60.0 };
    let beam = l / l_over_b;
    let draft = beam / (4.0 - 0.8 * u);
    let freeboard = 0.7 + 0.04 * l;
    let stations = STATIONS
        .iter()
        .map(|&(t, b, d, dr, fl, sh)| Station {
            t,
            beam_m: if t >= 1.0 { STEM_BEAM_M } else { b * beam },
            draft_m: d * draft,
            deadrise_deg: dr + PLANING_DEADRISE_DEG * (1.0 - u) * (1.0 - t),
            flare_deg: fl,
            sheer_m: sh * freeboard,
        })
        .collect();
    HullShell {
        length_m: l,
        stations,
        bow_rake_deg: 30.0 - 8.0 * u,
        transom_rake_deg: 8.0,
        transom_width: 1.0,
        mirrored: true,
        shell_cells: 1,
    }
}

/// Default planes for a shell: watertight decks every [`DEFAULT_DECK_SPACING_M`] below the lowest deck edge,
/// down to [`LOWEST_DECK_ABOVE_KEEL_M`] above the keel, and evenly spaced watertight transverse bulkheads.
/// Empty for an invalid shell.
pub(crate) fn default_planes(shell: &HullShell) -> Vec<InternalPlane> {
    let Some(field) = ShellField::new(shell) else { return Vec::new() };
    let lay = field.layout();
    let mut planes = Vec::new();
    let lowest_sheer = shell.stations.iter().map(|s| s.sheer_m).fold(f64::INFINITY, f64::min);
    let deepest = shell.stations.iter().map(|s| s.draft_m).fold(0.0, f64::max);
    let mut deck_m = lowest_sheer - DEFAULT_DECK_SPACING_M;
    while deck_m >= -deepest + LOWEST_DECK_ABOVE_KEEL_M {
        let y = lay.waterline_y as i32 + (deck_m / CELL_M).floor() as i32;
        if y >= 0 && y < lay.size[1] as i32 {
            planes.push(InternalPlane { axis: Axis::Y, offset: y as i16, watertight: true });
        }
        deck_m -= DEFAULT_DECK_SPACING_M;
    }
    let n = (shell.length_m / BULKHEAD_SPACING_M).round().clamp(1.0, 10.0) as i32;
    for k in 1..=n {
        let x_m = shell.length_m * k as f64 / (n + 1) as f64;
        let x = (x_m / CELL_M).floor() as i32 - lay.x_offset as i32;
        if x >= 0 && x < lay.size[0] as i32 {
            planes.push(InternalPlane { axis: Axis::X, offset: x as i16, watertight: true });
        }
    }
    planes
}

pub(crate) fn default_hull(length_m: f64) -> Hull {
    let shell = default_shell(length_m);
    let planes = default_planes(&shell);
    let waterline = ShellField::new(&shell).map(|f| f.layout().waterline_y).unwrap_or(0);
    Hull { source: HullSource::Lofted(shell), planes, design_waterline_y: waterline }
}
