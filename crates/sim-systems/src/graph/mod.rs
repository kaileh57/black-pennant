//! Contract-owned types (no work unit). The ship control graph at run time. Units in this directory:
//! `library.rs` = `sys-graph-lib`, `generate.rs` = `sys-graph-gen`, `compile.rs` = `sys-graph-compile`,
//! `vm.rs` = `sys-graph-vm` (hard).
//! DESIGN.md L277-279 (one graph per vessel; defaults are nodes, not hidden code), L320 (regeneration and
//! pinning), L322-336 (control ladder, rule semantics), L338-349 (node library), L369-373 (execution order).
//!
//! The stored form is `content::graph` (pinned nodes, replaced stages, rules, scripts, officer settings); this
//! crate never defines a second stored form. Flow: `generate` builds the default graph from the placed parts
//! and kit fragments; `compile` merges it with the stored overrides and L2 rules, sorts it topologically
//! (ties by ascending node id) and resolves ports to value slots; `vm` evaluates it at 30 Hz under a
//! work-unit budget. Officer stages are nodes whose bodies are the typed functions in `crate::emcon`,
//! `crate::tracks`, `crate::engage`, `crate::guidance`, `crate::officers`; the VM reaches them through
//! [`StageHost`]. Every layer falls back to the one below: a faulted script, a pinned node that lost its input
//! or a graph over budget hands its sinks to the officer stage it replaced, from that same tick (L324, L371).
//!
//! # Two stages whose work happens outside the per-ship graph tick
//! Per-ship graph ticks run in parallel over an immutable side picture, so two stages are split:
//! - `TrackFusion`: the fusion itself runs once per side at 10 Hz in the Sim's track phase, because the picture
//!   is shared over the datalink. The node in a ship's graph is that ship's window on it: its inputs are the
//!   ship's raw detections (already delivered to the side fusion), its readable outputs are the filter gains
//!   and the track list the ship's later stages consume (the side picture as of the last update; only the
//!   ship's own local tracks while it is cut off from the datalink). A player subgraph that replaces it
//!   supplies the track list this ship's own stages read (it can filter, reorder or select tracks by id); it
//!   cannot create tracks or change the shared picture. Filter gains are fixed by the spec (L468), so the
//!   node has no writable gain parameters.
//! - `PnGuidance`: one node per launcher in the launching ship's graph. Per graph tick it decides launch and
//!   target for that launcher and exposes the navigation constant N (tunable at L1 and L3, L520), which is
//!   captured by each body at launch (`guidance::missile::Guided::pn_n`). Steering of bodies in flight runs
//!   per body in the Sim's guidance phase. A player subgraph that replaces the node drives the launch and
//!   target sinks, may set N, and may supply a midcourse aim point that rides the 1 Hz datalink update while
//!   the launcher holds the track; terminal homing always runs the built-in seeker logic on the missile.
pub mod compile;
pub mod generate;
pub mod library;
pub mod vm;

use crate::prelude::*;
use crate::protocol::event::ScriptFault;
use crate::sensors::Detection;
use content::graph::{NodeKind, OfficerStage, PortType, ScriptSource, SinkKind, SourceKind, Value};

