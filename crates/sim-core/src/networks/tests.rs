use super::*;
use crate::compartments::testkit::*;
use content::cell::{CellGrid, CellPos};
use content::enums::NetworkKind::{Data, Fuel, Power};
use content::part::{EngineDef, Flammable, PortRole::*};
use content::tuning::NetworkTuning;

/// Six compartments (three below the deck at y = 6, three above), a 100 kW generator in compartment 0, a
/// 40 kW consumer in compartment 2, a console in compartment 3 and a powered sensor on Data in compartment 5.
fn design() -> (CellGrid, VesselGrid, CompartmentSet, Vec<PartRuntime>, Catalogue) {
    let g = box_hull([32, 12, 12], &[10, 21], &[6]);
    let gen = block(&g, [3, 1, 3], [4, 2, 4]);
    let load = block(&g, [26, 1, 3], [27, 2, 4]);
    let console = block(&g, [4, 7, 5], [4, 7, 5]);
    let sensor = block(&g, [27, 8, 5], [27, 8, 5]);
    let v = build(g.clone(), &[(PartSlot(0), gen.clone(), false, Flammable::No), (PartSlot(1), load.clone(), false, Flammable::No), (PartSlot(2), console.clone(), false, Flammable::No), (PartSlot(3), sensor.clone(), false, Flammable::No)]);
    let cat = catalogue(vec![
        def("gen", PartFunction::Generator { kw: 100.0, fuel_lps: 0.0 }, vec![], 2000.0),
        def("load", PartFunction::Structure, vec![port(Power, Consume, 40.0, 0.0)], 500.0),
        def("console", PartFunction::Console, vec![port(Power, Consume, 1.0, 0.0)], 100.0),
        def("sensor", PartFunction::Structure, vec![port(Power, Consume, 20.0, 0.0), port(Data, Link, 0.0, 0.0)], 100.0),
    ]);
    let mut parts = vec![
        part(0, 0, gen, &g, PartState::Generator { output_kw: 0.0 }),
        part(1, 1, load, &g, PartState::Inert),
        part(2, 2, console, &g, PartState::Inert),
        part(3, 3, sensor, &g, PartState::Inert),
    ];
    parts[1].priority = 1;
    parts[2].priority = 1;
    parts[3].priority = 4;
    let (set, _) = CompartmentSet::derive(&v, &parts);
    set.link_parts(&mut parts);
    (g, v, set, parts, cat)
}

fn tuning() -> NetworkTuning { NetworkTuning::default() }

#[test]
fn router_is_deterministic_and_doubles_routes() {
    let (_, v, set, parts, cat) = design();
    let a = route(&v, &set, &parts, &NetworkEdits::default(), &cat);
    let b = route(&v, &set, &parts, &NetworkEdits::default(), &cat);
    assert_eq!(a, b);
    assert!(a.broken_pins.is_empty());
    assert_eq!(a.attach, vec![0, 2, 3, 5]);
    let power: Vec<&Conduit> = a.conduits.iter().filter(|c| c.network == Power).collect();
    // Terminals 0 (root), 2, 3 and 5: three links, each doubled.
    assert_eq!(power.iter().filter(|c| !c.redundant).count(), 3);
    assert_eq!(power.iter().filter(|c| c.redundant).count(), 3);
    for c in &a.conduits {
        assert!(c.cells.len() >= 2);
        for cell in &c.cells { assert!(v.is_solid(v.pos(*cell)), "conduits run on structure"); }
        for w in c.cells.windows(2) {
            let (p, q) = (v.pos(w[0]), v.pos(w[1]));
            assert_eq!((p.x - q.x).abs() + (p.y - q.y).abs() + (p.z - q.z).abs(), 1, "face-connected run");
        }
    }
    for p in a.conduits.iter().filter(|c| !c.redundant) {
        let r = a.conduits.iter().find(|r| r.redundant && r.network == p.network && r.from == p.from && r.to == p.to).expect("redundant pair");
        assert!(p.cells.iter().all(|c| !r.cells.contains(c)), "one destroyed cell never cuts both");
    }
    let order: Vec<_> = a.conduits.iter().map(|c| (kind_index(c.network), c.from, c.to, c.redundant)).collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
    assert!(unconnected_consumers(&a, &parts, &set, &cat).is_empty());
}

