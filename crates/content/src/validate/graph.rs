//! Structural checks of stored graph content (DESIGN.md L320-397). Content cannot see the generated graph or
//! the node library's design-dependent signatures; sim-systems' compiler type-checks the rest. What is checked
//! here is everything the stored data alone decides:
//! - node ids are unique within each stored graph and carry no shadow bit;
//! - in pinned graphs and kit fragments every edge touches at least one stored node (a replacement subgraph
//!   may wire a generated node straight to the replaced stage's id); everywhere an endpoint with a player id
//!   (at or above [`USER_NODE_BASE`]) is stored (generated ids below it may be absent: they are regenerated);
//! - no edge leaves a sink or enters a source; a constant source has only output 0;
//! - script nodes name an existing script, and script ports exist with types that match where both ends are
//!   known (script ports and constants), by the library's rule: scalars and angles mix, bools and scalars mix;
//! - no input port has two different drivers;
//! - among stored nodes, every cycle passes through a latch or timer (their outputs are last tick's state);
//! - constant parameters are finite; scripts have at most 255 ports per side; a stage is replaced once;
//! - at most [`MAX_RULES`] rules.
use super::Issue;
use crate::graph::{ControlGraph, GraphOverrides, MathOp, NodeKind, PortRef, PortType, Rule, ScriptSource, SourceKind, Value, MAX_RULES, USER_NODE_BASE};
use crate::ids::GraphNodeId;
use std::collections::{BTreeMap, BTreeSet};

/// Shadow-node bit the compiler reserves (`sim_systems::graph::compile::SHADOW_BIT`); never stored.
const SHADOW_BIT: u32 = 1 << 31;
const MAX_PORTS: usize = 255;

fn bad(detail: impl Into<String>) -> Issue { Issue::BadGraph { detail: detail.into() } }

pub(super) fn validate_graph(overrides: &GraphOverrides, rules: &[Rule]) -> Vec<Issue> {
    let mut issues = Vec::new();
    if rules.len() > MAX_RULES {
        issues.push(Issue::TooManyRules(rules.len()));
    }
    for (i, s) in overrides.scripts.iter().enumerate() {
        if s.inputs.len() > MAX_PORTS || s.outputs.len() > MAX_PORTS {
            issues.push(bad(format!("script {i} has more than {MAX_PORTS} ports on one side")));
        }
    }
    check_graph(&overrides.pinned, &overrides.scripts, "pinned graph", false, &mut issues);
    let mut stages = BTreeSet::new();
    for r in &overrides.replaced {
        if !stages.insert(r.stage) {
            issues.push(bad(format!("stage {:?} is replaced twice", r.stage)));
        }
        check_graph(&r.subgraph, &overrides.scripts, &format!("{:?} replacement", r.stage), true, &mut issues);
    }
    issues
}

/// A finite-number check over a stored value.
fn finite_value(v: &Value) -> bool {
    match v {
        Value::Scalar(x) => x.is_finite(),
        Value::Vec3(v) => v.iter().all(|x| x.is_finite()),
        _ => true,
    }
}

/// Port type of a constant, as the node library assigns it (presets and rungs travel on scalar ports).
fn value_type(v: &Value) -> PortType {
    match v {
        Value::None | Value::Scalar(_) | Value::Preset(_) | Value::Rung(_) => PortType::Scalar,
        Value::Bool(_) => PortType::Bool,
        Value::Vec3(_) => PortType::Vec3,
        Value::Text(_) => PortType::Text,
        Value::Stance(_) => PortType::Stance,
        Value::Emcon(_) => PortType::Emcon,
    }
}

/// The node library's wire compatibility rule.
fn compatible(from: PortType, to: PortType) -> bool {
    use PortType as T;
    from == to || matches!((from, to), (T::Scalar, T::Angle) | (T::Angle, T::Scalar) | (T::Bool, T::Scalar) | (T::Scalar, T::Bool))
}

