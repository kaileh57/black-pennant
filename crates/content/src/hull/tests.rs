use super::*;
use crate::cell::{shape_corner_mask, CellPos, Face, Material, ShapeKind, MAX_ENEMY_HULL_M, MAX_PLAYER_HULL_M};
use std::collections::VecDeque;

const LENGTHS: [f64; 5] = [10.0, 26.0, 50.0, 90.0, 150.0];

fn hull_of(shell: HullShell) -> Hull {
    let planes = default_planes(&shell);
    let wl = shell_layout(&shell).waterline_y;
    Hull { source: HullSource::Lofted(shell), planes, design_waterline_y: wl }
}

fn station(t: f64, beam_m: f64, draft_m: f64) -> Station {
    Station { t, beam_m, draft_m, deadrise_deg: 0.0, flare_deg: 0.0, sheer_m: 2.0 }
}

/// A plain box-like barge: vertical sides, flat bottom on a lattice plane, square ends.
fn barge() -> HullShell {
    HullShell {
        length_m: 20.0,
        stations: vec![station(0.0, 6.0, 2.0), station(0.3, 6.0, 2.0), station(0.7, 6.0, 2.0), station(1.0, 6.0, 2.0)],
        bow_rake_deg: 0.0,
        transom_rake_deg: 0.0,
        transom_width: 1.0,
        mirrored: true,
        shell_cells: 1,
    }
}

fn pos(g: &CellGrid, i: usize) -> CellPos { g.pos(crate::ids::CellIndex(i as u32)) }

/// Empty cells reachable from outside the grid through faces of empty cells.
fn flood_outside(g: &CellGrid) -> Vec<bool> {
    let [sx, sy, sz] = g.size.map(|v| v as i16);
    let mut seen = vec![false; g.len()];
    let mut q = VecDeque::new();
    for (i, c) in g.cells.iter().enumerate() {
        let p = pos(g, i);
        let edge = p.x == 0 || p.y == 0 || p.z == 0 || p.x == sx - 1 || p.y == sy - 1 || p.z == sz - 1;
        if edge && c.is_empty() {
            seen[i] = true;
            q.push_back(p);
        }
    }
    while let Some(p) = q.pop_front() {
        for f in Face::ALL {
            let (dx, dy, dz) = f.offset();
            let n = CellPos::new(p.x + dx, p.y + dy, p.z + dz);
            if let Some(ni) = g.index(n) {
                let ni = ni.index();
                if !seen[ni] && g.cells[ni].is_empty() {
                    seen[ni] = true;
                    q.push_back(n);
                }
            }
        }
    }
    seen
}

#[test]
fn default_shells_validate_and_respect_mode_limits() {
    for l in LENGTHS {
        let s = default_shell(l);
        assert_eq!(validate_shell(&s, MAX_ENEMY_HULL_M), Ok(()), "{l} m");
        assert!(s.stations.len() >= MIN_STATIONS && s.stations.len() <= MAX_STATIONS);
    }
    assert_eq!(validate_shell(&default_shell(90.0), MAX_PLAYER_HULL_M), Ok(()));
    assert!(matches!(validate_shell(&default_shell(120.0), MAX_PLAYER_HULL_M), Err(ShellError::Length { .. })));
    assert_eq!(default_shell(f64::NAN).length_m, 10.0);
    assert_eq!(default_shell(1e9).length_m, 150.0);
}

#[test]
fn validation_rejects_bad_shells() {
    let mut s = default_shell(40.0);
    s.stations.truncate(3);
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::StationCount(3)));
    let mut s = default_shell(40.0);
    s.stations.swap(2, 3);
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::StationOrder));
    let mut s = default_shell(40.0);
    s.stations[3].draft_m = f64::NAN;
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::Param("draft_m")));
    let mut s = default_shell(40.0);
    s.stations[2].deadrise_deg = 89.0;
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::Param("deadrise_deg")));
    let mut s = default_shell(40.0);
    s.length_m = 9.0;
    assert!(matches!(validate_shell(&s, 90.0), Err(ShellError::Length { .. })));
    let mut s = default_shell(40.0);
    s.shell_cells = 0;
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::Param("shell_cells")));
    let mut s = default_shell(40.0);
    for st in &mut s.stations {
        st.beam_m = 0.5;
    }
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::Param("beam_m")));
    // A 10 m hull with deep sheer and steep rakes at both ends leaves no run of hull at the deck.
    let mut s = default_shell(10.0);
    for st in &mut s.stations {
        st.sheer_m = 20.0;
    }
    s.bow_rake_deg = 60.0;
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::Param("bow_rake_deg")));
}

