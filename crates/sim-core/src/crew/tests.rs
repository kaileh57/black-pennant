use super::*;
use crate::compartments::testkit::*;
use crate::networks::Conduit;
use crate::parts::PartState;
use content::cell::CellGrid;
use content::enums::NetworkKind;
use content::part::Flammable;
use content::tuning::CrewTuning;

struct Fx { g: CellGrid, grid: VesselGrid, comps: CompartmentSet, parts: Vec<PartRuntime>, net: Networks, crew: CrewState, cat: Catalogue }

/// Six-compartment box; 4 berths in compartment 0, 4 in compartment 1, a 6-place passenger berth in
/// compartment 2 and a damaged gun-like part in compartment 4.
fn fx() -> Fx {
    let g = box_hull([32, 12, 12], &[10, 21], &[6]);
    let b0 = block(&g, [2, 1, 2], [3, 1, 2]);
    let b1 = block(&g, [12, 1, 2], [13, 1, 2]);
    let pax = block(&g, [24, 1, 2], [25, 1, 2]);
    let gear = block(&g, [14, 7, 2], [15, 7, 3]);
    let grid = build(g.clone(), &[(PartSlot(0), b0.clone(), false, Flammable::No), (PartSlot(1), b1.clone(), false, Flammable::No), (PartSlot(2), pax.clone(), false, Flammable::No), (PartSlot(3), gear.clone(), false, Flammable::No)]);
    let cat = catalogue(vec![
        def("berth", PartFunction::Berths { count: 4 }, vec![], 100.0),
        def("pax", PartFunction::PassengerBerths { count: 6 }, vec![], 100.0),
        def("gear", PartFunction::Structure, vec![], 100.0),
    ]);
    let mut parts = vec![
        part(0, 0, b0, &g, PartState::Inert),
        part(1, 0, b1, &g, PartState::Inert),
        part(2, 1, pax, &g, PartState::Inert),
        part(3, 2, gear, &g, PartState::Inert),
    ];
    let (comps, _) = CompartmentSet::derive(&grid, &parts);
    comps.link_parts(&mut parts);
    let t = CrewTuning::default();
    let crew = CrewState { original_berths: 8, surviving_berths: 8, teams: vec![Team::default(); team_count(8, &t) as usize], ..Default::default() };
    Fx { g, grid, comps, parts, net: Networks::default(), crew, cat }
}

fn tick(f: &mut Fx, orders: &[TeamOrder], done: &mut Vec<JobDone>) -> Vec<(CompartmentId, u8)> {
    let pose = Pose::default();
    let io = CrewIo { crew: &mut f.crew, comps: &mut f.comps, parts: &mut f.parts, net: &mut f.net, grid: &mut f.grid, orders, pose: &pose };
    crew_tick(io, &f.cat, &CrewTuning::default(), 0.5, 1.0 / 60.0, done)
}

fn run(f: &mut Fx, orders: &[TeamOrder], seconds: f64) -> Vec<JobDone> {
    let mut done = Vec::new();
    for _ in 0..libm::round(seconds * 60.0) as u32 { tick(f, orders, &mut done); }
    done
}

#[test]
fn teams_and_berth_factor() {
    let t = CrewTuning::default();
    assert_eq!((team_count(0, &t), team_count(8, &t), team_count(24, &t), team_count(200, &t)), (1, 2, 6, 6));
    assert_eq!(team_count(4096, &t), 6, "no wrap past 255 teams");
    assert_eq!(team_count(0, &CrewTuning { min_teams: 9, max_teams: 3, ..t }), 3, "bad tuning does not panic");
    let mut f = fx();
    tick(&mut f, &[], &mut Vec::new());
    assert_eq!((f.crew.surviving_berths, f.crew.berth_factor()), (8, Some(1.0)));
    assert_eq!((f.comps.compartments[0].berths, f.comps.compartments[1].berths), (4, 4));
    f.comps.compartments[1].fire = 50.0;
    tick(&mut f, &[], &mut Vec::new());
    assert_eq!(f.crew.berth_factor(), Some(2.0));
    f.comps.compartments[0].water_m3 = f.comps.compartments[0].volume_m3 * 0.6;
    let fires = tick(&mut f, &[TeamOrder::FightFire(CompartmentId(1)), TeamOrder::Idle], &mut Vec::new());
    assert_eq!(f.crew.berth_factor(), None);
    assert!(fires.is_empty(), "with no surviving berths no job runs");
}

#[test]
fn patch_takes_ten_seconds_scaled_by_berths() {
    let mut f = fx();
    let cells = [idx(&f.g, 4, 0, 4), idx(&f.g, 5, 0, 4)];
    for &c in &cells { f.grid.add_damage(c, 8); }
    f.comps.update_after_damage(&mut f.grid, &cells, 10_000);
    let orders = [TeamOrder::Patch { compartment: CompartmentId(0) }];
    let done = run(&mut f, &orders, 9.9);
    assert!(done.is_empty());
    let done = run(&mut f, &orders, 0.2);
    assert_eq!(done, vec![JobDone::FacePatched(CompartmentId(0))]);
    assert_eq!(f.comps.openings.iter().filter(|o| o.patched).count(), 1);
    // Half the berths burning: the second face takes twice as long.
    f.comps.compartments[1].fire = 50.0;
    assert!(run(&mut f, &orders, 19.0).is_empty());
    assert_eq!(run(&mut f, &orders, 1.2), vec![JobDone::FacePatched(CompartmentId(0))]);
    // Nothing left to patch: the team waits.
    assert!(run(&mut f, &orders, 30.0).is_empty());
}

