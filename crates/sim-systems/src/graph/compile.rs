//! Unit `sys-graph-compile`. Merge the generated graph with stored overrides and compile it.
//! DESIGN.md L320 (pinned nodes survive regeneration; a pinned node that loses its input becomes a Ship's
//! Note), L336 (L2 rule semantics), L346 (replace one stage, keep the rest), L313 (ruleset layer limits).
//!
//! L2 rules: at most 32 per ship; compiled into graph nodes so every rule is visible at L3; evaluated every
//! graph tick top to bottom; a matching rule overrides the officer output on the sink it names for as long as
//! its condition holds; of two matching rules on one sink the one nearer the top wins; the tick after the
//! condition stops holding the officer's output returns.
//!
//! # Layers and fallback in the compiled graph
//! Every node is either officer layer (generated and kit nodes: always evaluated) or player layer (pinned
//! nodes, replacement subgraphs, rule nodes, script nodes: budgeted, and dead for the tick when an input is
//! dead). Where a player value enters the officer layer the compiler leaves a node with a `fallback`:
//! - a pinned generated node keeps its generated original as a *shadow* (same id with [`SHADOW_BIT`] set),
//!   and so does a generated node whose input a pinned edge rewired; the shadow is evaluated lazily, only on a
//!   tick when its owner cannot produce live outputs;
//! - a replaced stage instance becomes a *proxy* (kind `Stage`, origin `Replacement`, the instance's own id)
//!   whose inputs are the stage's outputs as the subgraph drives them (undriven outputs read the stock stage);
//!   the stock stage becomes the proxy's lazy shadow;
//! - an L2 rule becomes condition nodes plus one *splice* Select per overridden port (origin `Rule`): it
//!   outputs the rule's value while the condition holds and passes the value below it through otherwise,
//!   including when its condition could not be evaluated. Splices nest bottom rule innermost, so the rule
//!   nearer the top wins.
//!
//! # Rule targets
//! `RuleValue::Set(v)` overrides the sink's first port whose type fits `v` (presets only on `ShipPreset`, ID
//! rungs only on `TrackIdOverride`); on `TrackStanceOverride` and `TrackIdOverride` the track is the
//! designated track. `RuleValue::NearestContact(filter)` retargets a mount: on a gun's `MountFirePermission`
//! it sets the solver's target and fire inputs (and the sink's target port), on `MountAimPoint` the solver's
//! target, on `MissileTarget` the guidance node's target and on `MissileLaunch` its target and fire inputs.
//! The filter's stance floor is raised to Suspect, so no rule can point a weapon at a Neutral or Friend
//! track (L486), and the rule holds only while a contact matches the filter. A rule naming a sink the design
//! does not have is `RuleSinkMissing`.
//!
//! # Replacement subgraphs
//! A `StageReplacement` subgraph names the replaced stage instance by its generated id
//! (`generate::stage_node(stage, part)`): an edge *from* that id reads the stage's input with that port index,
//! an edge *to* that id drives the stage's output with that port index. Other generated ids may be read
//! directly. For the per-mount stages (`BallisticSolver`, `PnGuidance`) a subgraph that names the ship-wide
//! id `stage_node(stage, None)` is a template instantiated once per mount of that stage: its node ids are
//! renumbered into id group 8 with the mount's slot, and part slot `u16::MAX` in its node kinds (or in a
//! stage node's `part` parameter) means "this mount". A subgraph that names specific instances replaces only
//! those, with its node ids kept (they must be player ids).
use super::generate::{self, GenInput};
use super::library::{self, SigFacts};
use super::{CompiledGraph, CompiledNode, GraphIssue, InputSrc, NodeOrigin, PortValue};
use crate::ship::DesignFacts;
use content::enums::{ControlLayer, OfficerRole, Stance};
use content::graph::{ControlGraph, GraphEdge, GraphNode, GraphOverrides, MathOp, NodeKind, OfficerSettings, OfficerStage, Operand, PortRef, PortType, Rule, RuleValue, ScriptSource, SinkKind, SourceKind, TrackFilter, Value, MAX_RULES, USER_NODE_BASE};
use content::ids::{GraphNodeId, KitSlot, PartSlot};
use content::ruleset::Ruleset;
use std::collections::{BTreeMap, BTreeSet};

/// Bit set on the id of a lazy shadow node (the officer original of an overridden node).
pub const SHADOW_BIT: u32 = 1 << 31;
pub fn is_shadow(id: GraphNodeId) -> bool { id.0 & SHADOW_BIT != 0 }
fn shadow(id: GraphNodeId) -> GraphNodeId { GraphNodeId(id.0 | SHADOW_BIT) }
/// Port index used in issues about a node as a whole (its part binding or its existence).
pub const WHOLE_NODE: u8 = u8::MAX;
/// Nesting depth of rule conditions beyond which the rest is ignored (network and file input is untrusted).
const MAX_CONDITION_DEPTH: usize = 16;

/// Whether a compiled node is a replacement proxy (identity over its inputs, falling back to the stock stage).
pub fn is_proxy(n: &CompiledNode) -> bool { matches!(n.origin, NodeOrigin::Replacement(_)) && matches!(n.kind, NodeKind::Stage(_)) && n.fallback.is_some() }
/// Whether a compiled node is a rule splice (Select that passes its `else` through unless the rule holds).
pub fn is_splice(n: &CompiledNode) -> bool { matches!(n.origin, NodeOrigin::Rule(_)) && n.kind == NodeKind::Math(MathOp::Select) }
/// Whether a compiled node is in the budgeted player layer.
pub fn is_player(n: &CompiledNode) -> bool {
    matches!(n.origin, NodeOrigin::Pinned | NodeOrigin::Replacement(_) | NodeOrigin::Rule(_) | NodeOrigin::Script(_)) && !is_proxy(n) && !is_splice(n)
}

#[derive(Clone, Debug, PartialEq)]
enum Wire { Default, Const(PortValue), Edge(GraphNodeId, u8) }

#[derive(Clone, Debug)]
struct WNode {
    id: GraphNodeId,
    kind: NodeKind,
    params: BTreeMap<String, Value>,
    origin: NodeOrigin,
    officer: Option<OfficerRole>,
    part: Option<PartSlot>,
    inputs: Vec<Wire>,
    in_types: Vec<PortType>,
    in_defaults: Vec<PortValue>,
    out_types: Vec<PortType>,
    fallback: Option<GraphNodeId>,
    /// Pinned edits keep their own stored parameters ahead of settings.
    own_params_first: bool,
    /// Replacement index a subgraph node or proxy belongs to.
    replacement: Option<usize>,
}

/// What the compiler reads besides the overrides.
pub(crate) struct Env<'a> {
    pub gin: GenInput<'a>,
    pub settings: &'a OfficerSettings,
    pub part_count: usize,
    pub compartment_count: usize,
}

struct Ctx<'a, 'e> {
    env: &'e Env<'a>,
    scripts: &'e [ScriptSource],
    nodes: BTreeMap<GraphNodeId, WNode>,
    issues: Vec<GraphIssue>,
    luau: bool,
    max_layer: ControlLayer,
    rule_sources: BTreeMap<String, GraphNodeId>,
    rule_counter: u32,
}

fn sf<'a>(env: &Env<'a>) -> SigFacts<'a> { env.gin.sig_facts() }

impl<'a, 'e> Ctx<'a, 'e> {
    fn issue(&mut self, i: GraphIssue) { if !self.issues.contains(&i) { self.issues.push(i); } }

    /// (input types, input defaults, output types) of a kind on this design.
    fn sig(&self, kind: &NodeKind) -> (Vec<PortType>, Vec<PortValue>, Vec<PortType>) {
        if let NodeKind::Script(i) = kind {
            return match self.scripts.get(*i as usize) {
                Some(s) => (s.inputs.iter().take(library::MAX_PORTS).map(|p| p.1).collect(), vec![PortValue::None; s.inputs.len().min(library::MAX_PORTS)], s.outputs.iter().take(library::MAX_PORTS).map(|p| p.1).collect()),
                None => (Vec::new(), Vec::new(), Vec::new()),
            };
        }
        let sig = library::signature(kind, &sf(self.env));
        (sig.inputs.iter().map(|p| p.ty).collect(), sig.inputs.iter().map(|p| p.default.clone()).collect(), sig.outputs.iter().map(|p| p.ty).collect())
    }

    fn wnode(&self, id: GraphNodeId, kind: NodeKind, params: BTreeMap<String, Value>, origin: NodeOrigin, officer: Option<OfficerRole>, part: Option<PartSlot>) -> WNode {
        let (in_types, in_defaults, out_types) = self.sig(&kind);
        WNode { id, inputs: vec![Wire::Default; in_types.len()], in_types, in_defaults, out_types, kind, params, origin, officer, part, fallback: None, own_params_first: false, replacement: None }
    }

    /// Whether a part-bound kind refers to a part this design has, in the role the kind needs.
    fn binding_ok(&self, kind: &NodeKind, part: Option<PartSlot>) -> bool {
        let g = &self.env.gin;
        let in_list = |l: &[PartSlot], s: PartSlot| l.contains(&s);
        match kind {
            NodeKind::Source(SourceKind::PartState(s)) => (s.0 as usize) < self.env.part_count,
            NodeKind::Source(SourceKind::Detections(s)) => in_list(g.sensor_slots, *s),
            NodeKind::Sink(k) => self.sink_valid(*k),
            NodeKind::Stage(st) if library::stage_is_per_part(*st) => match part {
                Some(p) => g.mounts.iter().any(|m| m.slot == p && library::mount_stage(m.weapon) == Some(*st)),
                None => false,
            },
            NodeKind::Script(i) => (*i as usize) < self.scripts.len(),
            _ => true,
        }
    }

    fn sink_valid(&self, sink: SinkKind) -> bool {
        let g = &self.env.gin;
        let mount = |s: PartSlot| g.mounts.iter().find(|m| m.slot == s).and_then(|m| library::mount_stage(m.weapon));
        match sink {
            SinkKind::MountAimPoint(s) => mount(s) == Some(OfficerStage::BallisticSolver),
            SinkKind::MountFirePermission(s) => mount(s).is_some(),
            SinkKind::MissileLaunch(s) | SinkKind::MissileTarget(s) => mount(s) == Some(OfficerStage::PnGuidance),
            SinkKind::EmissionState(s) => g.emitters.iter().any(|e| e.slot == s),
            SinkKind::PowerPriority(s) => g.consumer_slots.contains(&s),
            SinkKind::Pump(s) => g.pumps.contains(&s),
            SinkKind::Door(s) => g.doors.contains(&s),
            SinkKind::FloodValve(c) => generate::compartment_ok(c, self.env.compartment_count),
            SinkKind::HelmHeading | SinkKind::HelmSpeed | SinkKind::TrackStanceOverride | SinkKind::TrackIdOverride | SinkKind::ShipPreset | SinkKind::ShipEmcon => true,
        }
    }

