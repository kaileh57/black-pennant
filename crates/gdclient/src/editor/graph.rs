//! Unit `gd-editor-graph`. The L3 control graph model and L4 script sources on `BpEditor`.
//! DESIGN.md L279 (one ship control graph; defaults are nodes, not hidden code), L320 (the graph regenerates
//! when the design changes; edited nodes are pinned and survive; a pinned node that lost its input becomes a
//! Ship's Note), L331-336 (L3 with live values on edges; rules compile into visible nodes), L338-349 (node
//! library; any officer stage can be replaced by a player subgraph), L355-391 (L4 Luau: later phase, behind
//! the `luau` feature; sources arrive disabled, L2123), L1736 (GraphEdit with stock chrome off; this model
//! layer owns the connections), L112 (graphs are edited only in dry dock or during a splash).
//! Generation and compilation are `sim_systems::graph`; this unit exposes the generated graph merged with
//! the blueprint's `GraphOverrides` and edits only the overrides.
//!
//! # The view
//! The graph shown is the compiled graph of the design under edit (`graph::compile::to_view`), recompiled on
//! the main thread from the newest built design whenever only settings, rules or overrides changed since it
//! was built, so an edit shows at once. Rule nodes are shown as rules (not editable as nodes). A replaced
//! stage shows as the stock stage it stands for, and its subgraph's nodes and edges are shown as stored, with
//! edges from the stage node reading the stage's inputs and edges into it driving its outputs (the
//! compiler's boundary convention). A per-mount stage (`BallisticSolver`, `PnGuidance`) is replaced for every
//! mount at once through its ship-wide id, which the view shows as one extra boundary node.
//!
//! # Choices the spec leaves open
//! - "No cycle without a latch or filter": the compiler breaks every cycle, latches included, so the editor
//!   refuses every connection that closes a loop; feedback is built with the stateful nodes' own memory.
//! - A generated edge cannot be cut, only replaced: connecting another output to the port pins the node with
//!   the new edge (L320). Removing is for player nodes and player edges.
//! - Dragging a node is layout only: positions are kept beside the design and written into the stored nodes
//!   on save and on the next graph edit, so a drag never costs a revision. Generated nodes keep their
//!   positions for the session.
//! - Forking a stage starts its subgraph with one Select node reading the stage's first input: every output
//!   stays the stock stage's until the player drives it, so a fresh fork changes nothing (the compiler does
//!   not allow the stage itself inside its own replacement).
//! - Script nodes declare their ports in the source, one per line: `--@in name: type` and
//!   `--@out name: type`, with `type` one of `bool scalar angle vec3 text stance emcon track track_list
//!   detection_list part_state orders`.
use super::BpEditor;
use crate::convert::{d, empty_msg, msg_dict};
use crate::enums::GdEnum;
use crate::text::Lookup;
use content::graph::{ControlGraph, GraphEdge, GraphNode, GraphOverrides, MathOp, NodeKind, OfficerStage, PortRef, PortType, ScriptSource, SinkKind, SourceKind, StageReplacement, Value, USER_NODE_BASE};
use content::ids::{CompartmentId, GraphNodeId, PartSlot};
use godot::prelude::*;
use sim_systems::graph::{compile, generate, library, CompiledGraph, GraphIssue, NodeOrigin};
use sim_systems::protocol::{Msg, MsgArg};
use sim_systems::ship::ShipDesign;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

/// Text keys this unit emits (`game/i18n/dock.csv`).
pub(crate) const GRAPH_KEYS: &[&str] = &[
    "dock.undo.graph", "dock.undo.graph_replace", "dock.undo.graph_restore", "dock.undo.script",
    "dock.graph.title_part", "dock.graph.title_rule", "dock.graph.param.part", "dock.graph.param.compartment", "dock.graph.param.value",
    "dock.graph.lost_input", "dock.graph.lost_part", "dock.graph.type_mismatch", "dock.graph.not_allowed_node", "dock.graph.cycle",
    "dock.graph.rule_sink", "dock.graph.script_off", "dock.graph.layer",
    "dock.script.error", "dock.script.bad_port", "dock.script.untitled",
    // Node descriptions in the library (one per node kind).
    "dock.node.abs", "dock.node.add", "dock.node.alpha_beta", "dock.node.and", "dock.node.ballistic_solver", "dock.node.bearing_to", "dock.node.clamp", "dock.node.compare_eq", "dock.node.compare_ge", "dock.node.compare_gt", "dock.node.compare_le", "dock.node.compare_lt", "dock.node.compare_ne", "dock.node.constant", "dock.node.counter", "dock.node.damage_control_planner", "dock.node.detections", "dock.node.div", "dock.node.door", "dock.node.emission_controller", "dock.node.emission_state", "dock.node.evasion", "dock.node.flood_valve", "dock.node.helm_autopilot", "dock.node.helm_heading", "dock.node.helm_speed", "dock.node.latch", "dock.node.load_shedder", "dock.node.low_pass", "dock.node.max", "dock.node.min", "dock.node.missile_launch", "dock.node.missile_target", "dock.node.mount_aim_point", "dock.node.mount_assignment", "dock.node.mount_fire_permission", "dock.node.mul", "dock.node.neg", "dock.node.not", "dock.node.or", "dock.node.own_ship", "dock.node.part_state", "dock.node.pid", "dock.node.pn_guidance", "dock.node.power_priority", "dock.node.pump", "dock.node.range_to", "dock.node.script", "dock.node.select", "dock.node.ship_emcon", "dock.node.ship_preset", "dock.node.station_keeping", "dock.node.sub", "dock.node.theatre_orders", "dock.node.threat_ranking", "dock.node.timer", "dock.node.track_fusion", "dock.node.track_id_override", "dock.node.track_list", "dock.node.track_stance_override", "dock.node.vec_add", "dock.node.vec_cross", "dock.node.vec_dot", "dock.node.vec_length", "dock.node.vec_make", "dock.node.vec_normalize", "dock.node.vec_scale", "dock.node.vec_split", "dock.node.vec_sub",
];

/// The text key of a node kind's description in the library.
fn description_key(kind: &NodeKind) -> String { format!("dock.node.{}", library::node_key(kind).trim_start_matches("graph.node.")) }

/// Most nodes and edges the player may add (a hostile file is refused by validation; this bounds the editor).
const MAX_PLAYER_NODES: usize = 4_096;
/// Most script sources.
const MAX_SCRIPTS: usize = 64;
/// Most characters of a script name.
const MAX_SCRIPT_NAME: usize = 48;
/// Most bytes of a script source (the Luau host's own limit).
const MAX_SCRIPT_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------------------------------------
// Node types (pure).

/// The stable type id of a node kind (`math.add`, `sink.helm_speed`, `script:0`).
pub(crate) fn type_id(kind: &NodeKind) -> String {
    let short = library::node_key(kind).trim_start_matches("graph.node.");
    match kind {
        NodeKind::Source(_) => format!("source.{short}"),
        NodeKind::Math(_) => format!("math.{short}"),
        NodeKind::Stage(_) => format!("stage.{short}"),
        NodeKind::Sink(_) => format!("sink.{short}"),
        NodeKind::Script(i) => format!("script:{i}"),
    }
}
/// The node kind of a type id, with part-bound kinds bound to slot 0 or compartment 0.
pub(crate) fn kind_of_type(t: &str) -> Option<NodeKind> {
    let t = t.trim();
    if let Some(i) = t.strip_prefix("script:") { return i.parse::<u16>().ok().map(NodeKind::Script); }
    library::palette().into_iter().map(|(k, _)| k).find(|k| type_id(k) == t)
}
/// Palette group of a kind (`Bp.NODE_*`).
fn group_of(kind: &NodeKind) -> i64 { match kind { NodeKind::Source(_) => 0, NodeKind::Math(_) => 1, NodeKind::Stage(_) => 2, NodeKind::Sink(_) => 3, NodeKind::Script(_) => 4 } }
/// The part or compartment a kind is bound to.
fn binding_of(kind: &NodeKind) -> Option<(&'static str, u16)> {
    match kind {
        NodeKind::Source(SourceKind::PartState(p) | SourceKind::Detections(p)) => Some(("part", p.0)),
        NodeKind::Sink(SinkKind::FloodValve(c)) => Some(("compartment", c.0)),
        NodeKind::Sink(k) => generate::sink_part(*k).map(|p| ("part", p.0)),
        _ => None,
    }
}
/// The same kind bound to another part or compartment.
fn rebind(kind: &NodeKind, to: u16) -> NodeKind {
    let p = PartSlot(to);
    match kind {
        NodeKind::Source(SourceKind::PartState(_)) => NodeKind::Source(SourceKind::PartState(p)),
        NodeKind::Source(SourceKind::Detections(_)) => NodeKind::Source(SourceKind::Detections(p)),
        NodeKind::Sink(k) => NodeKind::Sink(match k {
            SinkKind::MountAimPoint(_) => SinkKind::MountAimPoint(p), SinkKind::MountFirePermission(_) => SinkKind::MountFirePermission(p),
            SinkKind::MissileLaunch(_) => SinkKind::MissileLaunch(p), SinkKind::MissileTarget(_) => SinkKind::MissileTarget(p),
            SinkKind::EmissionState(_) => SinkKind::EmissionState(p), SinkKind::PowerPriority(_) => SinkKind::PowerPriority(p),
            SinkKind::Pump(_) => SinkKind::Pump(p), SinkKind::Door(_) => SinkKind::Door(p), SinkKind::FloodValve(_) => SinkKind::FloodValve(CompartmentId(to)),
            other => *other,
        }),
        other => other.clone(),
    }
}

/// Named, typed ports of a node, in port order.
type Ports = Vec<(String, PortType)>;