#[test]
fn garbage_never_panics() {
    let mut bad = vec![
        HullShell { length_m: f64::NAN, stations: vec![], bow_rake_deg: 0.0, transom_rake_deg: 0.0, transom_width: 1.0, mirrored: true, shell_cells: 1 },
        HullShell { length_m: 1e300, ..default_shell(50.0) },
        HullShell { bow_rake_deg: 90.0, transom_rake_deg: -90.0, ..default_shell(50.0) },
        HullShell { shell_cells: 255, ..default_shell(50.0) },
    ];
    let mut s = default_shell(50.0);
    s.stations[1].beam_m = f64::INFINITY;
    bad.push(s);
    let mut s = default_shell(50.0);
    s.stations.extend(std::iter::repeat_n(s.stations[6], 100));
    bad.push(s);
    for s in bad {
        assert!(validate_shell(&s, 150.0).is_err());
        let v = voxelize(&Hull { source: HullSource::Lofted(s.clone()), planes: vec![], design_waterline_y: i16::MIN });
        assert!(v.grid.is_empty());
        assert_eq!(v.design_waterline_y, 0);
        assert_eq!(shell_sdf(&s, [1.0, 1.0, 1.0]), f64::MAX);
        assert_eq!(shell_layout(&s), ShellLayout::default());
        assert!(default_planes(&s).is_empty());
    }
    let v = voxelize(&Hull { source: HullSource::Freeform, planes: vec![], design_waterline_y: 4 });
    assert!(v.grid.is_empty());
    assert_eq!(v.design_waterline_y, 4);
}

/// Structural checks every voxelized shell must pass: tight box on the layout, symmetric, sealed, one
/// face-connected piece, run-length round trip. Returns the result, interior-air and displaced cell counts.
fn check_structure(hull: &Hull, label: &str) -> (VoxelizedHull, usize, usize) {
    let HullSource::Lofted(shell) = &hull.source else { unreachable!() };
    let l = shell.length_m;
    let v = voxelize(&hull);
    let g = &v.grid;
    let lay = shell_layout(shell);
    assert_eq!(g.size, lay.size);
    assert_eq!(v.design_waterline_y, lay.waterline_y);
    assert!(g.size[0] as f64 * 0.5 <= l + 1e-9, "{label} hull is {} cells long", g.size[0]);
    assert!(g.size.iter().all(|&d| d > 0 && d <= crate::cell::MAX_GRID_DIM));
    assert_eq!(g.size[2] % 2, 0);

    // Tight: every boundary slab holds a solid cell, so the grid survives the run-length round trip.
    let mut lo = [i16::MAX; 3];
    let mut hi = [i16::MIN; 3];
    for (i, c) in g.cells.iter().enumerate() {
        if !c.is_empty() {
            let p = pos(g, i);
            for (k, v) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[k] = lo[k].min(v);
                hi[k] = hi[k].max(v);
            }
        }
    }
    assert_eq!(lo, [0, 0, 0], "{label}");
    assert_eq!(hi, g.size.map(|d| d as i16 - 1), "{label}");
    assert_eq!(&CellGrid::from_rows(&g.to_rows()), g);

    // Mirror symmetry: same material and paint, mirrored solid.
    let sz = g.size[2] as i16;
    for (i, c) in g.cells.iter().enumerate() {
        let p = pos(g, i);
        let m = g.get(CellPos::new(p.x, p.y, sz - 1 - p.z));
        assert_eq!((c.material, c.paint), (m.material, m.paint), "{label} at {p:?}");
        if !c.is_empty() {
            let mask = shape_corner_mask(c.shape);
            let mirrored = (0..8).filter(|b| mask & (1 << b) != 0).fold(0u8, |acc, b| acc | 1 << (b ^ 4));
            assert_eq!(mirrored, shape_corner_mask(m.shape), "{label} at {p:?}");
        }
    }

    // Sealed: no empty cell inside the hull is reachable from outside through faces, and the hull has air.
    let field = ShellField::new(shell).unwrap();
    let outside = flood_outside(g);
    let mut interior_air = 0usize;
    let mut displaced = 0usize;
    let mut bridges = 0usize;
    for (i, c) in g.cells.iter().enumerate() {
        let p = pos(g, i);
        let inside = field.sdf(p.centre_m()) <= 0.0;
        if c.is_empty() {
            assert_eq!(outside[i], !inside, "{label} at {p:?}");
            interior_air += inside as usize;
        } else if !inside {
            // Only the cubes that join a sliver to the rest of the hull lie outside the shell.
            assert!(c.shape.is_cube(), "{label}: shaped cell outside the shell at {p:?}");
            bridges += 1;
        }
        if inside && p.y < v.design_waterline_y {
            displaced += 1;
        }
    }
    assert!(bridges * 200 <= g.solid_count(), "{label}: {bridges} bridging cells");
    assert_eq!(solid_components(g), 1, "{label}: plating is not one face-connected piece");
    (v, interior_air, displaced)
}