    /// Active (non-shadow) generated or kit node with this id.
    fn is_officer_node(&self, id: GraphNodeId) -> bool { self.nodes.get(&id).is_some_and(|n| matches!(n.origin, NodeOrigin::Generated | NodeOrigin::Kit(_))) }

    /// Move node `id` to its shadow id (lazy) and return a copy to become the active override.
    fn shadow_out(&mut self, id: GraphNodeId) -> Option<WNode> {
        let mut orig = self.nodes.remove(&id)?;
        let active = orig.clone();
        orig.id = shadow(id);
        self.nodes.insert(orig.id, orig);
        Some(active)
    }

    /// Layer a player node needs.
    fn layer_of(kind: &NodeKind) -> ControlLayer { if matches!(kind, NodeKind::Script(_)) { ControlLayer::L4Script } else { ControlLayer::L3Graph } }
    fn layer_allowed(&self, layer: ControlLayer) -> bool {
        if layer == ControlLayer::L4Script { self.luau } else { layer <= self.max_layer }
    }

    // ---------------------------------------------------------------------------------------------------
    // Generated graph.

    fn load_generated(&mut self, g: &ControlGraph) {
        for n in &g.nodes {
            let (group, _, pf, _) = generate::id_fields(n.id);
            let origin = match group {
                generate::GROUP_KIT => self.env.gin.kits.iter().find(|k| k.parts.first().map(|p| p.0 + 1) == Some(pf)).map(|k| NodeOrigin::Kit(k.slot)).unwrap_or(NodeOrigin::Generated),
                generate::GROUP_KIT_PARTLESS => NodeOrigin::Kit(KitSlot(pf.saturating_sub(1))),
                _ => NodeOrigin::Generated,
            };
            let officer = generate::officer_of_code(group).or_else(|| kind_officer(&n.kind));
            let w = self.wnode(n.id, n.kind.clone(), n.params.clone(), origin, officer, generate::node_part(n));
            self.nodes.insert(n.id, w);
        }
        for e in &g.edges { self.set_input(e.to.node, e.to.port, Wire::Edge(e.from.node, e.from.port)); }
    }

    fn set_input(&mut self, node: GraphNodeId, port: u8, w: Wire) -> bool {
        let Some(n) = self.nodes.get_mut(&node) else { return false };
        match n.inputs.get_mut(port as usize) { Some(slot) => { *slot = w; true } None => false }
    }

    // ---------------------------------------------------------------------------------------------------
    // L3 pins.

    fn apply_pinned(&mut self, pinned: &ControlGraph) {
        let mut seen = BTreeSet::new();
        let mut pinned_ids: BTreeSet<GraphNodeId> = BTreeSet::new();
        for p in &pinned.nodes {
            if !seen.insert(p.id) { continue; }
            if is_shadow(p.id) { self.issue(GraphIssue::TypeMismatch { node: p.id, port: WHOLE_NODE }); continue; }
            let layer = Self::layer_of(&p.kind);
            if !self.layer_allowed(layer) { self.issue(GraphIssue::LayerNotAllowed { node: p.id }); continue; }
            if p.id.0 < USER_NODE_BASE {
                // An edited generated node.
                let Some(orig) = self.nodes.get(&p.id).filter(|n| matches!(n.origin, NodeOrigin::Generated | NodeOrigin::Kit(_))).cloned() else {
                    self.issue(GraphIssue::PinnedLostInput { node: p.id, port: WHOLE_NODE });
                    continue;
                };
                let part = generate::node_part(p).or(orig.part);
                if !self.binding_ok(&p.kind, part) { self.issue(GraphIssue::PinnedLostInput { node: p.id, port: WHOLE_NODE }); continue; }
                let orig_inputs = orig.inputs.clone();
                self.shadow_out(p.id);
                let origin = match p.kind { NodeKind::Script(i) => NodeOrigin::Script(i), _ => NodeOrigin::Pinned };
                let mut w = self.wnode(p.id, p.kind.clone(), p.params.clone(), origin, orig.officer.or_else(|| kind_officer(&p.kind)), part);
                // Ports the pinned edges do not drive keep the generated wiring (a design change may add ports).
                if p.kind == orig.kind { for (i, wire) in orig_inputs.into_iter().enumerate() { if let Some(s) = w.inputs.get_mut(i) { *s = wire; } } }
                w.fallback = Some(shadow(p.id));
                w.own_params_first = true;
                self.nodes.insert(p.id, w);
            } else {
                if self.nodes.contains_key(&p.id) { self.issue(GraphIssue::TypeMismatch { node: p.id, port: WHOLE_NODE }); continue; }
                if !self.user_kind_ok(p.id, &p.kind, generate::node_part(p)) { continue; }
                let origin = match p.kind { NodeKind::Script(i) => NodeOrigin::Script(i), _ => NodeOrigin::Pinned };
                let mut w = self.wnode(p.id, p.kind.clone(), p.params.clone(), origin, kind_officer(&p.kind), generate::node_part(p));
                w.own_params_first = true;
                self.nodes.insert(p.id, w);
            }
            pinned_ids.insert(p.id);
        }
        let mut edges = pinned.edges.clone();
        edges.sort();
        edges.dedup();
        for e in edges {
            if !(pinned_ids.contains(&e.from.node) || pinned_ids.contains(&e.to.node)) { continue; }
            let target = e.to.node;
            let Some(t) = self.nodes.get(&target) else { continue };
            if e.to.port as usize >= t.inputs.len() {
                self.issue(if pinned_ids.contains(&target) { GraphIssue::PinnedLostInput { node: target, port: e.to.port } } else { GraphIssue::TypeMismatch { node: target, port: e.to.port } });
                continue;
            }
            if !self.nodes.contains_key(&e.from.node) || is_shadow(e.from.node) {
                if pinned_ids.contains(&target) {
                    self.issue(GraphIssue::PinnedLostInput { node: target, port: e.to.port });
                    self.set_input(target, e.to.port, Wire::Default);
                }
                continue;
            }
            if !pinned_ids.contains(&target) {
                // A generated node read from a pinned node: it keeps its original as a shadow for fallback.
                if self.nodes.get(&target).is_some_and(|n| n.fallback.is_none()) && self.is_officer_node(target) {
                    if let Some(mut w) = self.shadow_out(target) { w.fallback = Some(shadow(target)); self.nodes.insert(target, w); }
                }
            }
            self.set_input(target, e.to.port, Wire::Edge(e.from.node, e.from.port));
        }
    }

    /// Checks shared by player nodes outside the generated id range. Records the issue and returns false when
    /// the node cannot be used.
    fn user_kind_ok(&mut self, id: GraphNodeId, kind: &NodeKind, part: Option<PartSlot>) -> bool {
        let twin = match kind {
            NodeKind::Stage(st) => Some(generate::stage_node(*st, part)),
            NodeKind::Sink(k) => Some(generate::sink_node(*k)),
            _ => None,
        };
        if let Some(t) = twin { if self.nodes.contains_key(&t) { self.issue(GraphIssue::TypeMismatch { node: id, port: WHOLE_NODE }); return false; } }
        if let NodeKind::Script(i) = kind {
            let scripts = self.scripts;
            match scripts.get(*i as usize) {
                None => { self.issue(GraphIssue::ScriptDisabled { script: *i }); return false; }
                Some(s) if !s.enabled => self.issue(GraphIssue::ScriptDisabled { script: *i }),
                Some(_) => {}
            }
        }
        if !self.binding_ok(kind, part) { self.issue(GraphIssue::PinnedLostInput { node: id, port: WHOLE_NODE }); return false; }
        true
    }

    // ---------------------------------------------------------------------------------------------------
    // L3 stage replacements.

    fn apply_replacements(&mut self, overrides: &GraphOverrides) {
        let mut done_stages = BTreeSet::new();
        for (ri, r) in overrides.replaced.iter().enumerate() {
            let wide = generate::stage_node(r.stage, None);
            if !done_stages.insert(r.stage) { self.issue(GraphIssue::TypeMismatch { node: wide, port: WHOLE_NODE }); continue; }
            let layer = if r.subgraph.nodes.iter().any(|n| matches!(n.kind, NodeKind::Script(_))) { ControlLayer::L4Script } else { ControlLayer::L3Graph };
            if !self.layer_allowed(ControlLayer::L3Graph) || !self.layer_allowed(layer) { self.issue(GraphIssue::LayerNotAllowed { node: wide }); continue; }
            let per_part = library::stage_is_per_part(r.stage);
            let referenced: BTreeSet<GraphNodeId> = r.subgraph.edges.iter().flat_map(|e| [e.from.node, e.to.node]).collect();
            let instances: Vec<GraphNodeId> = self.nodes.values()
                .filter(|n| n.kind == NodeKind::Stage(r.stage) && matches!(n.origin, NodeOrigin::Generated | NodeOrigin::Kit(_)) && n.fallback.is_none() && n.id.0 < USER_NODE_BASE && generate::officer_of_code(generate::id_fields(n.id).0).is_some())
                .map(|n| n.id).collect();
            let template = per_part && referenced.contains(&wide);
            let targets: Vec<GraphNodeId> = if template { instances.clone() } else { instances.iter().copied().filter(|i| referenced.contains(i)).collect() };
            if targets.is_empty() { self.issue(GraphIssue::PinnedLostInput { node: wide, port: WHOLE_NODE }); continue; }
            if template {
                for t in targets {
                    let part = generate::id_part(t);
                    let Some(pf) = part.map(|p| p.0 + 1) else { continue };
                    let mut map = BTreeMap::new();
                    for (k, n) in r.subgraph.nodes.iter().enumerate() {
                        if let Some(id) = generate::fragment_id(generate::GROUP_TEMPLATE, pf, k) { map.insert(n.id, id); }
                    }
                    self.instantiate(ri, r.stage, &r.subgraph, &map, &[(t, Some(wide))], part);
                }
            } else {
                let mut map = BTreeMap::new();
                for n in &r.subgraph.nodes { if n.id.0 >= USER_NODE_BASE && !is_shadow(n.id) { map.insert(n.id, n.id); } else { self.issue(GraphIssue::TypeMismatch { node: n.id, port: WHOLE_NODE }); } }
                let bounds: Vec<(GraphNodeId, Option<GraphNodeId>)> = targets.iter().map(|t| (*t, None)).collect();
                self.instantiate(ri, r.stage, &r.subgraph, &map, &bounds, None);
            }
        }
    }

