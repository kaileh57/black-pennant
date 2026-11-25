//! Unit `sys-graph-gen`. Auto-generation of the default ship control graph from placed parts.
//! DESIGN.md L25 (the editor generates the graph; one mechanism for novice and expert), L140 (a placed kit
//! carries its own graph fragment, so it already aims and fires), L303 (five officers, each a graph asset
//! built from the same nodes players use), L320 (regenerates whenever the design changes).
//!
//! Generated node ids are a pure function of (officer, stage, part slot) and stay below
//! `content::graph::USER_NODE_BASE`, so an edge from a pinned node to a generated node survives regeneration.
//! Generated nodes are never stored.
//!
//! # Id groups (bits 23..20 of a generated id)
//! - 0..=4: the officers in `OfficerRole` order (Helm, Engineer, Sensors, Gunnery, Damage Control). Codes:
//!   stages 0..=10 in `OfficerStage` order, sources 16 + `SourceKind` order, sinks 32 + `SinkKind` order,
//!   glue math nodes 48. The part field is slot + 1 (the compartment + 1 for flood valves), 0 = ship-wide.
//! - 5: L2 rule nodes (`compile`). 6: nodes of a placed kit with parts; the part field is the kit's first
//!   part slot + 1 and code and index hold the node's position in the fragment (up to 1,024 nodes).
//!   7: nodes of a kit without parts (an officer kit); the part field is the kit slot + 1. 8: instances of a
//!   per-part stage replacement template (`compile`).
//!
//! # Kit fragments
//! A kit's `KitDef::graph` is written against the kit's own part list: every part slot in a node kind
//! (`SourceKind::PartState`, `SourceKind::Detections`, part-bound `SinkKind`s) and the `part` parameter of a
//! stage node is an index into `KitDef::parts`, rebound to the placed slot through `KitPlacement::parts`. A
//! node bound to an index the placement does not have is dropped. A fragment edge may name a node outside the
//! fragment by its generated id written with the kit part index in the part field (for example the
//! generated `MountAssignment`, or the solver of the kit's own gun); the part field is rebound the same way.
//! A fragment stage or sink node that coincides with a generated node (same kind, same part) merges into it:
//! its parameters overlay the generated ones and its edges replace the generated edges into the ports they
//! drive, so a kit can retune or rewire its own mount without duplicating the officer.
use super::library::{self, SigFacts};
use crate::doctrine::MountInfo;
use crate::emcon::EmitterInfo;
use crate::ship::DesignFacts;
use content::blueprint::KitPlacement;
use content::graph::{ControlGraph, GraphEdge, GraphNode, NodeKind, OfficerStage, PortRef, SinkKind, SourceKind, Value, USER_NODE_BASE};
use content::ids::{CompartmentId, GraphNodeId, KitSlot, PartSlot};
use content::kit::KitDef;
use std::collections::{BTreeMap, BTreeSet};

/// Stable id of a generated node. `index` distinguishes several nodes of one stage and part (sources, sinks).
/// Real code: the id layout is contract. Layout: bits 23..20 officer, 19..14 stage or source or sink code,
/// 13..4 part slot + 1 (0 = ship-wide), 3..0 index.
pub fn generated_id(officer: u8, code: u8, part: Option<PartSlot>, index: u8) -> GraphNodeId {
    let p = part.map(|s| (s.0 as u32 + 1) & 0x3FF).unwrap_or(0);
    GraphNodeId(((officer as u32 & 0xF) << 20) | ((code as u32 & 0x3F) << 14) | (p << 4) | (index as u32 & 0xF))
}

/// Id group of rule nodes, assembly kits, partless kits and replacement templates.
pub const GROUP_RULE: u8 = 5;
pub const GROUP_KIT: u8 = 6;
pub const GROUP_KIT_PARTLESS: u8 = 7;
pub const GROUP_TEMPLATE: u8 = 8;
/// Highest part slot an id can encode (10-bit field holding slot + 1).
pub const MAX_ID_SLOT: u16 = 0x3FE;
/// Code of glue math nodes.
pub const CODE_GLUE: u8 = 48;