/// Number of 6-connected components of solid cells.
fn solid_components(g: &CellGrid) -> usize {
    let mut seen = vec![false; g.len()];
    let mut count = 0;
    for start in 0..g.len() {
        if seen[start] || g.cells[start].is_empty() {
            continue;
        }
        count += 1;
        seen[start] = true;
        let mut q = VecDeque::from([pos(g, start)]);
        while let Some(p) = q.pop_front() {
            for f in Face::ALL {
                let (dx, dy, dz) = f.offset();
                let n = CellPos::new(p.x + dx, p.y + dy, p.z + dz);
                if let Some(ni) = g.index(n) {
                    let ni = ni.index();
                    if !seen[ni] && !g.cells[ni].is_empty() {
                        seen[ni] = true;
                        q.push_back(n);
                    }
                }
            }
        }
    }
    count
}

#[test]
fn default_hulls_are_tight_symmetric_sealed_and_afloat() {
    for l in LENGTHS {
        let hull = default_hull(l);
        let (v, interior_air, displaced) = check_structure(&hull, &format!("{l} m"));
        let g = &v.grid;
        // A 10 m hull is only a few cells across, so its plating is most of it; larger hulls are mostly air.
        assert!(interior_air > 0, "{l} m hull has no interior");
        if l >= 26.0 {
            assert!(interior_air > g.solid_count() / 4, "{l} m hull is barely hollow");
        }

        // Afloat with room for parts: water displaced at the datum outweighs the bare structure. The 60 kg cell
        // is the placeholder steel mass in materials.ron.
        let displacement_kg = displaced as f64 * 0.125 * 1025.0;
        let structure_kg = g.solid_count() as f64 * 60.0;
        let ratio = displacement_kg / structure_kg;
        eprintln!("{l} m: size {:?}, {} solid, {interior_air} interior air, displacement / structure {ratio:.2}", g.size, g.solid_count());
        // At the placeholder 60 kg per cell a 10 m hull's plating (most of its cells) outweighs the water it
        // displaces (ratio about 0.7, 1.3 at 26 m, 1.8 at 90 m); the cell masses are tuned against the starter
        // hulls by `data-weapons`, so this only guards the geometry from getting heavier.
        if l >= 26.0 {
            assert!(ratio > if l >= 90.0 { 1.6 } else { 1.2 }, "{l} m: {displacement_kg} kg displaced, {structure_kg} kg steel");
        }
    }
}