    /// Add one instance of a replacement subgraph. `bounds` lists (instance id, template marker id): edges
    /// naming either id cross the stage boundary of that instance.
    fn instantiate(&mut self, ri: usize, stage: OfficerStage, sub: &ControlGraph, map: &BTreeMap<GraphNodeId, GraphNodeId>, bounds: &[(GraphNodeId, Option<GraphNodeId>)], part: Option<PartSlot>) {
        let origin = NodeOrigin::Replacement(stage);
        let mut added = Vec::new();
        for n in &sub.nodes {
            let Some(&id) = map.get(&n.id) else { continue };
            if self.nodes.contains_key(&id) { self.issue(GraphIssue::TypeMismatch { node: id, port: WHOLE_NODE }); continue; }
            let (kind, params) = bind_template(&n.kind, &n.params, part);
            if kind == NodeKind::Stage(stage) { self.issue(GraphIssue::TypeMismatch { node: id, port: WHOLE_NODE }); continue; }
            let probe = GraphNode { id: GraphNodeId(USER_NODE_BASE), kind: kind.clone(), params: params.clone(), pos: n.pos };
            let npart = generate::node_part(&probe);
            if !self.user_kind_ok(id, &kind, npart) { continue; }
            let mut w = self.wnode(id, kind, params, if let NodeKind::Script(i) = n.kind { NodeOrigin::Script(i) } else { origin }, Some(stage.officer()), npart);
            w.own_params_first = true;
            w.replacement = Some(ri);
            self.nodes.insert(id, w);
            added.push(id);
        }
        // Proxies for every instance.
        let mut stock_inputs: BTreeMap<GraphNodeId, Vec<Wire>> = BTreeMap::new();
        for (tid, _) in bounds {
            let Some(stock) = self.shadow_out(*tid) else { continue };
            stock_inputs.insert(*tid, stock.inputs.clone());
            let outs = stock.out_types.clone();
            let mut proxy = WNode {
                id: *tid, kind: NodeKind::Stage(stage), params: stock.params.clone(), origin, officer: Some(stage.officer()), part: stock.part,
                inputs: (0..outs.len()).map(|j| Wire::Edge(shadow(*tid), j as u8)).collect(), in_types: outs.clone(),
                in_defaults: vec![PortValue::None; outs.len()], out_types: outs, fallback: Some(shadow(*tid)), own_params_first: false, replacement: Some(ri),
            };
            proxy.inputs.truncate(library::MAX_PORTS);
            self.nodes.insert(*tid, proxy);
        }
        let boundary = |id: GraphNodeId| bounds.iter().find(|(t, m)| *t == id || *m == Some(id)).map(|(t, _)| *t);
        let mut edges = sub.edges.clone();
        edges.sort();
        edges.dedup();
        for e in &edges {
            // Source of the wire.
            let from = if let Some(t) = boundary(e.from.node) {
                stock_inputs.get(&t).and_then(|ins| ins.get(e.from.port as usize).cloned()).unwrap_or(Wire::Default)
            } else if let Some(&m) = map.get(&e.from.node) {
                if self.nodes.contains_key(&m) { Wire::Edge(m, e.from.port) } else { continue }
            } else if self.nodes.contains_key(&e.from.node) && !is_shadow(e.from.node) {
                Wire::Edge(e.from.node, e.from.port)
            } else {
                if let Some(&m) = map.get(&e.to.node) { self.issue(GraphIssue::PinnedLostInput { node: m, port: e.to.port }); }
                continue;
            };
            // Destination.
            if let Some(t) = boundary(e.to.node) {
                if !self.set_input(t, e.to.port, from) { self.issue(GraphIssue::TypeMismatch { node: t, port: e.to.port }); }
            } else if let Some(&m) = map.get(&e.to.node) {
                if self.nodes.contains_key(&m) && !self.set_input(m, e.to.port, from) { self.issue(GraphIssue::TypeMismatch { node: m, port: e.to.port }); }
            } else {
                // A replacement drives only the stage it replaces.
                self.issue(GraphIssue::TypeMismatch { node: e.to.node, port: e.to.port });
            }
        }
        let _ = added;
    }

    // ---------------------------------------------------------------------------------------------------
    // L2 rules.

    fn rule_id(i: usize, k: u32) -> GraphNodeId { GraphNodeId(((generate::GROUP_RULE as u32) << 20) | (((i as u32) & 0x3F) << 14) | (k & 0x3FFF)) }

    fn rule_node(&mut self, i: usize, k: &mut u32, kind: NodeKind, inputs: Vec<Wire>, params: BTreeMap<String, Value>, officer: Option<OfficerRole>) -> Option<GraphNodeId> {
        if *k > 0x3FFF { return None; }
        let id = Self::rule_id(i, *k);
        *k += 1;
        let mut w = self.wnode(id, kind, params, NodeOrigin::Rule(i as u8), officer, None);
        for (p, wire) in inputs.into_iter().enumerate() { if let Some(s) = w.inputs.get_mut(p) { *s = wire; } }
        self.nodes.insert(id, w);
        Some(id)
    }

    /// A source shared by every rule that reads it.
    fn rule_source(&mut self, i: usize, kind: SourceKind, params: BTreeMap<String, Value>) -> Option<GraphNodeId> {
        let key = format!("{kind:?}{params:?}");
        if let Some(id) = self.rule_sources.get(&key) { return Some(*id); }
        if self.rule_counter > 0x3FFF { return None; }
        let id = GraphNodeId(((generate::GROUP_RULE as u32) << 20) | (0x3F << 14) | self.rule_counter);
        self.rule_counter += 1;
        let w = self.wnode(id, NodeKind::Source(kind), params, NodeOrigin::Rule(i as u8), None, None);
        self.nodes.insert(id, w);
        self.rule_sources.insert(key, id);
        Some(id)
    }

    fn track_source(&mut self, i: usize, f: &TrackFilter) -> Option<GraphNodeId> {
        let mut p = BTreeMap::new();
        p.insert("min_stance".to_string(), Value::Scalar(stance_index(f.min_stance)));
        p.insert("domain".to_string(), Value::Scalar(f.domain.map(domain_index).unwrap_or(-1.0)));
        p.insert("min_grade".to_string(), Value::Scalar(grade_index(f.min_grade)));
        self.rule_source(i, SourceKind::TrackList, p)
    }

    fn operand(&mut self, i: usize, k: &mut u32, op: &Operand) -> Wire {
        let own = |c: &mut Self, port: u8| c.rule_source(i, SourceKind::OwnShip, BTreeMap::new()).map(|id| Wire::Edge(id, port)).unwrap_or(Wire::Default);
        match op {
            Operand::Const(c) => if c.is_finite() { Wire::Const(PortValue::Scalar(*c)) } else { Wire::Default },
            Operand::OwnSpeedKn => own(self, 3),
            Operand::OwnHeelDeg => {
                let heel = own(self, 4);
                let Some(abs) = self.rule_node(i, k, NodeKind::Math(MathOp::Abs), vec![heel], BTreeMap::new(), None) else { return Wire::Default };
                self.rule_node(i, k, NodeKind::Math(MathOp::Mul), vec![Wire::Edge(abs, 0), Wire::Const(PortValue::Scalar(180.0 / core::f64::consts::PI))], BTreeMap::new(), None).map(|m| Wire::Edge(m, 0)).unwrap_or(Wire::Default)
            }
            Operand::FuelFraction => own(self, 6),
            Operand::PowerMargin => own(self, 5),
            Operand::HullHealth => own(self, 7),
            Operand::InboundWeapons => own(self, 8),
            Operand::SortieTimeS => own(self, 9),
            Operand::NearestContactRangeM(f) => self.track_source(i, f).map(|id| Wire::Edge(id, 3)).unwrap_or(Wire::Default),
            Operand::ContactCount(f) => self.track_source(i, f).map(|id| Wire::Edge(id, 1)).unwrap_or(Wire::Default),
            Operand::PartHealth(s) | Operand::AmmoFraction(s) => {
                if (s.0 as usize) >= self.env.part_count { self.issue(GraphIssue::RuleSinkMissing { rule: i as u8 }); return Wire::Default; }
                let port = if matches!(op, Operand::PartHealth(_)) { 1 } else { 4 };
                self.rule_source(i, SourceKind::PartState(*s), BTreeMap::new()).map(|id| Wire::Edge(id, port)).unwrap_or(Wire::Default)
            }
        }
    }

    fn condition(&mut self, i: usize, k: &mut u32, c: &content::graph::Condition, depth: usize) -> Wire {
        use content::graph::Condition as C;
        if depth > MAX_CONDITION_DEPTH { return Wire::Default; }
        match c {
            C::Compare { lhs, cmp, rhs } => {
                let (a, b) = (self.operand(i, k, lhs), self.operand(i, k, rhs));
                self.rule_node(i, k, NodeKind::Math(MathOp::Compare(*cmp)), vec![a, b], BTreeMap::new(), None).map(|n| Wire::Edge(n, 0)).unwrap_or(Wire::Default)
            }
            C::All(list) | C::Any(list) => {
                let all = matches!(c, C::All(_));
                let op = if all { MathOp::And } else { MathOp::Or };
                let mut acc: Option<Wire> = None;
                for sub in list.iter().take(64) {
                    let w = self.condition(i, k, sub, depth + 1);
                    acc = Some(match acc {
                        None => w,
                        Some(prev) => self.rule_node(i, k, NodeKind::Math(op), vec![prev, w], BTreeMap::new(), None).map(|n| Wire::Edge(n, 0)).unwrap_or(Wire::Default),
                    });
                }
                acc.unwrap_or(Wire::Const(PortValue::Bool(all)))
            }
            C::Not(inner) => {
                let w = self.condition(i, k, inner, depth + 1);
                self.rule_node(i, k, NodeKind::Math(MathOp::Not), vec![w], BTreeMap::new(), None).map(|n| Wire::Edge(n, 0)).unwrap_or(Wire::Default)
            }
        }
    }

