use super::*;
use crate::engage::MountEngage;
use crate::text::csv_check::{check_msg, lint, parse, rendered_len, GLOSSARY};
use std::collections::BTreeMap as Map;

const CSV: &str = include_str!("../../../../game/i18n/officers.csv");
const SIM_CSV: &str = include_str!("../../../../game/i18n/sim.csv");

fn rows() -> Map<String, (String, String)> { let mut r = parse(CSV); r.extend(parse(SIM_CSV)); r }

struct Fx { stages: StageStates, control: ControlState, orders: ShipOrders, settings: OfficerSettings, tracks: Vec<Track>, parts: Vec<PartRuntime>, networks: Networks, previous: BridgeBoard, tick: u64 }
impl Fx {
    fn new() -> Self { Fx { stages: StageStates::default(), control: ControlState::default(), orders: ShipOrders::default(), settings: OfficerSettings::default(), tracks: vec![], parts: vec![], networks: Networks::default(), previous: BridgeBoard::default(), tick: 1_000 } }
    fn board(&self) -> BridgeBoard {
        build(&BoardIn { tick: self.tick, stages: &self.stages, control: &self.control, orders: &self.orders, settings: &self.settings, tracks: &self.tracks, own_pos: DVec3::ZERO, parts: &self.parts, networks: &self.networks, teams: 2, can_work: true, trigger_range_m: 20_000.0, layers: [ControlLayer::default(); 5], overrides: [0, 0, 0, 2, 0], previous: &self.previous })
    }
}

fn check_board(b: &BridgeBoard) {
    let rows = rows();
    assert_eq!(b.officers.len(), 5);
    for (r, role) in b.officers.iter().zip(OfficerRole::ALL) {
        assert_eq!(r.role, Some(role));
        for msg in [&r.task, &r.reason, &r.next].into_iter().chain(r.advisory.iter()) { check_msg(&rows, msg); }
    }
}

fn part(slot: u16, state: PartState) -> PartRuntime {
    PartRuntime { slot: PartSlot(slot), def: PartIdx(0), pos: content::cell::CellPos::new(0, 0, 0), rot: Default::default(), cells: vec![], centre: DVec3::ZERO, compartment: Some(CompartmentId(1)), health: 1.0, level: DamageLevel::Intact, links: Default::default(), priority: 3, state }
}

#[test]
fn csv_covers_every_key_and_follows_the_copy_rules() {
    let rows = parse(CSV);
    let keys = text_keys();
    assert_eq!(rows.len(), keys.len(), "CSV rows vs text_keys()");
    for k in &keys { assert!(rows.contains_key(*k), "{k} has no row"); }
    for (key, (_, en)) in &rows {
        assert!(key.starts_with("officer."));
        if key.starts_with("officer.radio.") {
            // Officers speak in two 40-character lines at most (copy rule 12); ours fit in one.
            assert!(rendered_len(en) <= 40, "{key}: radio line too long");
            lint(key, en, 80, GLOSSARY);
        } else {
            lint(key, en, 59, GLOSSARY);
        }
    }
}

#[test]
fn default_board_reads_cleanly() {
    let fx = Fx::new();
    let b = fx.board();
    check_board(&b);
    assert!(b.officers.iter().all(|r| r.holder == ControlHolder::Officer && r.advisory.is_none()));
    assert_eq!(b.officers[3].overrides, 2);
    assert_eq!(b.officers[2].reason.key.0, "sim.emcon.passive_no_trigger");
}

#[test]
fn gunnery_cites_the_reason_and_player_control_shows_advisory() {
    let mut fx = Fx::new();
    fx.stages.engage.mounts = vec![
        MountEngage { slot: PartSlot(2), status: EngageStatus::Holding { track: TrackId(5), reason: HoldReason::SuspectOutsideSelfDefence { range_m: 6_000.0, limit_m: 4_000.0 } }, ..Default::default() },
    ];
    let b = fx.board();
    check_board(&b);
    let g = &b.officers[3];
    assert_eq!(g.task, Msg::new("officer.gunnery.task.holding").arg(MsgArg::Track(TrackId(5))));
    assert_eq!(g.reason.key.0, "sim.hold.suspect_outside_self_defence");
    assert_eq!(g.next, Msg::new("officer.gunnery.next.in_range").num(4_000.0, Unit::Metres));
    fx.stages.engage.mounts.push(MountEngage { slot: PartSlot(3), status: EngageStatus::Engaging { track: TrackId(9), reason: FireReason::PointDefence }, ..Default::default() });
    fx.control.held = vec![ControlRole::Mount(PartSlot(2)), ControlRole::Helm];
    let b = fx.board();
    check_board(&b);
    assert_eq!(b.officers[3].task.key.0, "officer.gunnery.task.engaging");
    assert_eq!(b.officers[3].reason.key.0, "sim.fire.point_defence");
    assert_eq!(b.officers[3].holder, ControlHolder::Player);
    assert!(b.officers[3].advisory.is_some());
    assert_eq!(b.officers[0].holder, ControlHolder::Player);
    // Officers in command (player disconnected): every role shows Officer.
    fx.control.officers_in_command = true;
    assert!(fx.board().officers.iter().all(|r| r.holder == ControlHolder::Officer));
}

