use super::*;
use crate::compartments::testkit::*;
use content::cell::CellGrid;
use content::part::{Flammable, PartFunction};

const DT60: f64 = 1.0 / 60.0;

struct Fx { grid: VesselGrid, comps: CompartmentSet, parts: Vec<PartRuntime> }

/// Six-compartment box with an 1,800 kg magazine (watertight) in compartment 1 and a pump in compartment 4.
fn six() -> Fx {
    let g = box_hull([32, 12, 12], &[10, 21], &[6]);
    let mag = block(&g, [12, 1, 2], [13, 2, 3]);
    let pump = block(&g, [14, 7, 2], [14, 7, 2]);
    let grid = build(g.clone(), &[(PartSlot(0), mag.clone(), true, Flammable::Ammunition), (PartSlot(1), pump.clone(), false, Flammable::No)]);
    let mut parts = vec![
        part(0, 0, mag, &g, PartState::Magazine { rounds: 300, ammo_kg: 1800.0, flooded: false, cookoff_ticks: 0 }),
        part(1, 0, pump, &g, PartState::Pump { running: false, target: None }),
    ];
    let (comps, _) = CompartmentSet::derive(&grid, &parts);
    comps.link_parts(&mut parts);
    Fx { grid, comps, parts }
}

/// Two compartments side by side (bulkhead at x = 4), optionally with a shut door between them.
fn pair(door: bool) -> Fx {
    let g = box_hull([8, 6, 6], &[4], &[]);
    let cells = if door { block(&g, [4, 1, 2], [4, 2, 2]) } else { vec![] };
    let grid = build(g.clone(), &if door { vec![(PartSlot(0), cells.clone(), true, Flammable::No)] } else { vec![] });
    let mut parts = if door { vec![part(0, 0, cells, &g, PartState::Door { closed: true })] } else { vec![] };
    let (comps, _) = CompartmentSet::derive(&grid, &parts);
    comps.link_parts(&mut parts);
    Fx { grid, comps, parts }
}

fn step(f: &mut Fx, teams: &[(CompartmentId, u8)], factor: Option<f64>, seed: u64, tick: u64, dt: f64) -> FireOutcome {
    let io = FireIo { comps: &mut f.comps, parts: &mut f.parts, grid: &mut f.grid, berth_factor: factor };
    fire_tick(io, teams, FireKeys { seed, tick, vessel: 7 }, &FireTuning::default(), 0.5, dt, tick as f64 * dt)
}

#[test]
fn cookoff_radius_follows_the_cube_root() {
    assert_eq!(cookoff_radius_cells(1800.0, 4, 24), 16);
    assert_eq!(cookoff_radius_cells(0.0, 4, 24), 4);
    assert_eq!(cookoff_radius_cells(-5.0, 4, 24), 4);
    assert_eq!(cookoff_radius_cells(100_000.0, 4, 24), 24);
}

#[test]
fn unfought_magazine_fire_cooks_off_at_120_s() {
    let mut f = six();
    assert!(ignite_in(&mut f.comps, CompartmentId(1), &FireTuning::default(), 0.5));
    assert!(!ignite_in(&mut f.comps, CompartmentId(1), &FireTuning::default(), 0.5), "already burning");
    for t in 0..7199 {
        let o = step(&mut f, &[], Some(1.0), 1, t, DT60);
        assert!(o.cooked_off.is_empty(), "too early at tick {t}");
    }
    let o = step(&mut f, &[], Some(1.0), 1, 7199, DT60);
    assert_eq!(o.cooked_off, vec![(PartSlot(0), 16)]);
    assert!(matches!(f.parts[0].state, PartState::Magazine { rounds: 0, ammo_kg, .. } if ammo_kg == 0.0));
    assert_eq!(f.parts[0].level, DamageLevel::Destroyed);
    assert!(step(&mut f, &[], Some(1.0), 1, 7200, DT60).cooked_off.is_empty(), "reported once");
    // The pump in compartment 4 (above, across the deck) may have caught; scorch was written.
    assert!(f.grid.scorch.iter().any(|&s| s > 0));
}

#[test]
fn one_team_puts_out_a_fresh_fire_in_50_s() {
    let mut f = six();
    ignite_in(&mut f.comps, CompartmentId(4), &FireTuning::default(), 0.5);
    let team = [(CompartmentId(4), 1)];
    let mut out_at = None;
    for t in 0..4000 {
        let o = step(&mut f, &team, Some(1.0), 3, t, DT60);
        if o.extinguished.contains(&CompartmentId(4)) { out_at = Some(t + 1); break; }
    }
    let out_at = out_at.expect("fire out");
    assert!((2999..=3001).contains(&out_at), "out after {out_at} ticks");
    // Burn-down: the pump lost 1% per second for 50 s.
    assert!((f.parts[1].health - 0.5).abs() < 1e-6);
    // At berth factor 2 the same team needs twice as long.
    let mut f = six();
    ignite_in(&mut f.comps, CompartmentId(4), &FireTuning::default(), 0.5);
    let mut out_at = None;
    for t in 0..7000 { if step(&mut f, &team, Some(2.0), 3, t, DT60).extinguished.contains(&CompartmentId(4)) { out_at = Some(t + 1); break; } }
    assert!(out_at.is_some_and(|t| (5999..=6001).contains(&t)), "{out_at:?}");
}