/// Officer group of a role.
pub fn officer_code(role: content::enums::OfficerRole) -> u8 {
    use content::enums::OfficerRole as R;
    match role { R::Helm => 0, R::Engineer => 1, R::Sensors => 2, R::Gunnery => 3, R::DamageControl => 4 }
}
/// Officer of a group, if the group is an officer's.
pub fn officer_of_code(code: u8) -> Option<content::enums::OfficerRole> {
    use content::enums::OfficerRole as R;
    match code { 0 => Some(R::Helm), 1 => Some(R::Engineer), 2 => Some(R::Sensors), 3 => Some(R::Gunnery), 4 => Some(R::DamageControl), _ => None }
}
pub fn stage_code(stage: OfficerStage) -> u8 { library::ALL_STAGES.iter().position(|s| *s == stage).unwrap_or(0) as u8 }
pub fn source_code(source: &SourceKind) -> u8 {
    16 + match source { SourceKind::OwnShip => 0, SourceKind::PartState(_) => 1, SourceKind::Detections(_) => 2, SourceKind::TrackList => 3, SourceKind::TheatreOrders => 4, SourceKind::Constant(_) => 5 }
}
pub fn sink_code(sink: SinkKind) -> u8 {
    32 + match sink {
        SinkKind::HelmHeading => 0, SinkKind::HelmSpeed => 1, SinkKind::MountAimPoint(_) => 2, SinkKind::MountFirePermission(_) => 3,
        SinkKind::MissileLaunch(_) => 4, SinkKind::MissileTarget(_) => 5, SinkKind::EmissionState(_) => 6, SinkKind::PowerPriority(_) => 7,
        SinkKind::Pump(_) => 8, SinkKind::Door(_) => 9, SinkKind::FloodValve(_) => 10, SinkKind::TrackStanceOverride => 11,
        SinkKind::TrackIdOverride => 12, SinkKind::ShipPreset => 13, SinkKind::ShipEmcon => 14,
    }
}
/// Part (or compartment, for flood valves) a sink is bound to, as it appears in the id's part field.
pub fn sink_part(sink: SinkKind) -> Option<PartSlot> {
    match sink {
        SinkKind::MountAimPoint(s) | SinkKind::MountFirePermission(s) | SinkKind::MissileLaunch(s) | SinkKind::MissileTarget(s)
        | SinkKind::EmissionState(s) | SinkKind::PowerPriority(s) | SinkKind::Pump(s) | SinkKind::Door(s) => Some(s),
        SinkKind::FloodValve(c) => Some(PartSlot(c.0)),
        _ => None,
    }
}
/// The generated id of a sink node.
pub fn sink_node(sink: SinkKind) -> GraphNodeId { generated_id(officer_code(library::sink_officer(sink)), sink_code(sink), sink_part(sink), 0) }

/// Group (bits 23..20), code (19..14), part field (13..4) and index (3..0) of an id.
pub fn id_fields(id: GraphNodeId) -> (u8, u8, u16, u8) { (((id.0 >> 20) & 0xF) as u8, ((id.0 >> 14) & 0x3F) as u8, ((id.0 >> 4) & 0x3FF) as u16, (id.0 & 0xF) as u8) }
/// Part slot of an id's part field (`None` for ship-wide).
pub fn id_part(id: GraphNodeId) -> Option<PartSlot> { let (_, _, p, _) = id_fields(id); if p == 0 { None } else { Some(PartSlot(p - 1)) } }
/// Same id with another part field.
pub fn with_part(id: GraphNodeId, part: Option<PartSlot>) -> GraphNodeId {
    let p = part.map(|s| (s.0 as u32 + 1) & 0x3FF).unwrap_or(0);
    GraphNodeId((id.0 & !(0x3FF << 4)) | (p << 4))
}
/// Id of node `index` of a fragment in a 10-bit position space (kits and templates).
pub fn fragment_id(group: u8, part_field: u16, index: usize) -> Option<GraphNodeId> {
    if index >= 1024 || part_field > 0x3FF { return None; }
    Some(GraphNodeId(((group as u32 & 0xF) << 20) | (((index as u32 >> 4) & 0x3F) << 14) | ((part_field as u32) << 4) | (index as u32 & 0xF)))
}

/// The part a node is bound to: from its kind, or for stage nodes from its id (generated) or its `part`
/// parameter (kit and player nodes).
pub fn node_part(node: &GraphNode) -> Option<PartSlot> {
    match &node.kind {
        NodeKind::Source(SourceKind::PartState(s)) | NodeKind::Source(SourceKind::Detections(s)) => Some(*s),
        NodeKind::Sink(k) => match k { SinkKind::FloodValve(_) => None, k => sink_part(*k) },
        NodeKind::Stage(st) if library::stage_is_per_part(*st) => {
            if let Some(Value::Scalar(x)) = node.params.get("part") { if x.is_finite() && *x >= 0.0 && *x <= MAX_ID_SLOT as f64 { return Some(PartSlot(*x as u16)); } }
            if node.id.0 < USER_NODE_BASE && officer_of_code(id_fields(node.id).0).is_some() { id_part(node.id) } else { None }
        }
        _ => None,
    }
}

/// One placed kit as the generator sees it.
pub(crate) struct KitFrag<'a> { pub slot: KitSlot, pub parts: &'a [PartSlot], pub def: &'a KitDef }

