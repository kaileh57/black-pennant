//! Unit `sys-officer-engineer`. Engineer officer: power and fuel priority, load shedding. Deliberate limit:
//! fixed priority order, no prediction (L308). DESIGN.md L287-297.
//!
//! Every consumer has a priority 1..=5. On a shortage load is shed from priority 5 upward; priority 1 goes
//! last. Default table by category: 1 steering gear, the Data network and its consoles, bilge and
//! damage-control pumps; 2 propulsion motors (electric-drive hulls), fire-control radars and directors,
//! point-defence guns and missiles, decoy launchers; 3 main gun mounts, missile launchers, torpedo tubes;
//! 4 search radars, sonar, ESM, drone launch and recharge; 5 cranes, davits, mine rails, camera mast,
//! passenger-berth hotel load and every other non-combat consumer. sim-core's `networks::solve_tick` does the
//! shedding itself from the priorities in `VesselControls`; this stage only sets them.
use crate::prelude::*;
use content::part::{EngineKind, PartDef, PartFunction, PartTag, PortRole, SensorKind};
use sim_core::parts::{PartRuntime, PartState};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineerState { pub shed: Vec<PartSlot>, pub generation_kw: f64, pub demand_kw: f64, pub fuel_fraction: f64 }

/// Default priority of a part from the table above (auto-set value 2, gated by the power-starved trial at
/// L297). 0 = not a power consumer. Unit: `sys-officer-engineer`.
///
/// A part is a consumer when it has a Power `Consume` port, or is an electric propulsion motor (which draws
/// its shaft power from the Power network). The catalogue's authored `power_priority` wins when it is 1..=5,
/// so a gate fallback can retune the table by category in data; otherwise the table is applied by function
/// and tags.
pub fn default_priority(def: &PartDef) -> u8 {
    if !is_consumer(def) { return 0; }
    if (1..=5).contains(&def.power_priority) { return def.power_priority; }
    table_priority(def)
}

fn is_consumer(def: &PartDef) -> bool {
    def.ports.iter().any(|p| p.network == NetworkKind::Power && p.role == PortRole::Consume)
        || matches!(&def.function, PartFunction::Engine(e) if e.kind == EngineKind::ElectricMotor)
}

/// The L289-295 table by part function, with the `Steering` and `PointDefence` tags taking precedence.
fn table_priority(def: &PartDef) -> u8 {
    if def.tags.contains(&PartTag::Steering) { return 1; }
    if def.tags.contains(&PartTag::PointDefence) { return 2; }
    match &def.function {
        // 1: steering gear, the Data network and its consoles, bilge and damage-control pumps.
        PartFunction::Rudder { .. } | PartFunction::Console | PartFunction::Pump { .. } => 1,
        // 2: propulsion motors, fire-control radars and directors, point defence, decoy launchers. Self-screening
        // jammers are soft-kill defence alongside decoys.
        PartFunction::Engine(_) | PartFunction::Propulsor(_) | PartFunction::Director | PartFunction::Decoy(_) | PartFunction::Jammer { .. } => 2,
        PartFunction::Gun(g) if g.point_defence => 2,
        PartFunction::Sensor(s) if matches!(s.kind, SensorKind::FireControlRadar | SensorKind::Seeker) => 2,
        // 3: main gun mounts with their hoists and magazines, missile launchers, torpedo tubes, racks.
        PartFunction::Gun(_) | PartFunction::Hoist { .. } | PartFunction::Magazine { .. } | PartFunction::Launcher(_) => 3,
        // 4: search radars, sonar, ESM, drone launch and recharge.
        PartFunction::Sensor(s) if s.kind != SensorKind::Camera => 4,
        PartFunction::DroneBay { .. } | PartFunction::HeliDeck { .. } => 4,
        // 5: cranes, davits, mine rails, the camera mast, passenger-berth hotel load, everything else.
        PartFunction::Sensor(_) | PartFunction::Crane | PartFunction::RhibDavit | PartFunction::BoardingKit { .. } | PartFunction::RecoveryNet
        | PartFunction::MineRail { .. } | PartFunction::CameraMast { .. } | PartFunction::PassengerBerths { .. } | PartFunction::Berths { .. }
        | PartFunction::Door | PartFunction::Structure | PartFunction::Ballast { .. } | PartFunction::Generator { .. } | PartFunction::Battery { .. }
        | PartFunction::FuelTank { .. } => 5,
    }
}