/// Port types of a script source's declared ports.
const PORT_TYPE_NAMES: [(&str, PortType); 12] = [
    ("bool", PortType::Bool), ("scalar", PortType::Scalar), ("angle", PortType::Angle), ("vec3", PortType::Vec3), ("text", PortType::Text),
    ("stance", PortType::Stance), ("emcon", PortType::Emcon), ("track", PortType::Track), ("track_list", PortType::TrackList),
    ("detection_list", PortType::DetectionList), ("part_state", PortType::PartState), ("orders", PortType::Orders),
];
/// Most ports a script declares each way.
const MAX_SCRIPT_PORTS: usize = 16;

/// The ports a script source declares (`--@in name: type`, `--@out name: type`). `Err` is (line, word) of the
/// first declaration that names no port type.
pub(crate) fn script_ports(source: &str) -> Result<(Ports, Ports), (usize, String)> {
    let (mut ins, mut outs) = (Vec::new(), Vec::new());
    for (n, line) in source.lines().enumerate() {
        let l = line.trim();
        let (list, rest) = if let Some(r) = l.strip_prefix("--@in ") { (&mut ins, r) } else if let Some(r) = l.strip_prefix("--@out ") { (&mut outs, r) } else { continue };
        let Some((name, ty)) = rest.split_once(':') else { return Err((n + 1, rest.trim().chars().take(32).collect())) };
        let name: String = name.trim().chars().filter(|c| c.is_alphanumeric() || *c == '_').take(32).collect();
        let ty = ty.trim().to_ascii_lowercase();
        let Some((_, t)) = PORT_TYPE_NAMES.iter().find(|(k, _)| *k == ty) else { return Err((n + 1, ty.chars().take(32).collect())) };
        if name.is_empty() { return Err((n + 1, rest.trim().chars().take(32).collect())); }
        if list.len() < MAX_SCRIPT_PORTS && !list.iter().any(|(k, _): &(String, PortType)| *k == name) { list.push((name, *t)); }
    }
    Ok((ins, outs))
}

/// Whether adding `from -> to` to an edge list closes a loop.
pub(crate) fn closes_loop(edges: &[(GraphNodeId, GraphNodeId)], from: GraphNodeId, to: GraphNodeId) -> bool {
    if from == to { return true; }
    let mut adj: BTreeMap<GraphNodeId, Vec<GraphNodeId>> = BTreeMap::new();
    for (a, b) in edges { adj.entry(*a).or_default().push(*b); }
    let mut stack = vec![to];
    let mut seen = BTreeSet::new();
    while let Some(n) = stack.pop() {
        if n == from { return true; }
        if !seen.insert(n) { continue; }
        if let Some(next) = adj.get(&n) { stack.extend(next.iter().copied()); }
    }
    false
}

/// The first player node id not in use.
pub(crate) fn free_id(ov: &GraphOverrides) -> Option<GraphNodeId> {
    let used: BTreeSet<u32> = ov.pinned.nodes.iter().chain(ov.replaced.iter().flat_map(|r| r.subgraph.nodes.iter())).map(|n| n.id.0).filter(|i| *i >= USER_NODE_BASE).collect();
    if used.len() >= MAX_PLAYER_NODES { return None; }
    (USER_NODE_BASE..USER_NODE_BASE.saturating_add(MAX_PLAYER_NODES as u32 + 1)).find(|i| !used.contains(i)).map(GraphNodeId)
}

/// The boundary node a replacement's edges name: the ship-wide stage id.
pub(crate) fn boundary_id(stage: OfficerStage) -> GraphNodeId { generate::stage_node(stage, None) }

/// Rename script indices after source `removed` goes: nodes of it go, later ones shift down.
pub(crate) fn drop_script(ov: &mut GraphOverrides, removed: u16) {
    let fix = |g: &mut ControlGraph| {
        let gone: BTreeSet<GraphNodeId> = g.nodes.iter().filter(|n| n.kind == NodeKind::Script(removed)).map(|n| n.id).collect();
        g.nodes.retain(|n| !gone.contains(&n.id));
        g.edges.retain(|e| !gone.contains(&e.from.node) && !gone.contains(&e.to.node));
        for n in &mut g.nodes { if let NodeKind::Script(i) = &mut n.kind { if *i > removed { *i -= 1; } } }
    };
    fix(&mut ov.pinned);
    for r in &mut ov.replaced { fix(&mut r.subgraph); }
    if (removed as usize) < ov.scripts.len() { ov.scripts.remove(removed as usize); }
}

// ---------------------------------------------------------------------------------------------------------
// The view of the design under edit.

/// The graph the editor shows, for one revision.
pub(crate) struct Basis {
    /// The revision shown and the revision of the built design it was made from.
    rev: u64,
    design_rev: u64,
    design: Arc<ShipDesign>,
    /// The generator's graph for the design under edit (what `pin_node` checks against).
    generated: ControlGraph,
    compiled: Arc<CompiledGraph>,
    view: ControlGraph,
}

/// Graph unit state beside the design.
#[derive(Default)]
pub(crate) struct GraphCache {
    basis: std::cell::RefCell<Option<Rc<Basis>>>,
    /// Canvas positions moved since the last save, by node, for the design that began at `layout_for`.
    layout: BTreeMap<GraphNodeId, [f32; 2]>,
    layout_for: u64,
    /// The newest value of each scoped port during a splash (L216).
    live: BTreeMap<PortRef, Value>,
    /// Compile results of script sources, by source text.
    checks: std::cell::RefCell<BTreeMap<String, Option<String>>>,
}

/// Where a node lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Home { Generated, Pinned, Sub(usize), Boundary(usize), Rule }

fn home_of(ov: &GraphOverrides, b: &Basis, id: GraphNodeId) -> Option<Home> {
    if ov.pinned.nodes.iter().any(|n| n.id == id) { return Some(Home::Pinned); }
    if let Some(r) = ov.replaced.iter().position(|r| r.subgraph.nodes.iter().any(|n| n.id == id)) { return Some(Home::Sub(r)); }
    if let Some(r) = ov.replaced.iter().position(|r| boundary_id(r.stage) == id || r.subgraph.edges.iter().any(|e| (e.from.node == id || e.to.node == id) && id.0 < USER_NODE_BASE && matches!(b.generated.nodes.iter().find(|n| n.id == id).map(|n| &n.kind), Some(NodeKind::Stage(s)) if *s == r.stage))) {
        return Some(Home::Boundary(r));
    }
    if b.generated.nodes.iter().any(|n| n.id == id) { return Some(Home::Generated); }
    if b.compiled.nodes.iter().any(|n| n.id == id && matches!(n.origin, NodeOrigin::Rule(_))) { return Some(Home::Rule); }
    None
}

/// The stored node with this id, wherever it lives.
fn stored_node(ov: &GraphOverrides, id: GraphNodeId) -> Option<&GraphNode> {
    ov.pinned.nodes.iter().chain(ov.replaced.iter().flat_map(|r| r.subgraph.nodes.iter())).find(|n| n.id == id)
}

impl Basis {
    fn sig_facts(&self) -> library::SigFacts<'_> { library::SigFacts::of_design(&self.design) }
    /// Kind of any node the editor knows.
    fn kind(&self, ov: &GraphOverrides, id: GraphNodeId) -> Option<NodeKind> {
        if let Some(n) = stored_node(ov, id) { return Some(n.kind.clone()); }
        if let Some(r) = ov.replaced.iter().find(|r| boundary_id(r.stage) == id) { return Some(NodeKind::Stage(r.stage)); }
        self.view.nodes.iter().find(|n| n.id == id).or_else(|| self.generated.nodes.iter().find(|n| n.id == id)).map(|n| n.kind.clone())
    }
    fn ports(&self, kind: &NodeKind, scripts: &[ScriptSource]) -> (Ports, Ports) {
        if let NodeKind::Script(i) = kind {
            return scripts.get(*i as usize).map_or((Vec::new(), Vec::new()), |s| (s.inputs.clone(), s.outputs.clone()));
        }
        let sig = library::signature(kind, &self.sig_facts());
        (sig.inputs.iter().map(|p| (p.name.to_string(), p.ty)).collect(), sig.outputs.iter().map(|p| (p.name.to_string(), p.ty)).collect())
    }
}

/// Edges the editor shows, as plain (from, to) pairs and in full.
fn shown_edges(ov: &GraphOverrides, b: &Basis) -> Vec<(GraphEdge, bool)> {
    let hidden = hidden_nodes(b);
    let player: BTreeSet<GraphEdge> = ov.pinned.edges.iter().copied().collect();
    let mut out: Vec<(GraphEdge, bool)> = b.view.edges.iter().filter(|e| !hidden.contains(&e.from.node) && !hidden.contains(&e.to.node)).map(|e| (*e, !player.contains(e))).collect();
    for r in &ov.replaced { out.extend(r.subgraph.edges.iter().map(|e| (*e, false))); }
    out.sort_by_key(|(e, _)| *e);
    out.dedup_by_key(|(e, _)| *e);
    out
}

/// Compiled nodes that the view replaces by their stored form: instances of replacement subgraphs.
fn hidden_nodes(b: &Basis) -> BTreeSet<GraphNodeId> {
    b.compiled.nodes.iter().filter(|n| matches!(n.origin, NodeOrigin::Replacement(_)) && !compile::is_proxy(n)).map(|n| n.id).collect()
}