#[test]
fn no_patching_over_half_flooded() {
    let mut f = fx();
    let c = idx(&f.g, 4, 0, 4);
    f.grid.add_damage(c, 8);
    f.comps.update_after_damage(&mut f.grid, &[c], 10_000);
    f.comps.compartments[0].water_m3 = f.comps.compartments[0].volume_m3 * 0.55;
    let done = run(&mut f, &[TeamOrder::Idle, TeamOrder::Patch { compartment: CompartmentId(0) }], 30.0);
    assert!(done.is_empty());
}

#[test]
fn jury_rig_and_repair() {
    let mut f = fx();
    f.net.conduits.push(Conduit { network: NetworkKind::Power, from: CompartmentId(0), to: CompartmentId(1), cells: vec![], redundant: false, pinned: false, state: ConduitState::Cut });
    f.parts[3].health = 0.3;
    f.parts[3].level = DamageLevel::Heavy;
    let orders = [TeamOrder::JuryRig { conduit: 0 }, TeamOrder::Repair(PartSlot(3))];
    let done = run(&mut f, &orders, 20.05);
    assert_eq!(done, vec![JobDone::ConduitJuryRigged(0)]);
    assert_eq!(f.net.conduits[0].state, ConduitState::JuryRigged);
    let done = run(&mut f, &orders, 10.0);
    assert_eq!(done, vec![JobDone::PartRepaired(PartSlot(3))]);
    assert!((f.parts[3].health - 0.55).abs() < 1e-9);
    assert_eq!(f.parts[3].level, DamageLevel::Damaged);
    run(&mut f, &orders, 30.05);
    assert!((f.parts[3].health - 0.75).abs() < 1e-9, "capped at 75%");
    assert!(run(&mut f, &orders, 60.0).is_empty());
}

#[test]
fn repair_lowers_cell_buckets_to_two() {
    let mut f = fx();
    let cells = f.parts[3].cells.clone();
    f.grid.add_damage(cells[0], 6);
    f.grid.add_damage(cells[1], 3);
    f.parts[3].health = 0.5;
    run(&mut f, &[TeamOrder::Repair(PartSlot(3))], 30.05);
    assert_eq!((f.grid.bucket(cells[0]), f.grid.bucket(cells[1]), f.grid.bucket(cells[2])), (4, 2, 0));
}

#[test]
fn fire_teams_and_order_changes() {
    let mut f = fx();
    f.comps.compartments[4].fire = 100.0;
    let fires = tick(&mut f, &[TeamOrder::FightFire(CompartmentId(4)), TeamOrder::FightFire(CompartmentId(4))], &mut Vec::new());
    assert_eq!(fires, vec![(CompartmentId(4), 2)]);
    let c = idx(&f.g, 4, 0, 4);
    f.grid.add_damage(c, 8);
    f.comps.update_after_damage(&mut f.grid, &[c], 10_000);
    let patch = [TeamOrder::Patch { compartment: CompartmentId(0) }];
    run(&mut f, &patch, 5.0);
    assert!(f.crew.teams[0].progress_s > 4.9);
    run(&mut f, &[TeamOrder::Idle], 1.0 / 60.0);
    assert_eq!(f.crew.teams[0].progress_s, 0.0, "a changed order resets progress");
    assert!(run(&mut f, &patch, 9.9).is_empty());
}

#[test]
fn passengers_lost_with_their_berths() {
    let mut f = fx();
    f.crew.passengers = 6;
    tick(&mut f, &[], &mut Vec::new());
    assert_eq!((f.crew.passengers, f.crew.passengers_lost), (6, 0));
    f.comps.compartments[2].fire = 10.0;
    tick(&mut f, &[], &mut Vec::new());
    assert_eq!((f.crew.passengers, f.crew.passengers_lost), (0, 6));
    f.comps.compartments[2].fire = 0.0;
    tick(&mut f, &[], &mut Vec::new());
    assert_eq!((f.crew.passengers, f.crew.passengers_lost), (0, 6));
}

#[test]
fn fire_fought_reports_once_when_the_fire_is_out() {
    let mut f = fx();
    f.comps.compartments[2].fire = 100.0;
    let both = [TeamOrder::FightFire(CompartmentId(2)), TeamOrder::FightFire(CompartmentId(2))];
    let mut done = Vec::new();
    assert_eq!(tick(&mut f, &both, &mut done), vec![(CompartmentId(2), 2)]);
    assert!(done.is_empty());
    // The fire step puts it out; the officer moves both teams on in the same tick it sees that.
    f.comps.compartments[2].fire = 0.0;
    tick(&mut f, &[TeamOrder::Idle, TeamOrder::Idle], &mut done);
    assert_eq!(done, vec![JobDone::FireFought(CompartmentId(2))], "one report per compartment");
    done.clear();
    tick(&mut f, &[TeamOrder::Idle, TeamOrder::Idle], &mut done);
    // An order left on the dead fire reports nothing either.
    tick(&mut f, &both, &mut done);
    tick(&mut f, &both, &mut done);
    assert!(done.is_empty());
}
