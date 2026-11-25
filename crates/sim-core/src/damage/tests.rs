use super::*;
use crate::compartments::testkit::{block, box_hull, build, idx, part};
use crate::networks::{Conduit, ConduitState};
use crate::parts::PartState;
use content::cell::CellGrid;
use content::enums::{DamageLevel, NetworkKind};

/// Hand-built vessel pieces one application touches.
#[derive(Clone)]
struct Fx { g: CellGrid, grid: VesselGrid, parts: Vec<PartRuntime>, comps: CompartmentSet, net: Networks, pending: Vec<CellIndex> }
impl Fx {
    fn io(&mut self) -> DamageIo<'_> { DamageIo { grid: &mut self.grid, parts: &mut self.parts, compartments: &mut self.comps, networks: &mut self.net, pending: &mut self.pending } }
    fn apply(&mut self, damage: &[CellDamage]) -> DamageOutcome { apply_inner(self.io(), damage, false, None, None) }
    /// Run the phase-11 recompute over everything pending.
    fn recompute(&mut self) {
        let pending = self.pending.clone();
        let (_, more) = self.comps.update_after_damage(&mut self.grid, &pending, 1_000_000);
        assert!(!more);
    }
}

/// 20 x 8 x 8 box with a bulkhead at x = 10: compartment 0 aft, 1 forward. Optional parts with
/// (cells, watertight, flammable) flags; plain equipment parts are inert.
fn fixture(parts: &[(Vec<[i16; 3]>, bool, Flammable)]) -> Fx {
    let g = box_hull([20, 8, 8], &[10], &[]);
    let specs: Vec<(PartSlot, Vec<CellIndex>, bool, Flammable)> = parts.iter().enumerate().map(|(i, (cells, w, f))| {
        let mut v: Vec<CellIndex> = cells.iter().map(|c| idx(&g, c[0], c[1], c[2])).collect();
        v.sort();
        (PartSlot(i as u16), v, *w, *f)
    }).collect();
    let grid = build(g.clone(), &specs);
    let mut stamped = g.clone();
    for (_, cells, _, _) in &specs { for c in cells { stamped.cells[c.index()] = crate::compartments::testkit::BODY; } }
    let mut rt: Vec<PartRuntime> = specs.iter().map(|(s, cells, _, _)| part(s.0, 0, cells.clone(), &stamped, PartState::Inert)).collect();
    let (comps, _) = CompartmentSet::derive(&grid, &rt);
    comps.link_parts(&mut rt);
    let mut grid = grid;
    grid.sealed_air = comps.sealed_air_bits(stamped.len()).words;
    Fx { g: stamped, grid, parts: rt, comps, net: Networks::default(), pending: Vec::new() }
}

fn dmg(cells: &[CellIndex], b: u8) -> Vec<CellDamage> { cells.iter().map(|&c| CellDamage { cell: c, buckets: b }).collect() }

fn sorted_openings(c: &CompartmentSet) -> Vec<Opening> {
    let mut v = c.openings.clone();
    v.sort_by_key(opening_key);
    v
}

/// Apply through the damage unit then recompute, against grid damage alone then recompute: the opening sets,
/// labels and compartments must agree.
fn assert_matches_recompute(fx: &Fx, applications: &[Vec<CellDamage>]) {
    let mut a = fx.clone();
    for d in applications { a.apply(d); }
    let mut b = fx.clone();
    for d in applications {
        for e in d {
            let before = b.grid.bucket(e.cell);
            if b.grid.add_damage(e.cell, e.buckets) == BUCKET_DESTROYED && before < BUCKET_DESTROYED { b.pending.push(e.cell); }
        }
    }
    b.pending.sort();
    assert_eq!(a.pending, b.pending);
    a.recompute();
    b.recompute();
    assert_eq!(sorted_openings(&a.comps), sorted_openings(&b.comps));
    assert_eq!(a.comps.cell_to_compartment, b.comps.cell_to_compartment);
    let summary = |c: &CompartmentSet| c.compartments.iter().map(|k| (k.id, k.cells.len(), k.sealed)).collect::<Vec<_>>();
    assert_eq!(summary(&a.comps), summary(&b.comps));
}