#[test]
fn fallback_chain_and_jury_rig() {
    let (_, v, set, mut parts, cat) = design();
    let mut net = route(&v, &set, &parts, &NetworkEdits::default(), &cat);
    let dt = 1.0 / 60.0;
    solve_tick(&mut net, &mut parts, &cat, &tuning(), dt);
    assert_eq!((parts[1].links.power, parts[1].links.power_supplied), (LinkState::Network, 1.0));
    assert_eq!(parts[3].links.data, LinkState::Network);
    assert!((net.generation_kw - 100.0).abs() < 1e-9 && (net.supplied_kw - 61.0).abs() < 1e-9);
    assert_eq!(parts[1].links.battery_s, 30.0);
    // Cut every primary: service carries on over the redundant paths within the tick.
    let red: std::collections::BTreeSet<CellIndex> = net.conduits.iter().filter(|c| c.redundant).flat_map(|c| c.cells.iter().copied()).collect();
    let prim: Vec<CellIndex> = net.conduits.iter().filter(|c| !c.redundant).filter_map(|c| c.cells.iter().copied().find(|x| !red.contains(x))).collect();
    let cut = cut_conduits(&mut net, &prim);
    assert!(!cut.is_empty());
    let changed = solve_tick(&mut net, &mut parts, &cat, &tuning(), dt);
    assert!(changed.contains(&PartSlot(1)));
    assert_eq!((parts[1].links.power, parts[1].links.power_supplied), (LinkState::Redundant, 1.0));
    assert_ne!(parts[3].links.data, LinkState::LocalControl);
    // Cut the rest: the battery carries the part for 30 s, then it is dead; Data falls to local control.
    let rest: Vec<CellIndex> = net.conduits.iter().filter(|c| c.state != ConduitState::Cut).map(|c| c.cells[c.cells.len() / 2]).collect();
    cut_conduits(&mut net, &rest);
    assert!(net.conduits.iter().all(|c| c.state == ConduitState::Cut));
    assert!(unconnected_consumers(&net, &parts, &set, &cat).contains(&PartSlot(1)));
    solve_tick(&mut net, &mut parts, &cat, &tuning(), dt);
    assert_eq!((parts[1].links.power, parts[1].links.power_supplied), (LinkState::Buffer, 1.0));
    assert_eq!(parts[3].links.data, LinkState::LocalControl);
    for _ in 0..30 * 60 + 5 { solve_tick(&mut net, &mut parts, &cat, &tuning(), dt); }
    assert_eq!((parts[1].links.power, parts[1].links.power_supplied), (LinkState::Dead, 0.0));
    // Jury-rig everything: back on the network at half capacity.
    for i in 0..net.conduits.len() { assert!(jury_rig(&mut net, i as u16)); }
    assert!(!jury_rig(&mut net, 0));
    solve_tick(&mut net, &mut parts, &cat, &tuning(), dt);
    assert_eq!(parts[1].links.power, LinkState::Network);
    assert!((parts[1].links.power_supplied - 0.5).abs() < 1e-12);
}

#[test]
fn shedding_follows_priority_then_slot() {
    let g = CellGrid::new([1, 1, 1]);
    let cat = catalogue(vec![
        def("gen", PartFunction::Generator { kw: 100.0, fuel_lps: 0.0 }, vec![], 1.0),
        def("p60", PartFunction::Structure, vec![port(Power, Consume, 60.0, 0.0)], 1.0),
        def("p30", PartFunction::Structure, vec![port(Power, Consume, 30.0, 0.0)], 1.0),
    ]);
    let mut parts = vec![
        part(0, 2, vec![], &g, PartState::Inert),
        part(1, 0, vec![], &g, PartState::Generator { output_kw: 0.0 }),
        part(2, 2, vec![], &g, PartState::Inert),
        part(3, 1, vec![], &g, PartState::Inert),
        part(4, 2, vec![], &g, PartState::Inert),
    ];
    let pr = [5, 0, 3, 1, 5];
    for (p, &x) in parts.iter_mut().zip(pr.iter()) { p.priority = x; p.compartment = Some(CompartmentId(0)); }
    let mut net = Networks::default();
    solve_tick(&mut net, &mut parts, &cat, &tuning(), 1.0 / 60.0);
    let got: Vec<(f64, bool)> = parts.iter().map(|p| (p.links.power_supplied, p.links.shed)).collect();
    // p1 (60) and p3 (30) in full, the first p5 (slot 0) gets the last 10 kW, the second is shed.
    assert!((got[3].0 - 1.0).abs() < 1e-12 && (got[2].0 - 1.0).abs() < 1e-12);
    assert!((got[0].0 - 10.0 / 30.0).abs() < 1e-9 && !got[0].1);
    assert_eq!(got[4], (0.0, true));
    assert!(matches!(parts[1].state, PartState::Generator { output_kw } if (output_kw - 100.0).abs() < 1e-9));
    // Heavy damage halves the generator.
    parts[1].level = DamageLevel::Heavy;
    solve_tick(&mut net, &mut parts, &cat, &tuning(), 1.0 / 60.0);
    assert!((net.generation_kw - 50.0).abs() < 1e-9);
    assert!(parts[2].links.shed && parts[0].links.shed && !parts[3].links.shed);
}