#[test]
fn unusual_shells_voxelize_cleanly() {
    let base = default_shell(40.0);
    let mut variants: Vec<(&str, HullShell)> = vec![
        ("reverse bow and transom rake", HullShell { bow_rake_deg: -25.0, transom_rake_deg: -20.0, ..base.clone() }),
        ("steep rakes", HullShell { bow_rake_deg: 55.0, transom_rake_deg: 40.0, ..base.clone() }),
        ("double-ended", HullShell { transom_width: 0.0, ..base.clone() }),
        ("thick plating", HullShell { shell_cells: 4, ..default_shell(90.0) }),
        ("barge", barge()),
        ("90 m default", default_shell(90.0)),
    ];
    let mut tumble = base.clone();
    for s in &mut tumble.stations {
        s.flare_deg = -25.0;
    }
    variants.push(("tumblehome", tumble));
    let mut deep_v = base.clone();
    for s in &mut deep_v.stations {
        s.deadrise_deg = 40.0;
        s.flare_deg = 35.0;
    }
    variants.push(("deep V with flare", deep_v));
    let mut many = default_shell(60.0);
    many.stations = (0..16)
        .map(|k| {
            let t = k as f64 / 15.0;
            Station { t, beam_m: 2.0 + 6.0 * (1.0 - (2.0 * t - 1.0) * (2.0 * t - 1.0)), draft_m: 1.0 + (k % 3) as f64, deadrise_deg: 10.0, flare_deg: 5.0, sheer_m: 2.0 + (k % 4) as f64 }
        })
        .collect();
    variants.push(("16 wavy stations", many));
    for (label, shell) in variants {
        assert_eq!(validate_shell(&shell, MAX_ENEMY_HULL_M), Ok(()), "{label}");
        let (_, interior_air, _) = check_structure(&hull_of(shell), label);
        assert!(interior_air > 0, "{label}");
    }
}

#[test]
fn pinched_stations_are_rejected() {
    let mut s = default_shell(40.0);
    s.stations[3].beam_m = 0.4;
    assert_eq!(validate_shell(&s, 90.0), Err(ShellError::Param("beam_m")));
    // The end stations may close to nothing.
    let mut s = default_shell(40.0);
    s.stations[0].beam_m = 0.0;
    let n = s.stations.len();
    s.stations[n - 1].beam_m = 0.0;
    assert_eq!(validate_shell(&s, 90.0), Ok(()));
}

#[test]
fn paint_bands_and_decks() {
    assert!(PAINT_DECK >= PAINT_SIDES + 3, "decks at least 3 swatch steps darker than sides");
    // The voxelizer's indices must name the tokens the module doc promises in the default paint table.
    use crate::blueprint::io::default_paint_index;
    assert_eq!(default_paint_index("hull.haze"), Some(PAINT_SIDES));
    assert_eq!(default_paint_index("hull.deck"), Some(PAINT_DECK));
    assert_eq!(default_paint_index("hull.black"), Some(PAINT_BOOT_TOPPING));
    assert_eq!(default_paint_index("hull.antifouling"), Some(PAINT_ANTIFOULING));
    let hull = default_hull(50.0);
    let v = voxelize(&hull);
    let g = &v.grid;
    let wl = v.design_waterline_y;
    let field = ShellField::new(&default_shell(50.0)).unwrap();
    let mut counts = [0usize; 16];
    for (i, c) in g.cells.iter().enumerate() {
        if c.is_empty() {
            continue;
        }
        assert_eq!(c.material, Material::Steel);
        counts[c.paint as usize] += 1;
        let p = pos(g, i);
        let above = g.get(CellPos::new(p.x, p.y + 1, p.z));
        // Plating facing the sea below the waterline is antifouling; the waterline row is the black band.
        let outer = Face::ALL.iter().any(|f| {
            let (dx, dy, dz) = f.offset();
            let n = CellPos::new(p.x + dx, p.y + dy, p.z + dz);
            !g.in_bounds(n) || (g.get(n).is_empty() && field.sdf(n.centre_m()) > 0.0)
        });
        if outer {
            if p.y < wl {
                assert_eq!(c.paint, PAINT_ANTIFOULING, "{p:?}");
            } else if p.y == wl {
                assert_eq!(c.paint, PAINT_BOOT_TOPPING, "{p:?}");
            } else if c.paint == PAINT_DECK {
                assert!(above.is_empty(), "deck colour on a covered cell at {p:?}");
            } else {
                assert_eq!(c.paint, PAINT_SIDES, "{p:?}");
            }
        }
    }
    for k in [PAINT_SIDES, PAINT_DECK, PAINT_BOOT_TOPPING, PAINT_ANTIFOULING] {
        assert!(counts[k as usize] > 0, "no cells with paint {k}");
    }
    // The weather deck is mostly deck colour: amidships, the topmost cell of most columns.
    let x = g.size[0] / 2;
    let mut deck = 0;
    let mut cols = 0;
    for z in 0..g.size[2] {
        if let Some(y) = (0..g.size[1]).rev().find(|&y| !g.get(CellPos::new(x as i16, y as i16, z as i16)).is_empty()) {
            cols += 1;
            deck += (g.get(CellPos::new(x as i16, y as i16, z as i16)).paint == PAINT_DECK) as usize;
        }
    }
    assert!(deck * 10 >= cols * 7, "{deck} of {cols} top cells amidships are deck colour");
}