    /// Splice a rule Select ahead of `(target, port)`.
    fn splice(&mut self, i: usize, k: &mut u32, target: GraphNodeId, port: u8, cond: &Wire, value: Wire) -> bool {
        let Some(prev) = self.nodes.get(&target).and_then(|n| n.inputs.get(port as usize)).cloned() else { return false };
        let officer = self.nodes.get(&target).and_then(|n| n.officer);
        let Some(sel) = self.rule_node(i, k, NodeKind::Math(MathOp::Select), vec![cond.clone(), value, prev], BTreeMap::new(), officer) else { return false };
        self.set_input(target, port, Wire::Edge(sel, 0))
    }

    /// The active sink node, created for rules when the officers do not drive it.
    fn ensure_sink(&mut self, i: usize, sink: SinkKind) -> GraphNodeId {
        let id = generate::sink_node(sink);
        if !self.nodes.contains_key(&id) {
            let w = self.wnode(id, NodeKind::Sink(sink), BTreeMap::new(), NodeOrigin::Rule(i as u8), Some(library::sink_officer(sink)), generate::sink_part(sink));
            self.nodes.insert(id, w);
        }
        id
    }

    /// The officer-layer stage that serves a mount and can be retargeted (not replaced by a subgraph).
    fn mount_stage_node(&self, slot: PartSlot, stage: OfficerStage) -> Option<GraphNodeId> {
        let id = generate::stage_node(stage, Some(slot));
        self.nodes.get(&id).filter(|n| n.kind == NodeKind::Stage(stage) && !matches!(n.origin, NodeOrigin::Replacement(_))).map(|_| id)
    }