/// The overrides after joining two ports, or `None` when the join is not allowed.
fn connected(ov: &GraphOverrides, b: &Basis, from: (GraphNodeId, u8), to: (GraphNodeId, u8)) -> Option<GraphOverrides> {
    let (fh, th) = (home_of(ov, b, from.0)?, home_of(ov, b, to.0)?);
    if fh == Home::Rule || th == Home::Rule { return None; }
    // Port types: a boundary reads the stage's inputs and drives its outputs.
    let fk = b.kind(ov, from.0)?;
    let tk = b.kind(ov, to.0)?;
    let (f_in, f_out) = b.ports(&fk, &ov.scripts);
    let (t_in, t_out) = b.ports(&tk, &ov.scripts);
    let from_ty = if matches!(fh, Home::Boundary(_)) { f_in.get(from.1 as usize)?.1 } else { f_out.get(from.1 as usize)?.1 };
    let to_ty = if matches!(th, Home::Boundary(_)) { t_out.get(to.1 as usize)?.1 } else { t_in.get(to.1 as usize)?.1 };
    let generic = (tk == NodeKind::Math(MathOp::Select) && to.1 > 0) || fk == NodeKind::Math(MathOp::Select);
    if !generic && !library::compatible(from_ty, to_ty) { return None; }
    // Sources are read, never written; sinks are written, never read.
    if matches!(tk, NodeKind::Source(_)) && !matches!(th, Home::Boundary(_)) { return None; }
    let edge = GraphEdge { from: PortRef { node: from.0, port: from.1 }, to: PortRef { node: to.0, port: to.1 } };
    let mut next = ov.clone();
    let container = match (fh, th) {
        (_, Home::Sub(r) | Home::Boundary(r)) => Some(r),
        (Home::Sub(r), Home::Pinned) if is_free(ov, to.0) => Some(r),
        (Home::Sub(_) | Home::Boundary(_), _) => return None,
        _ => None,
    };
    match container {
        Some(r) => {
            // A free player node joins the subgraph it is wired into.
            for id in [from.0, to.0] {
                if is_free(&next, id) {
                    let Some(i) = next.pinned.nodes.iter().position(|n| n.id == id) else { continue };
                    let n = next.pinned.nodes.remove(i);
                    next.replaced[r].subgraph.nodes.push(n);
                }
            }
            if let (Home::Sub(a), Home::Sub(c)) = (home_of(&next, b, from.0).unwrap_or(Home::Rule), home_of(&next, b, to.0).unwrap_or(Home::Rule)) { if a != c { return None; } }
            if wire_loops(&next, b, edge) { return None; }
            let sub = &mut next.replaced[r].subgraph;
            sub.edges.retain(|e| e.to != edge.to);
            sub.edges.push(edge);
            sub.edges.sort();
            Some(next)
        }
        None => {
            // The main graph: pin the destination with the new edge (L320).
            let node = stored_node(ov, to.0).cloned().or_else(|| b.generated.nodes.iter().find(|n| n.id == to.0).cloned())?;
            let issues = compile::pin_node(&mut next, &b.generated, node, &[edge]);
            issues.is_empty().then_some(next)
        }
    }
}

/// Whether adding `edge` (into a replacement subgraph) closes a loop. A stage boundary is two nodes: edges
/// from it read the stage's inputs (nothing upstream inside the fork), edges into it drive the stage's
/// outputs, which reach the stage's consumers in the main graph.
fn wire_loops(ov: &GraphOverrides, b: &Basis, edge: GraphEdge) -> bool {
    let input_side = |r: usize| GraphNodeId(u32::MAX - r as u32);
    let outputs = |r: usize| -> Vec<GraphNodeId> {
        let stage = ov.replaced[r].stage;
        let mut v: Vec<GraphNodeId> = b.view.nodes.iter().filter(|n| n.kind == NodeKind::Stage(stage) && n.id.0 < USER_NODE_BASE).map(|n| n.id).collect();
        v.push(boundary_id(stage));
        v
    };
    let boundary_of = |id: GraphNodeId| ov.replaced.iter().position(|r| boundary_id(r.stage) == id || b.view.nodes.iter().any(|n| n.id == id && n.kind == NodeKind::Stage(r.stage) && id.0 < USER_NODE_BASE));
    let hidden = hidden_nodes(b);
    let mut pairs: Vec<(GraphNodeId, GraphNodeId)> = b.view.edges.iter().filter(|e| !hidden.contains(&e.from.node) && !hidden.contains(&e.to.node)).map(|e| (e.from.node, e.to.node)).collect();
    let add = |e: &GraphEdge, pairs: &mut Vec<(GraphNodeId, GraphNodeId)>| {
        let from = match boundary_of(e.from.node) { Some(r) if ov.replaced[r].subgraph.edges.contains(e) || *e == edge => input_side(r), _ => e.from.node };
        match boundary_of(e.to.node) {
            Some(r) => for o in outputs(r) { pairs.push((from, o)); },
            None => pairs.push((from, e.to.node)),
        }
    };
    for r in &ov.replaced { for e in r.subgraph.edges.iter().filter(|e| e.to != edge.to) { add(e, &mut pairs); } }
    let mut new_pairs = Vec::new();
    add(&edge, &mut new_pairs);
    new_pairs.iter().any(|(f, t)| closes_loop(&pairs, *f, *t))
}

/// A player node with no edge at all, waiting in `pinned` to be wired.
fn is_free(ov: &GraphOverrides, id: GraphNodeId) -> bool {
    id.0 >= USER_NODE_BASE && ov.pinned.nodes.iter().any(|n| n.id == id) && !ov.pinned.edges.iter().any(|e| e.from.node == id || e.to.node == id)
}

impl BpEditor {
    /// The view of the design under edit, cached per revision. `None` until the worker has built a design.
    pub(crate) fn graph_basis(&self) -> Option<Rc<Basis>> {
        let m = self.model.as_ref()?;
        let (design_rev, design) = m.design.clone()?;
        // A newer build of the same revision (the worker caught up) replaces the view.
        if let Some(b) = self.state.graph.basis.borrow().as_ref() { if b.rev == m.rev && b.design_rev == design_rev { return Some(b.clone()); } }
        let cat = crate::services::services().catalogue();
        let built = design.blueprint();
        let same = built.parts == m.blueprint.parts && built.kits == m.blueprint.kits && built.embedded_kits == m.blueprint.embedded_kits;
        // The generator reads parts and kits; when those changed since the build, the built design stands.
        let bp = if same { &m.blueprint } else { built };
        let facts = sim_systems::ship::DesignFacts { blueprint: bp, prefab: &design.prefab, catalogue: &cat, mounts: &design.mounts, emitters: &design.emitters, sensor_slots: &design.sensor_slots, consumer_slots: &design.consumer_slots, decoys: &design.decoys, pumps: &design.pumps, doors: &design.doors };
        let generated = generate::generate(&facts);
        // Settings, rules and overrides of the design under edit, compiled under the dock's ruleset.
        let compiled = if same { Arc::new(compile::compile_with(&facts, &bp.officers, &bp.graph, &bp.rules, &m.context.ruleset)) } else { design.graph.clone() };
        let view = compile::to_view(&compiled);
        let b = Rc::new(Basis { rev: m.rev, design_rev, design, generated, compiled, view });
        *self.state.graph.basis.borrow_mut() = Some(b.clone());
        Some(b)
    }

    /// Whether the generated graph of the design has a sink node (`None` before the first build).
    pub(crate) fn generated_has_sink(&self, sink: SinkKind) -> Option<bool> {
        let b = self.graph_basis()?;
        let id = generate::sink_node(sink);
        Some(b.generated.nodes.iter().any(|n| n.id == id))
    }
    /// Every sink the generated graph has, ascending (`None` before the first build).
    pub(crate) fn generated_sinks(&self) -> Option<Vec<SinkKind>> {
        let b = self.graph_basis()?;
        let mut v: Vec<SinkKind> = b.generated.nodes.iter().filter_map(|n| if let NodeKind::Sink(k) = n.kind { Some(k) } else { None }).collect();
        v.sort();
        v.dedup();
        Some(v)
    }
    /// Per-officer layers of the compiled graph, when built.
    pub(crate) fn graph_layers(&self) -> Option<BTreeMap<content::enums::OfficerRole, content::enums::ControlLayer>> { self.graph_basis().map(|b| b.compiled.layer_by_role.clone()) }