#[test]
fn flat_lattice_surfaces_round_to_cubes() {
    // Bottom, sides and deck all lie on lattice planes: the cells are a plain box, no wedge rows.
    let shell = barge();
    let lay = shell_layout(&shell);
    assert_eq!(lay.size, [40, 8, 12]);
    assert_eq!(lay.waterline_y, 4);
    let v = voxelize(&Hull { source: HullSource::Lofted(shell.clone()), planes: vec![], design_waterline_y: 4 });
    let g = &v.grid;
    for (i, c) in g.cells.iter().enumerate() {
        let p = pos(g, i);
        let boundary = p.x == 0 || p.x == 39 || p.y == 0 || p.y == 7 || p.z == 0 || p.z == 11;
        assert_eq!(!c.is_empty(), boundary, "{p:?}");
        if !c.is_empty() {
            assert_eq!(c.shape.kind(), ShapeKind::Cube, "{p:?}");
        }
    }
    // Distances on the barge are exact away from edges.
    let sdf = |p: [f64; 3]| shell_sdf(&shell, p);
    assert!((sdf([10.0, 2.0, 3.0]) + 2.0).abs() < 1e-9); // on the waterline: 2 m from deck and bottom
    assert!((sdf([10.0, 5.0, 3.0]) - 1.0).abs() < 1e-9); // 1 m above the deck
    assert!((sdf([10.0, 2.0, -1.0]) - 1.0).abs() < 1e-9); // 1 m off the port side
    assert!((sdf([-2.0, 2.0, 3.0]) - 2.0).abs() < 1e-9); // 2 m aft of the transom
    assert!((sdf([23.0, 8.0, 3.0]) - 5.0).abs() < 1e-9); // 3 m forward of the bow and 4 m above the deck
}

#[test]
fn shaped_cells_face_out_and_curved_hulls_use_them() {
    let v = voxelize(&default_hull(90.0));
    let shaped = v.grid.cells.iter().filter(|c| !c.is_empty() && !c.shape.is_cube()).count();
    assert!(shaped * 20 > v.grid.solid_count(), "only {shaped} shaped cells");
    // A shaped cell never sits against interior air with a partly open face.
    let field = ShellField::new(&default_shell(90.0)).unwrap();
    let g = &v.grid;
    for (i, c) in g.cells.iter().enumerate() {
        if c.is_empty() || c.shape.is_cube() {
            continue;
        }
        let p = pos(g, i);
        let full = crate::cell::full_face_mask(c.shape);
        for (b, f) in Face::ALL.iter().enumerate() {
            let (dx, dy, dz) = f.offset();
            let n = CellPos::new(p.x + dx, p.y + dy, p.z + dz);
            if g.in_bounds(n) && g.get(n).is_empty() && field.sdf(n.centre_m()) <= 0.0 {
                assert!(full & (1 << b) != 0, "shaped cell at {p:?} opens into the hull through {f:?}");
            }
        }
    }
}

#[test]
fn planes_cut_compartments() {
    let hull = default_hull(60.0);
    let decks = hull.planes.iter().filter(|p| p.axis == Axis::Y).count();
    let bulkheads = hull.planes.iter().filter(|p| p.axis == Axis::X).count();
    assert!(decks >= 1 && bulkheads == 5, "{decks} decks, {bulkheads} bulkheads");
    let with = voxelize(&hull);
    let without = voxelize(&Hull { planes: vec![], ..hull.clone() });
    assert!(with.grid.solid_count() > without.grid.solid_count());
    let g = &with.grid;
    for plane in &hull.planes {
        // Every cell of the plane's layer that is inside the hull is steel.
        let HullSource::Lofted(s) = &hull.source else { unreachable!() };
        let field = ShellField::new(s).unwrap();
        for (i, c) in g.cells.iter().enumerate() {
            let p = pos(g, i);
            let on = match plane.axis { Axis::X => p.x, Axis::Y => p.y, Axis::Z => p.z } == plane.offset;
            if on && field.sdf(p.centre_m()) <= 0.0 {
                assert!(!c.is_empty(), "hole in plane {plane:?} at {p:?}");
            }
        }
    }
    // A plane outside the grid is harmless.
    let far = Hull { planes: vec![InternalPlane { axis: Axis::Z, offset: 900, watertight: true }, InternalPlane { axis: Axis::X, offset: -3, watertight: false }], ..hull.clone() };
    assert_eq!(voxelize(&far).grid, without.grid);
}