    fn apply_rules(&mut self, rules: &[Rule]) {
        if rules.is_empty() { return; }
        if !self.layer_allowed(ControlLayer::L2Rules) { self.issue(GraphIssue::LayerNotAllowed { node: Self::rule_id(0, 0) }); return; }
        if rules.len() > MAX_RULES { self.issue(GraphIssue::TooManyRules(rules.len())); }
        let n = rules.len().min(MAX_RULES);
        // Bottom rule first, so the top rule's splice is outermost and wins.
        for i in (0..n).rev() {
            let r = &rules[i];
            if !r.enabled { continue; }
            if !self.sink_valid(r.sink) { self.issue(GraphIssue::RuleSinkMissing { rule: i as u8 }); continue; }
            let mut k = 0u32;
            let cond = self.condition(i, &mut k, &r.when, 0);
            let first = Self::rule_id(i, 0);
            match &r.value {
                RuleValue::NearestContact(f) => {
                    // Never point a weapon at a Neutral or Friend track.
                    let mut f = *f;
                    if f.min_stance < Stance::Suspect { f.min_stance = Stance::Suspect; }
                    let Some(src) = self.track_source(i, &f) else { continue };
                    // The rule holds only while a matching contact exists, so it never writes "no target" over
                    // the officer's (or forces fire at nothing).
                    let Some(any) = self.rule_node(i, &mut k, NodeKind::Math(MathOp::Compare(content::graph::Cmp::Gt)), vec![Wire::Edge(src, 1), Wire::Const(PortValue::Scalar(0.0))], BTreeMap::new(), None) else { continue };
                    let Some(both) = self.rule_node(i, &mut k, NodeKind::Math(MathOp::And), vec![cond.clone(), Wire::Edge(any, 0)], BTreeMap::new(), None) else { continue };
                    let cond = Wire::Edge(both, 0);
                    let track = Wire::Edge(src, 2);
                    let yes = Wire::Const(PortValue::Bool(true));
                    let ok = match r.sink {
                        SinkKind::MountFirePermission(s) | SinkKind::MountAimPoint(s) | SinkKind::MissileTarget(s) | SinkKind::MissileLaunch(s) => {
                            let fire = matches!(r.sink, SinkKind::MountFirePermission(_) | SinkKind::MissileLaunch(_));
                            let stage = self.env.gin.mounts.iter().find(|m| m.slot == s).and_then(|m| library::mount_stage(m.weapon));
                            match stage.and_then(|st| self.mount_stage_node(s, st)) {
                                Some(node) => {
                                    let mut ok = self.splice(i, &mut k, node, 0, &cond, track.clone());
                                    if fire { ok &= self.splice(i, &mut k, node, 1, &cond, yes); }
                                    if let SinkKind::MountFirePermission(_) = r.sink { let sink = self.ensure_sink(i, r.sink); ok &= self.splice(i, &mut k, sink, 1, &cond, track); }
                                    ok
                                }
                                None => {
                                    // The mount's stage is replaced: override the sink's track port.
                                    let sink = self.ensure_sink(i, r.sink);
                                    match r.sink {
                                        SinkKind::MountFirePermission(_) => self.splice(i, &mut k, sink, 1, &cond, track),
                                        SinkKind::MissileTarget(_) => self.splice(i, &mut k, sink, 0, &cond, track),
                                        _ => false,
                                    }
                                }
                            }
                        }
                        _ => false,
                    };
                    if !ok { self.issue(GraphIssue::TypeMismatch { node: first, port: 0 }); }
                }
                RuleValue::Set(v) => {
                    let Some(port) = set_port(r.sink, v) else { self.issue(GraphIssue::TypeMismatch { node: first, port: 0 }); continue };
                    let sink = self.ensure_sink(i, r.sink);
                    let pv = PortValue::from_value(v);
                    let mut ok = self.splice(i, &mut k, sink, port, &cond, Wire::Const(pv));
                    if matches!(r.sink, SinkKind::TrackStanceOverride | SinkKind::TrackIdOverride) {
                        let orders = self.rule_source(i, SourceKind::TheatreOrders, BTreeMap::new());
                        if let Some(o) = orders { ok &= self.splice(i, &mut k, sink, 0, &cond, Wire::Edge(o, 1)); }
                    }
                    if !ok { self.issue(GraphIssue::TypeMismatch { node: first, port }); }
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------
    // Typing.

    /// Type of output `port` of `id`; Selects take the type of what is wired to `then` (or `else`).
    fn out_type(&self, id: GraphNodeId, port: u8, depth: usize) -> Option<PortType> {
        let n = self.nodes.get(&id)?;
        if n.kind == NodeKind::Math(MathOp::Select) && depth < 64 {
            for p in [1usize, 2] {
                match n.inputs.get(p) {
                    Some(Wire::Edge(s, sp)) => if let Some(t) = self.out_type(*s, *sp, depth + 1) { return Some(t); },
                    Some(Wire::Const(v)) if *v != PortValue::None => return Some(const_type(v)),
                    _ => {}
                }
            }
            return None;
        }
        n.out_types.get(port as usize).copied()
    }

    fn type_check(&mut self) {
        let ids: Vec<GraphNodeId> = self.nodes.keys().copied().collect();
        for id in ids {
            let n = self.nodes[&id].clone();
            let generic = n.kind == NodeKind::Math(MathOp::Select);
            for (p, w) in n.inputs.iter().enumerate() {
                let want = if generic && p > 0 { None } else { n.in_types.get(p).copied() };
                let bad = match w {
                    Wire::Default => false,
                    Wire::Const(v) => want.is_some_and(|t| !library::value_fits(v, t)),
                    Wire::Edge(src, sp) => match self.nodes.get(src) {
                        None => true,
                        Some(s) if *sp as usize >= s.out_types.len() => true,
                        Some(_) => match (self.out_type(*src, *sp, 0), want) { (Some(f), Some(t)) => !library::compatible(f, t), _ => false },
                    },
                };
                if bad {
                    let lost = matches!(w, Wire::Edge(src, _) if !self.nodes.contains_key(src));
                    let issue = if lost && n.origin == NodeOrigin::Pinned { GraphIssue::PinnedLostInput { node: id, port: p as u8 } } else { GraphIssue::TypeMismatch { node: id, port: p as u8 } };
                    self.issue(issue);
                    self.set_input(id, p as u8, Wire::Default);
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------
    // Ordering.

    fn deps(n: &WNode) -> impl Iterator<Item = GraphNodeId> + '_ {
        n.inputs.iter().filter_map(|w| if let Wire::Edge(s, _) = w { Some(*s) } else { None }).chain(n.fallback)
    }

    /// Kahn's algorithm, ties by ascending id. Returns the order and the nodes left in cycles.
    fn sort(&self) -> (Vec<GraphNodeId>, Vec<GraphNodeId>) {
        let mut indeg: BTreeMap<GraphNodeId, usize> = self.nodes.keys().map(|k| (*k, 0)).collect();
        let mut out: BTreeMap<GraphNodeId, Vec<GraphNodeId>> = BTreeMap::new();
        for n in self.nodes.values() {
            let mut seen = BTreeSet::new();
            for d in Self::deps(n) {
                if !self.nodes.contains_key(&d) || !seen.insert(d) { continue; }
                *indeg.get_mut(&n.id).expect("node") += 1;
                out.entry(d).or_default().push(n.id);
            }
        }
        let mut ready: BTreeSet<GraphNodeId> = indeg.iter().filter(|(_, d)| **d == 0).map(|(k, _)| *k).collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(id) = ready.pop_first() {
            order.push(id);
            if let Some(cs) = out.get(&id) {
                for c in cs { let d = indeg.get_mut(c).expect("node"); *d -= 1; if *d == 0 { ready.insert(*c); } }
            }
        }
        let left = indeg.iter().filter(|(_, d)| **d > 0).map(|(k, _)| *k).collect();
        (order, left)
    }

    /// Remove a player node, restoring the officer node it overrode.
    fn remove_override(&mut self, id: GraphNodeId) {
        if let Some(n) = self.nodes.remove(&id) {
            if let Some(f) = n.fallback.filter(|f| *f == shadow(id)) {
                if let Some(mut orig) = self.nodes.remove(&f) { orig.id = id; self.nodes.insert(id, orig); }
            }
        }
    }

    fn break_cycles(&mut self) -> Vec<GraphNodeId> {
        for pass in 0..6 {
            let (order, left) = self.sort();
            if left.is_empty() { return order; }
            let left_set: BTreeSet<GraphNodeId> = left.iter().copied().collect();
            let player: Vec<GraphNodeId> = left.iter().copied().filter(|id| {
                let n = &self.nodes[id];
                !is_shadow(*id) && (!matches!(n.origin, NodeOrigin::Generated | NodeOrigin::Kit(_)) || n.fallback.is_some())
            }).collect();
            let victims: Vec<GraphNodeId> = if player.is_empty() || pass >= 4 { left.iter().copied().filter(|i| !is_shadow(*i)).collect() } else { player };
            // A replacement caught in a cycle goes as a whole.
            let reps: BTreeSet<usize> = victims.iter().filter_map(|v| self.nodes.get(v).and_then(|n| n.replacement)).collect();
            let mut all: Vec<GraphNodeId> = victims.clone();
            all.extend(self.nodes.values().filter(|n| n.replacement.is_some_and(|r| reps.contains(&r))).map(|n| n.id));
            all.sort();
            all.dedup();
            for v in &all {
                if left_set.contains(v) { self.issue(GraphIssue::Cycle { node: *v }); }
                if matches!(self.nodes.get(v).map(|n| n.origin), Some(NodeOrigin::Generated | NodeOrigin::Kit(_))) && self.nodes.get(v).is_some_and(|n| n.fallback.is_none()) {
                    self.nodes.remove(v);
                } else {
                    self.remove_override(*v);
                }
            }
            // Anything still pointing at a removed node reads its default.
            let present: BTreeSet<GraphNodeId> = self.nodes.keys().copied().collect();
            for n in self.nodes.values_mut() {
                for w in n.inputs.iter_mut() { if let Wire::Edge(s, _) = w { if !present.contains(s) { *w = Wire::Default; } } }
                if n.fallback.is_some_and(|f| !present.contains(&f)) { n.fallback = None; }
            }
        }
        let (order, left) = self.sort();
        for id in left { self.nodes.remove(&id); }
        order.into_iter().filter(|id| self.nodes.contains_key(id)).collect()
    }
}

fn kind_officer(kind: &NodeKind) -> Option<OfficerRole> {
    match kind {
        NodeKind::Stage(s) => Some(s.officer()),
        NodeKind::Sink(k) => Some(library::sink_officer(*k)),
        _ => None,
    }
}

fn const_type(v: &PortValue) -> PortType {
    match v {
        PortValue::Bool(_) => PortType::Bool,
        PortValue::Vec3(_) => PortType::Vec3,
        PortValue::Text(_) => PortType::Text,
        PortValue::Stance(_) => PortType::Stance,
        PortValue::Emcon(_) => PortType::Emcon,
        PortValue::Track(_) => PortType::Track,
        PortValue::TrackList(_) => PortType::TrackList,
        PortValue::Detections(_) => PortType::DetectionList,
        PortValue::Part(_) => PortType::PartState,
        PortValue::Orders => PortType::Orders,
        PortValue::None | PortValue::Scalar(_) | PortValue::Preset(_) | PortValue::Rung(_) => PortType::Scalar,
    }
}

fn stance_index(s: Stance) -> f64 { match s { Stance::Friend => 0.0, Stance::Neutral => 1.0, Stance::Suspect => 2.0, Stance::Hostile => 3.0 } }
fn domain_index(d: content::enums::Domain) -> f64 { use content::enums::Domain as D; match d { D::Surface => 0.0, D::Subsurface => 1.0, D::Air => 2.0, D::Weapon => 3.0 } }
fn grade_index(g: content::enums::TrackGrade) -> f64 { use content::enums::TrackGrade as G; match g { G::Bearing => 0.0, G::Coarse => 1.0, G::Fine => 2.0, G::Lock => 3.0 } }

/// Port of `sink` a `RuleValue::Set(v)` overrides.
fn set_port(sink: SinkKind, v: &Value) -> Option<u8> {
    match (sink, v) {
        (SinkKind::ShipPreset, Value::Preset(_) | Value::Scalar(_)) => Some(0),
        (SinkKind::TrackIdOverride, Value::Rung(_) | Value::Scalar(_)) => Some(1),
        (_, Value::Preset(_) | Value::Rung(_) | Value::None | Value::Text(_)) => None,
        (SinkKind::ShipPreset | SinkKind::TrackIdOverride, _) => None,
        _ => {
            let sig = library::sink_sig(sink);
            let ty = library::value_type(v);
            let exact = sig.inputs.iter().position(|p| p.ty == ty || (ty == PortType::Scalar && p.ty == PortType::Angle));
            exact.map(|p| p as u8)
        }
    }
}

/// Bind a template node to an instance part: part slot `u16::MAX` means "this mount".
fn bind_template(kind: &NodeKind, params: &BTreeMap<String, Value>, part: Option<PartSlot>) -> (NodeKind, BTreeMap<String, Value>) {
    let mut params = params.clone();
    let Some(p) = part else { return (kind.clone(), params) };
    let b = |s: PartSlot| if s.0 == u16::MAX { p } else { s };
    let kind = match kind {
        NodeKind::Source(SourceKind::PartState(s)) => NodeKind::Source(SourceKind::PartState(b(*s))),
        NodeKind::Source(SourceKind::Detections(s)) => NodeKind::Source(SourceKind::Detections(b(*s))),
        NodeKind::Sink(k) => NodeKind::Sink(match *k {
            SinkKind::MountAimPoint(s) => SinkKind::MountAimPoint(b(s)),
            SinkKind::MountFirePermission(s) => SinkKind::MountFirePermission(b(s)),
            SinkKind::MissileLaunch(s) => SinkKind::MissileLaunch(b(s)),
            SinkKind::MissileTarget(s) => SinkKind::MissileTarget(b(s)),
            other => other,
        }),
        other => other.clone(),
    };
    if let Some(Value::Scalar(x)) = params.get("part") { if *x == u16::MAX as f64 { params.insert("part".into(), Value::Scalar(p.0 as f64)); } }
    (kind, params)
}

fn allowed_layer(ruleset: &Ruleset) -> ControlLayer { ruleset.kind.max_layer() }

/// Compile a design's graph: generate defaults, apply `overrides.pinned` (nodes and every edge touching one),
/// swap replaced stages for their subgraphs, compile `rules` into condition and select nodes ahead of their
/// sinks, add script nodes, check port types, sort topologically (ties by node id), assign value and memory
/// slots and record the fallback of every non-default writer. Under a ruleset whose
/// `RulesetKind::max_layer` is below a node's layer, that node is dropped with `GraphIssue::LayerNotAllowed`
/// (standard PvP uses stock fire control). Script nodes need `Ruleset::luau`. Fragments of player-authored
/// (embedded, non-stock) kits count as graph edits. Stage parameters come from the design's
/// `OfficerSettings` (`officers::tuning::stage_params`). Never fails: every problem is an issue and the
/// affected sink falls back to its officer stage. Unit: `sys-graph-compile`.
pub fn compile(facts: &DesignFacts<'_>, overrides: &GraphOverrides, rules: &[Rule], ruleset: &Ruleset) -> CompiledGraph {
    compile_with(facts, &facts.blueprint.officers, overrides, rules, ruleset)
}

/// [`compile`] with explicit officer settings instead of the blueprint's: for a recompile after an in-sortie
/// L0 or L1 change (`ShipDesign::recompile`), where the live settings differ from the stored ones. Added by
/// `sys-graph-compile` (additive).
pub fn compile_with(facts: &DesignFacts<'_>, settings: &OfficerSettings, overrides: &GraphOverrides, rules: &[Rule], ruleset: &Ruleset) -> CompiledGraph {
    let max = allowed_layer(ruleset);
    let mut dropped = Vec::new();
    let keep = |p: &content::blueprint::KitPlacement| max >= ControlLayer::L3Graph || generate::kit_is_stock(facts, p);
    for (i, p) in facts.blueprint.kits.iter().enumerate() {
        if !keep(p) && generate::kit_def(facts, p).is_some_and(|d| !d.graph.nodes.is_empty()) {
            let id = match p.parts.first() {
                Some(s) => generate::fragment_id(generate::GROUP_KIT, s.0.saturating_add(1).min(0x3FF), 0),
                None => generate::fragment_id(generate::GROUP_KIT_PARTLESS, (i as u16).saturating_add(1).min(0x3FF), 0),
            };
            if let Some(id) = id { dropped.push(GraphIssue::LayerNotAllowed { node: id }); }
        }
    }
    let env = Env { gin: generate::input_from(facts, &keep), settings, part_count: facts.blueprint.parts.len(), compartment_count: facts.prefab.compartments.compartments.len() };
    let mut g = compile_env(&env, overrides, rules, max, ruleset.luau);
    for i in dropped { if !g.issues.contains(&i) { g.issues.push(i); } }
    g
}

pub(crate) fn compile_env(env: &Env<'_>, overrides: &GraphOverrides, rules: &[Rule], max_layer: ControlLayer, luau: bool) -> CompiledGraph {
    let mut ctx = Ctx { env, scripts: &overrides.scripts, nodes: BTreeMap::new(), issues: Vec::new(), luau, max_layer, rule_sources: BTreeMap::new(), rule_counter: 0 };
    let generated = generate::generate_from(&env.gin);
    ctx.load_generated(&generated);
    if !overrides.pinned.nodes.is_empty() { ctx.apply_pinned(&overrides.pinned); }
    if !overrides.replaced.is_empty() { ctx.apply_replacements(overrides); }
    ctx.apply_rules(rules);
    ctx.type_check();
    let order = ctx.break_cycles();
    emit(&ctx, &order, overrides)
}

fn param_values(n: &WNode, baked: &BTreeMap<(OfficerStage, &'static str), f64>) -> Vec<PortValue> {
    let specs = library::params_of(&n.kind);
    let stage = if let NodeKind::Stage(s) = n.kind { Some(s) } else { None };
    specs.iter().map(|p| {
        let own = match n.params.get(p.key) { Some(Value::Scalar(x)) if x.is_finite() => Some(*x), Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }), _ => None };
        let set = stage.and_then(|s| baked.get(&(s, p.key)).copied());
        let v = if n.own_params_first { own.or(set) } else { set.or(own) }.unwrap_or(p.default);
        PortValue::Scalar(if p.min <= p.max { v.clamp(p.min, p.max) } else { v })
    }).collect()
}

/// Control layer the stored settings put each officer at (L1 when anything of theirs is tuned).
fn settings_layers(s: &OfficerSettings) -> BTreeMap<OfficerRole, ControlLayer> {
    let mut m: BTreeMap<OfficerRole, ControlLayer> = OfficerRole::ALL.iter().map(|r| (*r, ControlLayer::L0Doctrine)).collect();
    let mut l1 = |r: OfficerRole| { m.insert(r, ControlLayer::L1Tune); };
    for (k, v) in &s.sliders { if v.is_finite() { if let Some(def) = crate::officers::tuning::slider(k) { l1(def.role); } } }
    if s.emcon != crate::officers::tuning::preset_emcon(s.preset) || s.burst_trigger_range_m.is_some() || !s.radar_hz.is_empty() { l1(OfficerRole::Sensors); }
    if s.self_defence_range_m.is_some() || !s.doctrine.is_empty() || !s.mount_doctrine.is_empty() || !s.mount_limits.is_empty() { l1(OfficerRole::Gunnery); }
    if !s.power_priority.is_empty() { l1(OfficerRole::Engineer); }
    if !s.repair_order.is_empty() { l1(OfficerRole::DamageControl); }
    m
}

fn emit(ctx: &Ctx<'_, '_>, order: &[GraphNodeId], overrides: &GraphOverrides) -> CompiledGraph {
    let baked = crate::officers::tuning::stage_params(ctx.env.settings);
    let index: BTreeMap<GraphNodeId, u32> = order.iter().enumerate().map(|(i, id)| (*id, i as u32)).collect();
    let mut base: BTreeMap<GraphNodeId, u32> = BTreeMap::new();
    let mut cursor = 0u32;
    let mut mem = 0u32;
    let mut nodes = Vec::with_capacity(order.len());
    for id in order {
        let n = &ctx.nodes[id];
        base.insert(*id, cursor);
        let out_count = n.out_types.len().min(library::MAX_PORTS) as u8;
        let stateful = matches!(n.kind, NodeKind::Math(MathOp::Latch | MathOp::Timer | MathOp::Counter | MathOp::Pid | MathOp::LowPass | MathOp::AlphaBeta));
        let mem_slot = if stateful { mem += 1; Some(mem - 1) } else { None };
        nodes.push(CompiledNode {
            id: *id, kind: n.kind.clone(), origin: n.origin, officer: n.officer, part: n.part, inputs: Vec::new(),
            params: param_values(n, &baked), out_base: cursor, out_count, mem_slot, fallback: n.fallback.and_then(|f| index.get(&f).copied()),
        });
        cursor += out_count as u32;
    }
    for (ci, id) in order.iter().enumerate() {
        let n = &ctx.nodes[id];
        nodes[ci].inputs = n.inputs.iter().enumerate().map(|(p, w)| match w {
            Wire::Edge(s, sp) => match (base.get(s), ctx.nodes.get(s)) {
                (Some(b), Some(src)) if (*sp as usize) < src.out_types.len().min(library::MAX_PORTS) => InputSrc::Slot(b + *sp as u32),
                _ => InputSrc::Const(n.in_defaults.get(p).cloned().unwrap_or_default()),
            },
            Wire::Const(v) => InputSrc::Const(v.clone()),
            Wire::Default => InputSrc::Const(n.in_defaults.get(p).cloned().unwrap_or_default()),
        }).collect();
    }
    // Officer of player nodes without one: the first consumer's.
    let mut consumers: BTreeMap<GraphNodeId, Vec<usize>> = BTreeMap::new();
    for (ci, id) in order.iter().enumerate() { for w in &ctx.nodes[id].inputs { if let Wire::Edge(s, _) = w { consumers.entry(*s).or_default().push(ci); } } }
    for ci in (0..nodes.len()).rev() {
        if nodes[ci].officer.is_some() { continue; }
        let found = consumers.get(&nodes[ci].id).and_then(|cs| cs.iter().find_map(|c| nodes[*c].officer));
        nodes[ci].officer = found;
    }

    let mut sinks: Vec<(SinkKind, u32)> = nodes.iter().enumerate().filter(|(_, n)| !is_shadow(n.id)).filter_map(|(i, n)| if let NodeKind::Sink(k) = n.kind { Some((k, i as u32)) } else { None }).collect();
    sinks.sort_by_key(|s| s.0);
    sinks.dedup_by_key(|s| s.0);
    let scripts: Vec<u32> = nodes.iter().enumerate().filter(|(_, n)| !is_shadow(n.id) && matches!(n.kind, NodeKind::Script(_))).map(|(i, _)| i as u32).collect();

    let mut layer_by_role = settings_layers(ctx.env.settings);
    for n in nodes.iter().filter(|n| !is_shadow(n.id)) {
        let layer = match n.origin {
            NodeOrigin::Pinned | NodeOrigin::Replacement(_) => ControlLayer::L3Graph,
            NodeOrigin::Rule(_) => ControlLayer::L2Rules,
            NodeOrigin::Script(_) => ControlLayer::L4Script,
            NodeOrigin::Generated | NodeOrigin::Kit(_) => if n.fallback.is_some() { ControlLayer::L3Graph } else { continue },
        };
        if let Some(r) = n.officer { let e = layer_by_role.entry(r).or_default(); if layer > *e { *e = layer; } }
    }
    let script_enabled = |n: &CompiledNode| match n.kind { NodeKind::Script(i) => overrides.scripts.get(i as usize).is_some_and(|s| s.enabled), _ => true };
    let node_evals_per_tick = nodes.iter().filter(|n| !is_shadow(n.id) && script_enabled(n)).count() as u32;

    CompiledGraph {
        nodes, value_slots: cursor, mem_slots: mem, sinks, scripts, script_sources: overrides.scripts.clone(),
        issues: ctx.issues.clone(), layer_by_role, node_evals_per_tick,
    }
}

/// Index of the node producing value slot `slot`.
pub fn producer_of(graph: &CompiledGraph, slot: u32) -> Option<usize> {
    let i = graph.nodes.partition_point(|n| n.out_base + n.out_count as u32 <= slot);
    graph.nodes.get(i).filter(|n| n.out_base <= slot && slot < n.out_base + n.out_count as u32).map(|_| i)
}
/// Index of the active (non-shadow) node with this id.
pub fn node_index(graph: &CompiledGraph, id: GraphNodeId) -> Option<usize> { if is_shadow(id) { None } else { graph.nodes.iter().position(|n| n.id == id) } }

/// The L3 view of a compiled graph: generated plus pinned plus rule nodes as an editable `ControlGraph`
/// (rule nodes carry their rule index so the editor shows them as rules). Shadows are hidden; a replaced
/// stage is shown as the stock stage with its own inputs, its outputs read by the same consumers. Replacement
/// and script nodes are included. Rule nodes carry their index in a `rule` parameter. Unit: `sys-graph-compile`.
pub fn to_view(graph: &CompiledGraph) -> ControlGraph {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut depth = vec![0u32; graph.nodes.len()];
    let mut rows: BTreeMap<u32, u32> = BTreeMap::new();
    for (i, n) in graph.nodes.iter().enumerate() {
        if is_shadow(n.id) { continue; }
        // A proxy shows as the stock stage it stands for.
        let shown = if is_proxy(n) { n.fallback.and_then(|f| graph.nodes.get(f as usize)).unwrap_or(n) } else { n };
        let mut params = BTreeMap::new();
        for (spec, v) in library::params_of(&shown.kind).iter().zip(&shown.params) { params.insert(spec.key.to_string(), v.to_value()); }
        if let NodeOrigin::Rule(r) = n.origin { params.insert("rule".to_string(), Value::Scalar(r as f64)); }
        if let (NodeKind::Stage(st), Some(p)) = (&shown.kind, shown.part) { if library::stage_is_per_part(*st) { params.insert("part".to_string(), Value::Scalar(p.0 as f64)); } }
        let mut d = 0;
        for (port, src) in shown.inputs.iter().enumerate() {
            let InputSrc::Slot(s) = src else { continue };
            let Some(pi) = producer_of(graph, *s) else { continue };
            let p = &graph.nodes[pi];
            d = d.max(depth[pi] + 1);
            edges.push(GraphEdge { from: PortRef { node: GraphNodeId(p.id.0 & !SHADOW_BIT), port: (*s - p.out_base) as u8 }, to: PortRef { node: n.id, port: port as u8 } });
        }
        depth[i] = d;
        let row = rows.entry(d).or_insert(0);
        let pos = [d as f32 * 240.0, *row as f32 * 90.0];
        *row += 1;
        nodes.push(GraphNode { id: n.id, kind: shown.kind.clone(), params, pos });
    }
    nodes.sort_by_key(|n| n.id);
    edges.sort();
    edges.dedup();
    ControlGraph { nodes, edges }
}

/// Pin a node the player edited: copy it and every edge touching it into `overrides.pinned`. Returns issues
/// when the edit is illegal, and then leaves `overrides` unchanged: an id the generated graph does not have
/// below `USER_NODE_BASE`, a player stage or sink that duplicates a generated one, an edge to a node that
/// exists nowhere, a port outside a fixed signature, a type mismatch, or a cycle. Edges that do not touch
/// `node` are ignored. Used by the editor's graph view. Unit: `sys-graph-compile`.
pub fn pin_node(overrides: &mut GraphOverrides, generated: &ControlGraph, node: content::graph::GraphNode, edges: &[content::graph::GraphEdge]) -> Vec<GraphIssue> {
    let mut issues = Vec::new();
    let id = node.id;
    if is_shadow(id) { return vec![GraphIssue::TypeMismatch { node: id, port: WHOLE_NODE }]; }
    let gen_ids: BTreeMap<GraphNodeId, &GraphNode> = generated.nodes.iter().map(|n| (n.id, n)).collect();
    if id.0 < USER_NODE_BASE {
        if !gen_ids.contains_key(&id) { return vec![GraphIssue::PinnedLostInput { node: id, port: WHOLE_NODE }]; }
    } else {
        let twin = match &node.kind {
            NodeKind::Stage(st) => Some(generate::stage_node(*st, generate::node_part(&node))),
            NodeKind::Sink(k) => Some(generate::sink_node(*k)),
            _ => None,
        };
        if twin.is_some_and(|t| gen_ids.contains_key(&t)) { return vec![GraphIssue::TypeMismatch { node: id, port: WHOLE_NODE }]; }
    }
    if let NodeKind::Script(i) = node.kind { if overrides.scripts.get(i as usize).is_none() { return vec![GraphIssue::ScriptDisabled { script: i }]; } }
    let pinned_ids: BTreeMap<GraphNodeId, &GraphNode> = overrides.pinned.nodes.iter().filter(|n| n.id != id).map(|n| (n.id, n)).collect();
    let kind_of = |nid: GraphNodeId| -> Option<NodeKind> { if nid == id { Some(node.kind.clone()) } else { pinned_ids.get(&nid).or(gen_ids.get(&nid)).map(|n| n.kind.clone()) } };
    // Signatures that do not depend on the design (stages and scripts are checked at compile time).
    let fixed = |k: &NodeKind| -> Option<library::NodeSigLite> { library::NodeSigLite::of(k) };
    let mine: Vec<GraphEdge> = edges.iter().copied().filter(|e| e.from.node == id || e.to.node == id).collect();
    for e in &mine {
        let (Some(fk), Some(tk)) = (kind_of(e.from.node), kind_of(e.to.node)) else {
            issues.push(GraphIssue::PinnedLostInput { node: id, port: if e.to.node == id { e.to.port } else { WHOLE_NODE } });
            continue;
        };
        let (fs, ts) = (fixed(&fk), fixed(&tk));
        if let Some(fs) = &fs { if e.from.port as usize >= fs.outputs.len() { issues.push(GraphIssue::TypeMismatch { node: e.from.node, port: e.from.port }); continue; } }
        if let Some(ts) = &ts { if e.to.port as usize >= ts.inputs.len() { issues.push(GraphIssue::TypeMismatch { node: e.to.node, port: e.to.port }); continue; } }
        if let (Some(fs), Some(ts)) = (&fs, &ts) {
            let generic = tk == NodeKind::Math(MathOp::Select) && e.to.port > 0 || fk == NodeKind::Math(MathOp::Select);
            if !generic && !library::compatible(fs.outputs[e.from.port as usize], ts.inputs[e.to.port as usize]) { issues.push(GraphIssue::TypeMismatch { node: e.to.node, port: e.to.port }); }
        }
    }
    // Cycle test over the merged graph as it would compile.
    let driven: BTreeSet<u8> = mine.iter().filter(|e| e.to.node == id).map(|e| e.to.port).collect();
    let mut adj: BTreeMap<GraphNodeId, Vec<GraphNodeId>> = BTreeMap::new();
    let all = generated.edges.iter().filter(|e| !(e.to.node == id && driven.contains(&e.to.port)))
        .chain(overrides.pinned.edges.iter().filter(|e| e.from.node != id && e.to.node != id))
        .chain(mine.iter());
    for e in all { adj.entry(e.from.node).or_default().push(e.to.node); }
    let mut stack = vec![id];
    let mut seen = BTreeSet::new();
    let mut cyclic = false;
    while let Some(n) = stack.pop() {
        for m in adj.get(&n).map(|v| v.as_slice()).unwrap_or(&[]) {
            if *m == id { cyclic = true; break; }
            if seen.insert(*m) { stack.push(*m); }
        }
        if cyclic { break; }
    }
    if cyclic { issues.push(GraphIssue::Cycle { node: id }); }
    if !issues.is_empty() { return issues; }
    overrides.pinned.nodes.retain(|n| n.id != id);
    overrides.pinned.edges.retain(|e| e.from.node != id && e.to.node != id);
    overrides.pinned.nodes.push(node);
    overrides.pinned.nodes.sort_by_key(|n| n.id);
    overrides.pinned.edges.extend(mine);
    overrides.pinned.edges.sort();
    overrides.pinned.edges.dedup();
    // One edge per input port: the newest pin wins.
    let mut by_port: BTreeMap<PortRef, GraphEdge> = BTreeMap::new();
    for e in overrides.pinned.edges.iter() { by_port.entry(e.to).and_modify(|x| if e.from.node == id || e.to.node == id { *x = *e }).or_insert(*e); }
    overrides.pinned.edges = by_port.into_values().collect();
    overrides.pinned.edges.sort();
    Vec::new()
}

/// Remove a pin so the node returns to its generated form (one-click revert, L351). Edges that also touch
/// another pinned node stay with that node. Unit: `sys-graph-compile`.
pub fn unpin_node(overrides: &mut GraphOverrides, node: content::ids::GraphNodeId) -> bool {
    let before = overrides.pinned.nodes.len();
    overrides.pinned.nodes.retain(|n| n.id != node);
    if overrides.pinned.nodes.len() == before { return false; }
    let still: BTreeSet<GraphNodeId> = overrides.pinned.nodes.iter().map(|n| n.id).collect();
    overrides.pinned.edges.retain(|e| still.contains(&e.from.node) || still.contains(&e.to.node));
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::graph::generate::tests::{frigate, Design};
    use content::graph::{Cmp, Condition, StageReplacement};

    pub(crate) fn env<'a>(d: &'a Design, settings: &'a OfficerSettings) -> Env<'a> { Env { gin: d.input(), settings, part_count: 16, compartment_count: 6 } }
    pub(crate) fn compile_d(d: &Design, ov: &GraphOverrides, rules: &[Rule]) -> CompiledGraph {
        let s = OfficerSettings::default();
        compile_env(&env(d, &s), ov, rules, ControlLayer::L3Graph, true)
    }
    fn idx(g: &CompiledGraph, id: GraphNodeId) -> usize { node_index(g, id).unwrap_or_else(|| panic!("no node {id:?}")) }
    fn src_of(g: &CompiledGraph, node: GraphNodeId, port: usize) -> Option<GraphNodeId> {
        match &g.nodes[idx(g, node)].inputs[port] { InputSrc::Slot(s) => producer_of(g, *s).map(|i| g.nodes[i].id), _ => None }
    }
    pub(crate) fn gun_rule(slot: u16, range_m: f64) -> Rule {
        Rule { when: Condition::Compare { lhs: Operand::NearestContactRangeM(TrackFilter::default()), cmp: Cmp::Lt, rhs: Operand::Const(range_m) }, sink: SinkKind::MountFirePermission(PartSlot(slot)), value: RuleValue::NearestContact(TrackFilter::default()), enabled: true }
    }

    #[test]
    fn default_graph_compiles_clean() {
        let d = frigate();
        let g = compile_d(&d, &GraphOverrides::default(), &[]);
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        assert_eq!(g, compile_d(&d, &GraphOverrides::default(), &[]), "deterministic");
        // Topological: every input slot is produced earlier.
        for (i, n) in g.nodes.iter().enumerate() {
            for s in &n.inputs { if let InputSrc::Slot(s) = s { assert!(producer_of(&g, *s).unwrap() < i); } }
            assert!(n.fallback.is_none());
        }
        assert!(g.sinks.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(g.node_evals_per_tick as usize, g.nodes.len());
        assert!(g.layer_by_role.values().all(|l| *l == ControlLayer::L0Doctrine));
        // Stage params baked from the default (cautious) preset.
        let rank = &g.nodes[idx(&g, generate::stage_node(OfficerStage::ThreatRanking, None))];
        assert_eq!(rank.params[0], PortValue::Scalar(0.5));
        // The view round-trips to the generated graph's nodes and edges.
        let view = to_view(&g);
        let gen = generate::generate_from(&d.input());
        assert_eq!(view.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), gen.nodes.iter().map(|n| n.id).collect::<Vec<_>>());
        assert_eq!(view.edges, gen.edges);
    }

    #[test]
    fn pinned_node_shadows_generated_and_loses_input() {
        let d = frigate();
        let rank = generate::stage_node(OfficerStage::ThreatRanking, None);
        let mut ov = GraphOverrides::default();
        let mut params = BTreeMap::new();
        params.insert("gunnery.aggression".to_string(), Value::Scalar(0.9));
        ov.pinned.nodes.push(GraphNode { id: rank, kind: NodeKind::Stage(OfficerStage::ThreatRanking), params, pos: [0.0, 0.0] });
        let g = compile_d(&d, &ov, &[]);
        let n = &g.nodes[idx(&g, rank)];
        assert_eq!(n.origin, NodeOrigin::Pinned);
        assert_eq!(n.params[0], PortValue::Scalar(0.9));
        let fb = &g.nodes[n.fallback.unwrap() as usize];
        assert_eq!(fb.id, shadow(rank));
        assert_eq!(fb.params[0], PortValue::Scalar(0.5));
        // Unwired ports keep the generated wiring.
        assert_eq!(src_of(&g, rank, 0), Some(generate::stage_node(OfficerStage::TrackFusion, None)));
        assert_eq!(g.layer_by_role[&OfficerRole::Gunnery], ControlLayer::L3Graph);
        assert_eq!(g.node_evals_per_tick as usize, g.nodes.len() - 1);
        // Pin the solver of a gun that is later removed: a Ship's Note, not a crash.
        let mut ov = GraphOverrides::default();
        let gone = generate::stage_node(OfficerStage::BallisticSolver, Some(PartSlot(14)));
        ov.pinned.nodes.push(GraphNode { id: gone, kind: NodeKind::Stage(OfficerStage::BallisticSolver), params: BTreeMap::new(), pos: [0.0, 0.0] });
        let g = compile_d(&d, &ov, &[]);
        assert!(g.issues.contains(&GraphIssue::PinnedLostInput { node: gone, port: WHOLE_NODE }));
        // A user node reading a removed radar's detections loses its input.
        let mut ov = GraphOverrides::default();
        let user = GraphNodeId(USER_NODE_BASE + 1);
        ov.pinned.nodes.push(GraphNode { id: user, kind: NodeKind::Math(MathOp::Abs), params: BTreeMap::new(), pos: [0.0, 0.0] });
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: generate::generated_id(2, 18, Some(PartSlot(13)), 0), port: 1 }, to: PortRef { node: user, port: 0 } });
        let g = compile_d(&d, &ov, &[]);
        assert!(g.issues.contains(&GraphIssue::PinnedLostInput { node: user, port: 0 }));
    }

    #[test]
    fn user_edge_into_generated_node_gets_fallback() {
        let d = frigate();
        let mut ov = GraphOverrides::default();
        let user = GraphNodeId(USER_NODE_BASE + 7);
        ov.pinned.nodes.push(GraphNode { id: user, kind: NodeKind::Source(SourceKind::Constant(Value::Scalar(1.0))), params: BTreeMap::new(), pos: [0.0, 0.0] });
        let heading = generate::sink_node(SinkKind::HelmHeading);
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: user, port: 0 }, to: PortRef { node: heading, port: 0 } });
        let g = compile_d(&d, &ov, &[]);
        let n = &g.nodes[idx(&g, heading)];
        assert_eq!(n.origin, NodeOrigin::Generated);
        assert_eq!(g.nodes[n.fallback.unwrap() as usize].id, shadow(heading));
        assert_eq!(src_of(&g, heading, 0), Some(user));
        assert_eq!(g.sinks.iter().filter(|s| s.0 == SinkKind::HelmHeading).count(), 1);
        // A type mismatch is reported and the port falls back to its default.
        let mut ov2 = ov.clone();
        ov2.pinned.nodes[0].kind = NodeKind::Source(SourceKind::Constant(Value::Vec3([0.0; 3])));
        let g = compile_d(&d, &ov2, &[]);
        assert!(g.issues.contains(&GraphIssue::TypeMismatch { node: heading, port: 0 }));
    }