    /// Write moved canvas positions into the stored nodes of `bp`. True when any moved.
    pub(crate) fn fold_graph_layout(&self, bp: &mut content::blueprint::Blueprint) -> bool {
        if self.state.graph.layout_for != self.state.first_rev || self.state.graph.layout.is_empty() { return false; }
        let mut changed = false;
        let layout = &self.state.graph.layout;
        let mut place = |n: &mut GraphNode| if let Some(p) = layout.get(&n.id) { if n.pos != *p { n.pos = *p; changed = true; } };
        for n in &mut bp.graph.pinned.nodes { place(n); }
        for r in &mut bp.graph.replaced { for n in &mut r.subgraph.nodes { place(n); } }
        changed
    }
    /// Positions of stored nodes are in the design now.
    pub(crate) fn graph_layout_saved(&mut self) {
        let Some(m) = self.model.as_ref() else { return };
        let stored: BTreeSet<GraphNodeId> = m.blueprint.graph.pinned.nodes.iter().chain(m.blueprint.graph.replaced.iter().flat_map(|r| r.subgraph.nodes.iter())).map(|n| n.id).collect();
        self.state.graph.layout.retain(|id, _| !stored.contains(id));
    }
    /// Whether a moved position is not in the design yet.
    pub(crate) fn graph_layout_pending(&self) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let mut bp = m.blueprint.clone();
        self.fold_graph_layout(&mut bp)
    }

    /// Per-frame: collect scoped port values during a splash (L216).
    pub(crate) fn graph_frame(&mut self) {
        if !self.in_splash {
            if !self.state.graph.live.is_empty() { self.state.graph.live.clear(); }
            return;
        }
        let services = crate::services::services();
        let feed = services.feed.borrow();
        for ev in &feed.events {
            if let sim_systems::protocol::event::SimEvent::QueryReply { reply: sim_systems::protocol::event::QueryReply::Scope { port, value }, .. } = &ev.event {
                if self.state.graph.live.len() < 512 || self.state.graph.live.contains_key(port) { self.state.graph.live.insert(*port, value.clone()); }
            }
        }
    }

    /// Commit new overrides as one undo step, with moved positions folded in.
    fn commit_graph(&mut self, next: GraphOverrides, label: &str) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let mut bp = m.blueprint.clone();
        bp.graph = next;
        let folded = self.fold_graph_layout(&mut bp);
        let changed = self.commit(bp, label);
        if changed {
            if folded { self.graph_layout_saved(); }
            self.base_mut().emit_signal("graph_changed", &[]);
        }
        changed
    }
    fn overrides(&self) -> Option<&GraphOverrides> { self.model.as_ref().map(|m| &m.blueprint.graph) }
    /// Graph edits are allowed: the dock is open and the ruleset allows L3.
    fn graph_editable(&self) -> bool { self.model.as_ref().is_some_and(|m| super::max_layer(&m.context.ruleset) >= content::enums::ControlLayer::L3Graph) }

    fn node_title(&self, kind: &NodeKind, params: &BTreeMap<String, Value>, origin: Option<NodeOrigin>, lk: &dyn Lookup) -> Msg {
        let m = self.model.as_ref();
        let cat = crate::services::services().catalogue();
        let key = library::node_key(kind);
        if let Some(NodeOrigin::Rule(r)) = origin { return Msg::new("dock.graph.title_rule").arg(MsgArg::Int(r as i64 + 1)).arg(MsgArg::Key(content::ids::TextKey::new(key))); }
        if let NodeKind::Script(i) = kind {
            let name = m.and_then(|m| m.blueprint.graph.scripts.get(*i as usize)).map(|s| s.name.clone()).unwrap_or_default();
            return Msg::new("dock.name.plain").arg(MsgArg::Name(name));
        }
        let part = match kind {
            NodeKind::Stage(_) => match params.get("part") { Some(Value::Scalar(p)) if p.is_finite() && *p >= 0.0 && *p < u16::MAX as f64 => Some(PartSlot(*p as u16)), _ => None },
            NodeKind::Sink(SinkKind::FloodValve(c)) => return Msg::new("dock.graph.title_part").arg(MsgArg::Key(content::ids::TextKey::new(key))).arg(MsgArg::Compartment(*c)),
            k => binding_of(k).map(|(_, s)| PartSlot(s)),
        };
        match (part, m) {
            (Some(p), Some(m)) if p.index() < m.blueprint.parts.len() => Msg::new("dock.graph.title_part").arg(MsgArg::Key(content::ids::TextKey::new(key))).arg(MsgArg::Name(super::control::part_name_text(lk, &m.blueprint, p, &cat))),
            _ => Msg::new(key),
        }
    }

    fn ports_array(ports: &[(String, PortType)]) -> VarArray {
        ports.iter().map(|(name, ty)| d! { "name" => StringName::from(library::port_key(name).unwrap_or(name.as_str())), "type" => ty.to_gd() }.to_variant()).collect()
    }

    fn params_dict(kind: &NodeKind, params: &BTreeMap<String, Value>) -> VarDictionary {
        let mut out = VarDictionary::new();
        for spec in library::params_of(kind) {
            let v = match params.get(spec.key) { Some(Value::Scalar(x)) if x.is_finite() => *x, Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 }, _ => spec.default };
            out.set(spec.key, v);
        }
        if let Some((key, v)) = binding_of(kind) { out.set(key, v as i64); }
        if let NodeKind::Source(SourceKind::Constant(v)) = kind { out.set("value", &crate::convert::value_dict(v)); }
        if let Some(Value::Scalar(p)) = params.get("part") { if matches!(kind, NodeKind::Stage(_)) && p.is_finite() { out.set("part", *p as i64); } }
        if let Some(Value::Scalar(r)) = params.get("rule") { if r.is_finite() { out.set("rule", *r as i64); } }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn node_dict(&self, b: &Basis, ov: &GraphOverrides, n: &GraphNode, origin: Option<NodeOrigin>, generated: bool, replaced: Option<OfficerStage>, officer: Option<content::enums::OfficerRole>, broken: &BTreeSet<GraphNodeId>, lk: &dyn Lookup) -> VarDictionary {
        let (ins, outs) = b.ports(&n.kind, &ov.scripts);
        let pos = if self.state.graph.layout_for == self.state.first_rev { self.state.graph.layout.get(&n.id).copied() } else { None }.unwrap_or(n.pos);
        let pinned = ov.pinned.nodes.iter().any(|p| p.id == n.id);
        d! {
            "id" => n.id.0 as i64,
            "type" => GString::from(type_id(&n.kind).as_str()),
            "group" => group_of(&n.kind),
            "title" => msg_dict(&self.node_title(&n.kind, &n.params, origin, lk)),
            "pos" => Vector2::new(if pos[0].is_finite() { pos[0] } else { 0.0 }, if pos[1].is_finite() { pos[1] } else { 0.0 }),
            "generated" => generated,
            "pinned" => pinned,
            "replaced_stage" => replaced.map_or(-1, |s| s.to_gd()),
            "officer" => officer.map_or(-1, |o| o.to_gd()),
            "params" => Self::params_dict(&n.kind, &n.params),
            "broken" => broken.contains(&n.id),
            "inputs" => Self::ports_array(&ins),
            "outputs" => Self::ports_array(&outs),
        }
    }

    /// Every node the editor shows, ascending id.
    fn all_node_dicts(&self) -> Vec<(Option<content::enums::OfficerRole>, i64, VarDictionary)> {
        let (Some(b), Some(ov)) = (self.graph_basis(), self.overrides()) else { return Vec::new() };
        let lk = crate::text::EngineLookup::new();
        let hidden = hidden_nodes(&b);
        let broken: BTreeSet<GraphNodeId> = b.compiled.issues.iter().filter_map(|i| if let GraphIssue::PinnedLostInput { node, .. } = i { Some(*node) } else { None }).collect();
        let compiled: BTreeMap<GraphNodeId, &sim_systems::graph::CompiledNode> = b.compiled.nodes.iter().filter(|n| !compile::is_shadow(n.id)).map(|n| (n.id, n)).collect();
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for n in &b.view.nodes {
            if hidden.contains(&n.id) { continue; }
            let c = compiled.get(&n.id);
            let origin = c.map(|c| c.origin);
            let replaced = match origin { Some(NodeOrigin::Replacement(s)) => Some(s), _ => None };
            let generated = matches!(origin, Some(NodeOrigin::Generated | NodeOrigin::Kit(_) | NodeOrigin::Rule(_) | NodeOrigin::Replacement(_))) || n.id.0 < USER_NODE_BASE;
            let officer = c.and_then(|c| c.officer);
            seen.insert(n.id);
            // A stored node keeps the place the player gave it; the view lays out generated nodes itself.
            let placed;
            let n = match stored_node(ov, n.id) { Some(s) => { placed = GraphNode { pos: s.pos, ..n.clone() }; &placed } None => n };
            out.push((officer, n.id.0 as i64, self.node_dict(&b, ov, n, origin, generated, replaced, officer, &broken, &lk)));
        }
        for r in &ov.replaced {
            let wide = boundary_id(r.stage);
            if !seen.contains(&wide) && r.subgraph.edges.iter().any(|e| e.from.node == wide || e.to.node == wide) {
                let mut params = BTreeMap::new();
                for spec in library::params_of(&NodeKind::Stage(r.stage)) { params.insert(spec.key.to_string(), Value::Scalar(spec.default)); }
                let n = GraphNode { id: wide, kind: NodeKind::Stage(r.stage), params, pos: [0.0, 0.0] };
                seen.insert(wide);
                out.push((Some(r.stage.officer()), wide.0 as i64, self.node_dict(&b, ov, &n, Some(NodeOrigin::Replacement(r.stage)), true, Some(r.stage), Some(r.stage.officer()), &broken, &lk)));
            }
            for n in &r.subgraph.nodes {
                if !seen.insert(n.id) { continue; }
                let origin = if let NodeKind::Script(i) = n.kind { NodeOrigin::Script(i) } else { NodeOrigin::Replacement(r.stage) };
                out.push((Some(r.stage.officer()), n.id.0 as i64, self.node_dict(&b, ov, n, Some(origin), false, Some(r.stage), Some(r.stage.officer()), &broken, &lk)));
            }
        }
        // Player nodes the compiler left out (a loop, a bad binding) still show, marked broken.
        for n in &ov.pinned.nodes {
            if !seen.insert(n.id) { continue; }
            let mut broken = broken.clone();
            broken.insert(n.id);
            out.push((None, n.id.0 as i64, self.node_dict(&b, ov, n, Some(NodeOrigin::Pinned), n.id.0 < USER_NODE_BASE, None, None, &broken, &lk)));
        }
        out.sort_by_key(|(_, id, _)| *id);
        out
    }

    /// The node and its home for an edit, or `None`.
    fn editable(&self, id: i64) -> Option<(Rc<Basis>, GraphOverrides, GraphNodeId, Home)> {
        if !self.graph_editable() { return None; }
        let b = self.graph_basis()?;
        let ov = self.overrides()?.clone();
        let id = GraphNodeId(u32::try_from(id).ok()?);
        let h = home_of(&ov, &b, id)?;
        Some((b, ov, id, h))
    }

    /// An issue as a sentence naming its node.
    fn graph_issue_dict(&self, b: &Basis, ov: &GraphOverrides, issue: &GraphIssue, lk: &dyn Lookup) -> VarDictionary {
        let name = |id: GraphNodeId| -> String {
            let real = GraphNodeId(id.0 & !compile::SHADOW_BIT);
            match b.kind(ov, real) {
                Some(k) => { let params = stored_node(ov, real).map(|n| n.params.clone()).unwrap_or_default(); crate::text::msg_text(lk, &self.node_title(&k, &params, None, lk)) }
                None => format!("{}", id.0 & 0xFFFF),
            }
        };
        let port_name = |id: GraphNodeId, port: u8, input: bool| -> String {
            let Some(k) = b.kind(ov, id) else { return (port as u32 + 1).to_string() };
            let (ins, outs) = b.ports(&k, &ov.scripts);
            let list = if input { ins } else { outs };
            list.get(port as usize).map(|(n, _)| library::port_key(n).map(|k| lk.tr(k)).unwrap_or_else(|| n.clone())).unwrap_or_else(|| (port as u32 + 1).to_string())
        };
        let whole = compile::WHOLE_NODE;
        let (msg, node) = match issue {
            GraphIssue::PinnedLostInput { node, port } if *port == whole => (Msg::new("dock.graph.lost_part").arg(MsgArg::Name(name(*node))), Some(*node)),
            GraphIssue::PinnedLostInput { node, port } => (Msg::new("dock.graph.lost_input").arg(MsgArg::Name(name(*node))).arg(MsgArg::Name(port_name(*node, *port, true))), Some(*node)),
            GraphIssue::TypeMismatch { node, port } if *port == whole => (Msg::new("dock.graph.not_allowed_node").arg(MsgArg::Name(name(*node))), Some(*node)),
            GraphIssue::TypeMismatch { node, port } => (Msg::new("dock.graph.type_mismatch").arg(MsgArg::Name(name(*node))).arg(MsgArg::Name(port_name(*node, *port, true))), Some(*node)),
            GraphIssue::Cycle { node } => (Msg::new("dock.graph.cycle").arg(MsgArg::Name(name(*node))), Some(*node)),
            GraphIssue::RuleSinkMissing { rule } => (Msg::new("dock.graph.rule_sink").arg(MsgArg::Int(*rule as i64 + 1)), None),
            GraphIssue::TooManyRules(n) => return crate::convert::issue_dict(&content::validate::Issue::TooManyRules(*n)),
            GraphIssue::ScriptDisabled { script } => (Msg::new("dock.graph.script_off").arg(MsgArg::Name(ov.scripts.get(*script as usize).map(|s| s.name.clone()).unwrap_or_default())), None),
            GraphIssue::LayerNotAllowed { node } => (Msg::new("dock.graph.layer").arg(MsgArg::Name(name(*node))), Some(*node)),
        };
        let part = node.and_then(|n| b.kind(ov, GraphNodeId(n.0 & !compile::SHADOW_BIT))).and_then(|k| binding_of(&k).filter(|(k, _)| *k == "part").map(|(_, s)| s as i64)).unwrap_or(-1);
        d! { "text" => msg_dict(&msg), "fatal" => false, "part" => part, "cell" => Variant::nil() }
    }

    /// A script source as `ScriptDict`.
    fn script_dict(&self, index: usize, s: &ScriptSource) -> VarDictionary {
        let error = match script_ports(&s.source) {
            Err((line, word)) => msg_dict(&Msg::new("dock.script.bad_port").arg(MsgArg::Int(line as i64)).arg(MsgArg::Name(word))),
            Ok(_) => match self.script_check(&s.source) { Some(e) => msg_dict(&Msg::new("dock.script.error").arg(MsgArg::Name(e))), None => empty_msg() },
        };
        d! {
            "index" => index as i64, "name" => GString::from(s.name.as_str()), "source" => GString::from(s.source.as_str()), "enabled" => s.enabled,
            "inputs" => Self::ports_array(&s.inputs), "outputs" => Self::ports_array(&s.outputs), "error" => error,
        }
    }
    /// Luau's compile check of a source, cached by text; `None` = clean.
    fn script_check(&self, source: &str) -> Option<String> {
        if let Some(r) = self.state.graph.checks.borrow().get(source) { return r.clone(); }
        #[cfg(feature = "luau")]
        let r = sim_systems::script::luau::check_source(source).err().map(|e| e.chars().take(400).collect::<String>());
        #[cfg(not(feature = "luau"))]
        let r: Option<String> = None;
        let mut checks = self.state.graph.checks.borrow_mut();
        if checks.len() > 256 { checks.clear(); }
        checks.insert(source.to_string(), r.clone());
        r
    }
    /// Scripts may be written in this build and dock.
    fn scripts_allowed(&self) -> bool { cfg!(feature = "luau") && self.model.as_ref().is_some_and(|m| super::max_layer(&m.context.ruleset) >= content::enums::ControlLayer::L4Script) }
}