#[test]
fn fuel_feeds_engines_and_runs_dry() {
    let g = CellGrid::new([1, 1, 1]);
    let diesel = PartFunction::Engine(EngineDef { kind: EngineKind::Diesel, shaft_kw: 1000.0, fuel_lps: 1.0, spool_s: 5.0 });
    let cat = catalogue(vec![def("tank", PartFunction::FuelTank { litres: 2.0 }, vec![], 1.0), def("engine", diesel, vec![], 1.0)]);
    let mut parts = vec![part(0, 0, vec![], &g, PartState::FuelTank { litres: 2.0 }), part(1, 1, vec![], &g, PartState::Engine { throttle: 1.0, output_kw: 0.0 })];
    for p in &mut parts { p.compartment = Some(CompartmentId(0)); }
    let mut net = Networks::default();
    let dt = 1.0 / 60.0;
    for _ in 0..60 { solve_tick(&mut net, &mut parts, &cat, &tuning(), dt); }
    assert!(matches!(parts[0].state, PartState::FuelTank { litres } if (litres - 1.0).abs() < 1e-9));
    assert_eq!(parts[1].links.fuel_supplied, 1.0);
    assert!((net.fuel_flow_lps - 1.0).abs() < 1e-9 && (net.fuel_l - 1.0).abs() < 1e-9);
    // The tank runs dry; the day tank carries the engine for its 60 s, then it starves.
    for _ in 0..120 { solve_tick(&mut net, &mut parts, &cat, &tuning(), dt); }
    assert!(matches!(parts[0].state, PartState::FuelTank { litres } if litres == 0.0));
    assert_eq!((parts[1].links.fuel, parts[1].links.fuel_supplied), (LinkState::Buffer, 1.0));
    for _ in 0..60 * 60 { solve_tick(&mut net, &mut parts, &cat, &tuning(), dt); }
    assert_eq!((parts[1].links.fuel, parts[1].links.fuel_supplied), (LinkState::Dead, 0.0));
    let _ = Fuel;
}

#[test]
fn pins_and_breakers() {
    let (g, v, set, parts, cat) = design();
    let auto = route(&v, &set, &parts, &NetworkEdits::default(), &cat);
    let link = auto.conduits.iter().find(|c| c.network == Power && !c.redundant && c.to == CompartmentId(2)).expect("link to 2").clone();
    let cells: Vec<content::cell::CellPos> = link.cells.iter().map(|c| g.pos(*c)).collect();
    let mut edits = NetworkEdits::default();
    edits.pinned_routes.push(content::network::PinnedRoute { network: Power, from: PartSlot(0), to: PartSlot(1), cells: cells.clone() });
    // A route through a missing slot, and one that jumps, are both broken.
    edits.pinned_routes.push(content::network::PinnedRoute { network: Power, from: PartSlot(0), to: PartSlot(9), cells: cells.clone() });
    edits.pinned_routes.push(content::network::PinnedRoute { network: Power, from: PartSlot(0), to: PartSlot(1), cells: vec![cells[0], cells[cells.len() - 1]] });
    let net = route(&v, &set, &parts, &edits, &cat);
    assert_eq!(net.broken_pins, vec![1, 2]);
    assert!(net.conduits.iter().any(|c| c.pinned && c.cells == link.cells));
    assert!(unconnected_consumers(&net, &parts, &set, &cat).is_empty());
    // An open breaker on every Power conduit leaves the consumers without supply.
    let mut edits = NetworkEdits::default();
    for c in auto.conduits.iter().filter(|c| c.network == Power) { edits.breakers.push(content::network::Breaker { network: Power, cell: g.pos(c.cells[1]), closed: false }); }
    let net = route(&v, &set, &parts, &edits, &cat);
    assert!(unconnected_consumers(&net, &parts, &set, &cat).contains(&PartSlot(1)));
}