    #[test]
    fn rules_splice_top_outermost_and_clamp_stance() {
        let d = frigate();
        let mut friendly = gun_rule(2, 4_000.0);
        friendly.value = RuleValue::NearestContact(TrackFilter { min_stance: Stance::Friend, ..Default::default() });
        let rules = vec![gun_rule(2, 4_000.0), friendly, Rule { when: Condition::All(vec![]), sink: SinkKind::Pump(PartSlot(40)), value: RuleValue::Set(Value::Bool(true)), enabled: true }];
        let g = compile_d(&d, &GraphOverrides::default(), &rules);
        assert!(g.issues.contains(&GraphIssue::RuleSinkMissing { rule: 2 }));
        let solver = generate::stage_node(OfficerStage::BallisticSolver, Some(PartSlot(2)));
        let outer = src_of(&g, solver, 0).unwrap();
        assert_eq!(g.nodes[idx(&g, outer)].origin, NodeOrigin::Rule(0), "top rule outermost");
        let inner = src_of(&g, outer, 2).unwrap();
        assert_eq!(g.nodes[idx(&g, inner)].origin, NodeOrigin::Rule(1));
        assert_eq!(src_of(&g, inner, 2), Some(generate::stage_node(OfficerStage::MountAssignment, None)));
        // Every track source a rule hands a weapon filters Suspect or worse.
        for n in &g.nodes {
            if n.kind == NodeKind::Source(SourceKind::TrackList) { assert!(matches!(n.params[0], PortValue::Scalar(x) if x >= 2.0)); }
        }
        assert!(g.nodes.iter().filter(|n| matches!(n.origin, NodeOrigin::Rule(_))).all(|n| n.id.0 >> 20 == 5));
        assert_eq!(g.layer_by_role[&OfficerRole::Gunnery], ControlLayer::L2Rules);
        let too_many: Vec<Rule> = (0..40).map(|_| gun_rule(3, 100.0)).collect();
        let g = compile_d(&d, &GraphOverrides::default(), &too_many);
        assert!(g.issues.contains(&GraphIssue::TooManyRules(40)));
        assert!(g.nodes.iter().all(|n| !matches!(n.origin, NodeOrigin::Rule(r) if r >= 32)));
    }

