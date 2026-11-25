//! Unit `sys-graph-vm` (hard: budgeted execution with layered fallback; determinism).
//! The game's own bytecode VM for control graphs; it cannot reach the host (L2115).
//! DESIGN.md L324 (each layer falls back to the one below), L336 (rules every graph tick), L369-373 (order,
//! script failure handling), L416 (control share of the tick), L973 (graph node evaluations are work units).
//!
//! One call evaluates one vessel's graph for one 30 Hz graph tick, nodes in the compiled order. Budgets are
//! node evaluations and Luau interrupts, never elapsed time. On a script fault (throw, memory, budget): every
//! output it wrote this tick is discarded, it greys out for the rest of the sortie, and the officer stage it
//! replaced drives those sinks from this same tick; later script nodes that did not run this tick fall back
//! for this one tick and run normally next tick. A wall-clock backstop trip (reported by the script host)
//! disables the script and marks the tick not reproducible.
//!
//! # Liveness and fallback
//! Every value slot is live or dead for the tick. Officer-layer nodes (generated and kit) always run and are
//! counted but never cut by the budget, and their count is reserved out of the budget first. Player-layer
//! nodes (`compile::is_player`) run, in order, only while what is left of the budget lasts and every input is
//! live; otherwise their outputs are dead. A node with a `fallback` never has dead
//! outputs: when it cannot run it copies the outputs of its fallback node, which is a lazy shadow evaluated on
//! demand at that point (`compile::SHADOW_BIT`). Rule splices pass their `else` through unless their rule's
//! condition is live and true. So whatever the budget, faults or missing inputs, every sink is written by the
//! highest layer that could run, and by the officers otherwise.
//!
//! # Provenance
//! Each slot also carries who decided it: player nodes stamp their own origin (pinned, replacement, rule while
//! it holds, script), officer nodes pass on the first player origin among their inputs. A sink write's
//! `WriteOrigin` is the first player origin among its inputs, else its own if it is a pinned sink, else the
//! officer's.
use super::compile::{self, is_player, is_proxy, is_shadow, is_splice};
use super::library;
use super::{CompiledGraph, CompiledNode, GraphState, InputSrc, NodeMem, NodeOrigin, PortValue, SinkWrite, SinkWrites, StageHost, WriteOrigin};
use crate::protocol::event::ScriptFault;
use crate::script::ScriptHost;
use content::graph::{NodeKind, SourceKind};
use content::ids::GraphNodeId;
use sim_core::EntityId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmBudget { pub node_evals: u32, pub luau_interrupts: u32 }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VmResult {
    pub sinks: SinkWrites,
    /// Work units used (Complexity points and tracing spans).
    pub node_evals: u32,
    pub luau_interrupts: u32,
    /// Scripts that faulted this tick.
    pub faults: Vec<(GraphNodeId, ScriptFault)>,
    /// The node-evaluation budget ran out; remaining sinks came from officer stages.
    pub over_budget: bool,
    /// A script was stopped only by the wall-clock backstop.
    pub not_reproducible: bool,
}

