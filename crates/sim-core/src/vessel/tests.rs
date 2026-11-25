use super::testkit::{blueprint, place, vessel};
use super::*;
use crate::compartments::testkit::box_hull;
use crate::compartments::OpeningKind;
use crate::math::{DQuat, DVec3};
use crate::parts::PartState;
use content::cell::CELL_M;
use content::tuning::StabilityTuning;

fn cat() -> &'static Catalogue { content::catalogue::catalogue() }

/// 22 m x 5 m x 6 m steel box: bulkheads at x = 11, 22, 33, a deck at y = 5 and a 40 mm bottom, so it floats
/// about 0.87 m deep with its lower bays below the waterline.
fn box_bp() -> Blueprint {
    let mut g = box_hull([44, 10, 12], &[11, 22, 33], &[5]);
    for c in g.cells.iter_mut().take(44 * 12) { c.armour = content::cell::ArmourTier::Mm40; }
    blueprint(&g, 2)
}

fn level_pose(p: &VesselPrefab) -> Pose { Pose { pos: DVec3::new(0.0, -p.waterline_y, 0.0), rot: DQuat::IDENTITY } }

#[test]
fn prefab_is_deterministic_and_complete() {
    let mut bp = box_bp();
    place(&mut bp, "berths.crew", [2, 1, 2]);
    place(&mut bp, "berths.crew", [13, 1, 2]);
    place(&mut bp, "pump.small", [6, 1, 6]);
    let a = build_prefab(&bp, cat());
    let b = build_prefab(&bp, cat());
    assert_eq!(a, b);
    assert!(!a.hash.is_empty());
    assert_eq!(a.hash, content::blueprint::io::compute_hash(&bp));
    assert_eq!(a.parts.len(), 3);
    assert_eq!(a.berths, 8);
    assert_eq!(a.dims, [22.0, 5.0, 6.0]);
    assert!(a.dry_mass.mass_kg > 10_000.0);
    assert!(!a.cuboids.is_empty() && !a.blocks.blocks.is_empty());
    assert_eq!(a.work_counts[0] as usize, a.cuboids.len());
    // Eight compartments: four bays, below and above the deck.
    assert_eq!(a.work_counts[3], 8);
    assert!(a.parts.iter().all(|p| p.compartment.is_some()));
    // The loaded box floats with a draft inside the hull, and the lower bays carry flood valves.
    assert!(a.waterline_y > 0.5 && a.waterline_y < 2.5, "waterline {}", a.waterline_y);
    let valves: Vec<_> = a.compartments.openings.iter().filter(|o| o.kind == OpeningKind::FloodValve).collect();
    assert_eq!(valves.len(), 4);
    assert!(valves.iter().all(|o| o.area_m2 == 0.1));
    // Sealed air is set for the renderer.
    assert_eq!(a.grid.sealed_air.len(), a.grid.design.cells.len().div_ceil(64));
    assert!(a.grid.sealed_air.iter().any(|&w| w != 0));
    // The root is a live keel cell.
    let root = a.grid.pos(a.root_cell);
    assert_eq!(root.y, 0);
    assert!(a.grid.is_solid(root));
}

#[test]
fn waterline_matches_displacement() {
    let p = build_prefab(&box_bp(), cat());
    // A level box: displaced volume = length * beam * draft.
    let rho = cat().tuning.buoyancy.water_density;
    let draft = p.dry_mass.mass_kg / rho / (22.0 * 6.0);
    assert!((p.waterline_y - draft).abs() < 0.05, "waterline {} vs {}", p.waterline_y, draft);
}

#[test]
fn bad_blueprint_does_not_panic() {
    let mut bp = box_bp();
    place(&mut bp, "no.such.part", [3, 3, 3]);
    place(&mut bp, "pump.small", [-5, 900, 3]);
    let p = build_prefab(&bp, cat());
    assert_eq!(p.parts.len(), 2);
    let empty = blueprint(&content::cell::CellGrid::new([0, 0, 0]), 0);
    let e = build_prefab(&empty, cat());
    assert_eq!(e.dims, [0.0; 3]);
    let a = projected_areas_prefab(&e, cat());
    assert!(a.above_m2.iter().all(|&v| v == 0.0));
    let v = vessel(&e, Pose::default());
    assert_eq!(v.top_height_m(), 0.0);
}

#[test]
fn getters_follow_the_pose() {
    let p = build_prefab(&box_bp(), cat());
    let mut v = vessel(&p, level_pose(&p));
    assert!((v.top_height_m() - (5.0 - p.waterline_y)).abs() < 1e-9);
    // Heel 10 degrees to starboard: starboard side down.
    let heel = 10f64.to_radians();
    v.kin.pose.rot = DQuat::from_xyzw(libm::sin(0.5 * heel), 0.0, 0.0, libm::cos(0.5 * heel));
    let (h, t) = v.heel_trim();
    assert!((h - heel).abs() < 1e-9 && t.abs() < 1e-9);
    // Forward speed along the bow at any heading.
    v.kin.pose = Pose::from_heading(DVec3::ZERO, 1.0);
    v.kin.lin_vel = v.kin.pose.rot * DVec3::new(5.0, 0.0, 2.0);
    assert!((v.forward_speed() - 5.0).abs() < 1e-9);
    v.kin.lin_vel = DVec3::splat(f64::NAN);
    assert_eq!(v.forward_speed(), 0.0);
    // Propulsion power sums the propulsors.
    v.parts.push(crate::compartments::testkit::part(9, 0, vec![], &p.grid.design, PartState::Propulsor { shaft_kw: 700.0, thrust_n: 1.0, steer_rad: 0.0 }));
    assert_eq!(v.propulsion_kw(), 700.0);
}