    #[test]
    fn nearest_contact_rules_need_a_contact() {
        let d = frigate();
        let mut r = gun_rule(2, 4_000.0);
        r.when = Condition::All(vec![]);
        let g = compile_d(&d, &GraphOverrides::default(), &[r]);
        let solver = generate::stage_node(OfficerStage::BallisticSolver, Some(PartSlot(2)));
        let sel = src_of(&g, solver, 0).unwrap();
        let and = src_of(&g, sel, 0).unwrap();
        assert_eq!(g.nodes[idx(&g, and)].kind, NodeKind::Math(MathOp::And));
        let cmp = src_of(&g, and, 1).unwrap();
        assert_eq!(g.nodes[idx(&g, cmp)].kind, NodeKind::Math(MathOp::Compare(Cmp::Gt)));
    }

    #[test]
    fn set_rules_create_missing_sinks() {
        let d = frigate();
        let rules = vec![
            Rule { when: Condition::Compare { lhs: Operand::HullHealth, cmp: Cmp::Lt, rhs: Operand::Const(0.5) }, sink: SinkKind::ShipEmcon, value: RuleValue::Set(Value::Emcon(content::enums::Emcon::Active)), enabled: true },
            Rule { when: Condition::Not(Box::new(Condition::Any(vec![]))), sink: SinkKind::TrackStanceOverride, value: RuleValue::Set(Value::Stance(Stance::Hostile)), enabled: true },
            Rule { when: Condition::Any(vec![]), sink: SinkKind::HelmSpeed, value: RuleValue::Set(Value::Stance(Stance::Hostile)), enabled: true },
        ];
        let g = compile_d(&d, &GraphOverrides::default(), &rules);
        assert!(g.sinks.iter().any(|s| s.0 == SinkKind::ShipEmcon));
        let ts = generate::sink_node(SinkKind::TrackStanceOverride);
        assert!(src_of(&g, ts, 0).is_some() && src_of(&g, ts, 1).is_some());
        assert!(g.issues.iter().any(|i| matches!(i, GraphIssue::TypeMismatch { node, .. } if node.0 >> 20 == 5)), "{:?}", g.issues);
    }