/// The `LoadShedder` stage: priority per consumer (settings overrides first, then defaults), in ascending
/// slot. Also refreshes the state the Bridge Board shows. Unit: `sys-officer-engineer`.
///
/// Overrides outside 1..=5 are ignored (player data); overrides on parts that draw no power are ignored, so a
/// non-consumer is never put on the shedding list. Parts whose catalogue index is unknown are skipped.
pub fn load_shedder(parts: &[PartRuntime], overrides: &BTreeMap<PartSlot, u8>, networks: &sim_core::networks::Networks, catalogue: &Catalogue, state: &mut EngineerState) -> Vec<(PartSlot, u8)> {
    let mut out = Vec::with_capacity(parts.len());
    let mut shed = Vec::new();
    for p in parts {
        let Some((_, def)) = catalogue.parts.get_index(p.def.index()) else { continue };
        let default = default_priority(def);
        if default == 0 { continue; }
        let priority = match overrides.get(&p.slot) { Some(&o) if (1..=5).contains(&o) => o, _ => default };
        out.push((p.slot, priority));
        if p.links.shed { shed.push(p.slot); }
    }
    out.sort_by_key(|&(s, _)| s);
    shed.sort();
    state.shed = shed;
    state.generation_kw = finite(networks.generation_kw);
    state.demand_kw = finite(networks.demand_kw);
    state.fuel_fraction = fuel_fraction(parts, catalogue);
    out
}

/// Fuel left in the tanks as a fraction of their capacity. A hull without fuel tanks (battery or
/// shore-powered) never runs low and reports 1.0.
pub(crate) fn fuel_fraction(parts: &[PartRuntime], catalogue: &Catalogue) -> f64 {
    let (mut tank_l, mut cap_l) = (0.0, 0.0);
    for p in parts {
        let Some((_, def)) = catalogue.parts.get_index(p.def.index()) else { continue };
        if let PartFunction::FuelTank { litres } = def.function {
            cap_l += finite(litres);
            if let PartState::FuelTank { litres } = p.state { tank_l += finite(litres); }
        }
    }
    if cap_l > 0.0 { (tank_l / cap_l).clamp(0.0, 1.0) } else { 1.0 }
}

fn finite(v: f64) -> f64 { if v.is_finite() { v.max(0.0) } else { 0.0 } }

#[cfg(test)]
mod tests {
    use super::*;
    use content::part::*;
    use content::part::weapons::CalibreBand;

    fn def(function: PartFunction, consumes: bool) -> PartDef {
        PartDef {
            id: PartId::new("p"), name: TextKey::new("n"), description: TextKey::new("d"), category: PartCategory::Hull, tier: Tier::default(),
            phase: Default::default(), size: [1, 1, 1], placement: Placement::Anywhere, mass_kg: 1.0, cost: 1,
            ports: if consumes { vec![PortDef { network: NetworkKind::Power, role: PortRole::Consume, rate: 5.0, capacity: 0.0 }] } else { vec![] },
            power_priority: 0, buffer_s: None, watertight: false, flammable: Flammable::No, rcs_factor: 1.0, tags: vec![], function,
            model: ProcModel::default(), icon: Default::default(), replaced_by: None, standard_limit: None,
        }
    }
    fn sensor(kind: SensorKind) -> PartFunction { PartFunction::Sensor(SensorDef { kind, r_ref_m: 1.0, rate_hz: 5.0, rpm: None, fov_half_deg: 180.0, emitter: String::new() }) }
    fn gun(pd: bool) -> PartFunction {
        PartFunction::Gun(GunDef { calibre_mm: 76, band: CalibreBand::Medium, barrels: 1, rof_rpm: 80.0, muzzle_mps: 900.0, shell_kg: 6.0, train_rate_dps: 30.0, elevation_rate_dps: 30.0, elevation_min_deg: -10.0, elevation_max_deg: 85.0, dispersion_mrad: 1.0, point_defence: pd, ready_rounds: 10 })
    }