#[godot_api(secondary)]
impl BpEditor {
    /// Nodes of the compiled graph for one officer (`Bp.OFFICER_*`) or all (`Bp.NONE`):
    /// `Array[GraphNodeDict]`, ascending id. Generated nodes carry `generated: true`.
    #[func]
    fn graph_nodes(&self, officer: i64) -> Array<VarDictionary> {
        let want = if officer < 0 { None } else { match content::enums::OfficerRole::from_gd(officer) { Some(r) => Some(r), None => return Array::new() } };
        let mut out = Array::new();
        for (o, _, dict) in self.all_node_dicts() {
            if want.is_none_or(|w| o == Some(w)) { out.push(&dict); }
        }
        out
    }
    /// Every connection: `Array[GraphEdgeDict]`.
    #[func]
    fn graph_edges(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let (Some(b), Some(ov)) = (self.graph_basis(), self.overrides()) else { return out };
        for (e, generated) in shown_edges(ov, &b) {
            out.push(&d! { "from_node" => e.from.node.0 as i64, "from_port" => e.from.port as i64, "to_node" => e.to.node.0 as i64, "to_port" => e.to.port as i64, "generated" => generated });
        }
        out
    }
    /// The node library (L338-349): `Array[NodeTypeDict]`, grouped sources, maths and logic, officer stages,
    /// sinks, then script nodes when L4 is in the build.
    #[func]
    fn node_library(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let basis = self.graph_basis();
        let none = library::SigFacts { mounts: &[], sensor_slots: &[], consumer_slots: &[], pumps: &[], doors: &[] };
        let facts = basis.as_ref().map(|b| library::SigFacts::of_design(&b.design));
        let facts = facts.as_ref().unwrap_or(&none);
        let mut kinds: Vec<(NodeKind, &'static str)> = library::palette();
        kinds.sort_by_key(|(k, _)| group_of(k));
        let graph_ok = self.graph_editable();
        for (kind, key) in kinds {
            let sig = library::signature(&kind, facts);
            let mut params: VarArray = sig.params.iter().map(|p| d! { "key" => GString::from(p.key), "label" => StringName::from(p.label), "type" => StringName::from("scalar"), "default" => p.default }.to_variant()).collect();
            if let Some((k, _)) = binding_of(&kind) { params.push(&d! { "key" => GString::from(k), "label" => StringName::from(format!("dock.graph.param.{k}").as_str()), "type" => StringName::from(k), "default" => 0i64 }.to_variant()); }
            if let NodeKind::Source(SourceKind::Constant(v)) = &kind { params.push(&d! { "key" => GString::from("value"), "label" => StringName::from("dock.graph.param.value"), "type" => StringName::from("value"), "default" => crate::convert::value_dict(v) }.to_variant()); }
            let ins: Vec<(String, PortType)> = sig.inputs.iter().map(|p| (p.name.to_string(), p.ty)).collect();
            let outs: Vec<(String, PortType)> = sig.outputs.iter().map(|p| (p.name.to_string(), p.ty)).collect();
            out.push(&d! {
                "type" => GString::from(type_id(&kind).as_str()), "group" => group_of(&kind), "name" => StringName::from(key),
                "description" => StringName::from(description_key(&kind).as_str()), "inputs" => Self::ports_array(&ins), "outputs" => Self::ports_array(&outs),
                "params" => params, "writable" => !matches!(kind, NodeKind::Source(_)), "phase_ok" => graph_ok,
            });
        }
        if cfg!(feature = "luau") {
            let ok = self.scripts_allowed();
            for (i, s) in self.overrides().map(|o| o.scripts.clone()).unwrap_or_default().iter().enumerate() {
                let kind = NodeKind::Script(i as u16);
                out.push(&d! {
                    "type" => GString::from(type_id(&kind).as_str()), "group" => group_of(&kind), "name" => StringName::from(s.name.as_str()),
                    "description" => StringName::from("dock.node.script"), "inputs" => Self::ports_array(&s.inputs), "outputs" => Self::ports_array(&s.outputs),
                    "params" => VarArray::new(), "writable" => true, "phase_ok" => ok,
                });
            }
        }
        out
    }
    /// Add a node of a library type at a canvas position. Returns its id or `Bp.NONE`.
    #[func]
    fn graph_add_node(&mut self, node_type: GString, pos: Vector2) -> i64 {
        if !self.graph_editable() { return -1; }
        let Some(kind) = kind_of_type(&node_type.to_string()) else { return -1 };
        let (Some(b), Some(ov)) = (self.graph_basis(), self.overrides().cloned()) else { return -1 };
        if let NodeKind::Script(i) = kind { if !self.scripts_allowed() || ov.scripts.get(i as usize).is_none() { return -1; } }
        let Some(id) = free_id(&ov) else { return -1 };
        let p = [if pos.x.is_finite() { pos.x.clamp(-1e6, 1e6) } else { 0.0 }, if pos.y.is_finite() { pos.y.clamp(-1e6, 1e6) } else { 0.0 }];
        // A part-bound kind takes the first part it can bind to.
        let candidates: Vec<NodeKind> = match binding_of(&kind) {
            None => vec![kind.clone()],
            Some(("compartment", _)) => (0..b.design.prefab.compartments.compartments.len().min(u16::MAX as usize) as u16).map(|c| rebind(&kind, c)).collect(),
            Some(_) => {
                let slots: Vec<u16> = match &kind {
                    NodeKind::Source(SourceKind::Detections(_)) => b.design.sensor_slots.iter().map(|s| s.0).collect(),
                    NodeKind::Sink(_) => {
                        // A player sink only where the generator made none (the compiler refuses a twin).
                        let cat = crate::services::services().catalogue();
                        let Some(m) = self.model.as_ref() else { return -1 };
                        (0..m.blueprint.parts.len().min(u16::MAX as usize) as u16).filter(|s| matches!(rebind(&kind, *s), NodeKind::Sink(k) if super::control::sink_plausible(k, &m.blueprint, &cat))).collect()
                    }
                    _ => (0..self.model.as_ref().map_or(0, |m| m.blueprint.parts.len()).min(u16::MAX as usize) as u16).collect(),
                };
                slots.into_iter().map(|s| rebind(&kind, s)).collect()
            }
        };
        for k in candidates.into_iter().take(1_024) {
            let mut next = ov.clone();
            let node = GraphNode { id, kind: k, params: BTreeMap::new(), pos: p };
            if compile::pin_node(&mut next, &b.generated, node, &[]).is_empty() {
                if !self.commit_graph(next, "dock.undo.graph") { return -1; }
                self.state.graph.layout_for = self.state.first_rev;
                return id.0 as i64;
            }
        }
        -1
    }
    /// Remove a player-added node. Generated nodes cannot be removed, only replaced or bypassed.
    #[func]
    fn graph_remove_node(&mut self, node: i64) -> bool {
        let Some((_, mut ov, id, home)) = self.editable(node) else { return false };
        if id.0 < USER_NODE_BASE { return false; }
        match home {
            Home::Pinned => {
                ov.pinned.nodes.retain(|n| n.id != id);
                ov.pinned.edges.retain(|e| e.from.node != id && e.to.node != id);
            }
            Home::Sub(r) => {
                let g = &mut ov.replaced[r].subgraph;
                g.nodes.retain(|n| n.id != id);
                g.edges.retain(|e| e.from.node != id && e.to.node != id);
            }
            _ => return false,
        }
        // Edges elsewhere that read the node go with it.
        for r in &mut ov.replaced { r.subgraph.edges.retain(|e| e.from.node != id); }
        self.state.graph.layout.remove(&id);
        self.commit_graph(ov, "dock.undo.graph")
    }
    /// Canvas position only; no undo step.
    #[func]
    fn graph_move_node(&mut self, node: i64, pos: Vector2) {
        if self.model.is_none() || !(pos.x.is_finite() && pos.y.is_finite()) { return; }
        let Ok(id) = u32::try_from(node) else { return };
        if self.state.graph.layout_for != self.state.first_rev {
            self.state.graph.layout.clear();
            self.state.graph.layout_for = self.state.first_rev;
        }
        if self.state.graph.layout.len() >= 4 * MAX_PLAYER_NODES && !self.state.graph.layout.contains_key(&GraphNodeId(id)) { return; }
        self.state.graph.layout.insert(GraphNodeId(id), [pos.x.clamp(-1e6, 1e6), pos.y.clamp(-1e6, 1e6)]);
    }
    /// True when the two ports may be joined (types match, no cycle without a latch or filter).
    #[func]
    fn graph_can_connect(&self, from_node: i64, from_port: i64, to_node: i64, to_port: i64) -> bool {
        if !self.graph_editable() { return false; }
        let (Some(b), Some(ov)) = (self.graph_basis(), self.overrides()) else { return false };
        let (Ok(f), Ok(fp), Ok(t), Ok(tp)) = (u32::try_from(from_node), u8::try_from(from_port), u32::try_from(to_node), u8::try_from(to_port)) else { return false };
        connected(ov, &b, (GraphNodeId(f), fp), (GraphNodeId(t), tp)).is_some()
    }
    /// Join two ports; an input port holds one connection, so this replaces any existing one. Connecting to a
    /// generated node pins it.
    #[func]
    fn graph_connect(&mut self, from_node: i64, from_port: i64, to_node: i64, to_port: i64) -> bool {
        if !self.graph_editable() { return false; }
        let (Some(b), Some(ov)) = (self.graph_basis(), self.overrides().cloned()) else { return false };
        let (Ok(f), Ok(fp), Ok(t), Ok(tp)) = (u32::try_from(from_node), u8::try_from(from_port), u32::try_from(to_node), u8::try_from(to_port)) else { return false };
        let Some(next) = connected(&ov, &b, (GraphNodeId(f), fp), (GraphNodeId(t), tp)) else { return false };
        if next == ov { return true; }
        self.commit_graph(next, "dock.undo.graph")
    }
    #[func]
    fn graph_disconnect(&mut self, from_node: i64, from_port: i64, to_node: i64, to_port: i64) -> bool {
        if !self.graph_editable() { return false; }
        let Some(mut ov) = self.overrides().cloned() else { return false };
        let (Ok(f), Ok(fp), Ok(t), Ok(tp)) = (u32::try_from(from_node), u8::try_from(from_port), u32::try_from(to_node), u8::try_from(to_port)) else { return false };
        let edge = GraphEdge { from: PortRef { node: GraphNodeId(f), port: fp }, to: PortRef { node: GraphNodeId(t), port: tp } };
        let before = ov.pinned.edges.len() + ov.replaced.iter().map(|r| r.subgraph.edges.len()).sum::<usize>();
        ov.pinned.edges.retain(|e| *e != edge);
        for r in &mut ov.replaced { r.subgraph.edges.retain(|e| *e != edge); }
        let after = ov.pinned.edges.len() + ov.replaced.iter().map(|r| r.subgraph.edges.len()).sum::<usize>();
        if after == before { return false; }
        self.commit_graph(ov, "dock.undo.graph")
    }
    /// Set a node parameter (`NodeTypeDict.params[].key`). Editing a generated node pins it.
    #[func]
    fn graph_set_param(&mut self, node: i64, key: GString, value: Variant) -> bool {
        let Some((b, mut ov, id, home)) = self.editable(node) else { return false };
        let key = key.to_string();
        // The node as it would be stored.
        let mut n = match home {
            Home::Pinned | Home::Sub(_) => match stored_node(&ov, id) { Some(n) => n.clone(), None => return false },
            Home::Generated => match b.generated.nodes.iter().find(|n| n.id == id) { Some(n) => n.clone(), None => return false },
            Home::Boundary(_) | Home::Rule => return false,
        };
        // A replaced stage keeps its stock parameters: tune the subgraph instead.
        if ov.replaced.iter().any(|r| matches!(n.kind, NodeKind::Stage(s) if s == r.stage)) { return false; }
        let number = crate::convert::variant_f64(&value).or_else(|| value.try_to::<bool>().ok().map(|b| if b { 1.0 } else { 0.0 }));
        if let Some(spec) = library::params_of(&n.kind).into_iter().find(|p| p.key == key) {
            let Some(x) = number else { return false };
            let x = if spec.min <= spec.max { x.clamp(spec.min, spec.max) } else { x };
            n.params.insert(key, Value::Scalar(x));
        } else if key == "value" {
            let NodeKind::Source(SourceKind::Constant(_)) = n.kind else { return false };
            let v = match value.try_to::<VarDictionary>() {
                Ok(dict) => match crate::convert::value_from_dict(&dict) { Some(v) => v, None => return false },
                Err(_) => match value.try_to::<bool>() { Ok(b) if value.get_type() == VariantType::BOOL => Value::Bool(b), _ => match number { Some(x) => Value::Scalar(x), None => return false } },
            };
            n.kind = NodeKind::Source(SourceKind::Constant(v));
        } else if let Some((bkey, _)) = binding_of(&n.kind).filter(|(k, _)| *k == key) {
            // Only player nodes change what they are bound to.
            if id.0 < USER_NODE_BASE { return false; }
            let Some(x) = number.filter(|x| x.fract() == 0.0 && *x >= 0.0 && *x < u16::MAX as f64) else { return false };
            let limit = if bkey == "compartment" { b.design.prefab.compartments.compartments.len() } else { self.model.as_ref().map_or(0, |m| m.blueprint.parts.len()) };
            if x as usize >= limit { return false; }
            n.kind = rebind(&n.kind, x as u16);
        } else {
            return false;
        }
        match home {
            Home::Generated | Home::Pinned => {
                let edges: Vec<GraphEdge> = ov.pinned.edges.iter().copied().filter(|e| e.from.node == id || e.to.node == id).collect();
                if !compile::pin_node(&mut ov, &b.generated, n, &edges).is_empty() { return false; }
            }
            Home::Sub(r) => {
                if let Some(s) = ov.replaced[r].subgraph.nodes.iter_mut().find(|s| s.id == id) { *s = n; }
            }
            _ => return false,
        }
        self.commit_graph(ov, "dock.undo.graph")
    }
    /// Pin or unpin a node. Unpinning returns it to its generated form on the next regeneration.
    #[func]
    fn graph_pin(&mut self, node: i64, pinned: bool) -> bool {
        let Some((b, mut ov, id, home)) = self.editable(node) else { return false };
        match (home, pinned) {
            (Home::Pinned, true) => true,
            (Home::Generated, true) => {
                let Some(n) = b.generated.nodes.iter().find(|n| n.id == id).cloned() else { return false };
                if ov.replaced.iter().any(|r| matches!(n.kind, NodeKind::Stage(s) if s == r.stage)) { return false; }
                if !compile::pin_node(&mut ov, &b.generated, n, &[]).is_empty() { return false; }
                self.commit_graph(ov, "dock.undo.graph")
            }
            (Home::Pinned, false) if id.0 < USER_NODE_BASE => {
                if !compile::unpin_node(&mut ov, id) { return false; }
                self.state.graph.layout.remove(&id);
                self.commit_graph(ov, "dock.undo.graph")
            }
            (Home::Generated, false) => true,
            _ => false,
        }
    }
    /// Replace one officer stage (`Bp.STAGE_*`) with an editable subgraph that starts as a copy of the stock
    /// stage (forking). Returns the id of the subgraph's first node or `Bp.NONE`.
    #[func]
    fn graph_replace_stage(&mut self, stage: i64) -> i64 {
        if !self.graph_editable() { return -1; }
        let Some(stage) = OfficerStage::from_gd(stage) else { return -1 };
        let (Some(b), Some(mut ov)) = (self.graph_basis(), self.overrides().cloned()) else { return -1 };
        if let Some(r) = ov.replaced.iter().find(|r| r.stage == stage) { return r.subgraph.nodes.iter().map(|n| n.id.0 as i64).min().unwrap_or(-1); }
        // The design must run the stage (no guns, no ballistic solver).
        if !b.generated.nodes.iter().any(|n| n.kind == NodeKind::Stage(stage) && n.id.0 < USER_NODE_BASE) { return -1; }
        // A stage pinned as a node cannot also be replaced.
        if ov.pinned.nodes.iter().any(|n| n.kind == NodeKind::Stage(stage) && n.id.0 < USER_NODE_BASE) { return -1; }
        let Some(anchor) = free_id(&ov) else { return -1 };
        let wide = boundary_id(stage);
        let at = b.view.nodes.iter().find(|n| n.kind == NodeKind::Stage(stage)).map_or([0.0, 0.0], |n| [n.pos[0] - 200.0, n.pos[1] + 120.0]);
        let mut sub = ControlGraph::default();
        sub.nodes.push(GraphNode { id: anchor, kind: NodeKind::Math(MathOp::Select), params: BTreeMap::new(), pos: at });
        sub.edges.push(GraphEdge { from: PortRef { node: wide, port: 0 }, to: PortRef { node: anchor, port: 2 } });
        ov.replaced.push(StageReplacement { stage, subgraph: sub });
        if !self.commit_graph(ov, "dock.undo.graph_replace") { return -1; }
        anchor.0 as i64
    }
    /// Drop the replacement and return to the stock stage.
    #[func]
    fn graph_restore_stage(&mut self, stage: i64) -> bool {
        if self.model.is_none() { return false; }
        let Some(stage) = OfficerStage::from_gd(stage) else { return false };
        let Some(mut ov) = self.overrides().cloned() else { return false };
        let before = ov.replaced.len();
        ov.replaced.retain(|r| r.stage != stage);
        if ov.replaced.len() == before { return false; }
        self.commit_graph(ov, "dock.undo.graph_restore")
    }
    /// Problems in the overrides: `Array[IssueDict]` (type mismatch, cycle, pinned node without input, layer
    /// not allowed by the ruleset).
    #[func]
    fn graph_issues(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(m) = self.model.as_ref() else { return out };
        let ov = &m.blueprint.graph;
        let lk = crate::text::EngineLookup::new();
        let mut seen = BTreeSet::new();
        let mut push = |dict: VarDictionary, out: &mut Array<VarDictionary>| {
            let key = format!("{:?}", dict.get("text"));
            if seen.insert(key) { out.push(&dict); }
        };
        // Structural problems of what is stored.
        for i in content::validate::validate_graph(ov, &m.blueprint.rules) { push(crate::convert::issue_dict(&i), &mut out); }
        let max = super::max_layer(&m.context.ruleset);
        let edits = !ov.pinned.nodes.is_empty() || !ov.pinned.edges.is_empty() || !ov.replaced.is_empty();
        if edits && max < content::enums::ControlLayer::L3Graph { push(crate::convert::issue_dict(&content::validate::Issue::LayerNotAllowed), &mut out); }
        if !ov.scripts.is_empty() && !m.context.ruleset.luau { push(crate::convert::issue_dict(&content::validate::Issue::ScriptsNotAllowed), &mut out); }
        if let Some(b) = self.graph_basis() {
            for i in &b.compiled.issues { push(self.graph_issue_dict(&b, ov, i, &lk), &mut out); }
        }
        out
    }
    /// During a splash: the latest value on each scoped port as `{ "<node>:<port>": String }` (already
    /// formatted with units), for printing on edges. Empty in dry dock.
    #[func]
    fn graph_live_values(&self) -> VarDictionary {
        let mut out = VarDictionary::new();
        if !self.in_splash || self.state.graph.live.is_empty() { return out; }
        let lk = crate::text::EngineLookup::new();
        let basis = self.graph_basis();
        let ov = self.overrides();
        for (port, value) in &self.state.graph.live {
            // Angles travel in radians; show them as bearings.
            let ty = basis.as_ref().zip(ov).and_then(|(b, ov)| b.kind(ov, port.node).map(|k| b.ports(&k, &ov.scripts).1)).and_then(|outs| outs.get(port.port as usize).map(|(n, t)| (n.clone(), *t)));
            let text = match (value, &ty) {
                (Value::Scalar(r), Some((_, PortType::Angle))) if r.is_finite() => lk.digits(&crate::text::bearing_text(r.to_degrees().rem_euclid(360.0))),
                (v, Some((name, _))) => {
                    let unit = if name.ends_with("_kn") { sim_systems::protocol::Unit::Knots } else if name.ends_with("_m") { sim_systems::protocol::Unit::Metres } else if name.ends_with("_s") { sim_systems::protocol::Unit::Seconds } else if name.ends_with("_kw") { sim_systems::protocol::Unit::Kilowatts } else { sim_systems::protocol::Unit::None };
                    crate::convert::value_text_with(&lk, v, unit)
                }
                (v, None) => crate::convert::value_text_with(&lk, v, sim_systems::protocol::Unit::None),
            };
            out.set(format!("{}:{}", port.node.0, port.port).as_str(), text.as_str());
        }
        out
    }

    // ---- L4 scripts (later phase: unit `gd-editor-graph` schedules this part last) ----

    /// Script sources in the design: `Array[ScriptDict]`. Empty when the build has no Luau.
    #[func]
    fn scripts(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        if !cfg!(feature = "luau") { return out; }
        let Some(ov) = self.overrides() else { return out };
        for (i, s) in ov.scripts.iter().enumerate() { out.push(&self.script_dict(i, s)); }
        out
    }
    /// Add an empty script node source. Returns its index or `Bp.NONE`.
    #[func]
    fn script_add(&mut self, name: GString) -> i64 {
        if !self.scripts_allowed() { return -1; }
        let Some(mut ov) = self.overrides().cloned() else { return -1 };
        if ov.scripts.len() >= MAX_SCRIPTS { return -1; }
        let mut name: String = super::clean_name(&name.to_string()).chars().take(MAX_SCRIPT_NAME).collect();
        if name.is_empty() { name = godot::classes::TranslationServer::singleton().translate("dock.script.untitled").to_string(); }
        // The player's own new script is theirs: it starts enabled (L2123 covers imported ones).
        ov.scripts.push(ScriptSource { name, source: String::new(), inputs: Vec::new(), outputs: Vec::new(), enabled: true });
        let index = ov.scripts.len() - 1;
        if !self.commit_graph(ov, "dock.undo.script") { return -1; }
        index as i64
    }
    /// Store new source text and compile-check it. Returns the updated `ScriptDict` (`error` empty = clean).
    #[func]
    fn script_set_source(&mut self, index: i64, source: GString) -> VarDictionary {
        if !cfg!(feature = "luau") { return VarDictionary::new(); }
        let Some(mut ov) = self.overrides().cloned() else { return VarDictionary::new() };
        let Some(i) = usize::try_from(index).ok().filter(|i| *i < ov.scripts.len()) else { return VarDictionary::new() };
        let mut text = source.to_string();
        if text.len() > MAX_SCRIPT_BYTES {
            let mut cut = MAX_SCRIPT_BYTES;
            while !text.is_char_boundary(cut) { cut -= 1; }
            text.truncate(cut);
        }
        let s = &mut ov.scripts[i];
        s.source = text;
        // Ports follow the declarations; a bad declaration keeps the old ports until it is fixed.
        if let Ok((ins, outs)) = script_ports(&s.source) { s.inputs = ins; s.outputs = outs; }
        let stored = s.clone();
        self.commit_graph(ov, "dock.undo.script");
        self.script_dict(i, &stored)
    }
    /// Scripts from shared designs arrive disabled; the player enables each one (L2123).
    #[func]
    fn script_set_enabled(&mut self, index: i64, enabled: bool) -> bool {
        if !cfg!(feature = "luau") { return false; }
        let Some(mut ov) = self.overrides().cloned() else { return false };
        let Some(i) = usize::try_from(index).ok().filter(|i| *i < ov.scripts.len()) else { return false };
        if enabled && !self.scripts_allowed() { return false; }
        if ov.scripts[i].enabled == enabled { return true; }
        ov.scripts[i].enabled = enabled;
        self.commit_graph(ov, "dock.undo.script")
    }
    #[func]
    fn script_remove(&mut self, index: i64) -> bool {
        let Some(mut ov) = self.overrides().cloned() else { return false };
        let Some(i) = u16::try_from(index).ok().filter(|i| (*i as usize) < ov.scripts.len()) else { return false };
        drop_script(&mut ov, i);
        self.commit_graph(ov, "dock.undo.script")
    }
    /// Re-arm scripts that went grey after a fault (error, memory, budget or backstop). Allowed in dry dock
    /// and during a splash only (L371); in a splash it sends `Command::RearmScripts`.
    #[func]
    fn script_rearm(&mut self) -> bool {
        if !cfg!(feature = "luau") || self.model.is_none() { return false; }
        if !self.in_splash {
            // A greyed-out script stays grey only for the sortie it faulted in; every basin and sortie launched
            // from the dock starts with its scripts armed.
            let ticking = matches!(crate::services::services().feed.borrow().state, crate::enums::SessionState::InSortie);
            return !ticking;
        }
        let services = crate::services::services();
        let mut c = services.connection.borrow_mut();
        match c.as_mut() {
            Some(conn) if conn.link.status().state == crate::seams::LinkState::Joined => {
                conn.link.send(crate::seams::ClientMsg::Command(sim_systems::protocol::Command::RearmScripts));
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_ids_round_trip() {
        for (k, _) in library::palette() {
            let t = type_id(&k);
            assert_eq!(kind_of_type(&t).as_ref(), Some(&k), "{t}");
        }
        assert_eq!(kind_of_type("script:3"), Some(NodeKind::Script(3)));
        assert_eq!(kind_of_type("math.nope"), None);
        assert_eq!(type_id(&NodeKind::Math(MathOp::Add)), "math.add");
        assert_eq!(rebind(&NodeKind::Sink(SinkKind::Pump(PartSlot(0))), 7), NodeKind::Sink(SinkKind::Pump(PartSlot(7))));
        assert_eq!(binding_of(&NodeKind::Sink(SinkKind::FloodValve(CompartmentId(2)))), Some(("compartment", 2)));
        assert_eq!(binding_of(&NodeKind::Sink(SinkKind::HelmHeading)), None);
    }

    #[test]
    fn script_ports_are_declared_in_the_source() {
        let src = "--@in target: vec3\n--@in speed_kn: scalar\n--@out heading: angle\nreturn function(t, s) return nil end\n";
        let (ins, outs) = script_ports(src).unwrap();
        assert_eq!(ins, vec![("target".to_string(), PortType::Vec3), ("speed_kn".to_string(), PortType::Scalar)]);
        assert_eq!(outs, vec![("heading".to_string(), PortType::Angle)]);
        assert_eq!(script_ports("--@out x: banana").unwrap_err(), (1, "banana".to_string()));
        assert!(script_ports("print('no ports')").unwrap().0.is_empty());
        let many: String = (0..40).map(|i| format!("--@in p{i}: bool\n")).collect();
        assert_eq!(script_ports(&many).unwrap().0.len(), MAX_SCRIPT_PORTS);
    }

    #[test]
    fn loops_are_found() {
        let (a, b, c) = (GraphNodeId(1), GraphNodeId(2), GraphNodeId(3));
        let edges = [(a, b), (b, c)];
        assert!(closes_loop(&edges, c, a));
        assert!(!closes_loop(&edges, a, c));
        assert!(closes_loop(&edges, a, a));
    }

    #[test]
    fn removing_a_script_renumbers_its_nodes() {
        let mut ov = GraphOverrides::default();
        for n in 0..3 { ov.scripts.push(ScriptSource { name: format!("s{n}"), source: String::new(), inputs: vec![], outputs: vec![], enabled: true }); }
        let id = |i: u32| GraphNodeId(USER_NODE_BASE + i);
        ov.pinned.nodes.push(GraphNode { id: id(0), kind: NodeKind::Script(0), params: BTreeMap::new(), pos: [0.0, 0.0] });
        ov.pinned.nodes.push(GraphNode { id: id(1), kind: NodeKind::Script(2), params: BTreeMap::new(), pos: [0.0, 0.0] });
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: id(0), port: 0 }, to: PortRef { node: id(1), port: 0 } });
        ov.replaced.push(StageReplacement { stage: OfficerStage::ThreatRanking, subgraph: ControlGraph { nodes: vec![GraphNode { id: id(2), kind: NodeKind::Script(1), params: BTreeMap::new(), pos: [0.0, 0.0] }], edges: vec![] } });
        drop_script(&mut ov, 1);
        assert_eq!(ov.scripts.len(), 2);
        assert_eq!(ov.pinned.nodes[1].kind, NodeKind::Script(1));
        assert!(ov.replaced[0].subgraph.nodes.is_empty());
        assert_eq!(ov.pinned.edges.len(), 1);
        drop_script(&mut ov, 0);
        assert!(ov.pinned.edges.is_empty());
        assert_eq!(free_id(&ov), Some(id(0)));
    }

    /// A built design and its editor view, as `graph_basis` makes them.
    fn basis_of(bp: &content::blueprint::Blueprint) -> Basis {
        let cat = content::catalogue::catalogue();
        let ruleset = content::ruleset::Ruleset::campaign(content::enums::Act::Act4);
        let prefab = Arc::new(sim_core::vessel::build_prefab(bp, cat));
        let design = Arc::new(ShipDesign::build(bp, prefab, cat, &ruleset));
        let facts = sim_systems::ship::DesignFacts { blueprint: bp, prefab: &design.prefab, catalogue: cat, mounts: &design.mounts, emitters: &design.emitters, sensor_slots: &design.sensor_slots, consumer_slots: &design.consumer_slots, decoys: &design.decoys, pumps: &design.pumps, doors: &design.doors };
        let generated = generate::generate(&facts);
        let compiled = Arc::new(compile::compile_with(&facts, &bp.officers, &bp.graph, &bp.rules, &ruleset));
        let view = compile::to_view(&compiled);
        Basis { rev: 1, design_rev: 1, design, generated, compiled, view }
    }
    fn recompiled(bp: &content::blueprint::Blueprint, ov: &GraphOverrides, b: &Basis) -> Arc<CompiledGraph> {
        let cat = content::catalogue::catalogue();
        b.design.recompile(&bp.officers, &bp.rules, ov, cat, &content::ruleset::Ruleset::campaign(content::enums::Act::Act4))
    }

    #[test]
    fn a_connection_the_editor_allows_compiles_clean() {
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let b = basis_of(&bp);
        assert!(b.compiled.issues.is_empty(), "{:?}", b.compiled.issues);
        let ov = GraphOverrides::default();
        // A player constant drives the helm speed sink: the sink is pinned with the new edge.
        let speed = generate::sink_node(SinkKind::HelmSpeed);
        let mut with_const = ov.clone();
        let c = free_id(&with_const).unwrap();
        assert!(compile::pin_node(&mut with_const, &b.generated, GraphNode { id: c, kind: NodeKind::Source(SourceKind::Constant(Value::Scalar(12.0))), params: BTreeMap::new(), pos: [0.0, 0.0] }, &[]).is_empty());
        let next = connected(&with_const, &b, (c, 0), (speed, 0)).expect("a constant may drive helm speed");
        assert!(next.pinned.nodes.iter().any(|n| n.id == speed));
        let g = recompiled(&bp, &next, &b);
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        // Wrong types are refused; so is writing into a source.
        let own = GraphNodeId(c.0 + 2);
        assert!(compile::pin_node(&mut with_const, &b.generated, GraphNode { id: own, kind: NodeKind::Source(SourceKind::OwnShip), params: BTreeMap::new(), pos: [0.0, 0.0] }, &[]).is_empty());
        assert!(connected(&with_const, &b, (own, 0), (speed, 0)).is_none(), "a position is not a speed");
        assert!(connected(&with_const, &b, (own, 3), (speed, 0)).is_some(), "own speed is a speed");
        assert!(connected(&with_const, &b, (c, 0), (own, 0)).is_none());
        // A loop is refused: helm speed cannot feed something upstream of itself.
        let sel = GraphNodeId(c.0 + 1);
        let mut looped = next.clone();
        assert!(compile::pin_node(&mut looped, &b.generated, GraphNode { id: sel, kind: NodeKind::Math(MathOp::Add), params: BTreeMap::new(), pos: [0.0, 0.0] }, &[GraphEdge { from: PortRef { node: c, port: 0 }, to: PortRef { node: sel, port: 0 } }]).is_empty());
        let b2 = Basis { compiled: recompiled(&bp, &looped, &b), ..basis_of(&bp) };
        let b2 = Basis { view: compile::to_view(&b2.compiled), ..b2 };
        assert!(connected(&looped, &b2, (sel, 0), (c, 0)).is_none());
    }

    #[test]
    fn a_forked_stage_starts_as_stock() {
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let b = basis_of(&bp);
        let mut ov = GraphOverrides::default();
        let anchor = free_id(&ov).unwrap();
        let wide = boundary_id(OfficerStage::ThreatRanking);
        let mut sub = ControlGraph::default();
        sub.nodes.push(GraphNode { id: anchor, kind: NodeKind::Math(MathOp::Select), params: BTreeMap::new(), pos: [0.0, 0.0] });
        sub.edges.push(GraphEdge { from: PortRef { node: wide, port: 0 }, to: PortRef { node: anchor, port: 2 } });
        ov.replaced.push(StageReplacement { stage: OfficerStage::ThreatRanking, subgraph: sub });
        let g = recompiled(&bp, &ov, &b);
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        assert!(g.nodes.iter().any(|n| n.id == wide && compile::is_proxy(n)), "the stage is replaced");
        // The anchor's output may now drive the stage's top-ranked track... only with a matching type.
        let b3 = Basis { compiled: g.clone(), view: compile::to_view(&g), ..basis_of(&bp) };
        assert_eq!(home_of(&ov, &b3, anchor), Some(Home::Sub(0)));
        assert_eq!(home_of(&ov, &b3, wide), Some(Home::Boundary(0)));
        let driven = connected(&ov, &b3, (wide, 0), (wide, 0)).expect("tracks in, ranked out");
        let g2 = recompiled(&bp, &driven, &b3);
        assert!(g2.issues.is_empty(), "{:?}", g2.issues);
        // The fork's own node drives the stage's ranked output: reading the stage's input and driving its
        // output is no loop.
        let wired = connected(&ov, &b3, (anchor, 0), (wide, 0)).expect("the anchor may drive the ranked list");
        let g3 = recompiled(&bp, &wired, &b3);
        assert!(g3.issues.is_empty(), "{:?}", g3.issues);
        // A real loop through the stage's consumers is refused: a node that reads mount assignment (which
        // reads the ranked list) may not drive the ranked list.
        let assign = generate::stage_node(OfficerStage::MountAssignment, None);
        let b4 = Basis { compiled: g3.clone(), view: compile::to_view(&g3), ..basis_of(&bp) };
        let mut looped = wired.clone();
        let reader = GraphNodeId(anchor.0 + 1);
        looped.replaced[0].subgraph.nodes.push(GraphNode { id: reader, kind: NodeKind::Math(MathOp::Select), params: BTreeMap::new(), pos: [0.0, 0.0] });
        looped.replaced[0].subgraph.edges.push(GraphEdge { from: PortRef { node: assign, port: 0 }, to: PortRef { node: reader, port: 1 } });
        if b4.view.nodes.iter().any(|n| n.id == assign) {
            assert!(connected(&looped, &b4, (reader, 0), (wide, 0)).is_none(), "a loop through the stage's consumers");
        }
    }
}