/// Everything the generator reads, decoupled from blueprint and prefab so the compiler and tests can build it.
pub(crate) struct GenInput<'a> {
    pub mounts: &'a [MountInfo],
    pub emitters: &'a [EmitterInfo],
    pub sensor_slots: &'a [PartSlot],
    pub consumer_slots: &'a [PartSlot],
    pub pumps: &'a [PartSlot],
    pub doors: &'a [PartSlot],
    pub kits: Vec<KitFrag<'a>>,
}
impl<'a> GenInput<'a> {
    pub fn sig_facts(&self) -> SigFacts<'a> { SigFacts { mounts: self.mounts, sensor_slots: self.sensor_slots, consumer_slots: self.consumer_slots, pumps: self.pumps, doors: self.doors } }
}

/// The kit definition a placement uses: the copy embedded in the blueprint, else the catalogue's.
pub(crate) fn kit_def<'a>(facts: &DesignFacts<'a>, placement: &KitPlacement) -> Option<&'a KitDef> {
    facts.blueprint.embedded_kits.get(&placement.kit).or_else(|| facts.catalogue.kit(&placement.kit))
}
/// Whether a placement's fragment is stock content (from the catalogue, not a player-authored embedded kit).
pub(crate) fn kit_is_stock(facts: &DesignFacts<'_>, placement: &KitPlacement) -> bool {
    match (facts.catalogue.kit(&placement.kit), facts.blueprint.embedded_kits.get(&placement.kit)) {
        (Some(c), Some(e)) => c == e,
        (Some(_), None) => true,
        _ => false,
    }
}

pub(crate) fn input_from<'a>(facts: &DesignFacts<'a>, keep_kit: &dyn Fn(&KitPlacement) -> bool) -> GenInput<'a> {
    let mut kits = Vec::new();
    for (i, placement) in facts.blueprint.kits.iter().enumerate() {
        if i > u16::MAX as usize || !keep_kit(placement) { continue; }
        if let Some(def) = kit_def(facts, placement) { kits.push(KitFrag { slot: KitSlot(i as u16), parts: &placement.parts, def }); }
    }
    GenInput { mounts: facts.mounts, emitters: facts.emitters, sensor_slots: facts.sensor_slots, consumer_slots: facts.consumer_slots, pumps: facts.pumps, doors: facts.doors, kits }
}

