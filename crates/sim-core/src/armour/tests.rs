use super::*;
use crate::entity::EntityId;
use crate::projectiles::ProjectileId;
use content::cell::{Cell, CellGrid, CellShape, Material};
use content::part::weapons::CalibreBand;
use content::part::Flammable;
use glam::DVec3;
use std::sync::Arc;

const STEEL: Cell = Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE };
fn armour(t: ArmourTier) -> Cell { Cell { armour: t, ..STEEL } }

fn build(size: [u16; 3], f: impl Fn(i16, i16, i16) -> Cell) -> VesselGrid {
    let mut g = CellGrid::new(size);
    for y in 0..size[1] as i16 { for z in 0..size[2] as i16 { for x in 0..size[0] as i16 { g.set(CellPos::new(x, y, z), f(x, y, z)); } } }
    VesselGrid::build(Arc::new(g), &[], &|_| (false, Flammable::No))
}

fn at(g: &VesselGrid, r: &HitResult, x: i16, y: i16, z: i16) -> u8 {
    let i = g.index(CellPos::new(x, y, z)).unwrap();
    r.damage.iter().find(|d| d.cell == i).map(|d| d.buckets).unwrap_or(0)
}
fn at_list(g: &VesselGrid, d: &[CellDamage], x: i16, y: i16, z: i16) -> u8 {
    let i = g.index(CellPos::new(x, y, z)).unwrap();
    d.iter().find(|c| c.cell == i).map(|c| c.buckets).unwrap_or(0)
}

fn band(b: CalibreBand) -> BandDef { BandDef::spec_default(b) }

/// A shell at its 1 km speed (penetration = P_1km).
fn shell(entry: [f64; 3], dir: [f64; 3], b: CalibreBand, rounds: u8) -> ShellHit {
    ShellHit { tick: 0, projectile: ProjectileId(0), vessel: EntityId::NONE, entry, dir, first_cell: CellIndex(0), speed_mps: 700.0, v_1km: 700.0, band: b, calibre_mm: 76, shooter: EntityId::NONE, rounds, world_point: DVec3::ZERO }
}

fn sorted_unique(r: &[CellDamage]) -> bool { r.windows(2).all(|w| w[0].cell < w[1].cell) }

#[test]
fn he_falloff_matches_the_spec_table() {
    let g = build([9, 9, 9], |_, _, _| STEEL);
    let mut out = Vec::new();
    let c = g.index(CellPos::new(4, 4, 4)).unwrap();
    he_burst(&g, c, 8, 3, &mut out);
    assert!(sorted_unique(&out));
    assert_eq!([at_list(&g, &out, 4, 4, 4), at_list(&g, &out, 5, 4, 4), at_list(&g, &out, 6, 4, 4), at_list(&g, &out, 7, 4, 4)], [8, 6, 4, 2]);
    assert_eq!(at_list(&g, &out, 4, 1, 4), 2);
    assert_eq!(at_list(&g, &out, 5, 5, 4), 6); // d = sqrt 2: ceil(5.17)
    assert_eq!(at_list(&g, &out, 6, 6, 5), 2); // d = 3
    assert_eq!(at_list(&g, &out, 7, 5, 4), 0); // d = sqrt 10 > 3
    assert_eq!(at_list(&g, &out, 8, 4, 4), 0);
}

#[test]
fn armour_takes_half_b_and_blocks_he_while_intact() {
    // Steel block with an 80 mm plane at x = 5.
    let mut g = build([9, 9, 9], |x, _, _| if x == 5 { armour(ArmourTier::Mm80) } else { STEEL });
    let c = g.index(CellPos::new(4, 4, 4)).unwrap();
    let mut out = Vec::new();
    he_burst(&g, c, 8, 3, &mut out);
    assert_eq!(at_list(&g, &out, 5, 4, 4), 3); // round(4 * 0.75)
    assert_eq!(at_list(&g, &out, 3, 4, 4), 6);
    assert_eq!(at_list(&g, &out, 6, 4, 4), 0); // shielded
    assert_eq!(at_list(&g, &out, 7, 4, 4), 0);
    // Damaged armour (bucket 2) no longer shields.
    let i = g.index(CellPos::new(5, 4, 4)).unwrap();
    g.set_bucket(i, 2);
    let mut out = Vec::new();
    he_burst(&g, c, 8, 3, &mut out);
    assert_eq!(at_list(&g, &out, 6, 4, 4), 4);
    // An armour cell at the burst centre takes floor(B / 2) and does not block.
    let g = build([9, 9, 9], |x, y, z| if (x, y, z) == (4, 4, 4) { armour(ArmourTier::Mm40) } else { STEEL });
    let mut out = Vec::new();
    he_burst(&g, g.index(CellPos::new(4, 4, 4)).unwrap(), 8, 3, &mut out);
    assert_eq!(at_list(&g, &out, 4, 4, 4), 4);
    assert_eq!(at_list(&g, &out, 5, 4, 4), 6);
}