#[test]
fn flooding_puts_fires_out() {
    let mut f = six();
    ignite_in(&mut f.comps, CompartmentId(3), &FireTuning::default(), 0.5);
    f.comps.compartments[3].water_m3 = f.comps.compartments[3].volume_m3 * 0.51;
    let o = step(&mut f, &[], Some(1.0), 1, 0, DT60);
    assert_eq!(o.extinguished, vec![CompartmentId(3)]);
    assert!(!ignite_in(&mut f.comps, CompartmentId(3), &FireTuning::default(), 0.5));
    // A flooded magazine's fire goes out at once.
    ignite_in(&mut f.comps, CompartmentId(1), &FireTuning::default(), 0.5);
    if let PartState::Magazine { flooded, .. } = &mut f.parts[0].state { *flooded = true; }
    let o = step(&mut f, &[], Some(1.0), 1, 1, DT60);
    assert!(o.extinguished.contains(&CompartmentId(1)));
}

/// Fraction of seeds in which a fire spreads into the neighbour within `secs` (1 s steps).
fn spread_within(door: bool, secs: u64, seeds: u64) -> f64 {
    let mut hits = 0;
    for seed in 0..seeds {
        let mut f = pair(door);
        ignite_in(&mut f.comps, CompartmentId(0), &FireTuning::default(), 0.5);
        for t in 0..secs {
            let o = step(&mut f, &[], Some(1.0), seed, t, 1.0);
            if o.ignited.contains(&CompartmentId(1)) { hits += 1; break; }
        }
    }
    hits as f64 / seeds as f64
}

#[test]
fn spread_is_one_percent_per_second_and_halved_by_a_shut_door() {
    let open = spread_within(false, 100, 400);
    assert!((open - (1.0 - libm::exp(-1.0))).abs() < 0.07, "P(spread in 100 s) = {open}");
    let shut = spread_within(true, 100, 400);
    assert!((shut - (1.0 - libm::exp(-0.5))).abs() < 0.07, "P(spread in 100 s through a shut door) = {shut}");
    // Open the door: full rate again.
    let mut f = pair(true);
    f.parts[0].state = PartState::Door { closed: false };
    f.comps.sync_doors(&f.parts);
    assert!(f.comps.open_door_between(CompartmentId(0), CompartmentId(1)));
}

#[test]
fn same_seed_same_fire() {
    let run = |seed| {
        let mut f = six();
        ignite_in(&mut f.comps, CompartmentId(0), &FireTuning::default(), 0.5);
        let mut log = Vec::new();
        for t in 0..400 { log.push(step(&mut f, &[], Some(1.0), seed, t, 1.0)); }
        (log, f.comps.compartments.iter().map(|c| c.fire).collect::<Vec<_>>())
    };
    assert_eq!(run(11), run(11));
}

#[test]
fn forecast_for_damage_control() {
    let mut f = six();
    let t = FireTuning::default();
    assert_eq!(forecast_in(&f.comps, &f.parts, Some(1.0), PartSlot(0), 1, &t), None, "not burning");
    ignite_in(&mut f.comps, CompartmentId(1), &t, 0.5);
    assert_eq!(forecast_in(&f.comps, &f.parts, Some(1.0), PartSlot(0), 1, &t), Some((120.0, true)));
    assert_eq!(forecast_in(&f.comps, &f.parts, Some(1.0), PartSlot(0), 0, &t), Some((120.0, false)));
    for k in 0..80 * 60 { step(&mut f, &[], Some(1.0), 5, k, DT60); }
    let (left, out) = forecast_in(&f.comps, &f.parts, Some(1.0), PartSlot(0), 1, &t).expect("burning");
    assert!((left - 40.0).abs() < 1e-6 && !out, "100 intensity at 2/s takes 50 s, only 40 s remain");
    assert_eq!(forecast_in(&f.comps, &f.parts, None, PartSlot(0), 3, &t).map(|x| x.1), Some(false), "no berths, no suppression");
    assert_eq!(forecast_in(&f.comps, &f.parts, Some(1.0), PartSlot(1), 1, &t), None, "not a magazine");
    let _ = PartFunction::Structure;
}