#[test]
fn radio_is_event_driven_and_rate_limited() {
    let mut fx = Fx::new();
    let b0 = fx.board();
    fx.previous = b0.clone();
    assert!(radio(&b0, &b0, fx.tick).is_empty());
    // Load shed starts: the Engineer radios once.
    fx.stages.engineer.shed = vec![PartSlot(4), PartSlot(5)];
    fx.tick += 6;
    let b1 = fx.board();
    let msgs = radio(&b0, &b1, fx.tick);
    assert_eq!(msgs, vec![(OfficerRole::Engineer, Msg::new("officer.radio.engineer.shedding").arg(MsgArg::Int(2)))]);
    check_msg(&rows(), &msgs[0].1);
    // Restored 1 s later: inside the gap, log only.
    fx.previous = b1.clone();
    fx.stages.engineer.shed.clear();
    fx.tick += 60;
    let b2 = fx.board();
    assert!(radio(&b1, &b2, fx.tick).is_empty());
    // Shed again after the gap: radios.
    fx.previous = b2.clone();
    fx.stages.engineer.shed = vec![PartSlot(4)];
    fx.tick += RADIO_GAP_S * 60;
    let b3 = fx.board();
    assert_eq!(radio(&b2, &b3, fx.tick).len(), 1);
    // Damage Control fire report and emitters on, both in one board.
    fx.previous = b3.clone();
    fx.stages.dc.plan = vec![(TeamOrder::FightFire(CompartmentId(4)), RepairPriority::OtherFires)];
    fx.parts.push(part(7, PartState::Sensor { bearing_rad: 0.0, swept_from_rad: 0.0, radiating: true, train_rad: 0.0 }));
    fx.tick += 6;
    let b4 = fx.board();
    assert_eq!(b4.radiating, vec![PartSlot(7)]);
    let msgs = radio(&b3, &b4, fx.tick);
    let roles: Vec<OfficerRole> = msgs.iter().map(|m| m.0).collect();
    assert_eq!(roles, vec![OfficerRole::Sensors, OfficerRole::DamageControl]);
    for (_, m) in &msgs { check_msg(&rows(), m); }
}

#[test]
fn why_chain_names_cause_and_node() {
    let mut graph = CompiledGraph::default();
    let node = |id: u32, stage: OfficerStage, part: Option<PartSlot>| crate::graph::CompiledNode { id: GraphNodeId(id), kind: NodeKind::Stage(stage), origin: crate::graph::NodeOrigin::Generated, officer: Some(stage.officer()), part, inputs: vec![], params: vec![], out_base: 0, out_count: 0, mem_slot: None, fallback: None };
    graph.nodes = vec![node(10, OfficerStage::MountAssignment, None), node(11, OfficerStage::BallisticSolver, Some(PartSlot(2))), node(12, OfficerStage::BallisticSolver, Some(PartSlot(3))), node(13, OfficerStage::LoadShedder, None)];
    let mut stages = StageStates::default();
    stages.engage.mounts = vec![MountEngage { slot: PartSlot(2), status: EngageStatus::Holding { track: TrackId(5), reason: HoldReason::TrainLimit { need_deg: 74.0, limit_deg: 70.0 } }, ..Default::default() }];
    let mut parts = vec![part(2, PartState::Inert), part(4, PartState::Inert)];
    parts[1].links.shed = true;
    let control = ControlState::default();
    let sinks = crate::graph::SinkWrites::default();
    let settings = OfficerSettings::default();
    let w = WhyIn { graph: &graph, sinks: &sinks, stages: &stages, control: &control, parts: &parts, tracks: &[], settings: &settings, function: None, own_pos: DVec3::ZERO, tick: 0, trigger_range_m: 20_000.0 };
    let rows = rows();
    let chain = why_chain(&w, PartSlot(2));
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].text, Msg::new("officer.why.holding").arg(MsgArg::Part(PartSlot(2))).arg(MsgArg::Track(TrackId(5))));
    assert_eq!(chain[0].node, Some(GraphNodeId(10)));
    assert_eq!(chain[1].text.key.0, "sim.hold.train_limit");
    assert_eq!(chain[1].node, Some(GraphNodeId(11)), "the solver bound to this mount");
    let shed = why_chain(&w, PartSlot(4));
    assert_eq!(shed[0].text.key.0, "officer.why.shed");
    assert_eq!(shed[0].node, Some(GraphNodeId(13)));
    let missing = why_chain(&w, PartSlot(99));
    assert_eq!(missing[0].text.key.0, "officer.why.no_part");
    for l in chain.iter().chain(&shed).chain(&missing) { check_msg(&rows, &l.text); }
}