/// Prepare state for a compiled graph (value and memory slots, defaults), keeping greyed-out scripts greyed.
/// Scopes carry over; node memory starts fresh because slots move when a graph is recompiled.
/// Unit: `sys-graph-vm`.
pub fn init_state(graph: &CompiledGraph, previous: Option<&GraphState>) -> GraphState {
    GraphState {
        values: vec![PortValue::None; graph.value_slots as usize],
        mem: vec![NodeMem::None; graph.mem_slots as usize],
        greyed: previous.map(|p| p.greyed.clone()).unwrap_or_default(),
        scoped: previous.map(|p| p.scoped.clone()).unwrap_or_default(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Done { No, Busy, Yes }

/// Recursion bound for lazy pulls (a shadow's inputs are always active nodes, so one level is the norm).
const MAX_PULL_DEPTH: u32 = 16;

struct Vm<'g, 'h, 's, 'st> {
    vessel: EntityId,
    graph: &'g CompiledGraph,
    state: &'st mut GraphState,
    host: &'h mut dyn StageHost,
    scripts: Option<&'s mut dyn ScriptHost>,
    budget: VmBudget,
    dt: f64,
    live: Vec<bool>,
    prov: Vec<Option<WriteOrigin>>,
    done: Vec<Done>,
    /// Per sink node: (values, all live, provenance).
    sink_vals: Vec<Option<(Vec<PortValue>, bool, Option<WriteOrigin>)>>,
    res: VmResult,
    /// A script faulted this tick: later scripts fall back for the rest of the tick.
    halted: bool,
    script_begun: bool,
    /// Player-layer evaluations allowed this tick: the budget minus the officer layer's fixed share, which is
    /// reserved up front so the cut does not depend on where player nodes fall in the order.
    player_allowance: u32,
    player_evals: u32,
    pool: Vec<Vec<PortValue>>,
}

impl Vm<'_, '_, '_, '_> {
    fn buf(&mut self, n: usize) -> Vec<PortValue> { let mut v = self.pool.pop().unwrap_or_default(); v.clear(); v.resize(n, PortValue::None); v }
    fn give(&mut self, v: Vec<PortValue>) { if self.pool.len() < 16 { self.pool.push(v); } }

    fn own_origin(n: &CompiledNode) -> Option<WriteOrigin> {
        match n.origin {
            NodeOrigin::Generated | NodeOrigin::Kit(_) => None,
            NodeOrigin::Pinned => Some(WriteOrigin::Pinned(n.id)),
            NodeOrigin::Replacement(s) => Some(WriteOrigin::Replacement(s)),
            NodeOrigin::Rule(r) => Some(WriteOrigin::Rule(r)),
            NodeOrigin::Script(_) => Some(WriteOrigin::Script(n.id)),
        }
    }

    /// Gather a node's inputs, evaluating lazy producers first.
    fn gather(&mut self, i: usize, depth: u32) -> (Vec<PortValue>, Vec<bool>, Vec<Option<WriteOrigin>>) {
        let graph = self.graph;
        let n = &graph.nodes[i];
        let count = n.inputs.len();
        let mut vals = self.buf(count);
        let mut live = vec![true; count];
        let mut prov = vec![None; count];
        for (p, src) in n.inputs.iter().enumerate() {
            match src {
                InputSrc::Const(v) => vals[p] = v.clone(),
                InputSrc::Slot(s) => {
                    let s = *s as usize;
                    if let Some(pi) = compile::producer_of(graph, s as u32) {
                        if self.done[pi] == Done::No && is_shadow(graph.nodes[pi].id) { self.eval(pi, depth + 1); }
                    }
                    if let Some(v) = self.state.values.get(s) { vals[p] = v.clone(); }
                    live[p] = self.live.get(s).copied().unwrap_or(false);
                    prov[p] = self.prov.get(s).copied().flatten();
                }
            }
        }
        (vals, live, prov)
    }

    fn write_out(&mut self, i: usize, out: &[PortValue], live: bool, prov: Option<WriteOrigin>) {
        let n = &self.graph.nodes[i];
        let base = n.out_base as usize;
        for j in 0..n.out_count as usize {
            let v = if live { out.get(j).cloned().map(library::sanitize).unwrap_or_default() } else { PortValue::None };
            if let Some(slot) = self.state.values.get_mut(base + j) { *slot = v; }
            if let Some(l) = self.live.get_mut(base + j) { *l = live; }
            if let Some(p) = self.prov.get_mut(base + j) { *p = prov; }
        }
    }

    /// Copy the outputs of fallback node `f` (evaluating it if it is lazy) into node `i`.
    fn use_fallback(&mut self, i: usize, f: usize, depth: u32) {
        if self.done[f] == Done::No { self.eval(f, depth + 1); }
        let (n, fb) = (&self.graph.nodes[i], &self.graph.nodes[f]);
        if matches!(n.kind, NodeKind::Sink(_)) {
            let vals = self.sink_vals[f].clone().map(|(v, _, p)| (v, true, p));
            self.sink_vals[i] = vals;
            return;
        }
        let (base, fbase) = (n.out_base as usize, fb.out_base as usize);
        for j in 0..n.out_count as usize {
            let (v, p) = if j < fb.out_count as usize && self.live.get(fbase + j).copied().unwrap_or(false) {
                (self.state.values.get(fbase + j).cloned().unwrap_or_default(), self.prov.get(fbase + j).copied().flatten())
            } else { (PortValue::None, None) };
            if let Some(slot) = self.state.values.get_mut(base + j) { *slot = v; }
            if let Some(l) = self.live.get_mut(base + j) { *l = true; }
            if let Some(pp) = self.prov.get_mut(base + j) { *pp = p; }
        }
    }

    fn budget_left(&self) -> bool { self.player_evals < self.player_allowance }

    fn eval(&mut self, i: usize, depth: u32) {
        if self.done[i] != Done::No { return; }
        self.done[i] = Done::Busy;
        if depth > MAX_PULL_DEPTH { self.dead(i); self.done[i] = Done::Yes; return; }
        let graph = self.graph;
        let n = &graph.nodes[i];
        let (vals, live, prov) = self.gather(i, depth);
        let all_live = live.iter().all(|l| *l);
        let fallback = n.fallback.map(|f| f as usize).filter(|f| *f < graph.nodes.len() && *f != i);
        let first_player = prov.iter().copied().flatten().next();

        // Sinks only record their inputs.
        if let NodeKind::Sink(_) = n.kind {
            self.res.node_evals += 1;
            if !all_live {
                if let Some(f) = fallback { self.give(vals); self.use_fallback(i, f, depth); self.done[i] = Done::Yes; return; }
            }
            let origin = first_player.or_else(|| if n.origin == NodeOrigin::Pinned { Some(WriteOrigin::Pinned(n.id)) } else { None });
            let vals_c = vals.clone();
            self.give(vals);
            self.sink_vals[i] = Some((vals_c, all_live, origin));
            self.done[i] = Done::Yes;
            return;
        }

        // Rule splices: the rule's value while its condition holds, else what lies below.
        if is_splice(n) {
            self.res.node_evals += 1;
            let holds = live.first().copied().unwrap_or(false) && live.get(1).copied().unwrap_or(false) && vals.first().is_some_and(|c| c.as_bool() && *c != PortValue::None);
            let (v, l, p) = if holds { (vals.get(1).cloned().unwrap_or_default(), true, Self::own_origin(n)) } else { (vals.get(2).cloned().unwrap_or_default(), live.get(2).copied().unwrap_or(false), prov.get(2).copied().flatten()) };
            self.give(vals);
            self.write_out(i, &[v], l, p);
            self.done[i] = Done::Yes;
            return;
        }

        // Replacement proxies: the subgraph's values for the stage's outputs, or the stock stage.
        if is_proxy(n) {
            if all_live {
                self.res.node_evals += 1;
                let p = first_player.or_else(|| Self::own_origin(n));
                self.write_out(i, &vals, true, p);
                self.give(vals);
            } else if let Some(f) = fallback {
                self.give(vals);
                self.use_fallback(i, f, depth);
            } else {
                self.give(vals);
                self.dead(i);
            }
            self.done[i] = Done::Yes;
            return;
        }

        let player = is_player(n);
        let mut runnable = all_live || !player && fallback.is_none();
        if runnable && player && !self.budget_left() { self.res.over_budget = true; runnable = false; }
        if !all_live && !player && fallback.is_some() { runnable = false; }
        if let (true, NodeKind::Script(idx)) = (runnable, &n.kind) { runnable = self.script_may_run(n.id, *idx); }

        if !runnable {
            self.give(vals);
            match fallback { Some(f) => self.use_fallback(i, f, depth), None => self.dead(i) }
            self.done[i] = Done::Yes;
            return;
        }

        let mut out = self.buf(n.out_count as usize);
        let ok = match &n.kind {
            NodeKind::Source(SourceKind::Constant(v)) => { if let Some(o) = out.first_mut() { *o = PortValue::from_value(v); } true }
            NodeKind::Source(s) => { if n.params.is_empty() { self.host.read_source(s, &mut out); } else { self.host.read_source_with(s, &n.params, &mut out); } true }
            NodeKind::Math(op) => {
                let mut scratch = NodeMem::None;
                let mem = match n.mem_slot { Some(m) => self.state.mem.get_mut(m as usize).unwrap_or(&mut scratch), None => &mut scratch };
                library::eval_math(*op, &vals, &n.params, mem, self.dt, &mut out);
                true
            }
            NodeKind::Stage(st) => { self.host.eval_stage(*st, n.part, &vals, &n.params, &mut out); true }
            NodeKind::Script(idx) => self.run_script(i, *idx, &vals, &mut out),
            NodeKind::Sink(_) => unreachable!("sinks handled above"),
        };
        self.give(vals);
        if ok {
            self.res.node_evals += 1;
            if player { self.player_evals += 1; }
            let p = if player { Self::own_origin(n) } else { first_player };
            self.write_out(i, &out, true, p);
        } else {
            match fallback { Some(f) => self.use_fallback(i, f, depth), None => self.dead(i) }
        }
        self.give(out);
        self.done[i] = Done::Yes;
    }

    fn dead(&mut self, i: usize) { self.write_out(i, &[], false, None); }

    /// Whether script node `id` may run this tick at all; when it may not, it falls back without greying.
    fn script_may_run(&self, id: GraphNodeId, idx: u16) -> bool {
        if self.scripts.is_none() || self.halted { return false; }
        if self.state.greyed.iter().any(|(g, _)| *g == id) { return false; }
        self.graph.script_sources.get(idx as usize).is_some_and(|s| s.enabled)
    }

    /// Run one script node. Returns false (outputs discarded) on a fault or when the host gives no API.
    fn run_script(&mut self, i: usize, idx: u16, inputs: &[PortValue], out: &mut [PortValue]) -> bool {
        let id = self.graph.nodes[i].id;
        let Some(scripts) = self.scripts.as_deref_mut() else { return false };
        let Some(api) = self.host.host_api() else { return false };
        if !self.script_begun { scripts.begin_tick(self.vessel); self.script_begun = true; }
        let mut tmp: Vec<PortValue> = vec![PortValue::None; out.len()];
        let run = scripts.run(self.vessel, idx, inputs, api, &mut tmp);
        self.res.luau_interrupts = self.res.luau_interrupts.saturating_add(run.interrupts);
        let fault = run.fault.or(if self.res.luau_interrupts > self.budget.luau_interrupts { Some(ScriptFault::Budget) } else { None });
        if let Some(f) = fault {
            self.res.faults.push((id, f));
            if !self.state.greyed.iter().any(|(g, _)| *g == id) { self.state.greyed.push((id, f)); }
            if f == ScriptFault::Backstop { self.res.not_reproducible = true; }
            self.halted = true;
            return false;
        }
        // Typed ports: a value that does not fit its declared type is no value.
        let types: Vec<content::graph::PortType> = self.graph.script_sources.get(idx as usize).map(|s| s.outputs.iter().map(|o| o.1).collect()).unwrap_or_default();
        for (j, v) in tmp.into_iter().enumerate() {
            let v = library::sanitize(v);
            if let Some(o) = out.get_mut(j) { *o = if types.get(j).is_some_and(|t| library::value_fits(&v, *t)) { v } else { PortValue::None }; }
        }
        true
    }
}

/// Evaluate one graph tick. `dt` is the graph period (1/30 s). `scripts` is `None` wherever scripts may not
/// run (clients, PvP lobbies, guests before the fuzz gate): script nodes then fall back as if faulted but are
/// not greyed. Unit: `sys-graph-vm`.
pub fn run_tick(vessel: EntityId, graph: &CompiledGraph, state: &mut GraphState, host: &mut dyn StageHost, scripts: Option<&mut dyn ScriptHost>, budget: VmBudget, dt: f64) -> VmResult {
    if state.values.len() != graph.value_slots as usize || state.mem.len() != graph.mem_slots as usize {
        let fresh = init_state(graph, Some(&*state));
        *state = fresh;
    }
    let slots = graph.value_slots as usize;
    let n = graph.nodes.len();
    let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let mut vm = Vm {
        vessel, graph, state, host, scripts, budget, dt,
        live: vec![false; slots], prov: vec![None; slots], done: vec![Done::No; n], sink_vals: vec![None; n],
        res: VmResult::default(), halted: false, script_begun: false, pool: Vec::new(),
        player_allowance: budget.node_evals.saturating_sub(graph.nodes.iter().filter(|n| !is_shadow(n.id) && !is_player(n)).count() as u32),
        player_evals: 0,
    };
    for i in 0..n { if !is_shadow(graph.nodes[i].id) { vm.eval(i, 0); } }

    let mut writes = Vec::with_capacity(graph.sinks.len());
    for (kind, idx) in &graph.sinks {
        let Some(node) = graph.nodes.get(*idx as usize) else { continue };
        let Some((vals, _, origin)) = vm.sink_vals.get(*idx as usize).cloned().flatten() else { continue };
        let values: Vec<PortValue> = vals.into_iter().enumerate().map(|(p, v)| library::normalize_sink_value(*kind, p, v)).collect();
        if values.iter().all(|v| *v == PortValue::None) { continue; }
        let origin = origin.unwrap_or(WriteOrigin::Officer(node.officer.unwrap_or_else(|| library::sink_officer(*kind))));
        writes.push(SinkWrite { sink: *kind, values, origin });
    }
    let mut res = vm.res;
    res.sinks = SinkWrites { writes };
    res
}

/// Clear greyed-out scripts (dry dock or splash only, L371). Unit: `sys-graph-vm`.
pub fn rearm(state: &mut GraphState) { state.greyed.clear(); }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::compile::tests::{compile_d, gun_rule};
    use crate::graph::generate::{self, tests::frigate};
    use crate::graph::{GraphIssue, PortValue};
    use crate::ids::TrackId;
    use crate::script::{HostApi, ScriptLimits, ScriptRun};
    use content::enums::OfficerRole;
    use content::graph::{ControlGraph, GraphEdge, GraphNode, GraphOverrides, MathOp, OfficerStage, PortRef, PortType, ScriptSource, SinkKind, Value, USER_NODE_BASE};
    use content::ids::PartSlot;
    use glam::DVec3;
    use std::collections::BTreeMap;

    /// A deterministic host: stages write recognisable values; the track list's nearest contact is
    /// track 9 at `range` metres, filtered by the min_stance parameter (Friend-admitting filters see none).
    #[derive(Default)]
    struct Host { range: f64, calls: Vec<OfficerStage>, api: Api }
    #[derive(Default)]
    struct Api;
    impl HostApi for Api { fn call(&mut self, _index: u16, _args: &[PortValue]) -> Result<PortValue, ScriptFault> { Ok(PortValue::None) } }
    impl StageHost for Host {
        fn read_source(&mut self, source: &SourceKind, out: &mut [PortValue]) {
            if let SourceKind::TheatreOrders = source { if let Some(o) = out.get_mut(0) { *o = PortValue::Orders; } if let Some(o) = out.get_mut(1) { *o = PortValue::Track(TrackId(4)); } }
            if let SourceKind::OwnShip = source { if let Some(o) = out.get_mut(7) { *o = PortValue::Scalar(0.4); } }
        }
        fn read_source_with(&mut self, source: &SourceKind, params: &[PortValue], out: &mut [PortValue]) {
            if *source != SourceKind::TrackList { return; }
            let friendly_ok = params.first().is_some_and(|p| p.as_f64() < 2.0);
            assert!(!friendly_ok, "rules never ask for friendly contacts");
            out[1] = PortValue::Scalar(1.0);
            out[2] = PortValue::Track(TrackId(9));
            out[3] = PortValue::Scalar(self.range);
        }
        fn eval_stage(&mut self, stage: OfficerStage, part: Option<PartSlot>, inputs: &[PortValue], _params: &[PortValue], out: &mut [PortValue]) {
            self.calls.push(stage);
            match stage {
                OfficerStage::MountAssignment => for (j, o) in out.iter_mut().enumerate() { *o = if j % 2 == 0 { PortValue::Track(TrackId(1)) } else { PortValue::Bool(false) }; },
                OfficerStage::BallisticSolver => {
                    // Aim at "the target's number" so tests can see which track the solver was given.
                    let t = match inputs.first() { Some(PortValue::Track(t)) => t.0 as f64, _ => -1.0 };
                    out[0] = PortValue::Vec3(DVec3::new(t, part.map(|p| p.0 as f64).unwrap_or(0.0), 0.0));
                    out[1] = inputs.get(1).cloned().unwrap_or_default();
                }
                OfficerStage::HelmAutopilot => { out[0] = PortValue::Scalar(1.0); out[1] = PortValue::Scalar(12.0); }
                OfficerStage::StationKeeping | OfficerStage::Evasion => { out[0] = PortValue::Scalar(2.0); out[1] = PortValue::Scalar(5.0); out[2] = PortValue::Bool(false); }
                _ => for o in out.iter_mut() { *o = PortValue::Bool(true); },
            }
        }
        fn host_api(&mut self) -> Option<&mut dyn HostApi> { Some(&mut self.api) }
    }

    const BIG: VmBudget = VmBudget { node_evals: 100_000, luau_interrupts: 5_000 };
    const DT: f64 = 1.0 / 30.0;

    fn tick(g: &CompiledGraph, st: &mut GraphState, host: &mut Host, budget: VmBudget) -> VmResult { run_tick(EntityId::default(), g, st, host, None, budget, DT) }

    #[test]
    fn officers_drive_every_sink() {
        let d = frigate();
        let g = compile_d(&d, &GraphOverrides::default(), &[]);
        let mut st = init_state(&g, None);
        let mut host = Host::default();
        let r = tick(&g, &mut st, &mut host, BIG);
        assert_eq!(r.node_evals, g.node_evals_per_tick);
        assert!(!r.over_budget);
        let head = r.sinks.get(SinkKind::HelmHeading).unwrap();
        assert_eq!(head.values, vec![PortValue::Scalar(1.0)], "autopilot wins while station keeping and evasion are idle");
        assert_eq!(head.origin, WriteOrigin::Officer(OfficerRole::Helm));
        let aim = r.sinks.get(SinkKind::MountAimPoint(PartSlot(3))).unwrap();
        assert_eq!(aim.values, vec![PortValue::Vec3(DVec3::new(1.0, 3.0, 0.0))]);
        assert!(r.sinks.writes.windows(2).all(|w| w[0].sink < w[1].sink));
        // Same inputs, same outputs.
        let mut st2 = init_state(&g, None);
        assert_eq!(r, tick(&g, &mut st2, &mut Host::default(), BIG));
    }

    #[test]
    fn rule_overrides_while_holding_then_officer_returns() {
        let d = frigate();
        let g = compile_d(&d, &GraphOverrides::default(), &[gun_rule(2, 4_000.0)]);
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        let mut st = init_state(&g, None);
        let mut host = Host { range: 3_000.0, ..Default::default() };
        let r = tick(&g, &mut st, &mut host, BIG);
        let fire = r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap();
        assert_eq!(fire.values, vec![PortValue::Bool(true), PortValue::Track(TrackId(9))]);
        assert_eq!(fire.origin, WriteOrigin::Rule(0));
        // The solver was retargeted, so the aim point follows the rule's contact.
        let aim = r.sinks.get(SinkKind::MountAimPoint(PartSlot(2))).unwrap();
        assert_eq!(aim.values, vec![PortValue::Vec3(DVec3::new(9.0, 2.0, 0.0))]);
        assert_eq!(aim.origin, WriteOrigin::Rule(0));
        // The other gun is untouched.
        assert_eq!(r.sinks.get(SinkKind::MountFirePermission(PartSlot(3))).unwrap().origin, WriteOrigin::Officer(OfficerRole::Gunnery));
        // The tick after the contact opens beyond 4 km the officer's output returns.
        host.range = 5_000.0;
        let r = tick(&g, &mut st, &mut host, BIG);
        let fire = r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap();
        assert_eq!(fire.values, vec![PortValue::Bool(false), PortValue::Track(TrackId(1))]);
        assert_eq!(fire.origin, WriteOrigin::Officer(OfficerRole::Gunnery));
        // A host that cannot filter contacts (the trait default) leaves the rule silent.
        struct Plain(Host);
        impl StageHost for Plain {
            fn read_source(&mut self, s: &SourceKind, o: &mut [PortValue]) { self.0.read_source(s, o) }
            fn eval_stage(&mut self, a: OfficerStage, b: Option<PartSlot>, c: &[PortValue], d: &[PortValue], e: &mut [PortValue]) { self.0.eval_stage(a, b, c, d, e) }
        }
        let mut plain = Plain(Host { range: 1_000.0, ..Default::default() });
        let r = run_tick(EntityId::default(), &g, &mut st, &mut plain, None, BIG, DT);
        assert_eq!(r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap().origin, WriteOrigin::Officer(OfficerRole::Gunnery));
        // Even with an always-true condition, no matching contact means the officer keeps the mount.
        let mut always = gun_rule(2, 4_000.0);
        always.when = content::graph::Condition::All(vec![]);
        let g = compile_d(&d, &GraphOverrides::default(), &[always]);
        let mut st = init_state(&g, None);
        let mut plain = Plain(Host::default());
        let r = run_tick(EntityId::default(), &g, &mut st, &mut plain, None, BIG, DT);
        let fire = r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap();
        assert_eq!(fire.values, vec![PortValue::Bool(false), PortValue::Track(TrackId(1))]);
        assert_eq!(fire.origin, WriteOrigin::Officer(OfficerRole::Gunnery));
        assert_eq!(r.sinks.get(SinkKind::MountAimPoint(PartSlot(2))).unwrap().values, vec![PortValue::Vec3(DVec3::new(1.0, 2.0, 0.0))]);
        // With a contact, the same rule takes the mount.
        let r = run_tick(EntityId::default(), &g, &mut st, &mut Host { range: 9_000.0, ..Default::default() }, None, BIG, DT);
        assert_eq!(r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap().origin, WriteOrigin::Rule(0));
    }

    #[test]
    fn over_budget_hands_sinks_to_officers() {
        let d = frigate();
        let g = compile_d(&d, &GraphOverrides::default(), &[gun_rule(2, 4_000.0)]);
        let mut st = init_state(&g, None);
        let mut host = Host { range: 3_000.0, ..Default::default() };
        let officer_nodes = g.nodes.iter().filter(|n| !compile::is_player(n) && !is_shadow(n.id)).count() as u32;
        let r = tick(&g, &mut st, &mut host, VmBudget { node_evals: officer_nodes, luau_interrupts: 0 });
        assert!(r.over_budget);
        let fire = r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap();
        assert_eq!(fire.origin, WriteOrigin::Officer(OfficerRole::Gunnery));
        assert_eq!(fire.values[0], PortValue::Bool(false));
    }

    #[test]
    fn pinned_node_falls_back_lazily() {
        let d = frigate();
        // Pin the helm-speed sink to a user low-pass filter of a constant: the shadow does not run normally.
        let speed = generate::sink_node(SinkKind::HelmSpeed);
        let k = GraphNodeId(USER_NODE_BASE + 1);
        let lp = GraphNodeId(USER_NODE_BASE + 2);
        let mut ov = GraphOverrides::default();
        ov.pinned.nodes.push(GraphNode { id: k, kind: NodeKind::Source(SourceKind::Constant(Value::Scalar(20.0))), params: BTreeMap::new(), pos: [0.0; 2] });
        ov.pinned.nodes.push(GraphNode { id: lp, kind: NodeKind::Math(MathOp::LowPass), params: BTreeMap::new(), pos: [0.0; 2] });
        ov.pinned.nodes.push(GraphNode { id: speed, kind: NodeKind::Sink(SinkKind::HelmSpeed), params: BTreeMap::new(), pos: [0.0; 2] });
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: k, port: 0 }, to: PortRef { node: lp, port: 0 } });
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: lp, port: 0 }, to: PortRef { node: speed, port: 0 } });
        let g = compile_d(&d, &ov, &[]);
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        let mut st = init_state(&g, None);
        let mut host = Host::default();
        let r = tick(&g, &mut st, &mut host, BIG);
        let w = r.sinks.get(SinkKind::HelmSpeed).unwrap();
        assert_eq!(w.values, vec![PortValue::Scalar(20.0)]);
        assert_eq!(w.origin, WriteOrigin::Pinned(lp));
        assert_eq!(r.node_evals, g.node_evals_per_tick);
        // Out of budget for the player nodes: the generated speed chain drives the sink this same tick.
        let officer = g.nodes.iter().filter(|n| !compile::is_player(n) && !is_shadow(n.id)).count() as u32;
        let r = tick(&g, &mut st, &mut host, VmBudget { node_evals: officer, luau_interrupts: 0 });
        let w = r.sinks.get(SinkKind::HelmSpeed).unwrap();
        assert_eq!(w.values, vec![PortValue::Scalar(12.0)]);
        assert_eq!(w.origin, WriteOrigin::Officer(OfficerRole::Helm));
        assert!(r.over_budget);
    }

    /// A script host scripted per call: `faults[n]` is the outcome of the n-th run.
    struct Scripts { outcomes: Vec<Option<ScriptFault>>, runs: usize, begun: usize }
    impl ScriptHost for Scripts {
        fn load(&mut self, _v: EntityId, s: &[ScriptSource], _l: ScriptLimits) -> Vec<Result<(), ScriptFault>> { s.iter().map(|_| Ok(())).collect() }
        fn begin_tick(&mut self, _v: EntityId) { self.begun += 1; }
        fn run(&mut self, _v: EntityId, index: u16, _i: &[PortValue], _a: &mut dyn HostApi, out: &mut [PortValue]) -> ScriptRun {
            let f = self.outcomes.get(self.runs).copied().flatten();
            self.runs += 1;
            if f.is_none() { out[0] = PortValue::Scalar(30.0 + index as f64); }
            ScriptRun { fault: f, interrupts: 100 }
        }
        fn unload(&mut self, _v: EntityId) {}
        fn memory_bytes(&self, _v: EntityId) -> usize { 0 }
    }

    fn scripted() -> (CompiledGraph, GraphOverrides) {
        let d = frigate();
        let src = |name: &str| ScriptSource { name: name.into(), source: String::new(), inputs: vec![], outputs: vec![("speed".into(), PortType::Scalar)], enabled: true };
        let speed = generate::sink_node(SinkKind::HelmSpeed);
        let heading = generate::sink_node(SinkKind::HelmHeading);
        let (a, b) = (GraphNodeId(USER_NODE_BASE + 1), GraphNodeId(USER_NODE_BASE + 2));
        let mut ov = GraphOverrides { scripts: vec![src("a"), src("b")], ..Default::default() };
        ov.pinned.nodes.push(GraphNode { id: a, kind: NodeKind::Script(0), params: BTreeMap::new(), pos: [0.0; 2] });
        ov.pinned.nodes.push(GraphNode { id: b, kind: NodeKind::Script(1), params: BTreeMap::new(), pos: [0.0; 2] });
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: a, port: 0 }, to: PortRef { node: speed, port: 0 } });
        ov.pinned.edges.push(GraphEdge { from: PortRef { node: b, port: 0 }, to: PortRef { node: heading, port: 0 } });
        (compile_d(&d, &ov, &[]), ov)
    }

    #[test]
    fn script_fault_greys_out_and_later_scripts_fall_back_once() {
        let (g, _) = scripted();
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        assert_eq!(g.scripts.len(), 2);
        let mut st = init_state(&g, None);
        let mut host = Host::default();
        let mut sh = Scripts { outcomes: vec![Some(ScriptFault::Error), None, None], runs: 0, begun: 0 };
        let r = run_tick(EntityId::default(), &g, &mut st, &mut host, Some(&mut sh), BIG, DT);
        assert_eq!(r.faults, vec![(GraphNodeId(USER_NODE_BASE + 1), ScriptFault::Error)]);
        assert_eq!(sh.runs, 1, "the second script does not run after a fault this tick");
        assert_eq!(sh.begun, 1);
        assert_eq!(r.sinks.get(SinkKind::HelmSpeed).unwrap().values, vec![PortValue::Scalar(12.0)], "officer drives from the same tick");
        assert_eq!(r.sinks.get(SinkKind::HelmHeading).unwrap().origin, WriteOrigin::Officer(OfficerRole::Helm));
        assert_eq!(st.greyed, vec![(GraphNodeId(USER_NODE_BASE + 1), ScriptFault::Error)]);
        // Next tick: the faulted one stays grey, the other runs again.
        let r = run_tick(EntityId::default(), &g, &mut st, &mut host, Some(&mut sh), BIG, DT);
        assert!(r.faults.is_empty());
        assert_eq!(r.sinks.get(SinkKind::HelmHeading).unwrap().values, vec![PortValue::Scalar(31.0)]);
        assert_eq!(r.sinks.get(SinkKind::HelmHeading).unwrap().origin, WriteOrigin::Script(GraphNodeId(USER_NODE_BASE + 2)));
        assert_eq!(r.sinks.get(SinkKind::HelmSpeed).unwrap().origin, WriteOrigin::Officer(OfficerRole::Helm));
        // A recompile keeps it grey; re-arming clears it.
        let st2 = init_state(&g, Some(&st));
        assert_eq!(st2.greyed.len(), 1);
        rearm(&mut st);
        assert!(st.greyed.is_empty());
    }

    #[test]
    fn backstop_and_budget_and_no_host() {
        let (g, _) = scripted();
        let mut st = init_state(&g, None);
        let mut host = Host::default();
        let mut sh = Scripts { outcomes: vec![Some(ScriptFault::Backstop)], runs: 0, begun: 0 };
        let r = run_tick(EntityId::default(), &g, &mut st, &mut host, Some(&mut sh), BIG, DT);
        assert!(r.not_reproducible);
        // Interrupts past the vehicle budget are a budget fault even if the host did not report one.
        let mut st = init_state(&g, None);
        let mut sh = Scripts { outcomes: vec![None, None], runs: 0, begun: 0 };
        let r = run_tick(EntityId::default(), &g, &mut st, &mut host, Some(&mut sh), VmBudget { node_evals: 100_000, luau_interrupts: 150 }, DT);
        assert_eq!(r.luau_interrupts, 200);
        assert_eq!(r.faults, vec![(GraphNodeId(USER_NODE_BASE + 2), ScriptFault::Budget)]);
        // No script host: fall back, never grey.
        let mut st = init_state(&g, None);
        let r = run_tick(EntityId::default(), &g, &mut st, &mut host, None, BIG, DT);
        assert!(r.faults.is_empty() && st.greyed.is_empty());
        assert_eq!(r.sinks.get(SinkKind::HelmSpeed).unwrap().origin, WriteOrigin::Officer(OfficerRole::Helm));
    }

    #[test]
    fn replacement_proxy_and_fallback() {
        use content::graph::StageReplacement;
        let d = frigate();
        // Replace threat ranking with "top = the designated track" for every output except the ranked list.
        let rank = generate::stage_node(OfficerStage::ThreatRanking, None);
        let orders = GraphNodeId(USER_NODE_BASE + 5);
        let sub = ControlGraph {
            nodes: vec![GraphNode { id: orders, kind: NodeKind::Source(SourceKind::TheatreOrders), params: BTreeMap::new(), pos: [0.0; 2] }],
            edges: vec![GraphEdge { from: PortRef { node: orders, port: 1 }, to: PortRef { node: rank, port: 1 } }],
        };
        let ov = GraphOverrides { replaced: vec![StageReplacement { stage: OfficerStage::ThreatRanking, subgraph: sub }], ..Default::default() };
        let g = compile_d(&d, &ov, &[]);
        assert!(g.issues.is_empty(), "{:?}", g.issues);
        let mut st = init_state(&g, None);
        let mut host = Host::default();
        let r = tick(&g, &mut st, &mut host, BIG);
        let pi = compile::node_index(&g, rank).unwrap();
        let p = &g.nodes[pi];
        assert_eq!(st.values[p.out_base as usize + 1], PortValue::Track(TrackId(4)));
        assert_eq!(host.calls.iter().filter(|s| **s == OfficerStage::ThreatRanking).count(), 1, "the stock stage still supplies the undriven outputs");
        assert_eq!(r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap().origin, WriteOrigin::Replacement(OfficerStage::ThreatRanking));
        // Over budget the subgraph's source does not run and the proxy hands back the stock outputs.
        let officer = g.nodes.iter().filter(|n| !compile::is_player(n) && !is_shadow(n.id)).count() as u32;
        let r = tick(&g, &mut st, &mut host, VmBudget { node_evals: officer, luau_interrupts: 0 });
        assert!(r.over_budget);
        assert_eq!(st.values[p.out_base as usize + 1], PortValue::Bool(true));
        assert_eq!(r.sinks.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap().origin, WriteOrigin::Officer(OfficerRole::Gunnery));
        let _ = GraphIssue::Cycle { node: rank };
    }

    #[test]
    fn hostile_state_is_handled() {
        // State sized for another graph is resized, not indexed out of range.
        let d = frigate();
        let g = compile_d(&d, &GraphOverrides::default(), &[]);
        let mut st = GraphState::default();
        let r = run_tick(EntityId::default(), &g, &mut st, &mut Host::default(), None, BIG, f64::NAN);
        assert_eq!(st.values.len(), g.value_slots as usize);
        assert!(r.sinks.get(SinkKind::HelmHeading).is_some());
        // An empty graph does nothing.
        let empty = CompiledGraph::default();
        let mut st = init_state(&empty, None);
        assert_eq!(run_tick(EntityId::default(), &empty, &mut st, &mut Host::default(), None, BIG, DT), VmResult::default());
    }
}