#[test]
fn status_disabled_capsized_sunk() {
    let mut bp = box_bp();
    place(&mut bp, "engine.diesel.small", [2, 1, 2]);
    place(&mut bp, "prop.small", [0, 0, 5]);
    let p = build_prefab(&bp, cat());
    let calm = |_: f64, _: f64| 0.0;
    let t = StabilityTuning::default();
    let mut v = vessel(&p, level_pose(&p));
    assert_eq!(v.evaluate_status(&calm, &t), None);
    assert_eq!(v.status, VesselStatus::Active);
    v.struck = true;
    assert_eq!(v.evaluate_status(&calm, &t), Some(VesselStatus::Disabled));
    v.struck = false;
    assert_eq!(v.evaluate_status(&calm, &t), Some(VesselStatus::Active));
    // Losing the engine disables.
    v.parts[0].level = content::enums::DamageLevel::Destroyed;
    assert_eq!(v.evaluate_status(&calm, &t), Some(VesselStatus::Disabled));
    // On its side at 100 degrees with the high deck edge dry: lost after 10 s of calls, not before.
    let a = 100f64.to_radians();
    v.kin.pose.rot = DQuat::from_xyzw(libm::sin(0.5 * a), 0.0, 0.0, libm::cos(0.5 * a));
    v.kin.pose.pos.y = 2.0;
    for _ in 0..599 { assert_eq!(v.evaluate_status(&calm, &t), None); }
    assert_eq!(v.evaluate_status(&calm, &t), Some(VesselStatus::Lost(LossCause::Capsized)));
    assert_eq!(v.evaluate_status(&calm, &t), None);
    // A knockdown that rights itself resets the timer.
    let mut k = vessel(&p, level_pose(&p));
    k.kin.pose = v.kin.pose;
    for _ in 0..300 { k.evaluate_status(&calm, &t); }
    k.kin.pose = level_pose(&p);
    k.evaluate_status(&calm, &t);
    assert_eq!(k.capsize_ticks, 0);
    // Fully inverted with the deck under water: sunk at once.
    let mut u = vessel(&p, level_pose(&p));
    u.kin.pose.rot = DQuat::from_xyzw(1.0, 0.0, 0.0, 0.0);
    u.kin.pose.pos.y = 3.0;
    assert_eq!(u.evaluate_status(&calm, &t), Some(VesselStatus::Lost(LossCause::Sunk)));
    // Sinking on an even keel.
    let mut w = vessel(&p, level_pose(&p));
    w.kin.pose.pos.y = -4.6;
    assert_eq!(w.evaluate_status(&calm, &t), None);
    w.kin.pose.pos.y = -5.0;
    assert_eq!(w.evaluate_status(&calm, &t), Some(VesselStatus::Lost(LossCause::Sunk)));
}

#[test]
fn sunk_test_survives_a_lost_top_deck() {
    let p = build_prefab(&box_bp(), cat());
    let calm = |_: f64, _: f64| 0.0;
    let t = StabilityTuning::default();
    let mut v = vessel(&p, level_pose(&p));
    // Even a hull with no propulsion is only disabled while it floats.
    assert_eq!(v.evaluate_status(&calm, &t), Some(VesselStatus::Disabled));
    let top = v.compartments.top_deck_y;
    assert_eq!(top, 9);
    let layer: Vec<_> = v.grid.solid_cells().into_iter().filter(|&c| v.grid.pos(c).y == top).collect();
    v.grid.remove_cells(&layer);
    assert_eq!(v.evaluate_status(&calm, &t), None);
    // The layer below now stands in: deck at y = 8 sinks when its centres go under.
    v.kin.pose.pos.y = -4.3;
    assert_eq!(v.evaluate_status(&calm, &t), Some(VesselStatus::Lost(LossCause::Sunk)));
}

#[test]
fn projected_areas_of_a_box() {
    let p = build_prefab(&box_bp(), cat());
    let a = projected_areas_prefab(&p, cat());
    let free = 5.0 - p.waterline_y;
    for k in 0..36 {
        let ang = ((10 * k + 5) as f64).to_radians();
        let width = 22.0 * libm::sin(ang).abs() + 6.0 * libm::cos(ang).abs();
        let rel = |got: f32, want: f64| ((got as f64 - want) / want).abs();
        assert!(rel(a.above_m2[k], width * free) < 0.03, "bin {k}: {} vs {}", a.above_m2[k], width * free);
        assert!(rel(a.submerged_m2[k], width * p.waterline_y) < 0.05, "bin {k}: {} vs {}", a.submerged_m2[k], width * p.waterline_y);
        // Bare steel has factor 1.
        assert!((a.rcs_m2[k] - a.above_m2[k]).abs() < 1e-3);
        assert_eq!(a.above_m2[k], a.above_m2[(k + 18) % 36]);
    }
    assert!(a.above_m2[9] > 2.5 * a.above_m2[0]);
    assert!((a.top_height_m as f64 - free).abs() < 1e-6);
    // The live vessel at its design draft agrees with the prefab.
    let v = vessel(&p, level_pose(&p));
    let b = projected_areas(&v, cat());
    for k in 0..36 { assert!((a.above_m2[k] - b.above_m2[k]).abs() < 1e-3); }
}