    #[test]
    fn table_rows_match_the_spec() {
        let cases: Vec<(PartFunction, u8)> = vec![
            (PartFunction::Rudder { area_m2: 1.0, max_deg: 35.0, rate_dps: 5.0 }, 1),
            (PartFunction::Console, 1),
            (PartFunction::Pump { m3ps: 0.05 }, 1),
            (PartFunction::Engine(EngineDef { kind: EngineKind::ElectricMotor, shaft_kw: 500.0, fuel_lps: 0.0, spool_s: 5.0 }), 2),
            (sensor(SensorKind::FireControlRadar), 2),
            (PartFunction::Director, 2),
            (gun(true), 2),
            (PartFunction::Decoy(DecoyLauncherDef { kind: DecoyKind::Chaff, rounds: 10, salvo: 2, reload_s: 10.0 }), 2),
            (gun(false), 3),
            (PartFunction::Hoist { rounds_per_min: 60.0 }, 3),
            (PartFunction::Launcher(LauncherDef { kind: LauncherKind::TorpedoTube, munition: MunitionId::new("t"), cells: 2, interval_s: 2.0, reload_s: 0.0, train_rate_dps: None }), 3),
            (sensor(SensorKind::SearchRadar), 4),
            (sensor(SensorKind::PassiveSonar), 4),
            (sensor(SensorKind::Esm), 4),
            (PartFunction::DroneBay { drone: MunitionId::new("d"), count: 1, recharge_s: 60.0 }, 4),
            (PartFunction::Crane, 5),
            (PartFunction::RhibDavit, 5),
            (PartFunction::MineRail { mine: MunitionId::new("m"), capacity: 4 }, 5),
            (PartFunction::CameraMast { fov_deg: 10.0, photo_range_m: 6_000.0 }, 5),
            (PartFunction::PassengerBerths { count: 8 }, 5),
            (sensor(SensorKind::Camera), 5),
        ];
        for (f, want) in cases {
            let d = def(f.clone(), true);
            assert_eq!(default_priority(&d), want, "{f:?}");
        }
    }

    #[test]
    fn tags_and_authored_values_and_non_consumers() {
        let mut d = def(gun(false), true);
        d.tags.push(PartTag::PointDefence);
        assert_eq!(default_priority(&d), 2);
        d.power_priority = 4;
        assert_eq!(default_priority(&d), 4, "authored 1..=5 wins");
        d.power_priority = 9;
        assert_eq!(default_priority(&d), 2, "out-of-range authored value falls back to the table");
        let mut n = def(PartFunction::Structure, false);
        n.power_priority = 3;
        assert_eq!(default_priority(&n), 0, "a part with no Power consume port is never a consumer");
        // Diesel engines burn fuel and draw no power.
        assert_eq!(default_priority(&def(PartFunction::Engine(EngineDef { kind: EngineKind::Diesel, shaft_kw: 1.0, fuel_lps: 1.0, spool_s: 1.0 }), false)), 0);
    }

    fn runtime(slot: u16, def: u16, state: PartState, shed: bool) -> PartRuntime {
        let mut links = sim_core::networks::PartLinks::default();
        links.shed = shed;
        PartRuntime { slot: PartSlot(slot), def: PartIdx(def), pos: content::cell::CellPos::new(0, 0, 0), rot: Default::default(), cells: vec![], centre: DVec3::ZERO, compartment: None, health: 1.0, level: DamageLevel::Intact, links, priority: 0, state }
    }

    #[test]
    fn shedder_applies_valid_overrides_and_reports_state() {
        let mut cat = Catalogue::default();
        cat.parts.insert(PartId::new("a.radar"), def(sensor(SensorKind::SearchRadar), true));
        cat.parts.insert(PartId::new("b.tank"), def(PartFunction::FuelTank { litres: 1_000.0 }, false));
        cat.parts.insert(PartId::new("c.crane"), def(PartFunction::Crane, true));
        let parts = vec![
            runtime(0, 0, PartState::Sensor { bearing_rad: 0.0, swept_from_rad: 0.0, radiating: false, train_rad: 0.0 }, false),
            runtime(1, 1, PartState::FuelTank { litres: 250.0 }, false),
            runtime(2, 2, PartState::Inert, true),
            runtime(3, 99, PartState::Inert, false), // unknown catalogue index: skipped, no panic
        ];
        let mut ov = BTreeMap::new();
        ov.insert(PartSlot(0), 1);
        ov.insert(PartSlot(1), 1); // tank is not a consumer: ignored
        ov.insert(PartSlot(2), 0); // invalid: default kept
        let mut net = sim_core::networks::Networks::default();
        net.generation_kw = 100.0;
        net.demand_kw = f64::NAN;
        let mut st = EngineerState::default();
        let out = load_shedder(&parts, &ov, &net, &cat, &mut st);
        assert_eq!(out, vec![(PartSlot(0), 1), (PartSlot(2), 5)]);
        assert_eq!(st.shed, vec![PartSlot(2)]);
        assert_eq!(st.generation_kw, 100.0);
        assert_eq!(st.demand_kw, 0.0);
        assert!((st.fuel_fraction - 0.25).abs() < 1e-12);
    }
}