#[test]
fn cook_off_destroys_everything_in_radius() {
    let g = build([9, 9, 9], |x, _, _| if x == 5 { armour(ArmourTier::Mm80) } else { STEEL });
    let mut out = Vec::new();
    he_burst(&g, g.index(CellPos::new(4, 4, 4)).unwrap(), COOKOFF_B, 3, &mut out);
    assert_eq!(at_list(&g, &out, 7, 4, 4), 8);
    assert_eq!(at_list(&g, &out, 5, 4, 4), 8);
    assert_eq!(at_list(&g, &out, 8, 4, 4), 0);
    assert_eq!(he_burst(&g, CellIndex(u32::MAX), 8, 3, &mut out), 0);
}

/// 80 mm outer plate at x = 0 over steel, shells along +X at the plate's centre.
fn plated() -> VesselGrid { build([10, 5, 5], |x, _, _| if x == 0 { armour(ArmourTier::Mm80) } else { STEEL }) }

#[test]
fn heavy_band_penetrates_80mm_and_fuses_after_its_delay() {
    let g = plated();
    let r = resolve_shell(&g, &shell([0.0, 2.5, 2.5], [1.0, 0.0, 0.0], CalibreBand::Large, 1), &band(CalibreBand::Large));
    assert_eq!(r.outcome, Some(HitOutcome::Penetrated));
    // 90 - 80 = 10 mm left, four steel cells at 2 mm: the fuse (delay 4) fires in the fifth solid cell.
    assert_eq!(r.burst, g.index(CellPos::new(4, 2, 2)));
    assert!(r.can_ignite && sorted_unique(&r.damage));
    // The burst cell takes K + B.
    assert_eq!(at(&g, &r, 4, 2, 2), 8);
    // The plate cell took path damage (4) plus HE at d = 4 > r: just 4.
    assert_eq!(at(&g, &r, 0, 2, 2), 4);
}

#[test]
fn medium_band_spalls_and_autocannon_stops_on_80mm() {
    let g = plated();
    let r = resolve_shell(&g, &shell([0.0, 2.5, 2.5], [1.0, 0.0, 0.0], CalibreBand::Medium, 1), &band(CalibreBand::Medium));
    assert_eq!(r.outcome, Some(HitOutcome::Spalled));
    assert_eq!(r.burst, g.index(CellPos::new(0, 2, 2)));
    // Burst in front of the plate: the plate takes round(3 * 2/3) = 2, the steel behind is shielded and
    // only takes the 1 bucket of spall.
    assert_eq!(at(&g, &r, 0, 2, 2), 2);
    assert_eq!(at(&g, &r, 1, 2, 2), 1);
    assert_eq!(at(&g, &r, 2, 2, 2), 0);
    let r = resolve_shell(&g, &shell([0.0, 2.5, 2.5], [1.0, 0.0, 0.0], CalibreBand::Auto, 1), &band(CalibreBand::Auto));
    assert_eq!(r.outcome, Some(HitOutcome::Stopped));
    assert_eq!(at(&g, &r, 1, 2, 2), 0);
    assert!(r.can_ignite);
}

#[test]
fn grazing_hit_on_cube_deck_armour_ricochets() {
    let g = build([10, 4, 4], |_, y, _| if y == 3 { armour(ArmourTier::Mm15) } else { STEEL });
    let a = 15f64.to_radians();
    let r = resolve_shell(&g, &shell([2.0, 4.0, 2.5], [libm::cos(a), -libm::sin(a), 0.0], CalibreBand::Large, 1), &band(CalibreBand::Large));
    assert_eq!(r.outcome, Some(HitOutcome::Ricochet));
    assert!(r.damage.is_empty() && !r.can_ignite && r.burst.is_none());
    // 30 degrees down: obliquity 60, the 15 mm deck costs 30 mm; the autocannon's 20 mm stops on it.
    let a = 30f64.to_radians();
    let r = resolve_shell(&g, &shell([2.0, 4.0, 2.5], [libm::cos(a), -libm::sin(a), 0.0], CalibreBand::Auto, 1), &band(CalibreBand::Auto));
    assert_eq!(r.outcome, Some(HitOutcome::Spalled));
}