    #[test]
    fn standard_ruleset_drops_graph_layers() {
        let d = frigate();
        let s = OfficerSettings::default();
        let mut ov = GraphOverrides::default();
        let rank = generate::stage_node(OfficerStage::ThreatRanking, None);
        ov.pinned.nodes.push(GraphNode { id: rank, kind: NodeKind::Stage(OfficerStage::ThreatRanking), params: BTreeMap::new(), pos: [0.0, 0.0] });
        let g = compile_env(&env(&d, &s), &ov, &[gun_rule(2, 4_000.0)], ControlLayer::L2Rules, false);
        assert!(g.issues.contains(&GraphIssue::LayerNotAllowed { node: rank }));
        assert_eq!(g.nodes[idx(&g, rank)].origin, NodeOrigin::Generated);
        assert!(g.nodes.iter().any(|n| matches!(n.origin, NodeOrigin::Rule(0))), "rules stay legal in standard");
    }

    #[test]
    fn template_replacement_covers_every_mount() {
        let d = frigate();
        // Replace every ballistic solver with "aim at nothing, fire only while the stock fire input holds".
        let wide = generate::stage_node(OfficerStage::BallisticSolver, None);
        let not = GraphNodeId(USER_NODE_BASE + 1);
        let sub = ControlGraph {
            nodes: vec![GraphNode { id: not, kind: NodeKind::Math(MathOp::Not), params: BTreeMap::new(), pos: [0.0, 0.0] }],
            edges: vec![
                GraphEdge { from: PortRef { node: wide, port: 1 }, to: PortRef { node: not, port: 0 } },
                GraphEdge { from: PortRef { node: not, port: 0 }, to: PortRef { node: wide, port: 1 } },
            ],
        };
        let ov = GraphOverrides { replaced: vec![StageReplacement { stage: OfficerStage::BallisticSolver, subgraph: sub }], ..Default::default() };
        let g = compile_d(&d, &ov, &[]);
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        for slot in [2u16, 3] {
            let sid = generate::stage_node(OfficerStage::BallisticSolver, Some(PartSlot(slot)));
            let proxy = &g.nodes[idx(&g, sid)];
            assert!(is_proxy(proxy));
            let stock = &g.nodes[proxy.fallback.unwrap() as usize];
            assert_eq!(stock.id, shadow(sid));
            // Output 1 (fire) is driven by this mount's own template instance; output 0 reads the stock.
            let driver = src_of(&g, sid, 1).unwrap();
            assert_eq!(generate::id_fields(driver).0, generate::GROUP_TEMPLATE);
            assert_eq!(generate::id_part(driver), Some(PartSlot(slot)));
            assert_eq!(src_of(&g, sid, 0), Some(shadow(sid)));
            assert_eq!(src_of(&g, driver, 0), Some(generate::stage_node(OfficerStage::MountAssignment, None)));
        }
        assert_eq!(g.layer_by_role[&OfficerRole::Gunnery], ControlLayer::L3Graph);
        let view = to_view(&g);
        assert!(view.nodes.iter().all(|n| !is_shadow(n.id)));
        let sid = generate::stage_node(OfficerStage::BallisticSolver, Some(PartSlot(2)));
        assert!(view.edges.iter().any(|e| e.to.node == sid && e.from.node == generate::stage_node(OfficerStage::MountAssignment, None)));
    }

    #[test]
    fn cycles_drop_the_player_nodes() {
        let d = frigate();
        // Feed the threat ranking's own output back into its input through a pinned edge.
        let rank = generate::stage_node(OfficerStage::ThreatRanking, None);
        let mut ov = GraphOverrides::default();
        ov.pinned.nodes.push(GraphNode { id: rank, kind: NodeKind::Stage(OfficerStage::ThreatRanking), params: BTreeMap::new(), pos: [0.0, 0.0] });
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: rank, port: 0 }, to: PortRef { node: rank, port: 0 } });
        let g = compile_d(&d, &ov, &[]);
        assert!(g.issues.contains(&GraphIssue::Cycle { node: rank }));
        let n = &g.nodes[idx(&g, rank)];
        assert_eq!(n.origin, NodeOrigin::Generated);
        assert!(n.fallback.is_none());
        assert!(!g.nodes.iter().any(|n| is_shadow(n.id)));
    }

    #[test]
    fn pin_and_unpin() {
        let d = frigate();
        let gen = generate::generate_from(&d.input());
        let mut ov = GraphOverrides::default();
        let user = GraphNodeId(USER_NODE_BASE + 3);
        let heading = generate::sink_node(SinkKind::HelmHeading);
        let node = GraphNode { id: user, kind: NodeKind::Source(SourceKind::Constant(Value::Scalar(1.0))), params: BTreeMap::new(), pos: [0.0, 0.0] };
        let e = GraphEdge { from: PortRef { node: user, port: 0 }, to: PortRef { node: heading, port: 0 } };
        assert!(pin_node(&mut ov, &gen, node.clone(), &[e]).is_empty());
        assert_eq!(ov.pinned.nodes.len(), 1);
        assert_eq!(ov.pinned.edges, vec![e]);
        // Illegal: wrong type, unknown generated id, duplicate sink, a cycle.
        let bad = GraphNode { kind: NodeKind::Source(SourceKind::OwnShip), ..node.clone() };
        assert!(!pin_node(&mut ov, &gen, bad, &[e]).is_empty());
        assert!(!pin_node(&mut ov, &gen, GraphNode { id: GraphNodeId(5), ..node.clone() }, &[]).is_empty());
        assert!(!pin_node(&mut ov, &gen, GraphNode { id: GraphNodeId(USER_NODE_BASE + 9), kind: NodeKind::Sink(SinkKind::HelmSpeed), params: BTreeMap::new(), pos: [0.0; 2] }, &[]).is_empty());
        let rank = generate::stage_node(OfficerStage::ThreatRanking, None);
        let fusion = generate::stage_node(OfficerStage::TrackFusion, None);
        let loop_edge = GraphEdge { from: PortRef { node: rank, port: 0 }, to: PortRef { node: fusion, port: 0 } };
        let fusion_node = gen.nodes.iter().find(|n| n.id == fusion).unwrap().clone();
        assert!(pin_node(&mut ov, &gen, fusion_node, &[loop_edge]).contains(&GraphIssue::Cycle { node: fusion }));
        assert_eq!(ov.pinned.nodes.len(), 1, "illegal edits change nothing");
        assert!(unpin_node(&mut ov, user));
        assert!(ov.pinned.edges.is_empty());
        assert!(!unpin_node(&mut ov, user));
    }
}