/// A value on a wire at run time. Mirrors `content::graph::PortType`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PortValue {
    #[default] None,
    Bool(bool),
    /// Scalars and angles (radians).
    Scalar(f64),
    Vec3(DVec3),
    Text(Arc<str>),
    Stance(Stance),
    Emcon(Emcon),
    Preset(DoctrinePreset),
    Rung(IdRung),
    Track(TrackId),
    /// Ordered list, for example the threat ranking's output.
    TrackList(Arc<Vec<TrackId>>),
    Detections(Arc<Vec<Detection>>),
    Part(PartSlot),
    /// The ship's standing orders; read through [`StageHost::read_source`].
    Orders,
}
impl PortValue {
    pub fn as_f64(&self) -> f64 { match self { PortValue::Scalar(v) => *v, PortValue::Bool(b) => if *b { 1.0 } else { 0.0 }, _ => 0.0 } }
    pub fn as_bool(&self) -> bool { match self { PortValue::Bool(b) => *b, PortValue::Scalar(v) => *v != 0.0, PortValue::None => false, _ => true } }
    /// Convert a stored constant. Unit: `sys-graph-lib`.
    pub fn from_value(v: &Value) -> PortValue { library::port_value_from(v) }
    /// Convert back for scopes and "current values printed on edges" (L331). Unit: `sys-graph-lib`.
    pub fn to_value(&self) -> Value { library::port_value_to(self) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PortSpec { pub name: &'static str, pub ty: PortType, pub default: PortValue }
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec { pub key: &'static str, pub label: &'static str, pub default: f64, pub min: f64, pub max: f64 }

/// Ports and parameters of one node kind for one design (stage and sink ports can depend on the mount list).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeSig { pub inputs: Vec<PortSpec>, pub outputs: Vec<PortSpec>, pub params: Vec<ParamSpec>, pub stateful: bool }

/// Where a compiled node came from; decides its frame colour, pin glyph and fallback (L320).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeOrigin { Generated, Kit(KitSlot), Pinned, Replacement(OfficerStage), Rule(u8), Script(u16) }

/// Where a node input reads from.
#[derive(Clone, Debug, PartialEq)]
pub enum InputSrc { Const(PortValue), Slot(u32) }

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledNode {
    pub id: GraphNodeId,
    pub kind: NodeKind,
    pub origin: NodeOrigin,
    pub officer: Option<OfficerRole>,
    /// Part the node is bound to (per-mount solver, per-sensor source).
    pub part: Option<PartSlot>,
    pub inputs: Vec<InputSrc>,
    /// Parameter values in signature order.
    pub params: Vec<PortValue>,
    /// First value slot of the outputs and their count.
    pub out_base: u32,
    pub out_count: u8,
    pub mem_slot: Option<u32>,
    /// For replacements, rules and scripts: the node index (in `CompiledGraph::nodes`) of the officer stage
    /// output that drives the same sink when this node is disabled, faulted or over budget.
    pub fallback: Option<u32>,
}

/// A problem found while compiling. Each becomes a Ship's Note; none stops the ship working (L320, L324).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GraphIssue {
    /// A pinned node lost an input (for example its radar was removed): ranked in the tier "a weapon, sensor or
    /// consumer cannot work".
    PinnedLostInput { node: GraphNodeId, port: u8 },
    TypeMismatch { node: GraphNodeId, port: u8 },
    Cycle { node: GraphNodeId },
    RuleSinkMissing { rule: u8 },
    TooManyRules(usize),
    ScriptDisabled { script: u16 },
    LayerNotAllowed { node: GraphNodeId },
}

/// The executable form. Built once per design (and again after a live splash edit); shared by `Arc`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompiledGraph {
    /// Topological order, ties by ascending node id (L369).
    pub nodes: Vec<CompiledNode>,
    pub value_slots: u32,
    pub mem_slots: u32,
    /// Node indices of sink nodes, ascending `SinkKind`.
    pub sinks: Vec<(SinkKind, u32)>,
    /// Node indices of script nodes in execution order; they share one interrupt counter (L369).
    pub scripts: Vec<u32>,
    pub script_sources: Vec<ScriptSource>,
    pub issues: Vec<GraphIssue>,
    /// Highest control layer any node of each officer uses (Bridge Board "control state").
    pub layer_by_role: BTreeMap<OfficerRole, ControlLayer>,
    /// Deterministic node count for Complexity points (L973).
    pub node_evals_per_tick: u32,
}

/// Per-node memory of stateful math nodes. Stage memory lives in the ship's officer states, not here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum NodeMem { #[default] None, Latch(bool), Timer { elapsed_s: f64, running: bool }, Counter(i64), Pid { integral: f64, prev_error: f64 }, LowPass(f64), AlphaBeta { x: f64, v: f64 } }

/// Mutable VM state of one vessel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphState {
    pub values: Vec<PortValue>,
    pub mem: Vec<NodeMem>,
    /// Script nodes greyed out for the rest of the sortie with the cause (L371). Re-armed only in dry dock or
    /// during a splash.
    pub greyed: Vec<(GraphNodeId, ScriptFault)>,
    /// Ports a client is scoping (L216).
    pub scoped: Vec<content::graph::PortRef>,
}

/// Who wrote a sink this tick: decides the A chip or hand glyph and the Bridge Board's layer (L351).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteOrigin { Officer(OfficerRole), Rule(u8), Pinned(GraphNodeId), Replacement(OfficerStage), Script(GraphNodeId) }

/// One sink written this graph tick: the values of the sink node's input ports in signature order.
#[derive(Clone, Debug, PartialEq)]
pub struct SinkWrite { pub sink: SinkKind, pub values: Vec<PortValue>, pub origin: WriteOrigin }

/// Everything the graph decided this tick, ascending `SinkKind`. `crate::ship::controls` turns it into
/// `sim_core::parts::VesselControls` and side-level effects.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SinkWrites { pub writes: Vec<SinkWrite> }
impl SinkWrites { pub fn get(&self, sink: SinkKind) -> Option<&SinkWrite> { self.writes.iter().find(|w| w.sink == sink) } }

/// How the VM reaches everything outside the graph: sources and officer stages. Implemented by
/// `crate::officers::stages::StageRunner` (unit `sys-officer-stages`), which marshals ports to the typed
/// stage functions. Tests may substitute a recording host.
pub trait StageHost {
    /// Fill the outputs of a source node (own-ship state, part state, raw detections of one sensor, the track
    /// list, standing orders, a constant).
    fn read_source(&mut self, source: &SourceKind, out: &mut [PortValue]);
    /// Run one officer stage: `inputs` and `params` in the stage's signature order, results into `out`.
    /// Stages that run slower than 30 Hz (track fusion and engagement at 10 Hz) hold their last outputs.
    fn eval_stage(&mut self, stage: OfficerStage, part: Option<PartSlot>, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]);
    /// Fill the outputs of a source node that has parameters: the `TrackList` source with its filter
    /// (`min_stance`, `domain`, `min_grade`, see `library::source_sig`), which also backs L2 rule operands.
    /// The default fails closed and leaves every output `None`, so a host that does not filter never hands a
    /// rule or a graph the wrong contacts. Added by `sys-graph-vm` (additive).
    fn read_source_with(&mut self, source: &SourceKind, params: &[PortValue], out: &mut [PortValue]) { let _ = (source, params, out); }
    /// The host API scripts may call during this ship's graph tick. `None` (the default) means scripts cannot
    /// reach the ship's view: script nodes then fall back to their officer stages without greying out.
    /// Added by `sys-graph-vm` (additive).
    fn host_api(&mut self) -> Option<&mut dyn crate::script::HostApi> { None }
}