/// Generate the default graph for a design: one Helm chain (orders to autopilot, station keeping and evasion
/// to the heading and speed sinks), the Sensors chain (per-sensor detections to track fusion; emission
/// controller to per-emitter sinks), the Gunnery chain (track list to threat ranking to mount assignment, a
/// ballistic solver per gun mount and a guidance node per launcher, to aim, fire-permission and launch sinks),
/// the Engineer's load shedder to per-consumer priority sinks, and the Damage Control planner to pump, door
/// and valve sinks; then splice in each placed kit's fragment with its part indices rebound to slots.
/// Deterministic: same design, same graph. Unit: `sys-graph-gen`.
pub fn generate(facts: &DesignFacts<'_>) -> ControlGraph { generate_from(&input_from(facts, &|_| true)) }

/// The generated node that implements a stage (for one part where the stage is per part), used as the
/// fallback target of replacements, rules and scripts. Unit: `sys-graph-gen`.
pub fn stage_node(stage: OfficerStage, part: Option<PartSlot>) -> GraphNodeId {
    let part = if library::stage_is_per_part(stage) { part } else { None };
    generated_id(officer_code(stage.officer()), stage_code(stage), part, 0)
}

/// Source node ids of one officer.
fn source_node(role: content::enums::OfficerRole, source: &SourceKind, part: Option<PartSlot>) -> GraphNodeId { generated_id(officer_code(role), source_code(source), part, 0) }
fn glue(role: content::enums::OfficerRole, index: u8) -> GraphNodeId { generated_id(officer_code(role), CODE_GLUE, None, index) }

struct Builder { nodes: BTreeMap<GraphNodeId, GraphNode>, edges: BTreeMap<PortRef, PortRef> }
impl Builder {
    fn node(&mut self, id: GraphNodeId, kind: NodeKind, pos: [f32; 2]) { self.nodes.insert(id, GraphNode { id, kind, params: BTreeMap::new(), pos }); }
    /// Wire `from` into `to` (one edge per input port; the last one wins).
    fn edge(&mut self, from: (GraphNodeId, u8), to: (GraphNodeId, u8)) {
        self.edges.insert(PortRef { node: to.0, port: to.1 }, PortRef { node: from.0, port: from.1 });
    }
}

/// Layout position: one column per pipeline step, one band per officer.
fn pos(role: content::enums::OfficerRole, column: u8, row: usize) -> [f32; 2] { [column as f32 * 240.0, officer_code(role) as f32 * 900.0 + row as f32 * 90.0] }

fn slot_ok(s: PartSlot) -> bool { s.0 <= MAX_ID_SLOT }

pub(crate) fn generate_from(input: &GenInput<'_>) -> ControlGraph {
    use content::enums::OfficerRole as R;
    use content::graph::MathOp;
    let mut b = Builder { nodes: BTreeMap::new(), edges: BTreeMap::new() };

    // ---- Sensors: detections per sensor into track fusion; emission controller to emitters.
    let fusion = stage_node(OfficerStage::TrackFusion, None);
    b.node(fusion, NodeKind::Stage(OfficerStage::TrackFusion), pos(R::Sensors, 1, 0));
    for (i, slot) in input.sensor_slots.iter().enumerate().take(library::MAX_PORTS) {
        if !slot_ok(*slot) { continue; }
        let src = source_node(R::Sensors, &SourceKind::Detections(*slot), Some(*slot));
        b.node(src, NodeKind::Source(SourceKind::Detections(*slot)), pos(R::Sensors, 0, i));
        b.edge((src, 0), (fusion, i as u8));
    }
    let emcon = stage_node(OfficerStage::EmissionController, None);
    b.node(emcon, NodeKind::Stage(OfficerStage::EmissionController), pos(R::Sensors, 2, 0));
    b.edge((fusion, 0), (emcon, 0));
    for (i, e) in input.emitters.iter().enumerate() {
        if !slot_ok(e.slot) { continue; }
        let sink = SinkKind::EmissionState(e.slot);
        let id = sink_node(sink);
        b.node(id, NodeKind::Sink(sink), pos(R::Sensors, 5, i));
        b.edge((emcon, library::emission_class_port(e.kind, e.point_defence)), (id, 0));
    }

    // ---- Helm: orders to autopilot, station keeping and evasion; evasion overrides station keeping, which
    // overrides the autopilot.
    let orders_h = source_node(R::Helm, &SourceKind::TheatreOrders, None);
    b.node(orders_h, NodeKind::Source(SourceKind::TheatreOrders), pos(R::Helm, 0, 0));
    let auto = stage_node(OfficerStage::HelmAutopilot, None);
    let station = stage_node(OfficerStage::StationKeeping, None);
    let evasion = stage_node(OfficerStage::Evasion, None);
    b.node(auto, NodeKind::Stage(OfficerStage::HelmAutopilot), pos(R::Helm, 1, 0));
    b.node(station, NodeKind::Stage(OfficerStage::StationKeeping), pos(R::Helm, 1, 1));
    b.node(evasion, NodeKind::Stage(OfficerStage::Evasion), pos(R::Helm, 1, 2));
    for st in [auto, station, evasion] { b.edge((orders_h, 0), (st, 0)); }
    b.edge((fusion, 0), (evasion, 1));
    let heading = sink_node(SinkKind::HelmHeading);
    let speed = sink_node(SinkKind::HelmSpeed);
    b.node(heading, NodeKind::Sink(SinkKind::HelmHeading), pos(R::Helm, 5, 0));
    b.node(speed, NodeKind::Sink(SinkKind::HelmSpeed), pos(R::Helm, 5, 1));
    for (k, (port, sink)) in [(0u8, heading), (1u8, speed)].into_iter().enumerate() {
        let s1 = glue(R::Helm, 2 * k as u8);
        let s2 = glue(R::Helm, 2 * k as u8 + 1);
        b.node(s1, NodeKind::Math(MathOp::Select), pos(R::Helm, 3, 2 * k));
        b.node(s2, NodeKind::Math(MathOp::Select), pos(R::Helm, 4, 2 * k));
        b.edge((station, 2), (s1, 0));
        b.edge((station, port), (s1, 1));
        b.edge((auto, port), (s1, 2));
        b.edge((evasion, 2), (s2, 0));
        b.edge((evasion, port), (s2, 1));
        b.edge((s1, 0), (s2, 2));
        b.edge((s2, 0), (sink, 0));
    }

    // ---- Gunnery: ranking, assignment, one solver or guidance node per mount.
    let orders_g = source_node(R::Gunnery, &SourceKind::TheatreOrders, None);
    b.node(orders_g, NodeKind::Source(SourceKind::TheatreOrders), pos(R::Gunnery, 0, 0));
    let ranking = stage_node(OfficerStage::ThreatRanking, None);
    b.node(ranking, NodeKind::Stage(OfficerStage::ThreatRanking), pos(R::Gunnery, 1, 0));
    b.edge((fusion, 0), (ranking, 0));
    b.edge((orders_g, 0), (ranking, 1));
    if !input.mounts.is_empty() {
        let assign = stage_node(OfficerStage::MountAssignment, None);
        b.node(assign, NodeKind::Stage(OfficerStage::MountAssignment), pos(R::Gunnery, 2, 0));
        b.edge((ranking, 0), (assign, 0));
        b.edge((orders_g, 0), (assign, 1));
        for (i, m) in input.mounts.iter().enumerate() {
            let Some((tp, fp)) = library::assignment_ports(i) else { break };
            let Some(stage) = library::mount_stage(m.weapon) else { continue };
            if !slot_ok(m.slot) { continue; }
            let node = stage_node(stage, Some(m.slot));
            b.node(node, NodeKind::Stage(stage), pos(R::Gunnery, 3, i));
            b.edge((assign, tp), (node, 0));
            b.edge((assign, fp), (node, 1));
            match stage {
                OfficerStage::BallisticSolver => {
                    let aim = sink_node(SinkKind::MountAimPoint(m.slot));
                    let fire = sink_node(SinkKind::MountFirePermission(m.slot));
                    b.node(aim, NodeKind::Sink(SinkKind::MountAimPoint(m.slot)), pos(R::Gunnery, 5, 2 * i));
                    b.node(fire, NodeKind::Sink(SinkKind::MountFirePermission(m.slot)), pos(R::Gunnery, 5, 2 * i + 1));
                    b.edge((node, 0), (aim, 0));
                    b.edge((node, 1), (fire, 0));
                    b.edge((assign, tp), (fire, 1));
                }
                _ => {
                    let launch = sink_node(SinkKind::MissileLaunch(m.slot));
                    let target = sink_node(SinkKind::MissileTarget(m.slot));
                    b.node(launch, NodeKind::Sink(SinkKind::MissileLaunch(m.slot)), pos(R::Gunnery, 5, 2 * i));
                    b.node(target, NodeKind::Sink(SinkKind::MissileTarget(m.slot)), pos(R::Gunnery, 5, 2 * i + 1));
                    b.edge((node, 0), (launch, 0));
                    b.edge((node, 2), (launch, 1));
                    b.edge((node, 1), (target, 0));
                    b.edge((node, 3), (target, 1));
                }
            }
        }
    }

    // ---- Engineer: load shedder to every consumer's priority.
    if !input.consumer_slots.is_empty() {
        let shed = stage_node(OfficerStage::LoadShedder, None);
        b.node(shed, NodeKind::Stage(OfficerStage::LoadShedder), pos(R::Engineer, 1, 0));
        for (i, slot) in input.consumer_slots.iter().enumerate() {
            let Some(p) = library::shedder_port(i) else { break };
            if !slot_ok(*slot) { continue; }
            let sink = sink_node(SinkKind::PowerPriority(*slot));
            b.node(sink, NodeKind::Sink(SinkKind::PowerPriority(*slot)), pos(R::Engineer, 5, i));
            b.edge((shed, p), (sink, 0));
        }
    }

    // ---- Damage control: the planner drives pumps and doors (flood valves stay with the player, L719; a rule
    // can still drive one).
    let dc = stage_node(OfficerStage::DamageControlPlanner, None);
    b.node(dc, NodeKind::Stage(OfficerStage::DamageControlPlanner), pos(R::DamageControl, 1, 0));
    for (i, slot) in input.pumps.iter().enumerate() {
        let Some((on, comp)) = library::dc_pump_ports(i) else { break };
        if !slot_ok(*slot) { continue; }
        let sink = sink_node(SinkKind::Pump(*slot));
        b.node(sink, NodeKind::Sink(SinkKind::Pump(*slot)), pos(R::DamageControl, 5, i));
        b.edge((dc, on), (sink, 0));
        b.edge((dc, comp), (sink, 1));
    }
    for (i, slot) in input.doors.iter().enumerate() {
        let Some(p) = library::dc_door_port(input.pumps.len(), i) else { break };
        if !slot_ok(*slot) { continue; }
        let sink = sink_node(SinkKind::Door(*slot));
        b.node(sink, NodeKind::Sink(SinkKind::Door(*slot)), pos(R::DamageControl, 5, input.pumps.len() + i));
        b.edge((dc, p), (sink, 0));
    }

    for kit in &input.kits { splice_kit(&mut b, kit); }

    let nodes: Vec<GraphNode> = b.nodes.into_values().collect();
    let ids: BTreeSet<GraphNodeId> = nodes.iter().map(|n| n.id).collect();
    let mut edges: Vec<GraphEdge> = b.edges.into_iter().filter(|(to, from)| ids.contains(&to.node) && ids.contains(&from.node)).map(|(to, from)| GraphEdge { from, to }).collect();
    edges.sort();
    ControlGraph { nodes, edges }
}

/// Rebind a kit part index to its placed slot.
fn rebind(kit: &KitFrag<'_>, index: PartSlot) -> Option<PartSlot> { kit.parts.get(index.0 as usize).copied().filter(|s| slot_ok(*s)) }

fn rebind_kind(kit: &KitFrag<'_>, kind: &NodeKind) -> Option<NodeKind> {
    let r = |s: PartSlot| rebind(kit, s);
    Some(match kind {
        NodeKind::Source(SourceKind::PartState(s)) => NodeKind::Source(SourceKind::PartState(r(*s)?)),
        NodeKind::Source(SourceKind::Detections(s)) => NodeKind::Source(SourceKind::Detections(r(*s)?)),
        NodeKind::Sink(k) => NodeKind::Sink(match *k {
            SinkKind::MountAimPoint(s) => SinkKind::MountAimPoint(r(s)?),
            SinkKind::MountFirePermission(s) => SinkKind::MountFirePermission(r(s)?),
            SinkKind::MissileLaunch(s) => SinkKind::MissileLaunch(r(s)?),
            SinkKind::MissileTarget(s) => SinkKind::MissileTarget(r(s)?),
            SinkKind::EmissionState(s) => SinkKind::EmissionState(r(s)?),
            SinkKind::PowerPriority(s) => SinkKind::PowerPriority(r(s)?),
            SinkKind::Pump(s) => SinkKind::Pump(r(s)?),
            SinkKind::Door(s) => SinkKind::Door(r(s)?),
            other => other,
        }),
        other => other.clone(),
    })
}

/// Splice one placed kit's fragment into the graph (rules in the module docs).
fn splice_kit(b: &mut Builder, kit: &KitFrag<'_>) {
    let frag = &kit.def.graph;
    if frag.nodes.is_empty() { return; }
    let (group, part_field) = match kit.parts.first() {
        Some(first) if slot_ok(*first) => (GROUP_KIT, first.0 + 1),
        Some(_) => return,
        None if kit.slot.0 < MAX_ID_SLOT => (GROUP_KIT_PARTLESS, kit.slot.0 + 1),
        None => return,
    };
    // Map fragment-local ids to graph ids; merged nodes map onto the generated node.
    let mut map: BTreeMap<GraphNodeId, GraphNodeId> = BTreeMap::new();
    let mut merged: BTreeSet<GraphNodeId> = BTreeSet::new();
    for (k, n) in frag.nodes.iter().enumerate() {
        if map.contains_key(&n.id) { continue; }
        let Some(kind) = rebind_kind(kit, &n.kind) else { continue };
        let mut params = n.params.clone();
        if let NodeKind::Stage(st) = kind {
            if library::stage_is_per_part(st) {
                match params.get("part") {
                    Some(Value::Scalar(x)) if x.is_finite() && *x >= 0.0 && *x <= u16::MAX as f64 => {
                        let Some(slot) = rebind(kit, PartSlot(*x as u16)) else { continue };
                        params.insert("part".into(), Value::Scalar(slot.0 as f64));
                    }
                    _ => { params.remove("part"); }
                }
            }
        }
        let probe = GraphNode { id: GraphNodeId(USER_NODE_BASE), kind: kind.clone(), params: params.clone(), pos: n.pos };
        let twin = match &kind {
            NodeKind::Stage(st) => Some(stage_node(*st, node_part(&probe))),
            NodeKind::Sink(sk) => Some(sink_node(*sk)),
            _ => None,
        }.filter(|id| b.nodes.get(id).is_some_and(|g| g.kind == kind));
        if let Some(gid) = twin {
            let g = b.nodes.get_mut(&gid).expect("twin exists");
            for (key, v) in params { if key != "part" { g.params.insert(key, v); } }
            map.insert(n.id, gid);
            merged.insert(gid);
            continue;
        }
        let Some(id) = fragment_id(group, part_field, k) else { continue };
        b.nodes.insert(id, GraphNode { id, kind, params, pos: n.pos });
        map.insert(n.id, id);
    }
    let resolve = |id: GraphNodeId, map: &BTreeMap<GraphNodeId, GraphNodeId>| -> Option<GraphNodeId> {
        if let Some(m) = map.get(&id) { return Some(*m); }
        if id.0 >= USER_NODE_BASE { return None; }
        // A generated id written against kit part indices.
        let gid = match id_part(id) { Some(p) => with_part(id, Some(rebind(kit, p)?)), None => id };
        Some(gid)
    };
    for e in &frag.edges {
        let (Some(from), Some(to)) = (resolve(e.from.node, &map), resolve(e.to.node, &map)) else { continue };
        if !b.nodes.contains_key(&from) || !b.nodes.contains_key(&to) { continue; }
        b.edge((from, e.from.port), (to, e.to.port));
    }
    let _ = merged;
}

/// Compartment ids used by flood-valve sinks are below this many (for validation by the compiler).
pub(crate) fn compartment_ok(c: CompartmentId, count: usize) -> bool { (c.0 as usize) < count && c.0 <= MAX_ID_SLOT }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::emcon::EmitterInfo;
    use crate::signature::EmitterKind;
    use content::graph::{MountLimits, WeaponClass};
    use glam::DVec3;

    pub(crate) fn mount(slot: u16, weapon: WeaponClass) -> MountInfo {
        MountInfo {
            slot: PartSlot(slot), part: content::ids::PartIdx(0), weapon, munition: None, pos_local: DVec3::ZERO, zero_rad: 0.0,
            limits: MountLimits { train_min_deg: -150.0, train_max_deg: 150.0, elevation_min_deg: -5.0, elevation_max_deg: 70.0 },
            effective_range_m: 8_000.0, director: None, kit: None, group: 0, muzzle_mps: 900.0, point_defence: weapon == WeaponClass::PointDefenceGun,
        }
    }
    pub(crate) fn emitter(slot: u16, kind: EmitterKind, pd: bool) -> EmitterInfo { EmitterInfo { slot: PartSlot(slot), kind, r_ref_m: 20_000.0, height_m: 10.0, rotation_s: 2.5, point_defence: pd } }

    /// A small frigate: two guns, a missile launcher, a decoy launcher, a search and a fire-control radar.
    pub(crate) struct Design { pub mounts: Vec<MountInfo>, pub emitters: Vec<EmitterInfo>, pub sensors: Vec<PartSlot>, pub consumers: Vec<PartSlot>, pub pumps: Vec<PartSlot>, pub doors: Vec<PartSlot> }
    pub(crate) fn frigate() -> Design {
        Design {
            mounts: vec![mount(2, WeaponClass::Gun), mount(3, WeaponClass::Gun), mount(4, WeaponClass::AntiShipMissile), mount(5, WeaponClass::Decoy)],
            emitters: vec![emitter(6, EmitterKind::SearchRadar, false), emitter(7, EmitterKind::FireControlRadar, true)],
            sensors: vec![PartSlot(6), PartSlot(7), PartSlot(8)],
            consumers: vec![PartSlot(2), PartSlot(3), PartSlot(6)],
            pumps: vec![PartSlot(9)],
            doors: vec![PartSlot(10), PartSlot(11)],
        }
    }
    impl Design {
        pub(crate) fn input(&self) -> GenInput<'_> {
            GenInput { mounts: &self.mounts, emitters: &self.emitters, sensor_slots: &self.sensors, consumer_slots: &self.consumers, pumps: &self.pumps, doors: &self.doors, kits: Vec::new() }
        }
    }
    fn find(g: &ControlGraph, id: GraphNodeId) -> &GraphNode { g.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("no node {id:?}")) }
    fn edge_into(g: &ControlGraph, id: GraphNodeId, port: u8) -> Option<PortRef> { g.edges.iter().find(|e| e.to == PortRef { node: id, port }).map(|e| e.from) }

    #[test]
    fn default_graph_is_complete_and_stable() {
        let d = frigate();
        let g = generate_from(&d.input());
        assert_eq!(g, generate_from(&d.input()), "deterministic");
        assert!(g.nodes.windows(2).all(|w| w[0].id < w[1].id));
        assert!(g.nodes.iter().all(|n| n.id.0 < USER_NODE_BASE));
        // Gun 2: solver, aim and fire sinks wired from mount assignment port 0 / 1.
        let solver = stage_node(OfficerStage::BallisticSolver, Some(PartSlot(2)));
        assert_eq!(edge_into(&g, solver, 0), Some(PortRef { node: stage_node(OfficerStage::MountAssignment, None), port: 0 }));
        assert_eq!(edge_into(&g, sink_node(SinkKind::MountAimPoint(PartSlot(2))), 0), Some(PortRef { node: solver, port: 0 }));
        // Launcher 4 (third mount): guidance from ports 4 and 5; no node for the decoy launcher.
        let pn = stage_node(OfficerStage::PnGuidance, Some(PartSlot(4)));
        assert_eq!(edge_into(&g, pn, 1), Some(PortRef { node: stage_node(OfficerStage::MountAssignment, None), port: 5 }));
        assert!(!g.nodes.iter().any(|n| node_part(n) == Some(PartSlot(5)) && matches!(n.kind, NodeKind::Stage(_))));
        // The PD fire-control radar follows the point-defence class output.
        assert_eq!(edge_into(&g, sink_node(SinkKind::EmissionState(PartSlot(7))), 0).map(|p| p.port), Some(2));
        // Doors follow the pumps on the planner.
        assert_eq!(edge_into(&g, sink_node(SinkKind::Door(PartSlot(11))), 0).map(|p| p.port), Some(3));
        // Helm: evasion select over station select over autopilot.
        let head = edge_into(&g, sink_node(SinkKind::HelmHeading), 0).unwrap().node;
        assert_eq!(edge_into(&g, head, 0).unwrap().node, stage_node(OfficerStage::Evasion, None));
        // Every edge names an existing node and a port inside its signature.
        let d2 = frigate();
        let f = d2.input().sig_facts();
        for e in &g.edges {
            let from = library::signature(&find(&g, e.from.node).kind, &f);
            let to = library::signature(&find(&g, e.to.node).kind, &f);
            assert!((e.from.port as usize) < from.outputs.len() && (e.to.port as usize) < to.inputs.len(), "{e:?}");
        }
    }

    #[test]
    fn ids_follow_the_layout() {
        let id = stage_node(OfficerStage::BallisticSolver, Some(PartSlot(12)));
        assert_eq!(id_fields(id), (3, 6, 13, 0));
        assert_eq!(id_part(id), Some(PartSlot(12)));
        assert_eq!(with_part(id, Some(PartSlot(1))), stage_node(OfficerStage::BallisticSolver, Some(PartSlot(1))));
        assert_eq!(stage_node(OfficerStage::ThreatRanking, Some(PartSlot(3))), stage_node(OfficerStage::ThreatRanking, None));
        assert_eq!(fragment_id(GROUP_KIT, 5, 1023).map(|i| i.0 >> 20), Some(6));
        assert!(fragment_id(GROUP_KIT, 5, 1024).is_none());
    }

    fn kit_with(graph: ControlGraph) -> KitDef {
        KitDef {
            format: 1, id: content::ids::KitId::new("test.gun"), version: 1, hash: content::ids::ContentHash::default(), name: String::new(), name_key: None,
            description_key: None, author: None, licence: "CC-BY-4.0".into(), lineage: Vec::new(), kind: content::kit::KitKind::Assembly,
            parts: Vec::new(), sockets: Vec::new(), settings: Vec::new(), graph, officer: None, tags: Vec::new(), tray_order: 0,
        }
    }

    #[test]
    fn kit_fragment_rebinds_and_merges() {
        use content::graph::MathOp;
        let d = frigate();
        // The kit's part 0 is the gun, part 1 its magazine. It tunes its solver and gates fire on ammunition.
        let solver_local = GraphNodeId(100);
        let mut solver_params = BTreeMap::new();
        solver_params.insert("part".to_string(), Value::Scalar(0.0));
        solver_params.insert("gunnery.converge_m".to_string(), Value::Scalar(0.5));
        let frag = ControlGraph {
            nodes: vec![
                GraphNode { id: solver_local, kind: NodeKind::Stage(OfficerStage::BallisticSolver), params: solver_params, pos: [0.0, 0.0] },
                GraphNode { id: GraphNodeId(101), kind: NodeKind::Source(SourceKind::PartState(PartSlot(1))), params: BTreeMap::new(), pos: [0.0, 0.0] },
                GraphNode { id: GraphNodeId(102), kind: NodeKind::Math(MathOp::And), params: BTreeMap::new(), pos: [0.0, 0.0] },
                GraphNode { id: GraphNodeId(103), kind: NodeKind::Sink(SinkKind::MountFirePermission(PartSlot(0))), params: BTreeMap::new(), pos: [0.0, 0.0] },
                GraphNode { id: GraphNodeId(104), kind: NodeKind::Source(SourceKind::PartState(PartSlot(7))), params: BTreeMap::new(), pos: [0.0, 0.0] },
            ],
            edges: vec![
                GraphEdge { from: PortRef { node: solver_local, port: 1 }, to: PortRef { node: GraphNodeId(102), port: 0 } },
                GraphEdge { from: PortRef { node: GraphNodeId(101), port: 2 }, to: PortRef { node: GraphNodeId(102), port: 1 } },
                GraphEdge { from: PortRef { node: GraphNodeId(102), port: 0 }, to: PortRef { node: GraphNodeId(103), port: 0 } },
            ],
        };
        let def = kit_with(frag);
        let parts = [PartSlot(3), PartSlot(12)];
        let mut input = d.input();
        input.kits.push(KitFrag { slot: KitSlot(0), parts: &parts, def: &def });
        let g = generate_from(&input);
        let solver = stage_node(OfficerStage::BallisticSolver, Some(PartSlot(3)));
        assert_eq!(find(&g, solver).params.get("gunnery.converge_m"), Some(&Value::Scalar(0.5)), "kit params overlay the generated solver");
        let fire = sink_node(SinkKind::MountFirePermission(PartSlot(3)));
        let and = edge_into(&g, fire, 0).unwrap().node;
        assert_eq!(id_fields(and).0, GROUP_KIT);
        assert_eq!(edge_into(&g, and, 0), Some(PortRef { node: solver, port: 1 }));
        let mag = edge_into(&g, and, 1).unwrap().node;
        assert_eq!(find(&g, mag).kind, NodeKind::Source(SourceKind::PartState(PartSlot(12))));
        // The target port of the merged sink keeps its generated wiring.
        assert_eq!(edge_into(&g, fire, 1).map(|p| p.node), Some(stage_node(OfficerStage::MountAssignment, None)));
        // A node bound to a kit part the placement lacks is dropped.
        assert!(!g.nodes.iter().any(|n| n.kind == NodeKind::Source(SourceKind::PartState(PartSlot(7)))));
    }
}