#[test]
fn buckets_apply_ascending_with_duplicates_summed() {
    let mut fx = fixture(&[]);
    let a = idx(&fx.g, 3, 0, 3);
    let b = idx(&fx.g, 4, 0, 3);
    let input = vec![CellDamage { cell: b, buckets: 2 }, CellDamage { cell: a, buckets: 3 }, CellDamage { cell: a, buckets: 6 }, CellDamage { cell: CellIndex(u32::MAX), buckets: 8 }, CellDamage { cell: b, buckets: 0 }];
    let out = fx.apply(&input);
    assert_eq!(fx.grid.bucket(a), 8);
    assert_eq!(fx.grid.bucket(b), 2);
    assert_eq!(out.destroyed, vec![a]);
    assert_eq!(fx.pending, vec![a]);
    let changes = fx.grid.drain_changes();
    assert_eq!(changes.iter().map(|c| (c.cell, c.bucket)).collect::<Vec<_>>(), vec![(a, 8), (b, 2)]);
    // Hitting a removed cell again changes nothing and destroys nothing.
    let again = fx.apply(&dmg(&[a], 4));
    assert_eq!(again, DamageOutcome::default());
    assert_eq!(fx.pending, vec![a]);
}

#[test]
fn bottom_breach_matches_recompute_and_unseals() {
    let fx = fixture(&[]);
    let hole = idx(&fx.g, 4, 0, 3);
    let mut a = fx.clone();
    let out = a.apply(&dmg(&[hole], 8));
    assert_eq!(out.breached, vec![CompartmentId(0)]);
    let br: Vec<&Opening> = a.comps.openings.iter().filter(|o| o.kind == OpeningKind::Breach).collect();
    assert_eq!(br.len(), 1);
    assert_eq!((br[0].a, br[0].b, br[0].cell, br[0].face, br[0].area_m2, br[0].open), (CompartmentId(0), None, hole, Face::YPos, 0.25, true));
    assert!(!a.comps.compartments[0].sealed && a.comps.compartments[1].sealed);
    // Labels are left for the recompute.
    assert_eq!(a.comps.cell_to_compartment[hole.index()], NO_COMPARTMENT);
    assert_matches_recompute(&fx, &[dmg(&[hole], 8)]);
}

#[test]
fn bulkhead_hole_joins_compartments_until_the_merge() {
    let fx = fixture(&[]);
    let cell = idx(&fx.g, 10, 3, 3);
    let mut a = fx.clone();
    let out = a.apply(&dmg(&[cell], 8));
    assert_eq!(out.breached, vec![CompartmentId(0), CompartmentId(1)]);
    let holes: Vec<&Opening> = a.comps.openings.iter().filter(|o| o.kind == OpeningKind::Hole).collect();
    assert_eq!(holes.len(), 1);
    assert_eq!((holes[0].a, holes[0].b, holes[0].cell, holes[0].face), (CompartmentId(0), Some(CompartmentId(1)), cell, Face::XNeg));
    assert!(a.comps.compartments.iter().all(|c| c.sealed));
    a.recompute();
    assert!(a.comps.openings.iter().all(|o| o.kind != OpeningKind::Hole));
    assert!(a.comps.compartments[1].is_absorbed());
    assert_matches_recompute(&fx, &[dmg(&[cell], 8)]);
}

#[test]
fn breach_through_a_bulkhead_edge_files_under_the_lowest_id() {
    // Side plate at the bulkhead and the bulkhead cell behind it: one cell to the sea, one joining 0 and 1.
    let fx = fixture(&[]);
    let plate = idx(&fx.g, 10, 3, 0);
    let behind = idx(&fx.g, 10, 3, 1);
    let mut a = fx.clone();
    a.apply(&dmg(&[plate, behind], 8));
    let br: Vec<&Opening> = a.comps.openings.iter().filter(|o| o.kind == OpeningKind::Breach).collect();
    assert_eq!(br.len(), 1);
    assert_eq!((br[0].a, br[0].cell, br[0].face), (CompartmentId(0), plate, Face::ZPos));
    assert_matches_recompute(&fx, &[dmg(&[plate, behind], 8)]);
    // Same cells over two applications, the second seeing the first's pending hole.
    let fx2 = fixture(&[]);
    let inner = idx(&fx2.g, 10, 4, 1);
    let outer = idx(&fx2.g, 10, 4, 0);
    assert_matches_recompute(&fx2, &[dmg(&[behind, inner], 8), dmg(&[plate, outer], 8)]);
}

#[test]
fn bursts_match_recompute() {
    let fx = fixture(&[]);
    for (centre, b, r) in [((10, 0, 0), 8u8, 2u8), ((10, 4, 0), 12, 3), ((2, 7, 4), 8, 2), ((15, 3, 7), crate::armour::COOKOFF_B, 3)] {
        let c = idx(&fx.g, centre.0, centre.1, centre.2);
        let mut d = Vec::new();
        crate::armour::he_burst(&fx.grid, c, b, r, &mut d);
        assert!(!d.is_empty());
        assert_matches_recompute(&fx, &[d.clone()]);
        // Two overlapping bursts in separate applications.
        let mut fx1 = fx.clone();
        fx1.apply(&d);
        let c2 = idx(&fx.g, (centre.0 + 2).min(19), centre.1, centre.2);
        let mut d2 = Vec::new();
        crate::armour::he_burst(&fx1.grid, c2, b, r, &mut d2);
        assert_matches_recompute(&fx, &[d, d2]);
    }
}