/// Two compartments split by a bulkhead three cells thick (a tank flat, say): the thin-wall graph leaves them
/// apart, so the router lays the link through the solid wall and the consumer beyond it is served.
#[test]
fn thick_bulkhead_still_joins_its_compartments() {
    let g = box_hull([24, 8, 8], &[10, 11, 12], &[]);
    let gen = block(&g, [3, 1, 3], [4, 2, 4]);
    let load = block(&g, [18, 1, 3], [19, 2, 4]);
    let v = build(g.clone(), &[(PartSlot(0), gen.clone(), false, Flammable::No), (PartSlot(1), load.clone(), false, Flammable::No)]);
    let cat = catalogue(vec![
        def("gen", PartFunction::Generator { kw: 100.0, fuel_lps: 0.0 }, vec![], 2000.0),
        def("load", PartFunction::Structure, vec![port(Power, Consume, 40.0, 0.0)], 500.0),
    ]);
    let mut parts = vec![part(0, 0, gen, &g, PartState::Generator { output_kw: 0.0 }), part(1, 1, load, &g, PartState::Inert)];
    let (set, _) = CompartmentSet::derive(&v, &parts);
    set.link_parts(&mut parts);
    assert_eq!(set.compartments.len(), 2);
    let a = route(&v, &set, &parts, &NetworkEdits::default(), &cat);
    assert_eq!(a, route(&v, &set, &parts, &NetworkEdits::default(), &cat), "deterministic");
    assert!(unconnected_consumers(&a, &parts, &set, &cat).is_empty(), "the consumer past the thick wall is served");
    let link = a.conduits.iter().find(|c| c.network == Power && !c.redundant).expect("a power conduit");
    for cell in &link.cells { assert!(v.is_solid(v.pos(*cell)), "conduits run on structure"); }
    for w in link.cells.windows(2) {
        let (p, q) = (v.pos(w[0]), v.pos(w[1]));
        assert_eq!((p.x - q.x).abs() + (p.y - q.y).abs() + (p.z - q.z).abs(), 1, "face-connected run");
    }
    let mut parts = parts;
    solve_tick(&mut a.clone(), &mut parts, &cat, &tuning(), 1.0 / 60.0);
    assert_eq!(parts[1].links.power, LinkState::Network);
}

/// A thin-wall-connected design gains no thick crossings, so its routes are unchanged.
#[test]
fn thick_crossings_only_join_separate_components() {
    let (_, v, set, _, _) = design();
    let d = Dims::new(v.size());
    let cls = classify(&v);
    let thin = wall_crossings(d, &cls, &set.cell_to_compartment);
    assert!(thick_crossings(d, &cls, &set.cell_to_compartment, set.compartments.len(), &thin, &BTreeSet::new()).is_empty());
}

/// A space wholly filled by open equipment (a pump boxed into the structure) is a compartment with no air
/// cells; the router still reaches it, so the pump is not left on its battery.
#[test]
fn equipment_filled_space_is_reached() {
    let g = box_hull([16, 8, 8], &[10, 12], &[]);
    let gen = block(&g, [3, 1, 3], [4, 2, 4]);
    // x = 11 between the two bulkheads, one cell high and two wide, filled by the pump.
    let mut g2 = g.clone();
    for y in 1..=6 { for z in 1..=6 { if !(y == 3 && (3..=4).contains(&z)) { g2.set(CellPos::new(11, y, z), STEEL); } } }
    let pump = block(&g2, [11, 3, 3], [11, 3, 4]);
    let load = block(&g2, [14, 1, 3], [14, 2, 3]);
    let v = build(g2.clone(), &[(PartSlot(0), gen.clone(), false, Flammable::No), (PartSlot(1), pump.clone(), false, Flammable::No), (PartSlot(2), load.clone(), false, Flammable::No)]);
    let cat = catalogue(vec![
        def("gen", PartFunction::Generator { kw: 100.0, fuel_lps: 0.0 }, vec![], 2000.0),
        def("pump", PartFunction::Structure, vec![port(Power, Consume, 10.0, 0.0)], 400.0),
    ]);
    let mut parts = vec![
        part(0, 0, gen, &g2, PartState::Generator { output_kw: 0.0 }),
        part(1, 1, pump, &g2, PartState::Inert),
        part(2, 1, load, &g2, PartState::Inert),
    ];
    let (set, _) = CompartmentSet::derive(&v, &parts);
    set.link_parts(&mut parts);
    let a = route(&v, &set, &parts, &NetworkEdits::default(), &cat);
    let pump_comp = a.attach[1];
    if let Some(c) = set.compartments.get(pump_comp as usize) {
        if c.cells.is_empty() { assert!(equipment_hub(&set, pump_comp, &cls_of(&v), Dims::new(v.size())).is_some()); }
    }
    assert!(unconnected_consumers(&a, &parts, &set, &cat).is_empty(), "{:?}", unconnected_consumers(&a, &parts, &set, &cat));
}

fn cls_of(v: &VesselGrid) -> Vec<u8> { classify(v) }