#[test]
fn oblique_plate_is_charged_per_layer_not_per_cell() {
    // 40 mm vertical plate one cell thick, shell at 45 degrees in the x-z plane: 56.6 mm effective.
    let one = build([8, 3, 20], |x, _, _| if x == 0 { armour(ArmourTier::Mm40) } else if x == 7 { STEEL } else { Cell::EMPTY });
    let s = core::f64::consts::FRAC_1_SQRT_2;
    let r = resolve_shell(&one, &shell([0.0, 1.5, 2.3], [s, 0.0, s], CalibreBand::Large, 1), &band(CalibreBand::Large));
    assert!(matches!(r.outcome, Some(HitOutcome::Penetrated) | Some(HitOutcome::PassedThrough)), "{:?}", r.outcome);
    // Two layers: 113 mm needed, 90 available; the second layer stops it with over half left, so it spalls.
    let two = build([8, 3, 20], |x, _, _| if x <= 1 { armour(ArmourTier::Mm40) } else if x == 7 { STEEL } else { Cell::EMPTY });
    let r = resolve_shell(&two, &shell([0.0, 1.5, 2.3], [s, 0.0, s], CalibreBand::Large, 1), &band(CalibreBand::Large));
    assert_eq!(r.outcome, Some(HitOutcome::Spalled));
}

/// Hollow hull: steel walls one cell thick.
fn hollow() -> VesselGrid { build([8, 6, 20], |x, y, z| if x == 0 || y == 0 || z == 0 || x == 7 || y == 5 || z == 19 { STEEL } else { Cell::EMPTY }) }

#[test]
fn big_shell_overpenetrates_a_light_hull_and_autocannon_fuses_on_the_far_wall() {
    let g = hollow();
    let hit = shell([4.5, 3.5, 0.0], [0.0, 0.0, 1.0], CalibreBand::Large, 1);
    let r = resolve_shell(&g, &hit, &band(CalibreBand::Large));
    assert_eq!(r.outcome, Some(HitOutcome::PassedThrough));
    assert_eq!(r.burst, None);
    assert_eq!(r.damage.len(), 2);
    assert!(r.damage.iter().all(|d| d.buckets == 4));
    assert!(r.can_ignite);
    // Merged rounds apply twice.
    let r2 = resolve_shell(&g, &ShellHit { rounds: 2, ..hit }, &band(CalibreBand::Large));
    assert!(r2.damage.iter().all(|d| d.buckets == 8));
    let r = resolve_shell(&g, &shell([4.5, 3.5, 0.0], [0.0, 0.0, 1.0], CalibreBand::Auto, 1), &band(CalibreBand::Auto));
    assert_eq!(r.outcome, Some(HitOutcome::Penetrated));
    assert_eq!(r.burst, g.index(CellPos::new(4, 3, 19)));
    assert_eq!(at(&g, &r, 4, 3, 19), 6); // K 2 + B 4
    assert_eq!(at(&g, &r, 4, 3, 0), 2);
}

#[test]
fn bad_input_resolves_to_nothing() {
    let g = hollow();
    assert_eq!(resolve_shell(&g, &shell([4.5, 3.5, 0.0], [f64::NAN, 0.0, 1.0], CalibreBand::Auto, 1), &band(CalibreBand::Auto)), HitResult::default());
    assert_eq!(resolve_shell(&g, &shell([4.5, 3.5, 0.0], [0.0, 0.0, 0.0], CalibreBand::Auto, 1), &band(CalibreBand::Auto)), HitResult::default());
    let inert = VesselGrid::build(Arc::new(CellGrid { size: [4, 4, 4], cells: vec![] }), &[], &|_| (false, Flammable::No));
    let r = resolve_shell(&inert, &shell([0.0, 1.0, 1.0], [1.0, 0.0, 0.0], CalibreBand::Auto, 1), &band(CalibreBand::Auto));
    assert!(r.damage.is_empty());
    let mut out = Vec::new();
    assert_eq!(he_burst(&inert, CellIndex(0), 8, 3, &mut out), 0);
    assert_eq!(resolve_warhead(&inert, [0.0; 3], [1.0, 0.0, 0.0], Fuse::Contact, 8, 4, &band(CalibreBand::Auto)), HitResult::default());
}