#[test]
fn patched_breach_reopens_when_its_plating_is_hit() {
    let mut fx = fixture(&[]);
    let hole = idx(&fx.g, 4, 0, 3);
    fx.apply(&dmg(&[hole], 8));
    fx.recompute();
    fx.comps.openings.iter_mut().for_each(|o| o.patched = true);
    // Damage away from the patch leaves it.
    let far = idx(&fx.g, 15, 0, 3);
    let out = fx.apply(&dmg(&[far], 1));
    assert!(out.breached.is_empty());
    assert!(fx.comps.openings.iter().all(|o| o.patched));
    // One bucket on the plate beside it reopens it.
    let next = idx(&fx.g, 5, 0, 3);
    let out = fx.apply(&dmg(&[next], 1));
    assert_eq!(out.breached, vec![CompartmentId(0)]);
    assert!(fx.comps.openings.iter().all(|o| !o.patched));
}

#[test]
fn part_health_and_conduit_cuts() {
    // A 2-cell inert part on the floor of compartment 0; a conduit through one of its cells.
    let mut fx = fixture(&[(vec![[3, 1, 3], [4, 1, 3]], false, Flammable::No)]);
    let c0 = idx(&fx.g, 3, 1, 3);
    let c1 = idx(&fx.g, 4, 1, 3);
    fx.net.conduits.push(Conduit { network: NetworkKind::Power, from: CompartmentId(0), to: CompartmentId(0), cells: vec![c1], redundant: false, pinned: false, state: ConduitState::Intact });
    let out = fx.apply(&dmg(&[c0], 4));
    assert!(out.conduits_cut.is_empty());
    assert_eq!(fx.parts[0].health, 0.75);
    let out = fx.apply(&dmg(&[c1], 8));
    assert_eq!(out.conduits_cut, vec![0]);
    assert_eq!(fx.net.conduits[0].state, ConduitState::Cut);
    assert_eq!(out.parts_changed, vec![PartSlot(0)]);
    assert_eq!(fx.parts[0].health, 0.25);
    assert_eq!(fx.parts[0].level, DamageLevel::Heavy);
    // Equipment inside a compartment becomes its air: no opening.
    assert!(out.breached.is_empty());
    assert!(fx.comps.openings.iter().all(|o| o.kind != OpeningKind::Breach));
}

fn tuning() -> FireTuning { FireTuning::default() }

fn ign(tick: u64, t: &FireTuning, flooded: f64) -> IgnitionCtx<'_> { IgnitionCtx { seed: 77, tick, vessel: 5, fire: t, flooded } }

/// A watertight 2x2x2 tank against the floor of compartment 0, and a hit on the floor plate beneath it.
fn tank(f: Flammable) -> (Fx, CellDamage) {
    let fx = fixture(&[(vec![[3, 1, 3], [4, 1, 3], [3, 1, 4], [4, 1, 4], [3, 2, 3], [4, 2, 3], [3, 2, 4], [4, 2, 4]], true, f)]);
    let floor = idx(&fx.g, 5, 0, 3);
    (fx, CellDamage { cell: floor, buckets: 1 })
}

fn ignition_rate(f: Flammable, trials: u64) -> f64 {
    let t = tuning();
    let (fx, hit) = tank(f);
    let mut lit = 0;
    for tick in 0..trials {
        let mut x = fx.clone();
        let out = apply_inner(x.io(), &[hit], true, Some(ign(tick, &t, 0.5)), None);
        if let Some(c) = out.ignited {
            assert_eq!(c, CompartmentId(0));
            assert!(x.comps.compartments[0].is_burning());
            lit += 1;
        }
    }
    lit as f64 / trials as f64
}

#[test]
fn ignition_chances_near_fuel_and_ammunition() {
    let fuel = ignition_rate(Flammable::Fuel, 2_000);
    let ammo = ignition_rate(Flammable::Ammunition, 2_000);
    assert!((0.12..0.18).contains(&fuel), "fuel {fuel}");
    assert!((0.21..0.29).contains(&ammo), "ammo {ammo}");
    // Nothing flammable within 2 cells: no fire.
    assert_eq!(ignition_rate(Flammable::No, 200), 0.0);
}