#[test]
fn waterline_follows_hull_field_and_clamps() {
    let mut hull = default_hull(30.0);
    let base = hull.design_waterline_y;
    hull.design_waterline_y = base + 2;
    let v = voxelize(&hull);
    assert_eq!(v.design_waterline_y, base + 2);
    assert!(v.grid.cells.iter().enumerate().any(|(i, c)| c.paint == PAINT_BOOT_TOPPING && pos(&v.grid, i).y == base + 2));
    assert!(!v.grid.cells.iter().enumerate().any(|(i, c)| c.paint == PAINT_BOOT_TOPPING && pos(&v.grid, i).y != base + 2));
    hull.design_waterline_y = i16::MAX;
    let v = voxelize(&hull);
    assert_eq!(v.design_waterline_y, v.grid.size[1] as i16);
    assert!(v.grid.cells.iter().all(|c| c.is_empty() || c.paint == PAINT_ANTIFOULING || c.paint == PAINT_DECK || c.paint == PAINT_SIDES));
}

#[test]
fn shell_options_change_the_cells() {
    let base = default_shell(40.0);
    let thick = HullShell { shell_cells: 3, ..base.clone() };
    let (a, b) = (voxelize(&hull_of(base.clone())), voxelize(&hull_of(thick)));
    assert_eq!(a.grid.size, b.grid.size);
    assert!(b.grid.solid_count() * 10 > a.grid.solid_count() * 14, "{} vs {}", b.grid.solid_count(), a.grid.solid_count());
    // A double-ended stern is narrower aft than a full transom.
    let pointed = HullShell { transom_width: 0.0, ..base.clone() };
    let lay_full = shell_layout(&base);
    let lay_pointed = shell_layout(&pointed);
    let width_at = |s: &HullShell, x: f64| {
        let f = ShellField::new(s).unwrap();
        (0..400).filter(|k| f.sdf_design([x, 0.5, (*k as f64 - 200.0) * 0.05]) <= 0.0).count()
    };
    assert!(width_at(&pointed, 0.6) < width_at(&base, 0.6) / 3);
    assert_eq!(lay_full.size[2], lay_pointed.size[2]);
    // Tumblehome narrows the deck relative to the chine.
    let mut tumble = base.clone();
    for s in &mut tumble.stations {
        s.flare_deg = -15.0;
    }
    let f = ShellField::new(&tumble).unwrap();
    let mid = 20.0;
    let hb = |y: f64| (0..400).filter(|k| f.sdf_design([mid, y, *k as f64 * 0.02]) <= 0.0).count();
    assert!(hb(1.0) < hb(-0.5));
}

#[test]
fn deterministic_and_fast() {
    let hull = default_hull(90.0);
    let a = voxelize(&hull);
    let start = std::time::Instant::now();
    let b = voxelize(&hull);
    let ms_90 = start.elapsed().as_secs_f64() * 1e3;
    assert_eq!(a, b);
    let big = default_hull(150.0);
    let start = std::time::Instant::now();
    let _ = voxelize(&big);
    let ms_150 = start.elapsed().as_secs_f64() * 1e3;
    eprintln!("voxelize: 90 m {ms_90:.2} ms ({} cells), 150 m {ms_150:.2} ms", a.grid.solid_count());
    // The gauge budget is 100 ms at 90 m (L177); the voxelizer gets a small slice of it. Timing is only
    // meaningful in an optimised build.
    if !cfg!(debug_assertions) {
        assert!(ms_90 < 50.0, "90 m voxelize took {ms_90} ms");
        assert!(ms_150 < 100.0, "150 m voxelize took {ms_150} ms");
    }
}