#[test]
fn warheads_burst_by_fuse_type() {
    let g = build([12, 6, 12], |_, _, _| STEEL);
    let auto = band(CalibreBand::Auto);
    // Torpedo: contact burst at the first solid cell, B 8, r 4.
    let r = resolve_warhead(&g, [-3.0, 2.5, 6.5], [1.0, 0.0, 0.0], Fuse::Contact, 8, 4, &auto);
    assert_eq!(r.burst, g.index(CellPos::new(0, 2, 6)));
    assert_eq!(at(&g, &r, 0, 2, 6), 8);
    assert_eq!(at(&g, &r, 4, 2, 6), 2); // ceil(8 * 1/5)
    assert!(r.can_ignite);
    // Mine below the keel: nearest solid cell.
    let r = resolve_warhead(&g, [6.2, -8.0, 3.7], [0.0; 3], Fuse::Influence { radius_m: 10.0 }, 8, 6, &auto);
    assert_eq!(r.burst, g.index(CellPos::new(6, 0, 3)));
    assert!(sorted_unique(&r.damage));
    // Anti-ship missile: penetrates up to 40 mm, bursts after a 2-cell delay.
    let r = resolve_warhead(&g, [0.0, 2.5, 6.5], [1.0, 0.0, 0.0], Fuse::Penetrate { armour_mm: 40.0, delay_cells: 2 }, 8, 4, &auto);
    assert_eq!(r.outcome, Some(HitOutcome::Penetrated));
    assert_eq!(r.burst, g.index(CellPos::new(2, 2, 6)));
    // SAM against a hull: the autocannon band's values (delay 1, K 2, B 4, r 1).
    let r = resolve_warhead(&g, [0.0, 2.5, 6.5], [1.0, 0.0, 0.0], Fuse::Proximity { radius_m: 8.0 }, 4, 1, &auto);
    assert_eq!(r.burst, g.index(CellPos::new(1, 2, 6)));
    assert_eq!(at(&g, &r, 1, 2, 6), 6);
    assert_eq!(resolve_warhead(&g, [0.0, 2.5, 6.5], [1.0, 0.0, 0.0], Fuse::None, 8, 4, &auto).damage.len(), 0);
}

#[test]
fn thin_strake_does_not_open_a_free_path_into_heavy_belt() {
    // Vertical plate at x = 0: a 5 mm strake (y = 3) over an 80 mm belt (y = 0..=2). A plunging shell enters
    // the strake's outer face and steps down into the belt through the belt's top face.
    let g = build([4, 5, 3], |x, y, _| match (x, y) { (0, 3) => armour(ArmourTier::Mm5), (0, 0..=2) => armour(ArmourTier::Mm80), _ => Cell::EMPTY });
    let a = 60f64.to_radians();
    let r = resolve_shell(&g, &shell([0.0, 3.9, 1.5], [libm::cos(a), -libm::sin(a), 0.0], CalibreBand::Large, 1), &band(CalibreBand::Large));
    assert!(matches!(r.outcome, Some(HitOutcome::Stopped) | Some(HitOutcome::Spalled)), "{:?}", r.outcome);
    assert_eq!(r.burst, g.index(CellPos::new(0, 2, 1)));
}

#[test]
fn sloped_armour_uses_the_shaped_cell_normal() {
    use content::cell::ShapeKind;
    let wedge = CellShape::new(ShapeKind::Wedge, 0);
    let n = content::cell::armour_normal(wedge, [-1.0, 0.0, 0.0]);
    let s = core::f64::consts::FRAC_1_SQRT_2;
    assert!((n[0] - s).abs() < 1e-9 && (n[1] - s).abs() < 1e-9 && n[2].abs() < 1e-9, "{n:?}");
    // A 40 mm cell at the +X end of a steel row, hit horizontally along -X by the 45 mm band.
    let make = |shape: CellShape| build([6, 3, 3], move |x, y, z| if (x, y, z) == (5, 1, 1) { Cell { shape, ..armour(ArmourTier::Mm40) } } else if y == 1 && z == 1 { STEEL } else { Cell::EMPTY });
    let hit = shell([6.0, 1.5, 1.5], [-1.0, 0.0, 0.0], CalibreBand::Medium, 1);
    // Square cube face: 40 mm, penetrated; the fuse (delay 2) fires in the third solid cell.
    let cube = make(CellShape::CUBE);
    let r = resolve_shell(&cube, &hit, &band(CalibreBand::Medium));
    assert_eq!(r.outcome, Some(HitOutcome::Penetrated));
    assert_eq!(r.burst, cube.index(CellPos::new(3, 1, 1)));
    // 45 degree slope: 56.6 mm needed, 45 available (over half), so it spalls the cell behind.
    let sloped = make(wedge);
    let r = resolve_shell(&sloped, &hit, &band(CalibreBand::Medium));
    assert_eq!(r.outcome, Some(HitOutcome::Spalled));
    assert_eq!(at(&sloped, &r, 4, 1, 1), 1);
}