/// Structural check of one stored graph (pinned nodes, a replacement subgraph or a kit fragment).
/// `subgraph` allows edges between two generated (unstored) ids, which a stage replacement uses to read the
/// stage's inputs and drive its outputs.
pub(super) fn check_graph(g: &ControlGraph, scripts: &[ScriptSource], what: &str, subgraph: bool, issues: &mut Vec<Issue>) {
    let mut nodes: BTreeMap<GraphNodeId, &NodeKind> = BTreeMap::new();
    for n in &g.nodes {
        if n.id.0 & SHADOW_BIT != 0 {
            issues.push(bad(format!("{what}: node {} uses the reserved shadow id range", n.id.0)));
        }
        if nodes.insert(n.id, &n.kind).is_some() {
            issues.push(bad(format!("{what}: duplicate node id {}", n.id.0)));
        }
        if let NodeKind::Script(i) = n.kind {
            if i as usize >= scripts.len() {
                issues.push(bad(format!("{what}: node {} names script {i}, which does not exist", n.id.0)));
            }
        }
        if let NodeKind::Source(SourceKind::Constant(v)) = &n.kind {
            if !finite_value(v) {
                issues.push(bad(format!("{what}: node {} holds a non-finite constant", n.id.0)));
            }
        }
        if n.params.values().any(|v| !finite_value(v)) {
            issues.push(bad(format!("{what}: node {} has a non-finite parameter", n.id.0)));
        }
    }
    let script = |k: &NodeKind| match k {
        NodeKind::Script(i) => scripts.get(*i as usize),
        _ => None,
    };
    let mut drivers: BTreeMap<PortRef, PortRef> = BTreeMap::new();
    for e in &g.edges {
        let (from, to) = (nodes.get(&e.from.node).copied(), nodes.get(&e.to.node).copied());
        let label = format!("{what}: edge {}:{} -> {}:{}", e.from.node.0, e.from.port, e.to.node.0, e.to.port);
        if from.is_none() && to.is_none() && !subgraph {
            issues.push(bad(format!("{label} touches no stored node")));
            continue;
        }
        let missing = |id: GraphNodeId, k: Option<&NodeKind>| k.is_none() && id.0 >= USER_NODE_BASE;
        if missing(e.from.node, from) || missing(e.to.node, to) {
            issues.push(bad(format!("{label} names a player node that does not exist")));
            continue;
        }
        let mut from_ty = None;
        if let Some(k) = from {
            match k {
                NodeKind::Sink(_) => {
                    issues.push(bad(format!("{label} leaves a sink")));
                    continue;
                }
                NodeKind::Source(SourceKind::Constant(v)) => {
                    if e.from.port != 0 {
                        issues.push(bad(format!("{label} reads a constant port that does not exist")));
                        continue;
                    }
                    from_ty = Some(value_type(v));
                }
                _ => {}
            }
            if let Some(s) = script(k) {
                match s.outputs.get(e.from.port as usize) {
                    Some((_, ty)) => from_ty = Some(*ty),
                    None => {
                        issues.push(bad(format!("{label} reads a script output that does not exist")));
                        continue;
                    }
                }
            }
        }
        let mut to_ty = None;
        if let Some(k) = to {
            if matches!(k, NodeKind::Source(_)) {
                issues.push(bad(format!("{label} enters a source")));
                continue;
            }
            if let Some(s) = script(k) {
                match s.inputs.get(e.to.port as usize) {
                    Some((_, ty)) => to_ty = Some(*ty),
                    None => {
                        issues.push(bad(format!("{label} drives a script input that does not exist")));
                        continue;
                    }
                }
            }
        }
        if let (Some(a), Some(b)) = (from_ty, to_ty) {
            if !compatible(a, b) {
                issues.push(bad(format!("{label} joins a {a:?} port to a {b:?} port")));
                continue;
            }
        }
        match drivers.get(&e.to) {
            Some(prev) if *prev != e.from => issues.push(bad(format!("{label}: the input already has a driver"))),
            Some(_) => {}
            None => {
                drivers.insert(e.to, e.from);
            }
        }
    }
    if has_unbroken_cycle(g, &nodes) {
        issues.push(bad(format!("{what}: a cycle does not pass through a latch or timer")));
    }
}

/// Kahn's algorithm over edges between stored nodes, leaving out edges from latches and timers (their outputs
/// are state from the previous tick, so feedback through them is legal).
fn has_unbroken_cycle(g: &ControlGraph, nodes: &BTreeMap<GraphNodeId, &NodeKind>) -> bool {
    let index: BTreeMap<GraphNodeId, usize> = nodes.keys().enumerate().map(|(i, id)| (*id, i)).collect();
    let kinds: Vec<&NodeKind> = nodes.values().copied().collect();
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); index.len()];
    let mut indeg = vec![0usize; index.len()];
    let mut seen = BTreeSet::new();
    for e in &g.edges {
        let (Some(&a), Some(&b)) = (index.get(&e.from.node), index.get(&e.to.node)) else { continue };
        if matches!(kinds[a], NodeKind::Math(MathOp::Latch | MathOp::Timer)) || !seen.insert((a, b)) {
            continue;
        }
        out[a].push(b);
        indeg[b] += 1;
    }
    let mut ready: Vec<usize> = (0..indeg.len()).filter(|&i| indeg[i] == 0).collect();
    let mut done = 0usize;
    while let Some(i) = ready.pop() {
        done += 1;
        for &j in &out[i] {
            indeg[j] -= 1;
            if indeg[j] == 0 {
                ready.push(j);
            }
        }
    }
    done < indeg.len()
}
