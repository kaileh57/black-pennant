use super::*;
use crate::compartments::testkit::*;
use content::cell::{CellGrid, CellPos};
use content::part::Flammable;
use content::ids::PartSlot;

fn close(a: f64, b: f64) -> bool { (a - b).abs() <= 1e-6 * (1.0 + a.abs().max(b.abs())) }

#[test]
fn single_cell_is_a_cube() {
    let mut g = CellGrid::new([3, 3, 3]);
    g.set(CellPos::new(1, 1, 1), STEEL);
    let v = build(g, &[]);
    let m = dry_mass(&v, &[], &catalogue(vec![]));
    assert_eq!(m.mass_kg, 60.0);
    assert_eq!(m.com, DVec3::splat(0.75));
    let i = 60.0 * 0.25 / 6.0;
    assert!(close(m.inertia.x_axis.x, i) && close(m.inertia.y_axis.y, i) && close(m.inertia.z_axis.z, i));
    assert!(close(m.inertia.x_axis.y, 0.0));
}

#[test]
fn parallel_axis_matches_combine() {
    let mut g = CellGrid::new([8, 2, 2]);
    g.set(CellPos::new(0, 0, 0), STEEL);
    g.set(CellPos::new(7, 1, 1), STEEL);
    let both = dry_mass(&build(g.clone(), &[]), &[], &catalogue(vec![]));
    let mut a = CellGrid::new([8, 2, 2]);
    a.set(CellPos::new(0, 0, 0), STEEL);
    let mut b = CellGrid::new([8, 2, 2]);
    b.set(CellPos::new(7, 1, 1), STEEL);
    let cat = catalogue(vec![]);
    let sum = combine(&dry_mass(&build(a, &[]), &[], &cat), &dry_mass(&build(b, &[]), &[], &cat));
    assert!(close(both.mass_kg, 120.0) && close(sum.mass_kg, 120.0));
    assert!((both.com - sum.com).length() < 1e-9);
    // Two 60 kg points 3.5 m apart in x and 0.5 m in y and z, plus the cubes' own inertia.
    let r = DVec3::new(3.5, 0.5, 0.5) * 0.5;
    let iyy = 2.0 * 60.0 * (r.x * r.x + r.z * r.z) + 2.0 * 60.0 * 0.25 / 6.0;
    assert!(close(both.inertia.y_axis.y, iyy), "{} vs {}", both.inertia.y_axis.y, iyy);
    for k in 0..3 { assert!((both.inertia.col(k) - sum.inertia.col(k)).length() < 1e-6); }
}

#[test]
fn part_mass_spreads_over_live_footprint() {
    let mut g = CellGrid::new([4, 2, 2]);
    g.set(CellPos::new(0, 0, 0), STEEL);
    let cells = block(&g, [2, 0, 0], [3, 0, 0]);
    let mut v = build(g.clone(), &[(PartSlot(0), cells.clone(), false, Flammable::No)]);
    let cat = catalogue(vec![def("gen", PartFunction::Structure, vec![], 1000.0)]);
    let parts = vec![part(0, 0, cells.clone(), &g, PartState::Inert)];
    let m = dry_mass(&v, &parts, &cat);
    assert!(close(m.mass_kg, 1060.0));
    v.add_damage(cells[1], 8);
    let m2 = dry_mass(&v, &parts, &cat);
    assert!(close(m2.mass_kg, 560.0), "half the part went with its cell: {}", m2.mass_kg);
}

#[test]
fn fluids_add_water_and_fuel() {
    let g = box_hull([10, 6, 6], &[], &[]);
    let v = build(g.clone(), &[]);
    let (mut set, _) = crate::compartments::CompartmentSet::derive(&v, &[]);
    let cat = catalogue(vec![def("tank", PartFunction::FuelTank { litres: 1000.0 }, vec![], 50.0)]);
    let dry = dry_mass(&v, &[], &cat);
    let wet = with_fluids(&dry, &[], &set, &cat);
    assert_eq!(wet.mass_kg, dry.mass_kg);
    set.compartments[0].water_m3 = 2.0;
    set.compartments[0].water_centroid = DVec3::new(1.0, 0.75, 1.5);
    let mut tank = part(0, 0, vec![], &g, PartState::FuelTank { litres: 100.0 });
    tank.centre = DVec3::new(4.0, 2.0, 1.5);
    let wet = with_fluids(&dry, &[tank], &set, &cat);
    let water = 2.0 * 1025.0;
    let fuel = 100.0 * FUEL_DENSITY_KG_PER_L;
    assert!(close(wet.mass_kg, dry.mass_kg + water + fuel));
    let com = (dry.com * dry.mass_kg + DVec3::new(1.0, 0.75, 1.5) * water + DVec3::new(4.0, 2.0, 1.5) * fuel) / wet.mass_kg;
    assert!((wet.com - com).length() < 1e-9);
    // Water low on one side raises roll inertia and shifts the centre of mass toward it.
    assert!(wet.com.x < dry.com.x);
}

#[test]
fn pumped_ballast_counts_without_a_dry_rebuild() {
    let g = box_hull([10, 6, 6], &[], &[]);
    let v = build(g.clone(), &[]);
    let (set, _) = crate::compartments::CompartmentSet::derive(&v, &[]);
    let cat = catalogue(vec![def("ballast", PartFunction::Ballast { max_kg: 4000.0 }, vec![], 200.0)]);
    let mut tank = part(0, 0, vec![], &g, PartState::Ballast { kg: 1000.0 });
    tank.centre = DVec3::new(1.0, 0.75, 1.5);
    let dry = dry_mass(&v, std::slice::from_ref(&tank), &cat);
    assert!(close(dry_mass(&v, &[], &cat).mass_kg, dry.mass_kg), "ballast water is not dry mass");
    let one = with_fluids(&dry, std::slice::from_ref(&tank), &set, &cat);
    assert!(close(one.mass_kg, dry.mass_kg + 1000.0));
    // The parts step pumps the tank; the same dry mass now carries the new load, clamped to capacity.
    tank.state = PartState::Ballast { kg: 9000.0 };
    let full = with_fluids(&dry, std::slice::from_ref(&tank), &set, &cat);
    assert!(close(full.mass_kg, dry.mass_kg + 4000.0));
    assert!(full.com.x < one.com.x);
    tank.state = PartState::Ballast { kg: f64::NAN };
    assert!(close(with_fluids(&dry, &[tank], &set, &cat).mass_kg, dry.mass_kg));
}

#[test]
fn degenerate_input_stays_finite() {
    let v = build(CellGrid::new([0, 0, 0]), &[]);
    let m = dry_mass(&v, &[], &Catalogue::default());
    assert_eq!(m.mass_kg, 1.0);
    let bad = MassProperties { mass_kg: f64::NAN, com: DVec3::ZERO, inertia: DMat3::IDENTITY };
    let c = combine(&bad, &m);
    assert!(c.mass_kg.is_finite() && c.com.is_finite() && c.inertia.x_axis.is_finite());
    let z = combine(&MassProperties { mass_kg: 0.0, ..m }, &MassProperties { mass_kg: -5.0, ..m });
    assert_eq!(z.mass_kg, 1.0);
}
