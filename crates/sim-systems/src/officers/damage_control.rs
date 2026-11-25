//! Unit `sys-officer-dc`. Damage Control officer: run pumps, isolate compartments, order repairs. Deliberate
//! limit: repairs in a fixed priority list (L311). DESIGN.md L311, L396, L638-648, L667-669, L703-725.
//!
//! Default repair list (player-reorderable; ties go to the lowest compartment index): fires in or next to a
//! magazine; breaches below the waterline, deepest first; other fires; cut conduits feeding priority 1 and 2
//! consumers; other cut conduits; part repairs, highest power priority first. Pumps drain their own
//! compartment or one directly adjacent compartment, deepest water first. By default only the player opens
//! flood valves; the officer auto-floods a burning magazine when 60 s of cook-off delay remain and the fire is
//! not projected out in time (`sim_core::fire::cookoff_forecast`).
use crate::prelude::*;
use content::graph::RepairPriority;
use content::cell::{CellGrid, CELL_M};
use content::part::PartFunction;
use sim_core::compartments::{Compartment, CompartmentSet, OpeningKind};
use sim_core::crew::CrewState;
use sim_core::networks::{ConduitState, Networks};
use sim_core::parts::{PartRuntime, PartState, TeamOrder};
use sim_core::vessel::Vessel;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DcState {
    /// Teams the player pinned to a job: (team index, order).
    pub pinned: Vec<(u8, TeamOrder)>,
    /// Valves, doors and pumps the player set by hand; the officer leaves them alone.
    pub manual_valves: Vec<(CompartmentId, bool)>,
    pub manual_doors: Vec<(PartSlot, bool)>,
    pub manual_pumps: Vec<(PartSlot, bool, Option<CompartmentId>)>,
    pub manual_magazine_floods: Vec<PartSlot>,
    /// What each team is doing and why, for the Bridge Board.
    pub plan: Vec<(TeamOrder, RepairPriority)>,
    /// Magazines the officer decided to flood; held in the flood list for the rest of the sortie so a
    /// fill already under way is never stopped half way (the ammunition is lost either way, L667).
    #[serde(default)]
    pub(crate) auto_flood: Vec<PartSlot>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DcOut {
    pub teams: Vec<TeamOrder>,
    pub pumps: Vec<(PartSlot, bool, Option<CompartmentId>)>,
    pub doors: Vec<(PartSlot, bool)>,
    pub open_valves: Vec<CompartmentId>,
    pub flood_magazines: Vec<PartSlot>,
}

/// The `DamageControlPlanner` stage: assign each team the highest job on the repair list that it can start
/// (berth factor permitting), run pumps, close doors around flooding and fire, and auto-flood a magazine
/// about to cook off. Unit: `sys-officer-dc`.
///
/// Team assignment: pinned teams keep their pin (a pin whose job no longer exists is handed back to the
/// officer); the other teams take the top jobs of the list, one team per job, and a team already working on a
/// selected job keeps it so its progress is not reset; teams left over after every job is covered join the
/// fires in list order (fire suppression adds up per team, the other jobs do not). Doors: every automatic door
/// stays closed in action, which closes every door around flooding and fire. Manual pump, door, valve and
/// magazine settings are passed through unchanged, so `DcOut` is the complete set of damage-control orders.
pub fn planner(vessel: &Vessel, repair_order: &[RepairPriority], state: &mut DcState, catalogue: &Catalogue, tuning: &TuningTable) -> DcOut {
    let view = DcView { parts: &vessel.parts, comps: &vessel.compartments, networks: &vessel.networks, crew: &vessel.crew, design: &vessel.grid.design, waterline_y: vessel.waterline_y };
    let forecast = |slot: PartSlot, teams: u8| sim_core::fire::cookoff_forecast(vessel, slot, teams, &tuning.fire);
    plan(&view, repair_order, state, catalogue, tuning, &forecast)
}

/// The parts of a vessel the planner reads.
pub(crate) struct DcView<'a> {
    pub parts: &'a [PartRuntime],
    pub comps: &'a CompartmentSet,
    pub networks: &'a Networks,
    pub crew: &'a CrewState,
    pub design: &'a CellGrid,
    pub waterline_y: f64,
}