#[test]
fn ignition_rules() {
    let t = tuning();
    let (fx, hit) = tank(Flammable::Ammunition);
    // Find a tick whose draw lights it.
    let tick = (0..200).find(|&k| { let mut x = fx.clone(); apply_inner(x.io(), &[hit], true, Some(ign(k, &t, 0.5)), None).ignited.is_some() }).expect("some draw lights");
    // Deterministic.
    let mut x = fx.clone();
    assert_eq!(apply_inner(x.io(), &[hit], true, Some(ign(tick, &t, 0.5)), None).ignited, Some(CompartmentId(0)));
    // No roll without an igniting hit.
    let mut x = fx.clone();
    assert_eq!(apply_inner(x.io(), &[hit], true, None, None).ignited, None);
    // Over half flooded cannot burn.
    let mut x = fx.clone();
    x.comps.compartments[0].water_m3 = x.comps.compartments[0].volume_m3 * 0.6;
    assert_eq!(apply_inner(x.io(), &[hit], true, Some(ign(tick, &t, 0.5)), None).ignited, None);
    // Already burning: not reported as newly lit.
    let mut x = fx.clone();
    x.comps.compartments[0].fire = 40.0;
    assert_eq!(apply_inner(x.io(), &[hit], true, Some(ign(tick, &t, 0.5)), None).ignited, None);
    assert_eq!(x.comps.compartments[0].fire, 40.0);
    // A hit that destroys the tank itself still counts it (proximity is read before the damage lands).
    let tank_cells = block(&fx.g, [3, 1, 3], [4, 2, 4]);
    let hits = (0..400).filter(|&k| { let mut x = fx.clone(); apply_inner(x.io(), &dmg(&tank_cells, 8), true, Some(ign(k, &t, 0.5)), None).ignited.is_some() }).count();
    assert!(hits > 40, "{hits}");
    // Scorch marks surviving cells of a burst.
    let mut x = fx.clone();
    apply_inner(x.io(), &[hit], true, None, Some(hit.cell));
    assert_eq!(x.grid.scorch[hit.cell.index()], SCORCH_PER_BUCKET);
}

#[test]
fn collision_threshold_and_crush() {
    // Resting contact of a 150 t hull (about m g dt per tick) does nothing; a hard ram does.
    assert_eq!(collision_budget(150_000.0, 150_000.0 * 9.81 / 60.0), None);
    assert_eq!(collision_budget(150_000.0, f64::NAN), None);
    assert_eq!(collision_budget(150_000.0, -1.0), None);
    let ram = collision_budget(150_000.0, 1.4e6).expect("ram");
    let fx = fixture(&[]);
    // Contact on the bow plate (x = 19), mid height.
    let at = [19.9, 3.5, 3.5];
    let small = crush(&fx.grid, at, 2.0 * COLLISION_NS_PER_BUCKET);
    assert_eq!(small, vec![CellDamage { cell: idx(&fx.g, 19, 3, 3), buckets: 2 }]);
    let big = crush(&fx.grid, at, ram);
    assert!(big.windows(2).all(|w| w[0].cell < w[1].cell));
    assert!(big.iter().all(|d| (1..=8).contains(&d.buckets) && fx.grid.is_solid(fx.g.pos(d.cell))));
    let destroyed = big.iter().filter(|d| d.buckets == 8).count();
    assert!(destroyed >= 5, "{destroyed}");
    let total = |v: &[CellDamage]| v.iter().map(|d| d.buckets as u32).sum::<u32>();
    // More impulse never does less, and the bill stays within the budget.
    let mut last = 0;
    for k in 1..40 {
        let b = k as f64 * 25_000.0;
        let v = crush(&fx.grid, at, b);
        let t = total(&v);
        assert!(t >= last, "{k}");
        assert!(t as f64 * COLLISION_NS_PER_BUCKET <= b + 1e-6);
        last = t;
    }
    // Far outside the box or non-finite: nothing.
    assert!(crush(&fx.grid, [500.0, 3.0, 3.0], ram).is_empty());
    assert!(crush(&fx.grid, [f64::NAN, 3.0, 3.0], ram).is_empty());
}

#[test]
fn burst_deep_inside_a_magazine_ignites_its_compartment() {
    // A 7 x 5 x 6 watertight magazine filling most of compartment 0; the hit is more than 2 cells from any air.
    let mut cells = Vec::new();
    for y in 1..=5 { for z in 1..=6 { for x in 2..=8 { cells.push([x, y, z]); } } }
    let fx = fixture(&[(cells, true, Flammable::Ammunition)]);
    assert_eq!(fx.parts[0].compartment, Some(CompartmentId(0)));
    let centre = CellDamage { cell: idx(&fx.g, 5, 3, 3), buckets: 1 };
    let t = tuning();
    let mut lit = 0;
    for tick in 0..400 {
        let mut x = fx.clone();
        if let Some(c) = apply_inner(x.io(), &[centre], true, Some(ign(tick, &t, 0.5)), Some(centre.cell)).ignited {
            assert_eq!(c, CompartmentId(0));
            lit += 1;
        }
    }
    assert!((60..140).contains(&lit), "{lit}");
}