/// Water below this many cubic metres is bilge residue, not worth a pump.
const PUMP_MIN_M3: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Job { order: TeamOrder, cat: RepairPriority }

/// The player's list made safe: duplicates dropped, missing entries appended in default order.
fn sanitize_order(order: &[RepairPriority]) -> Vec<RepairPriority> {
    let mut out: Vec<RepairPriority> = Vec::with_capacity(6);
    for &p in order.iter().chain(RepairPriority::DEFAULT_ORDER.iter()) { if !out.contains(&p) { out.push(p); } }
    out
}

fn function<'c>(catalogue: &'c Catalogue, p: &PartRuntime) -> Option<&'c PartFunction> { catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) }

pub(crate) fn plan(v: &DcView<'_>, repair_order: &[RepairPriority], state: &mut DcState, catalogue: &Catalogue, tuning: &TuningTable, forecast: &dyn Fn(PartSlot, u8) -> Option<(f64, bool)>) -> DcOut {
    let comps = &v.comps.compartments;
    let flooded_fraction = tuning.flooding.flooded_fraction;
    let comp = |c: CompartmentId| comps.get(c.index());
    let unworkable = |c: Option<CompartmentId>| c.and_then(comp).map(|c| c.is_burning() || c.fill_fraction() > flooded_fraction).unwrap_or(false);
    let neighbours = |c: CompartmentId| -> Vec<CompartmentId> {
        let mut n: Vec<CompartmentId> = v.comps.adjacency.iter().filter_map(|a| if a.a == c { Some(a.b) } else if a.b == c { Some(a.a) } else { None }).collect();
        n.sort();
        n.dedup();
        n
    };

    // ---- Candidate jobs per list entry ----
    let mut magazine_fires = Vec::new();
    let mut other_fires = Vec::new();
    for c in comps.iter().filter(|c| c.is_burning()) {
        let near_magazine = c.magazine.is_some() || neighbours(c.id).iter().any(|&n| comp(n).map(|n| n.magazine.is_some()).unwrap_or(false));
        if near_magazine { magazine_fires.push(c.id) } else { other_fires.push(c.id) }
    }
    // Breaches below the waterline, one job per compartment, deepest breach first; patching works only while
    // the compartment is under half flooded (L642-646).
    let mut breaches: BTreeMap<CompartmentId, f64> = BTreeMap::new();
    for o in &v.comps.openings {
        if o.kind != OpeningKind::Breach || o.b.is_some() || o.patched { continue; }
        let Some(c) = comp(o.a) else { continue };
        if c.fill_fraction() >= flooded_fraction { continue; }
        if o.cell.index() >= v.design.cells.len() { continue; }
        let y = (v.design.pos(o.cell).y as f64 + 0.5) * CELL_M;
        // Below the design waterline, or letting water in now (a hull settled deeper or heeled over).
        if y >= v.waterline_y && c.water_m3 <= PUMP_MIN_M3 { continue; }
        let e = breaches.entry(o.a).or_insert(f64::INFINITY);
        if y < *e { *e = y; }
    }
    let mut breaches: Vec<(CompartmentId, f64)> = breaches.into_iter().collect();
    breaches.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));

    // Compartments holding a priority 1 or 2 consumer or a power or fuel source: a cut conduit there starves
    // steering, pumps, point defence or the whole network.
    let mut critical = vec![false; comps.len()];
    for p in v.parts.iter().filter(|p| p.works()) {
        let Some(c) = p.compartment else { continue };
        let source = matches!(function(catalogue, p), Some(PartFunction::Generator { .. } | PartFunction::Battery { .. } | PartFunction::Engine(_) | PartFunction::FuelTank { .. }));
        if (1..=2).contains(&p.priority) || source { if let Some(f) = critical.get_mut(c.index()) { *f = true; } }
    }
    let is_critical = |c: CompartmentId| critical.get(c.index()).copied().unwrap_or(false);
    let mut conduits_critical = Vec::new();
    let mut conduits_other = Vec::new();
    for (i, k) in v.networks.conduits.iter().enumerate() {
        if k.state != ConduitState::Cut || i > u16::MAX as usize { continue; }
        let key = (k.from.min(k.to), i as u16);
        if is_critical(k.from) || is_critical(k.to) { conduits_critical.push(key) } else { conduits_other.push(key) }
    }
    conduits_critical.sort();
    conduits_other.sort();

    // Part repairs: highest power priority first; generation and propulsion rank with priority 2; parts that
    // draw no power and drive nothing come last. Destroyed parts wait for the tender.
    let cap = tuning.crew.repair_cap;
    let mut repairs: Vec<(u8, u16, PartSlot)> = Vec::new();
    for p in v.parts {
        if !p.works() || !(p.health < cap) || unworkable(p.compartment) { continue; }
        let rank = if (1..=5).contains(&p.priority) { p.priority } else {
            match function(catalogue, p) { Some(PartFunction::Engine(_) | PartFunction::Generator { .. } | PartFunction::Propulsor(_) | PartFunction::Battery { .. } | PartFunction::Rudder { .. }) => 2, Some(_) => 6, None => continue }
        };
        repairs.push((rank, p.compartment.map(|c| c.0).unwrap_or(u16::MAX), p.slot));
    }
    repairs.sort();

    let mut jobs: Vec<Job> = Vec::new();
    for cat in sanitize_order(repair_order) {
        match cat {
            RepairPriority::MagazineFires => jobs.extend(magazine_fires.iter().map(|&c| Job { order: TeamOrder::FightFire(c), cat })),
            RepairPriority::BreachesBelowWaterline => jobs.extend(breaches.iter().map(|&(c, _)| Job { order: TeamOrder::Patch { compartment: c }, cat })),
            RepairPriority::OtherFires => jobs.extend(other_fires.iter().map(|&c| Job { order: TeamOrder::FightFire(c), cat })),
            RepairPriority::ConduitsToCritical => jobs.extend(conduits_critical.iter().map(|&(_, i)| Job { order: TeamOrder::JuryRig { conduit: i }, cat })),
            RepairPriority::OtherConduits => jobs.extend(conduits_other.iter().map(|&(_, i)| Job { order: TeamOrder::JuryRig { conduit: i }, cat })),
            RepairPriority::PartRepairs => jobs.extend(repairs.iter().map(|&(_, _, s)| Job { order: TeamOrder::Repair(s), cat })),
        }
    }
    let category_of = |o: TeamOrder| -> RepairPriority {
        if let Some(j) = jobs.iter().find(|j| j.order == o) { return j.cat; }
        match o {
            TeamOrder::FightFire(c) => if magazine_fires.contains(&c) { RepairPriority::MagazineFires } else { RepairPriority::OtherFires },
            TeamOrder::Patch { .. } => RepairPriority::BreachesBelowWaterline,
            TeamOrder::JuryRig { conduit } => if conduits_critical.iter().any(|&(_, i)| i == conduit) { RepairPriority::ConduitsToCritical } else { RepairPriority::OtherConduits },
            TeamOrder::Repair(_) | TeamOrder::Idle => RepairPriority::PartRepairs,
        }
    };

    // ---- Teams ----
    let n_teams = if v.crew.teams.is_empty() { sim_core::crew::team_count(v.crew.original_berths, &tuning.crew) as usize } else { v.crew.teams.len() };
    let current = |t: usize| v.crew.teams.get(t).map(|t| t.order).unwrap_or_default();
    // A pin whose job is gone (fire out, breach sealed, conduit rigged, part at the cap or destroyed) is handed
    // back; pins naming a team the hull does not have are dropped.
    let still_needed = |o: &TeamOrder| -> bool {
        match *o {
            TeamOrder::Idle => true,
            TeamOrder::FightFire(c) => comp(c).map(|c| c.is_burning()).unwrap_or(false),
            TeamOrder::Patch { compartment } => v.comps.openings.iter().any(|op| op.a == compartment && op.kind == OpeningKind::Breach && op.b.is_none() && !op.patched)
                && comp(compartment).map(|c| c.fill_fraction() < flooded_fraction).unwrap_or(false),
            TeamOrder::JuryRig { conduit } => v.networks.conduits.get(conduit as usize).map(|k| k.state == ConduitState::Cut).unwrap_or(false),
            TeamOrder::Repair(s) => v.parts.get(s.index()).filter(|p| p.slot == s).or_else(|| v.parts.iter().find(|p| p.slot == s)).map(|p| p.works() && p.health < cap).unwrap_or(false),
        }
    };
    let mut pins: Vec<(u8, TeamOrder)> = Vec::new();
    for &(t, o) in state.pinned.iter().rev() {
        if (t as usize) < n_teams && !pins.iter().any(|&(pt, _)| pt == t) && still_needed(&o) { pins.push((t, o)); }
    }
    pins.sort_by_key(|&(t, _)| t);
    state.pinned = pins.clone();

    let mut teams = vec![TeamOrder::Idle; n_teams];
    let mut pinned = vec![false; n_teams];
    // With no surviving berths no job can start (L704): every team stands idle.
    let can_work = v.crew.berth_factor().is_some();
    if can_work {
        for &(t, o) in &pins { teams[t as usize] = o; pinned[t as usize] = true; }
        let free: Vec<usize> = (0..n_teams).filter(|&t| !pinned[t]).collect();
        let open: Vec<Job> = jobs.iter().copied().filter(|j| matches!(j.order, TeamOrder::FightFire(_)) || !pins.iter().any(|&(_, o)| o == j.order)).collect();
        let selected: Vec<Job> = open.iter().copied().take(free.len()).collect();
        let mut taken = vec![false; selected.len()];
        let mut busy = vec![false; n_teams];
        // Continuity first: a team already on a selected job keeps it.
        for &t in &free {
            if let Some(k) = (0..selected.len()).find(|&k| !taken[k] && selected[k].order == current(t)) {
                taken[k] = true;
                teams[t] = selected[k].order;
                busy[t] = true;
            }
        }
        let idle: Vec<usize> = free.iter().copied().filter(|&t| !busy[t]).collect();
        let mut rest = idle.into_iter();
        for (k, j) in selected.iter().enumerate() {
            if taken[k] { continue; }
            let Some(t) = rest.next() else { break };
            teams[t] = j.order;
            busy[t] = true;
        }
        // Spare teams double up on fires, most urgent first; one already on a burning fire stays there.
        let fires: Vec<TeamOrder> = jobs.iter().filter(|j| matches!(j.order, TeamOrder::FightFire(_))).map(|j| j.order).collect();
        if !fires.is_empty() {
            let mut k = 0;
            for t in free.iter().copied().filter(|&t| !busy[t]) {
                teams[t] = if fires.contains(&current(t)) { current(t) } else { let o = fires[k % fires.len()]; k += 1; o };
            }
        }
    }
    state.plan = teams.iter().filter(|o| **o != TeamOrder::Idle).map(|&o| (o, category_of(o))).collect();

    // ---- Valves (player only by default, L648) ----
    let mut valves: BTreeMap<CompartmentId, bool> = BTreeMap::new();
    for &(c, open) in &state.manual_valves { if comp(c).map(|c| c.below_waterline).unwrap_or(false) { valves.insert(c, open); } }
    let open_valves: Vec<CompartmentId> = valves.iter().filter(|(_, &o)| o).map(|(&c, _)| c).collect();

    // ---- Magazines ----
    let part_of = |s: PartSlot| v.parts.get(s.index()).filter(|p| p.slot == s).or_else(|| v.parts.iter().find(|p| p.slot == s));
    let is_magazine = |s: PartSlot| part_of(s).map(|p| matches!(function(catalogue, p), Some(PartFunction::Magazine { .. }))).unwrap_or(false);
    let mut flood: BTreeSet<PartSlot> = state.manual_magazine_floods.iter().copied().filter(|&s| is_magazine(s)).collect();
    flood.extend(state.auto_flood.iter().copied());
    for p in v.parts.iter().filter(|p| p.works() && matches!(function(catalogue, p), Some(PartFunction::Magazine { .. }))) {
        if flood.contains(&p.slot) { continue; }
        if matches!(p.state, PartState::Magazine { flooded: true, .. }) { continue; }
        // The compartment that names the magazine, else the one the part sits in.
        let Some(c) = comps.iter().find(|c| c.magazine == Some(p.slot)).map(|c| c.id).or(p.compartment) else { continue };
        if !comp(c).map(|c| c.is_burning()).unwrap_or(false) { continue; }
        let on_it = teams.iter().filter(|&&o| o == TeamOrder::FightFire(c)).count().min(u8::MAX as usize) as u8;
        if let Some((remaining_s, out_in_time)) = forecast(p.slot, on_it) {
            if remaining_s <= tuning.fire.autoflood_remaining_s && !out_in_time {
                flood.insert(p.slot);
                state.auto_flood.push(p.slot);
            }
        }
    }
    state.auto_flood.sort();
    state.auto_flood.dedup();
    let flooded_comps: BTreeSet<CompartmentId> = flood.iter().filter_map(|&s| part_of(s).and_then(|p| p.compartment)).collect();

    // ---- Pumps: own or one adjacent compartment, deepest water first; spread over the flooding ----
    let depth = |c: &Compartment| c.fill_fraction() * ((c.max[1] as f64 - c.min[1] as f64 + 1.0).max(1.0) * CELL_M);
    let mut manual_pumps: BTreeMap<PartSlot, (bool, Option<CompartmentId>)> = BTreeMap::new();
    for &(s, run, t) in &state.manual_pumps { manual_pumps.insert(s, (run, t)); }
    let mut claimed: BTreeSet<CompartmentId> = BTreeSet::new();
    let mut pumps = Vec::new();
    for p in v.parts.iter().filter(|p| matches!(function(catalogue, p), Some(PartFunction::Pump { .. }))) {
        let reach: Vec<CompartmentId> = match p.compartment { Some(c) if comp(c).is_some() => { let mut r = vec![c]; r.extend(neighbours(c)); r } _ => Vec::new() };
        if let Some(&(run, target)) = manual_pumps.get(&p.slot) {
            let target = target.filter(|t| reach.contains(t));
            pumps.push((p.slot, run, target));
            if run { if let Some(t) = target.or(p.compartment) { claimed.insert(t); } }
            continue;
        }
        if !p.works() { continue; }
        let mut wet: Vec<(&Compartment, f64)> = reach.iter().filter_map(|&c| comp(c))
            .filter(|c| c.water_m3 > PUMP_MIN_M3 && !c.flood_valve_open && !valves.get(&c.id).copied().unwrap_or(false) && !flooded_comps.contains(&c.id))
            .map(|c| (c, depth(c))).collect();
        wet.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
        let pick = wet.iter().find(|(c, _)| !claimed.contains(&c.id)).or(wet.first()).map(|(c, _)| c.id);
        match pick {
            Some(t) => { claimed.insert(t); pumps.push((p.slot, true, Some(t))); }
            None => pumps.push((p.slot, false, None)),
        }
    }

    // ---- Doors: closed in action unless the player opened one ----
    let mut manual_doors: BTreeMap<PartSlot, bool> = BTreeMap::new();
    for &(s, closed) in &state.manual_doors { manual_doors.insert(s, closed); }
    let doors = v.parts.iter().filter(|p| matches!(function(catalogue, p), Some(PartFunction::Door))).map(|p| (p.slot, manual_doors.get(&p.slot).copied().unwrap_or(true))).collect();

    DcOut { teams, pumps, doors, open_valves, flood_magazines: flood.into_iter().collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::cell::Face;
    use content::part::*;
    use sim_core::compartments::{Adjacency, Opening};
    use sim_core::crew::Team;
    use sim_core::networks::Conduit;

    fn pdef(function: PartFunction) -> PartDef {
        PartDef {
            id: PartId::new("p"), name: TextKey::new("n"), description: TextKey::new("d"), category: PartCategory::Hull, tier: Default::default(),
            phase: Default::default(), size: [1, 1, 1], placement: Placement::Anywhere, mass_kg: 1.0, cost: 1, ports: vec![], power_priority: 0,
            buffer_s: None, watertight: false, flammable: Flammable::No, rcs_factor: 1.0, tags: vec![], function, model: ProcModel::default(),
            icon: Default::default(), replaced_by: None, standard_limit: None,
        }
    }
    /// Catalogue indices: 0 structure, 1 magazine, 2 pump, 3 door, 4 generator.
    fn catalogue() -> Catalogue {
        let mut c = Catalogue::default();
        c.parts.insert(PartId::new("a"), pdef(PartFunction::Structure));
        c.parts.insert(PartId::new("b"), pdef(PartFunction::Magazine { calibre_mm: 76, rounds: 100, ammo_kg: 1_800.0 }));
        c.parts.insert(PartId::new("c"), pdef(PartFunction::Pump { m3ps: 0.05 }));
        c.parts.insert(PartId::new("d"), pdef(PartFunction::Door));
        c.parts.insert(PartId::new("e"), pdef(PartFunction::Generator { kw: 100.0, fuel_lps: 0.1 }));
        c
    }
    fn comp(id: u16, ymin: i16) -> Compartment {
        Compartment { id: CompartmentId(id), cells: vec![], volume_m3: 10.0, min: [0, ymin, 0], max: [3, ymin + 3, 3], centroid: DVec3::ZERO, water_m3: 0.0, water_plane_y: 0.0, water_centroid: DVec3::ZERO, free_surface_m2: 1.0, fire: 0.0, fire_ticks: 0, parts: vec![], magazine: None, berths: 4, below_waterline: true, flood_valve_open: false, sealed: true }
    }
    fn part(slot: u16, def: u16, comp: Option<u16>, priority: u8, health: f64) -> PartRuntime {
        let level = if health <= 0.0 { DamageLevel::Destroyed } else { DamageLevel::Intact };
        PartRuntime { slot: PartSlot(slot), def: PartIdx(def), pos: content::cell::CellPos::new(0, 0, 0), rot: Default::default(), cells: vec![], centre: DVec3::ZERO, compartment: comp.map(CompartmentId), health, level, links: Default::default(), priority, state: PartState::Inert }
    }
    fn breach(c: u16, cell: u32) -> Opening { Opening { a: CompartmentId(c), b: None, cell: CellIndex(cell), face: Face::YNeg, area_m2: 0.25, kind: OpeningKind::Breach, patched: false, open: true } }

    struct Fixture { parts: Vec<PartRuntime>, comps: CompartmentSet, networks: Networks, crew: CrewState, design: CellGrid }
    impl Fixture {
        fn new(teams: usize) -> Self {
            let comps = CompartmentSet { compartments: (0..4).map(|i| comp(i, 0)).collect(), cell_to_compartment: vec![], adjacency: vec![Adjacency { a: CompartmentId(0), b: CompartmentId(1), shared_faces: 4, vertical: false, has_door: true }, Adjacency { a: CompartmentId(1), b: CompartmentId(2), shared_faces: 4, vertical: false, has_door: false }], openings: vec![], top_deck_y: 10 };
            let crew = CrewState { original_berths: 8, surviving_berths: 8, teams: vec![Team::default(); teams], ..Default::default() };
            // 4 x 20 x 1 cells: cell index i has y = i / 4.
            Fixture { parts: vec![], comps, networks: Networks::default(), crew, design: CellGrid::new([4, 20, 1]) }
        }
        fn run(&self, order: &[RepairPriority], state: &mut DcState, forecast: &dyn Fn(PartSlot, u8) -> Option<(f64, bool)>) -> DcOut {
            let v = DcView { parts: &self.parts, comps: &self.comps, networks: &self.networks, crew: &self.crew, design: &self.design, waterline_y: 4.0 };
            plan(&v, order, state, &catalogue(), &TuningTable::default(), forecast)
        }
    }
    fn no_forecast(_: PartSlot, _: u8) -> Option<(f64, bool)> { None }

    #[test]
    fn list_order_magazine_fire_then_deepest_breach() {
        let mut f = Fixture::new(3);
        f.parts.push(part(0, 1, Some(0), 3, 1.0)); // magazine in compartment 0
        f.comps.compartments[0].magazine = Some(PartSlot(0));
        f.comps.compartments[1].fire = 100.0; // next to the magazine
        f.comps.compartments[3].fire = 100.0; // other fire
        f.comps.openings = vec![breach(2, 8 * 4), breach(3, 2 * 4), breach(2, 30 * 4)]; // y=8 (above 4 m), y=2 (1.25 m)
        let mut st = DcState::default();
        let out = f.run(&[], &mut st, &no_forecast);
        assert_eq!(out.teams, vec![TeamOrder::FightFire(CompartmentId(1)), TeamOrder::Patch { compartment: CompartmentId(3) }, TeamOrder::FightFire(CompartmentId(3))]);
        assert_eq!(st.plan[0].1, RepairPriority::MagazineFires);
        assert_eq!(st.plan[1].1, RepairPriority::BreachesBelowWaterline);
        // Player reorder: other fires before everything, duplicates and gaps tolerated.
        let out = f.run(&[RepairPriority::OtherFires, RepairPriority::OtherFires], &mut DcState::default(), &no_forecast);
        assert_eq!(out.teams[0], TeamOrder::FightFire(CompartmentId(3)));
    }

    #[test]
    fn teams_keep_selected_jobs_and_spares_double_up_on_fires() {
        let mut f = Fixture::new(3);
        f.comps.compartments[2].fire = 100.0;
        f.parts.push(part(5, 4, Some(1), 0, 0.5)); // damaged generator
        f.crew.teams[0].order = TeamOrder::Repair(PartSlot(5));
        let out = f.run(&[], &mut DcState::default(), &no_forecast);
        // Team 0 stays on its repair; team 1 takes the fire; team 2 joins the fire.
        assert_eq!(out.teams, vec![TeamOrder::Repair(PartSlot(5)), TeamOrder::FightFire(CompartmentId(2)), TeamOrder::FightFire(CompartmentId(2))]);
    }

    #[test]
    fn pins_hold_until_done_and_bad_pins_drop() {
        let mut f = Fixture::new(2);
        f.comps.compartments[2].fire = 100.0;
        f.networks.conduits.push(Conduit { network: NetworkKind::Power, from: CompartmentId(0), to: CompartmentId(1), cells: vec![], redundant: false, pinned: false, state: ConduitState::Cut });
        let mut st = DcState { pinned: vec![(0, TeamOrder::JuryRig { conduit: 0 }), (9, TeamOrder::Idle)], ..Default::default() };
        let out = f.run(&[], &mut st, &no_forecast);
        assert_eq!(out.teams, vec![TeamOrder::JuryRig { conduit: 0 }, TeamOrder::FightFire(CompartmentId(2))]);
        assert_eq!(st.pinned, vec![(0, TeamOrder::JuryRig { conduit: 0 })]);
        f.networks.conduits[0].state = ConduitState::JuryRigged;
        let out = f.run(&[], &mut st, &no_forecast);
        assert!(st.pinned.is_empty(), "finished pin handed back");
        assert_eq!(out.teams, vec![TeamOrder::FightFire(CompartmentId(2)), TeamOrder::FightFire(CompartmentId(2))]);
    }

    #[test]
    fn no_berths_means_no_jobs() {
        let mut f = Fixture::new(2);
        f.comps.compartments[2].fire = 100.0;
        f.crew.surviving_berths = 0;
        let mut st = DcState::default();
        let out = f.run(&[], &mut st, &no_forecast);
        assert!(out.teams.iter().all(|t| *t == TeamOrder::Idle));
        assert!(st.plan.is_empty());
    }

    #[test]
    fn magazine_floods_when_cookoff_is_near_and_fire_will_not_be_out() {
        let mut f = Fixture::new(1);
        // The part's own compartment is unknown: the compartment naming the magazine decides.
        f.parts.push(part(0, 1, None, 3, 1.0));
        f.comps.compartments[0].magazine = Some(PartSlot(0));
        f.comps.compartments[0].fire = 100.0;
        let mut st = DcState::default();
        // 70 s left: wait.
        let out = f.run(&[], &mut st, &|_, teams| { assert_eq!(teams, 1); Some((70.0, false)) });
        assert!(out.flood_magazines.is_empty());
        // 60 s left and projected out in time: wait.
        assert!(f.run(&[], &mut st, &|_, _| Some((60.0, true))).flood_magazines.is_empty());
        // 59 s left and not out in time: flood, and keep flooding afterwards.
        assert_eq!(f.run(&[], &mut st, &|_, _| Some((59.0, false))).flood_magazines, vec![PartSlot(0)]);
        assert_eq!(f.run(&[], &mut st, &|_, _| Some((200.0, true))).flood_magazines, vec![PartSlot(0)]);
    }

    #[test]
    fn pumps_take_the_deepest_reachable_water_and_spread() {
        let mut f = Fixture::new(1);
        f.parts.push(part(1, 2, Some(1), 1, 1.0));
        f.parts.push(part(2, 2, Some(1), 1, 1.0));
        f.parts.push(part(3, 2, Some(3), 1, 1.0)); // dry and alone
        f.parts.push(part(4, 3, Some(1), 5, 1.0));
        f.comps.compartments[0].water_m3 = 2.0;
        f.comps.compartments[2].water_m3 = 6.0;
        f.comps.compartments[1].water_m3 = 0.01;
        let mut st = DcState { manual_doors: vec![(PartSlot(4), false)], manual_valves: vec![(CompartmentId(3), true)], ..Default::default() };
        let out = f.run(&[], &mut st, &no_forecast);
        assert_eq!(out.pumps, vec![(PartSlot(1), true, Some(CompartmentId(2))), (PartSlot(2), true, Some(CompartmentId(0))), (PartSlot(3), false, None)]);
        assert_eq!(out.doors, vec![(PartSlot(4), false)]);
        assert_eq!(out.open_valves, vec![CompartmentId(3)]);
        // A manual pump target out of reach is cleared, not obeyed.
        st.manual_pumps = vec![(PartSlot(3), true, Some(CompartmentId(0)))];
        let out = f.run(&[], &mut st, &no_forecast);
        assert_eq!(out.pumps[2], (PartSlot(3), true, None));
    }

    #[test]
    fn breach_above_design_waterline_counts_once_water_comes_in() {
        let mut f = Fixture::new(1);
        f.comps.openings = vec![breach(2, 12 * 4)]; // y = 6.25 m, above the 4 m waterline
        assert_eq!(f.run(&[], &mut DcState::default(), &no_forecast).teams, vec![TeamOrder::Idle]);
        f.comps.compartments[2].water_m3 = 1.0;
        assert_eq!(f.run(&[], &mut DcState::default(), &no_forecast).teams, vec![TeamOrder::Patch { compartment: CompartmentId(2) }]);
    }
}
