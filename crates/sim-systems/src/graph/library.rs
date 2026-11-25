//! Unit `sys-graph-lib`. The node library: the signature of every node kind and the bodies of the math and
//! logic nodes. DESIGN.md L338-349.
//!
//! | Group | Nodes | Readable | Writable |
//! | Sources | own-ship state (pose, speed, heel, power, fuel, damage); part state per part; raw detections per
//! sensor with error; track list with uncertainty, ID rung and stance; theatre orders | yes | no |
//! | Math and logic | arithmetic, compare, select, latch, timer, counter, PID, low-pass filter, alpha-beta
//! filter, vector maths | yes | yes |
//! | Officer stages | track fusion, threat ranking, mount assignment, ballistic solver, proportional-navigation
//! guidance, load shedder, damage-control planner, emission controller | yes, with internal gains and per-tick
//! results | parameters; any stage replaceable by a player subgraph |
//! | Sinks | helm heading and speed; mount aim point and fire permission; missile launch and target; emission
//! state per emitter; power priority per consumer; pump and bulkhead commands; manual stance and ID override
//! per track | yes | yes |
//!
//! # Port tables (blueprint format: stored edges name ports by index)
//! Types: B Bool, S Scalar, A Angle (radians; headings and bearings are compass, 0 = north, clockwise), V Vec3
//! (world metres), T Track, TL TrackList, D DetectionList, P PartState, O Orders, E Emcon, St Stance. "x n"
//! means one port per entry of the named design list, in that list's order (ascending slot). A value of `None`
//! means "no value": a sink input left at `None` is "no command" and the part's own default stands.
//!
//! Sources (no inputs):
//! - `OwnShip`: out 0 position V, 1 velocity V, 2 heading A, 3 speed_kn S, 4 heel A (signed), 5 power_margin_kw
//!   S, 6 fuel_fraction S, 7 hull_health S (0..1), 8 inbound_weapons S (inbound weapon tracks), 9 sortie_time_s S.
//! - `PartState(slot)`: out 0 part P, 1 health S (0..1), 2 works B, 3 powered B, 4 ammo_fraction S, 5 train A,
//!   6 elevation A.
//! - `Detections(slot)`: out 0 detections D, 1 count S, 2 sigma_range_m S, 3 sigma_bearing A.
//! - `TrackList`: params `min_stance` (0 Friend .. 3 Hostile, default 2 Suspect), `domain` (-1 any, 0 surface,
//!   1 subsurface, 2 air, 3 weapon), `min_grade` (0 Bearing .. 3 Lock). Out 0 tracks TL (filtered, nearest
//!   first), 1 count S, 2 nearest T, 3 nearest_range_m S, 4 inbound_count S. The VM reads it through
//!   `StageHost::read_source_with` so the host applies the filter.
//! - `TheatreOrders`: out 0 orders O, 1 designated T, 2 speed_kn S, 3 weapons_tight B, 4 evade B,
//!   5 keep_range_m S.
//! - `Constant(v)`: out 0 value, typed by the constant (presets and ID rungs travel on Scalar ports, see below).
//!
//! Officer stages (inputs; outputs; parameters). Stage parameter keys are the L1 slider keys of
//! `crate::officers::tuning` (identity mapping), so the stage runner can give typed stage functions a slider
//! map built straight from the parameters:
//! - `HelmAutopilot`: in 0 orders O; out 0 heading A, 1 speed_kn S, 2 cross_track_m S; params
//!   `helm.responsiveness`, `helm.cruise_speed`, `helm.lookahead`.
//! - `StationKeeping`: in 0 orders O; out 0 heading A, 1 speed_kn S, 2 active B, 3 distance_m S; params
//!   `helm.responsiveness`, `helm.station_radius_m`.
//! - `Evasion`: in 0 orders O, 1 threats TL; out 0 heading A, 1 speed_kn S, 2 active B; params `helm.evasion`.
//! - `TrackFusion`: in detections D x sensor_slots; out 0 tracks TL, 1 count S, 2 gate_sigma S,
//!   3 alpha_fire_control S, 4 alpha_search S, 5 alpha_passive S; no params (gains fixed by L462-468).
//! - `ThreatRanking`: in 0 tracks TL, 1 orders O; out 0 ranked TL, 1 top T, 2 inbound_count S; params
//!   `gunnery.aggression`, `gunnery.self_defence_range_m`.
//! - `MountAssignment`: in 0 ranked TL, 1 orders O; out (target T, fire B) x mounts, then engaged S; params
//!   `gunnery.aggression`, `gunnery.open_fire_range`. Port helper: [`assignment_ports`].
//! - `BallisticSolver` (one per gun mount, bound to the mount's part): in 0 target T, 1 fire B; out
//!   0 aim_point V, 1 fire B, 2 train A, 3 elevation A, 4 time_of_flight_s S, 5 iterations S, 6 residual_m S,
//!   7 converged B, 8 in_range B; params `gunnery.iteration_cap`, `gunnery.converge_m`.
//! - `PnGuidance` (one per launcher, bound to the launcher's part): in 0 target T, 1 fire B; out 0 launch B,
//!   1 target T, 2 n S, 3 aim_point V (midcourse aim; `None` = the track's predicted position); params
//!   `gunnery.pn_surface`, `gunnery.pn_air` (N by target domain, L520).
//! - `LoadShedder`: no inputs; out priority S x consumer_slots, then generation_kw S, demand_kw S,
//!   shed_count S; params `engineer.reserve_margin`, `engineer.restore_delay_s`, `engineer.fuel_reserve`. Port helper: [`shedder_port`].
//! - `DamageControlPlanner`: no inputs; out (pump B, pump_compartment S) x pumps, then door_closed B x doors,
//!   then teams_busy S, flooding S, fires S; params `dc.pump_threshold_m`, `dc.door_discipline`,
//!   `dc.magazine_flood_s`. Port helpers: [`dc_pump_ports`], [`dc_door_port`].
//! - `EmissionController`: in 0 tracks TL, 1 setting E (`None` = the ship's EMCON setting); out
//!   0 radiate_search B, 1 radiate_fire_control B, 2 radiate_point_defence B, 3 radiate_sonar B,
//!   4 radiate_jammer B, 5 lights B, 6 esm_warning B; params `sensors.burst_trigger_range_m`,
//!   `sensors.burst_rotations`, `sensors.burst_repeat_s`, `sensors.pd_radar_range_m`. Emitters of one class
//!   share one output and the per-emitter sinks read their class ([`emission_class_port`]).
//!
//! Sinks (inputs only):
//! - `HelmHeading`: 0 heading A. `HelmSpeed`: 0 speed_kn S.
//! - `MountAimPoint(slot)`: 0 aim_point V. `MountFirePermission(slot)`: 0 fire B, 1 target T.
//! - `MissileLaunch(slot)`: 0 launch B, 1 n S. `MissileTarget(slot)`: 0 target T, 1 aim_point V.
//! - `EmissionState(slot)`: 0 radiate B. `PowerPriority(slot)`: 0 priority S (1..=5).
//! - `Pump(slot)`: 0 on B, 1 compartment S (`None` = its own). `Door(slot)`: 0 closed B.
//!   `FloodValve(compartment)`: 0 open B.
//! - `TrackStanceOverride`: 0 track T, 1 stance St. `TrackIdOverride`: 0 track T, 1 rung S.
//! - `ShipPreset`: 0 preset S. `ShipEmcon`: 0 emcon E.
//!
//! `content::graph::PortType` has no preset or ID-rung type, so those travel on Scalar ports, either as the
//! enum index (`DoctrinePreset` 0 aggressive, 1 cautious, 2 silent running; `IdRung` 0 unknown .. 4 exact) or
//! as the typed `PortValue::Preset` / `PortValue::Rung`. [`normalize_sink_value`] turns either into the typed
//! value before a sink write leaves the VM.
use super::{NodeMem, NodeSig, ParamSpec, PortSpec, PortValue};
use crate::doctrine::MountInfo;
use crate::signature::EmitterKind;
use content::enums::{DoctrinePreset, IdRung};
use content::graph::{Cmp, MathOp, NodeKind, OfficerStage, PortType, SinkKind, SourceKind, Value, WeaponClass};
use content::ids::PartSlot;
use glam::DVec3;
use std::sync::Arc;

/// Facts about a design that shape node signatures.
pub struct SigFacts<'a> {
    pub mounts: &'a [MountInfo],
    pub sensor_slots: &'a [content::ids::PartSlot],
    pub consumer_slots: &'a [content::ids::PartSlot],
    /// Pumps and doors the damage-control planner drives (one output per entry). Added by `sys-graph-lib`.
    pub pumps: &'a [content::ids::PartSlot],
    pub doors: &'a [content::ids::PartSlot],
}
impl<'a> SigFacts<'a> {
    /// Signature facts of a design under construction.
    pub fn of(facts: &crate::ship::DesignFacts<'a>) -> Self {
        Self { mounts: facts.mounts, sensor_slots: facts.sensor_slots, consumer_slots: facts.consumer_slots, pumps: facts.pumps, doors: facts.doors }
    }
    /// Signature facts of a built design.
    pub fn of_design(design: &'a crate::ship::ShipDesign) -> Self {
        Self { mounts: &design.mounts, sensor_slots: &design.sensor_slots, consumer_slots: &design.consumer_slots, pumps: &design.pumps, doors: &design.doors }
    }
}

/// Most ports one node can have (`PortRef::port` is a `u8`; 255 is reserved for "the node itself" in issues).
/// Per-entry stage outputs beyond it are not exposed and the parts concerned keep their part defaults.
pub const MAX_PORTS: usize = 255;

// ---------------------------------------------------------------------------------------------------------
// Port helpers shared with the generator, the compiler and the stage runner.

/// Output ports (target, fire) of mount `index` (in `SigFacts::mounts` order) on `MountAssignment`.
pub fn assignment_ports(index: usize) -> Option<(u8, u8)> {
    let t = index.checked_mul(2)?;
    if t + 1 < MAX_PORTS - 1 { Some((t as u8, (t + 1) as u8)) } else { None }
}
/// Output port of consumer `index` (in `SigFacts::consumer_slots` order) on `LoadShedder`.
pub fn shedder_port(index: usize) -> Option<u8> { if index < MAX_PORTS - 3 { Some(index as u8) } else { None } }
/// Output ports (on, compartment) of pump `index` on `DamageControlPlanner`.
pub fn dc_pump_ports(index: usize) -> Option<(u8, u8)> {
    let p = index.checked_mul(2)?;
    if p + 1 < MAX_PORTS - 3 { Some((p as u8, (p + 1) as u8)) } else { None }
}
/// Output port of door `index` on `DamageControlPlanner` for a design with `pumps` pumps.
pub fn dc_door_port(pumps: usize, index: usize) -> Option<u8> {
    let p = pumps.min((MAX_PORTS - 3) / 2) * 2 + index;
    if p < MAX_PORTS - 3 { Some(p as u8) } else { None }
}
/// Output port of `EmissionController` that drives emitters of this kind.
pub fn emission_class_port(kind: EmitterKind, point_defence: bool) -> u8 {
    match kind {
        EmitterKind::SearchRadar | EmitterKind::NavRadar => 0,
        EmitterKind::FireControlRadar | EmitterKind::Seeker => if point_defence { 2 } else { 1 },
        EmitterKind::ActiveSonar => 3,
        EmitterKind::Jammer => 4,
    }
}
/// Whether a stage runs once per part (gun mount or launcher) rather than once per ship.
pub fn stage_is_per_part(stage: OfficerStage) -> bool { matches!(stage, OfficerStage::BallisticSolver | OfficerStage::PnGuidance) }
/// The per-part stage that serves a mount of this weapon class (`None` for decoy launchers, which the
/// engagement stage's decoy control fires directly).
pub fn mount_stage(weapon: WeaponClass) -> Option<OfficerStage> {
    match weapon {
        WeaponClass::Gun | WeaponClass::PointDefenceGun => Some(OfficerStage::BallisticSolver),
        WeaponClass::AntiShipMissile | WeaponClass::Sam | WeaponClass::PointDefenceMissile | WeaponClass::Torpedo | WeaponClass::DepthCharge => Some(OfficerStage::PnGuidance),
        WeaponClass::Decoy => None,
    }
}
/// Officer whose output a sink carries by default.
pub fn sink_officer(sink: SinkKind) -> content::enums::OfficerRole {
    use content::enums::OfficerRole as R;
    match sink {
        SinkKind::HelmHeading | SinkKind::HelmSpeed => R::Helm,
        SinkKind::MountAimPoint(_) | SinkKind::MountFirePermission(_) | SinkKind::MissileLaunch(_) | SinkKind::MissileTarget(_) | SinkKind::ShipPreset => R::Gunnery,
        SinkKind::EmissionState(_) | SinkKind::TrackStanceOverride | SinkKind::TrackIdOverride | SinkKind::ShipEmcon => R::Sensors,
        SinkKind::PowerPriority(_) => R::Engineer,
        SinkKind::Pump(_) | SinkKind::Door(_) | SinkKind::FloodValve(_) => R::DamageControl,
    }
}

/// Whether a value on a wire of type `from` may feed a port of type `to`. Scalars and angles mix; a Bool feeds
/// a Scalar as 0 or 1 and a Scalar feeds a Bool as "not zero"; everything else must match exactly.
pub fn compatible(from: PortType, to: PortType) -> bool {
    use PortType as T;
    from == to || matches!((from, to), (T::Scalar, T::Angle) | (T::Angle, T::Scalar) | (T::Bool, T::Scalar) | (T::Scalar, T::Bool))
}

/// Port type of a stored constant. Presets and ID rungs travel on Scalar ports.
pub fn value_type(v: &Value) -> PortType {
    match v {
        Value::None | Value::Scalar(_) | Value::Preset(_) | Value::Rung(_) => PortType::Scalar,
        Value::Bool(_) => PortType::Bool,
        Value::Vec3(_) => PortType::Vec3,
        Value::Text(_) => PortType::Text,
        Value::Stance(_) => PortType::Stance,
        Value::Emcon(_) => PortType::Emcon,
    }
}

/// Whether a run-time value may sit on a port of type `ty` (`None` fits everywhere).
pub fn value_fits(v: &PortValue, ty: PortType) -> bool {
    use PortType as T;
    match v {
        PortValue::None => true,
        PortValue::Bool(_) => matches!(ty, T::Bool | T::Scalar),
        PortValue::Scalar(_) => matches!(ty, T::Scalar | T::Angle | T::Bool),
        PortValue::Vec3(_) => ty == T::Vec3,
        PortValue::Text(_) => ty == T::Text,
        PortValue::Stance(_) => ty == T::Stance,
        PortValue::Emcon(_) => ty == T::Emcon,
        PortValue::Preset(_) | PortValue::Rung(_) => ty == T::Scalar,
        PortValue::Track(_) => ty == T::Track,
        PortValue::TrackList(_) => ty == T::TrackList,
        PortValue::Detections(_) => ty == T::DetectionList,
        PortValue::Part(_) => ty == T::PartState,
        PortValue::Orders => ty == T::Orders,
    }
}

/// Turn a sink input into its typed form: presets and ID rungs from their Scalar index, Bool and Scalar into
/// the port's type, and anything that does not fit (or a non-finite number) into `None`.
pub fn normalize_sink_value(sink: SinkKind, port: usize, v: PortValue) -> PortValue {
    let v = sanitize(v);
    match (sink, port) {
        (SinkKind::ShipPreset, 0) => match v {
            PortValue::Preset(_) => v,
            PortValue::Scalar(x) => preset_from_index(x).map(PortValue::Preset).unwrap_or(PortValue::None),
            _ => PortValue::None,
        },
        (SinkKind::TrackIdOverride, 1) => match v {
            PortValue::Rung(_) => v,
            PortValue::Scalar(x) => rung_from_index(x).map(PortValue::Rung).unwrap_or(PortValue::None),
            _ => PortValue::None,
        },
        _ => {
            let sig = sink_sig(sink);
            let Some(spec) = sig.inputs.get(port) else { return PortValue::None };
            match (spec.ty, v) {
                (PortType::Bool, PortValue::Scalar(x)) => PortValue::Bool(x != 0.0),
                (PortType::Scalar | PortType::Angle, PortValue::Bool(b)) => PortValue::Scalar(if b { 1.0 } else { 0.0 }),
                (ty, v) => if value_fits(&v, ty) { v } else { PortValue::None },
            }
        }
    }
}

fn preset_from_index(x: f64) -> Option<DoctrinePreset> {
    match x.round() as i64 { 0 => Some(DoctrinePreset::Aggressive), 1 => Some(DoctrinePreset::Cautious), 2 => Some(DoctrinePreset::SilentRunning), _ => None }
}
fn rung_from_index(x: f64) -> Option<IdRung> {
    match x.round() as i64 { 0 => Some(IdRung::Unknown), 1 => Some(IdRung::Domain), 2 => Some(IdRung::Type), 3 => Some(IdRung::Class), 4 => Some(IdRung::Exact), _ => None }
}

/// Replace non-finite numbers by `None` (L445: no NaN ever leaves a node).
pub fn sanitize(v: PortValue) -> PortValue {
    match v {
        PortValue::Scalar(x) if !x.is_finite() => PortValue::None,
        PortValue::Vec3(x) if !x.is_finite() => PortValue::None,
        v => v,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Signatures.

fn port(name: &'static str, ty: PortType) -> PortSpec { PortSpec { name, ty, default: PortValue::None } }
fn port_d(name: &'static str, ty: PortType, default: PortValue) -> PortSpec { PortSpec { name, ty, default } }
fn param(key: &'static str, label: &'static str, default: f64, min: f64, max: f64) -> ParamSpec { ParamSpec { key, label, default, min, max } }

/// Ports and parameters of a node kind. The stable port order and parameter keys defined here are part of the
/// blueprint format (stored edges name ports by index, stored params by key): changing one needs a migration.
/// Script nodes have no library signature (their ports come from their `ScriptSource`). Unit: `sys-graph-lib`.
pub fn signature(kind: &NodeKind, facts: &SigFacts<'_>) -> NodeSig {
    match kind {
        NodeKind::Source(s) => source_sig(s, facts),
        NodeKind::Math(op) => math_sig(*op),
        NodeKind::Stage(st) => stage_sig(*st, facts),
        NodeKind::Sink(k) => sink_sig(*k),
        NodeKind::Script(_) => NodeSig::default(),
    }
}

pub fn source_sig(source: &SourceKind, _facts: &SigFacts<'_>) -> NodeSig {
    use PortType as T;
    let outputs = match source {
        SourceKind::OwnShip => vec![
            port("position", T::Vec3), port("velocity", T::Vec3), port("heading", T::Angle), port("speed_kn", T::Scalar),
            port("heel", T::Angle), port("power_margin_kw", T::Scalar), port("fuel_fraction", T::Scalar), port("hull_health", T::Scalar),
            port("inbound_weapons", T::Scalar), port("sortie_time_s", T::Scalar),
        ],
        SourceKind::PartState(_) => vec![
            port("part", T::PartState), port("health", T::Scalar), port("works", T::Bool), port("powered", T::Bool),
            port("ammo_fraction", T::Scalar), port("train", T::Angle), port("elevation", T::Angle),
        ],
        SourceKind::Detections(_) => vec![port("detections", T::DetectionList), port("count", T::Scalar), port("sigma_range_m", T::Scalar), port("sigma_bearing", T::Angle)],
        SourceKind::TrackList => vec![port("tracks", T::TrackList), port("count", T::Scalar), port("nearest", T::Track), port("nearest_range_m", T::Scalar), port("inbound_count", T::Scalar)],
        SourceKind::TheatreOrders => vec![
            port("orders", T::Orders), port("designated", T::Track), port("speed_kn", T::Scalar), port("weapons_tight", T::Bool),
            port("evade", T::Bool), port("keep_range_m", T::Scalar),
        ],
        SourceKind::Constant(v) => vec![port_d("value", value_type(v), port_value_from(v))],
    };
    NodeSig { inputs: Vec::new(), outputs, params: source_params(source), stateful: false }
}

fn source_params(source: &SourceKind) -> Vec<ParamSpec> {
    match source {
        SourceKind::TrackList => vec![
            param("min_stance", "graph.param.min_stance", 2.0, 0.0, 3.0),
            param("domain", "graph.param.domain", -1.0, -1.0, 3.0),
            param("min_grade", "graph.param.min_grade", 0.0, 0.0, 3.0),
        ],
        _ => Vec::new(),
    }
}

pub fn math_sig(op: MathOp) -> NodeSig {
    use PortType as T;
    let s = |n| port(n, T::Scalar);
    let b = |n| port(n, T::Bool);
    let v = |n| port(n, T::Vec3);
    let (inputs, outputs, stateful) = match op {
        MathOp::Add | MathOp::Sub => (vec![s("a"), port_d("b", T::Scalar, PortValue::Scalar(0.0))], vec![s("out")], false),
        MathOp::Mul | MathOp::Div => (vec![s("a"), port_d("b", T::Scalar, PortValue::Scalar(1.0))], vec![s("out")], false),
        MathOp::Min | MathOp::Max => (vec![s("a"), s("b")], vec![s("out")], false),
        MathOp::Abs | MathOp::Neg => (vec![s("a")], vec![s("out")], false),
        MathOp::Clamp => (vec![s("x"), port_d("min", T::Scalar, PortValue::Scalar(0.0)), port_d("max", T::Scalar, PortValue::Scalar(1.0))], vec![s("out")], false),
        MathOp::Compare(_) => (vec![s("a"), s("b")], vec![b("out")], false),
        MathOp::And | MathOp::Or => (vec![b("a"), b("b")], vec![b("out")], false),
        MathOp::Not => (vec![b("a")], vec![b("out")], false),
        // Select is generic: `then`, `else` and `out` carry whatever type is wired to `then` (or `else`).
        MathOp::Select => (vec![b("cond"), s("then"), s("else")], vec![s("out")], false),
        MathOp::Latch => (vec![b("set"), b("reset")], vec![b("q")], true),
        MathOp::Timer => (vec![b("run"), b("reset")], vec![s("elapsed_s"), b("done")], true),
        MathOp::Counter => (vec![b("up"), b("down"), b("reset")], vec![s("count")], true),
        MathOp::Pid => (vec![s("setpoint"), s("measured")], vec![s("out"), s("error")], true),
        MathOp::LowPass => (vec![s("x")], vec![s("out")], true),
        MathOp::AlphaBeta => (vec![s("x")], vec![s("estimate"), s("rate")], true),
        MathOp::VecMake => (vec![s("x"), s("y"), s("z")], vec![v("v")], false),
        MathOp::VecSplit => (vec![v("v")], vec![s("x"), s("y"), s("z")], false),
        MathOp::VecAdd | MathOp::VecSub | MathOp::VecCross => (vec![v("a"), v("b")], vec![v("out")], false),
        MathOp::VecScale => (vec![v("v"), port_d("k", T::Scalar, PortValue::Scalar(1.0))], vec![v("out")], false),
        MathOp::VecDot => (vec![v("a"), v("b")], vec![s("out")], false),
        MathOp::VecLength => (vec![v("v")], vec![s("out")], false),
        MathOp::VecNormalize => (vec![v("v")], vec![v("out")], false),
        MathOp::BearingTo => (vec![v("from"), v("to")], vec![port("bearing", T::Angle)], false),
        MathOp::RangeTo => (vec![v("from"), v("to")], vec![s("range_m")], false),
    };
    NodeSig { inputs, outputs, params: math_params(op), stateful }
}

fn math_params(op: MathOp) -> Vec<ParamSpec> {
    match op {
        MathOp::Compare(_) => vec![param("epsilon", "graph.param.epsilon", 1e-6, 0.0, 1e6)],
        MathOp::Timer => vec![param("duration_s", "graph.param.duration_s", 5.0, 0.0, 3_600.0)],
        MathOp::Pid => vec![
            param("kp", "graph.param.kp", 1.0, 0.0, 100.0),
            param("ki", "graph.param.ki", 0.0, 0.0, 100.0),
            param("kd", "graph.param.kd", 0.0, 0.0, 100.0),
            param("out_min", "graph.param.out_min", -1.0, -1e6, 1e6),
            param("out_max", "graph.param.out_max", 1.0, -1e6, 1e6),
            param("wrap", "graph.param.wrap", 0.0, 0.0, 1.0),
        ],
        MathOp::LowPass => vec![param("time_constant_s", "graph.param.time_constant_s", 1.0, 0.0, 600.0)],
        MathOp::AlphaBeta => vec![param("alpha", "graph.param.alpha", 0.5, 0.0, 1.0), param("beta", "graph.param.beta", 0.5 * 0.5 / 1.5, 0.0, 1.0)],
        _ => Vec::new(),
    }
}

/// Parameters of a stage (independent of the design). Keys are L1 slider keys or settings keys.
pub fn stage_params_spec(stage: OfficerStage) -> Vec<ParamSpec> {
    match stage {
        OfficerStage::HelmAutopilot => vec![
            param("helm.responsiveness", "graph.param.helm.responsiveness", 0.5, 0.0, 1.0),
            param("helm.cruise_speed", "graph.param.helm.cruise_speed", 0.7, 0.3, 1.0),
            param("helm.lookahead", "graph.param.helm.lookahead", 1.5, 1.0, 4.0),
        ],
        OfficerStage::StationKeeping => vec![
            param("helm.responsiveness", "graph.param.helm.responsiveness", 0.5, 0.0, 1.0),
            param("helm.station_radius_m", "graph.param.helm.station_radius_m", 100.0, 20.0, 500.0),
        ],
        OfficerStage::Evasion => vec![param("helm.evasion", "graph.param.helm.evasion", 0.5, 0.0, 1.0)],
        OfficerStage::TrackFusion => Vec::new(),
        OfficerStage::ThreatRanking => vec![
            param("gunnery.aggression", "graph.param.gunnery.aggression", 0.5, 0.0, 1.0),
            param("gunnery.self_defence_range_m", "graph.param.gunnery.self_defence_range_m", 4_000.0, 500.0, 20_000.0),
        ],
        OfficerStage::MountAssignment => vec![
            param("gunnery.aggression", "graph.param.gunnery.aggression", 0.5, 0.0, 1.0),
            param("gunnery.open_fire_range", "graph.param.gunnery.open_fire_range", 1.0, 0.5, 1.0),
        ],
        OfficerStage::BallisticSolver => vec![
            param("gunnery.iteration_cap", "graph.param.gunnery.iteration_cap", 6.0, 1.0, 10.0),
            param("gunnery.converge_m", "graph.param.gunnery.converge_m", 0.25, 0.05, 2.0),
        ],
        OfficerStage::PnGuidance => vec![
            param("gunnery.pn_surface", "graph.param.gunnery.pn_surface", 3.0, 2.0, 6.0),
            param("gunnery.pn_air", "graph.param.gunnery.pn_air", 4.0, 2.0, 6.0),
        ],
        OfficerStage::LoadShedder => vec![
            param("engineer.reserve_margin", "graph.param.engineer.reserve_margin", 0.05, 0.0, 0.3),
            param("engineer.restore_delay_s", "graph.param.engineer.restore_delay_s", 2.0, 0.0, 10.0),
            param("engineer.fuel_reserve", "graph.param.engineer.fuel_reserve", 0.1, 0.0, 0.5),
        ],
        OfficerStage::DamageControlPlanner => vec![
            param("dc.pump_threshold_m", "graph.param.dc.pump_threshold_m", 0.05, 0.0, 0.5),
            param("dc.door_discipline", "graph.param.dc.door_discipline", 0.5, 0.0, 1.0),
            param("dc.magazine_flood_s", "graph.param.dc.magazine_flood_s", 60.0, 30.0, 120.0),
        ],
        OfficerStage::EmissionController => vec![
            param("sensors.burst_trigger_range_m", "graph.param.sensors.burst_trigger_range_m", 20_000.0, 2_000.0, 60_000.0),
            param("sensors.burst_rotations", "graph.param.sensors.burst_rotations", 3.0, 1.0, 6.0),
            param("sensors.burst_repeat_s", "graph.param.sensors.burst_repeat_s", 30.0, 10.0, 60.0),
            param("sensors.pd_radar_range_m", "graph.param.sensors.pd_radar_range_m", 8_000.0, 2_000.0, 12_000.0),
        ],
    }
}

/// Port types of a kind whose ports do not depend on the design (sources, math, sinks); `None` for stages
/// and scripts.
pub(crate) struct NodeSigLite { pub inputs: Vec<PortType>, pub outputs: Vec<PortType> }
impl NodeSigLite {
    pub(crate) fn of(kind: &NodeKind) -> Option<Self> {
        let none = SigFacts { mounts: &[], sensor_slots: &[], consumer_slots: &[], pumps: &[], doors: &[] };
        let sig = match kind { NodeKind::Stage(_) | NodeKind::Script(_) => return None, k => signature(k, &none) };
        Some(Self { inputs: sig.inputs.iter().map(|p| p.ty).collect(), outputs: sig.outputs.iter().map(|p| p.ty).collect() })
    }
}

/// Parameters of any node kind; parameters never depend on the design.
pub fn params_of(kind: &NodeKind) -> Vec<ParamSpec> {
    match kind {
        NodeKind::Source(s) => source_params(s),
        NodeKind::Math(op) => math_params(*op),
        NodeKind::Stage(st) => stage_params_spec(*st),
        NodeKind::Sink(_) | NodeKind::Script(_) => Vec::new(),
    }
}

pub fn stage_sig(stage: OfficerStage, facts: &SigFacts<'_>) -> NodeSig {
    use PortType as T;
    let (inputs, outputs) = match stage {
        OfficerStage::HelmAutopilot => (vec![port("orders", T::Orders)], vec![port("heading", T::Angle), port("speed_kn", T::Scalar), port("cross_track_m", T::Scalar)]),
        OfficerStage::StationKeeping => (vec![port("orders", T::Orders)], vec![port("heading", T::Angle), port("speed_kn", T::Scalar), port("active", T::Bool), port("distance_m", T::Scalar)]),
        OfficerStage::Evasion => (vec![port("orders", T::Orders), port("threats", T::TrackList)], vec![port("heading", T::Angle), port("speed_kn", T::Scalar), port("active", T::Bool)]),
        OfficerStage::TrackFusion => {
            let n = facts.sensor_slots.len().min(MAX_PORTS);
            (vec![port("detections", T::DetectionList); n], vec![
                port("tracks", T::TrackList), port("count", T::Scalar), port("gate_sigma", T::Scalar),
                port("alpha_fire_control", T::Scalar), port("alpha_search", T::Scalar), port("alpha_passive", T::Scalar),
            ])
        }
        OfficerStage::ThreatRanking => (vec![port("tracks", T::TrackList), port("orders", T::Orders)], vec![port("ranked", T::TrackList), port("top", T::Track), port("inbound_count", T::Scalar)]),
        OfficerStage::MountAssignment => {
            let mut out = Vec::new();
            for i in 0..facts.mounts.len() {
                if assignment_ports(i).is_none() { break; }
                out.push(port("target", T::Track));
                out.push(port("fire", T::Bool));
            }
            out.push(port("engaged", T::Scalar));
            (vec![port("ranked", T::TrackList), port("orders", T::Orders)], out)
        }
        OfficerStage::BallisticSolver => (vec![port("target", T::Track), port("fire", T::Bool)], vec![
            port("aim_point", T::Vec3), port("fire", T::Bool), port("train", T::Angle), port("elevation", T::Angle),
            port("time_of_flight_s", T::Scalar), port("iterations", T::Scalar), port("residual_m", T::Scalar),
            port("converged", T::Bool), port("in_range", T::Bool),
        ]),
        OfficerStage::PnGuidance => (vec![port("target", T::Track), port("fire", T::Bool)], vec![port("launch", T::Bool), port("target", T::Track), port("n", T::Scalar), port("aim_point", T::Vec3)]),
        OfficerStage::LoadShedder => {
            let n = facts.consumer_slots.len().min(MAX_PORTS - 3);
            let mut out = vec![port("priority", T::Scalar); n];
            out.extend([port("generation_kw", T::Scalar), port("demand_kw", T::Scalar), port("shed_count", T::Scalar)]);
            (Vec::new(), out)
        }
        OfficerStage::DamageControlPlanner => {
            let mut out = Vec::new();
            for i in 0..facts.pumps.len() {
                if dc_pump_ports(i).is_none() { break; }
                out.push(port("pump", T::Bool));
                out.push(port("pump_compartment", T::Scalar));
            }
            for i in 0..facts.doors.len() {
                if dc_door_port(facts.pumps.len(), i).is_none() { break; }
                out.push(port("door_closed", T::Bool));
            }
            out.extend([port("teams_busy", T::Scalar), port("flooding", T::Scalar), port("fires", T::Scalar)]);
            (Vec::new(), out)
        }
        OfficerStage::EmissionController => (vec![port("tracks", T::TrackList), port("setting", T::Emcon)], vec![
            port("radiate_search", T::Bool), port("radiate_fire_control", T::Bool), port("radiate_point_defence", T::Bool),
            port("radiate_sonar", T::Bool), port("radiate_jammer", T::Bool), port("lights", T::Bool), port("esm_warning", T::Bool),
        ]),
    };
    NodeSig { inputs, outputs, params: stage_params_spec(stage), stateful: true }
}

pub fn sink_sig(sink: SinkKind) -> NodeSig {
    use PortType as T;
    let inputs = match sink {
        SinkKind::HelmHeading => vec![port("heading", T::Angle)],
        SinkKind::HelmSpeed => vec![port("speed_kn", T::Scalar)],
        SinkKind::MountAimPoint(_) => vec![port("aim_point", T::Vec3)],
        SinkKind::MountFirePermission(_) => vec![port("fire", T::Bool), port("target", T::Track)],
        SinkKind::MissileLaunch(_) => vec![port("launch", T::Bool), port("n", T::Scalar)],
        SinkKind::MissileTarget(_) => vec![port("target", T::Track), port("aim_point", T::Vec3)],
        SinkKind::EmissionState(_) => vec![port("radiate", T::Bool)],
        SinkKind::PowerPriority(_) => vec![port("priority", T::Scalar)],
        SinkKind::Pump(_) => vec![port("on", T::Bool), port("compartment", T::Scalar)],
        SinkKind::Door(_) => vec![port("closed", T::Bool)],
        SinkKind::FloodValve(_) => vec![port("open", T::Bool)],
        SinkKind::TrackStanceOverride => vec![port("track", T::Track), port("stance", T::Stance)],
        SinkKind::TrackIdOverride => vec![port("track", T::Track), port("rung", T::Scalar)],
        SinkKind::ShipPreset => vec![port("preset", T::Scalar)],
        SinkKind::ShipEmcon => vec![port("emcon", T::Emcon)],
    };
    NodeSig { inputs, outputs: Vec::new(), params: Vec::new(), stateful: false }
}

// ---------------------------------------------------------------------------------------------------------
// Math and logic bodies.

/// A finite scalar from a wire: Scalar, or Bool as 0/1. Anything else (including `None`) is missing.
fn num(v: Option<&PortValue>) -> Option<f64> {
    match v? {
        PortValue::Scalar(x) if x.is_finite() => Some(*x),
        PortValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}
fn vec3(v: Option<&PortValue>) -> Option<DVec3> { match v? { PortValue::Vec3(x) if x.is_finite() => Some(*x), _ => None } }
/// Three-valued logic input: `None` (unknown) for missing values.
fn logic(v: Option<&PortValue>) -> Option<bool> {
    match v? {
        PortValue::Bool(b) => Some(*b),
        PortValue::Scalar(x) if x.is_finite() => Some(*x != 0.0),
        _ => None,
    }
}
/// Plain truth of a control input (set, reset, run, cond): missing counts as false.
fn truth(v: Option<&PortValue>) -> bool { logic(v).unwrap_or(false) }
fn par(params: &[PortValue], i: usize, default: f64) -> f64 { num(params.get(i)).unwrap_or(default) }
fn scalar_out(x: f64) -> PortValue { if x.is_finite() { PortValue::Scalar(x) } else { PortValue::None } }
fn vec_out(v: DVec3) -> PortValue { if v.is_finite() { PortValue::Vec3(v) } else { PortValue::None } }
fn set(out: &mut [PortValue], i: usize, v: PortValue) { if let Some(o) = out.get_mut(i) { *o = v; } }

/// Ordering of two comparable wire values: numbers (and Bools) numerically, ordered enums by rank, tracks and
/// parts by number. Text and vectors only compare for equality (`Some(Equal)`, else `None`).
fn compare_values(a: &PortValue, b: &PortValue, eps: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if let (Some(x), Some(y)) = (num(Some(a)), num(Some(b))) {
        if (x - y).abs() <= eps { return Some(Ordering::Equal); }
        return x.partial_cmp(&y);
    }
    match (a, b) {
        (PortValue::Stance(x), PortValue::Stance(y)) => Some(x.cmp(y)),
        (PortValue::Emcon(x), PortValue::Emcon(y)) => Some(x.cmp(y)),
        (PortValue::Preset(x), PortValue::Preset(y)) => Some(x.cmp(y)),
        (PortValue::Rung(x), PortValue::Rung(y)) => Some(x.cmp(y)),
        (PortValue::Track(x), PortValue::Track(y)) => Some(x.cmp(y)),
        (PortValue::Part(x), PortValue::Part(y)) => Some(x.cmp(y)),
        (PortValue::Text(x), PortValue::Text(y)) => if x == y { Some(Ordering::Equal) } else { None },
        (PortValue::Vec3(x), PortValue::Vec3(y)) if x.is_finite() && y.is_finite() => if (*x - *y).length() <= eps { Some(Ordering::Equal) } else { None },
        _ => None,
    }
}
/// Both values present, finite and of the same kind (so "cannot order" means "unequal").
fn same_kind_present(a: &PortValue, b: &PortValue) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b) && sanitize(a.clone()) != PortValue::None && sanitize(b.clone()) != PortValue::None
}

/// Wrap an angle to [-pi, pi).
fn wrap_pi(a: f64) -> f64 {
    let tau = core::f64::consts::TAU;
    a - tau * libm::floor((a + core::f64::consts::PI) / tau)
}

/// Counter memory packs the count with the previous `up` and `down` inputs for edge detection.
fn counter_unpack(v: i64) -> (i64, bool, bool) { (v >> 2, v & 2 != 0, v & 1 != 0) }
fn counter_pack(count: i64, up: bool, down: bool) -> i64 {
    let c = count.clamp(-(1i64 << 60), 1i64 << 60);
    (c << 2) | ((up as i64) << 1) | (down as i64)
}

/// Evaluate one math or logic node for a graph tick of `dt` seconds. Transcendentals through libm. NaN inputs
/// produce `PortValue::None` outputs, never NaN state (L445). Unit: `sys-graph-lib`.
///
/// Semantics worth knowing: logic is three-valued (a missing input is "unknown"; a known false decides And, a
/// known true decides Or); Select takes `else` unless `cond` is known true; Timer is an on-delay timer
/// (counts while `run` holds, clears when it drops or on `reset`; `done` once `duration_s` is reached);
/// Counter counts rising edges of `up` and `down`; PID clamps its integral so the integral term alone never
/// exceeds the output range, and with `wrap` set treats the error as an angle; RangeTo and BearingTo are
/// horizontal (range rings and compass bearings).
pub fn eval_math(op: MathOp, inputs: &[PortValue], params: &[PortValue], mem: &mut NodeMem, dt: f64, out: &mut [PortValue]) {
    for o in out.iter_mut() { *o = PortValue::None; }
    let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let i = |n: usize| inputs.get(n);
    match op {
        MathOp::Add | MathOp::Sub | MathOp::Mul | MathOp::Div | MathOp::Min | MathOp::Max => {
            if let (Some(a), Some(b)) = (num(i(0)), num(i(1))) {
                let r = match op {
                    MathOp::Add => a + b,
                    MathOp::Sub => a - b,
                    MathOp::Mul => a * b,
                    MathOp::Div => if b == 0.0 { f64::NAN } else { a / b },
                    MathOp::Min => a.min(b),
                    _ => a.max(b),
                };
                set(out, 0, scalar_out(r));
            }
        }
        MathOp::Abs => if let Some(a) = num(i(0)) { set(out, 0, scalar_out(a.abs())) },
        MathOp::Neg => if let Some(a) = num(i(0)) { set(out, 0, scalar_out(-a)) },
        MathOp::Clamp => {
            if let (Some(x), Some(lo), Some(hi)) = (num(i(0)), num(i(1)), num(i(2))) {
                let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                set(out, 0, scalar_out(x.clamp(lo, hi)));
            }
        }
        MathOp::Compare(cmp) => {
            use std::cmp::Ordering as O;
            let eps = par(params, 0, 1e-6).max(0.0);
            if let (Some(a), Some(b)) = (i(0), i(1)) {
                let r = match (cmp, compare_values(a, b, eps)) {
                    (Cmp::Eq, Some(o)) => Some(o == O::Equal),
                    (Cmp::Ne, Some(o)) => Some(o != O::Equal),
                    (Cmp::Eq, None) => if same_kind_present(a, b) { Some(false) } else { None },
                    (Cmp::Ne, None) => if same_kind_present(a, b) { Some(true) } else { None },
                    (Cmp::Lt, Some(o)) => Some(o == O::Less),
                    (Cmp::Le, Some(o)) => Some(o != O::Greater),
                    (Cmp::Gt, Some(o)) => Some(o == O::Greater),
                    (Cmp::Ge, Some(o)) => Some(o != O::Less),
                    _ => None,
                };
                if let Some(r) = r { set(out, 0, PortValue::Bool(r)); }
            }
        }
        MathOp::And => {
            let (a, b) = (logic(i(0)), logic(i(1)));
            let r = if a == Some(false) || b == Some(false) { Some(false) } else if a.is_none() || b.is_none() { None } else { Some(true) };
            if let Some(r) = r { set(out, 0, PortValue::Bool(r)); }
        }
        MathOp::Or => {
            let (a, b) = (logic(i(0)), logic(i(1)));
            let r = if a == Some(true) || b == Some(true) { Some(true) } else if a.is_none() || b.is_none() { None } else { Some(false) };
            if let Some(r) = r { set(out, 0, PortValue::Bool(r)); }
        }
        MathOp::Not => if let Some(a) = logic(i(0)) { set(out, 0, PortValue::Bool(!a)) },
        MathOp::Select => {
            let chosen = if truth(i(0)) { i(1) } else { i(2) };
            set(out, 0, sanitize(chosen.cloned().unwrap_or_default()));
        }
        MathOp::Latch => {
            let mut q = if let NodeMem::Latch(q) = mem { *q } else { false };
            if truth(i(1)) { q = false; } else if truth(i(0)) { q = true; }
            *mem = NodeMem::Latch(q);
            set(out, 0, PortValue::Bool(q));
        }
        MathOp::Timer => {
            let duration = par(params, 0, 5.0).max(0.0);
            let mut elapsed = if let NodeMem::Timer { elapsed_s, .. } = mem { *elapsed_s } else { 0.0 };
            let run = truth(i(0));
            if truth(i(1)) || !run { elapsed = 0.0; } else { elapsed = (elapsed + dt).min(1e9); }
            *mem = NodeMem::Timer { elapsed_s: elapsed, running: run };
            set(out, 0, PortValue::Scalar(elapsed));
            set(out, 1, PortValue::Bool(run && elapsed >= duration));
        }
        MathOp::Counter => {
            let (mut count, prev_up, prev_down) = if let NodeMem::Counter(v) = mem { counter_unpack(*v) } else { (0, false, false) };
            let (up, down) = (truth(i(0)), truth(i(1)));
            if truth(i(2)) { count = 0; } else {
                if up && !prev_up { count += 1; }
                if down && !prev_down { count -= 1; }
            }
            let packed = counter_pack(count, up, down);
            *mem = NodeMem::Counter(packed);
            set(out, 0, PortValue::Scalar(counter_unpack(packed).0 as f64));
        }
        MathOp::Pid => {
            let (Some(sp), Some(pv)) = (num(i(0)), num(i(1))) else { return };
            let kp = par(params, 0, 1.0);
            let ki = par(params, 1, 0.0);
            let kd = par(params, 2, 0.0);
            let (mut lo, mut hi) = (par(params, 3, -1.0), par(params, 4, 1.0));
            if lo > hi { std::mem::swap(&mut lo, &mut hi); }
            let wrap = par(params, 5, 0.0) >= 0.5;
            let mut e = sp - pv;
            if wrap { e = wrap_pi(e); }
            let (mut integral, prev) = if let NodeMem::Pid { integral, prev_error } = mem { (*integral, *prev_error) } else { (0.0, e) };
            integral += e * dt;
            if ki > 0.0 {
                let limit = lo.abs().max(hi.abs()) / ki;
                integral = integral.clamp(-limit, limit);
            }
            let mut de = e - prev;
            if wrap { de = wrap_pi(de); }
            let d = if dt > 0.0 { kd * de / dt } else { 0.0 };
            let u = (kp * e + ki * integral + d).clamp(lo, hi);
            if integral.is_finite() && e.is_finite() { *mem = NodeMem::Pid { integral, prev_error: e }; }
            set(out, 0, scalar_out(u));
            set(out, 1, scalar_out(e));
        }
        MathOp::LowPass => {
            let Some(x) = num(i(0)) else { return };
            let tau = par(params, 0, 1.0).max(0.0);
            let y = match mem {
                NodeMem::LowPass(y) if tau > 0.0 => { let a = dt / (tau + dt); *y + a * (x - *y) }
                _ => x,
            };
            if y.is_finite() { *mem = NodeMem::LowPass(y); }
            set(out, 0, scalar_out(y));
        }
        MathOp::AlphaBeta => {
            let Some(z) = num(i(0)) else { return };
            let alpha = par(params, 0, 0.5).clamp(0.0, 1.0);
            let beta = par(params, 1, 0.5 * 0.5 / 1.5).clamp(0.0, 1.0);
            let (x, v) = match mem {
                NodeMem::AlphaBeta { x, v } if dt > 0.0 => {
                    let xp = *x + *v * dt;
                    let r = z - xp;
                    (xp + alpha * r, *v + beta * r / dt)
                }
                NodeMem::AlphaBeta { v, .. } => (z, *v),
                _ => (z, 0.0),
            };
            if x.is_finite() && v.is_finite() { *mem = NodeMem::AlphaBeta { x, v }; }
            set(out, 0, scalar_out(x));
            set(out, 1, scalar_out(v));
        }
        MathOp::VecMake => if let (Some(x), Some(y), Some(z)) = (num(i(0)), num(i(1)), num(i(2))) { set(out, 0, vec_out(DVec3::new(x, y, z))) },
        MathOp::VecSplit => if let Some(v) = vec3(i(0)) { set(out, 0, scalar_out(v.x)); set(out, 1, scalar_out(v.y)); set(out, 2, scalar_out(v.z)); },
        MathOp::VecAdd => if let (Some(a), Some(b)) = (vec3(i(0)), vec3(i(1))) { set(out, 0, vec_out(a + b)) },
        MathOp::VecSub => if let (Some(a), Some(b)) = (vec3(i(0)), vec3(i(1))) { set(out, 0, vec_out(a - b)) },
        MathOp::VecCross => if let (Some(a), Some(b)) = (vec3(i(0)), vec3(i(1))) { set(out, 0, vec_out(a.cross(b))) },
        MathOp::VecScale => if let (Some(v), Some(k)) = (vec3(i(0)), num(i(1))) { set(out, 0, vec_out(v * k)) },
        MathOp::VecDot => if let (Some(a), Some(b)) = (vec3(i(0)), vec3(i(1))) { set(out, 0, scalar_out(a.dot(b))) },
        MathOp::VecLength => if let Some(v) = vec3(i(0)) { set(out, 0, scalar_out(v.length())) },
        MathOp::VecNormalize => if let Some(v) = vec3(i(0)) { let l = v.length(); if l > 1e-12 { set(out, 0, vec_out(v / l)) } },
        MathOp::BearingTo => {
            if let (Some(a), Some(b)) = (vec3(i(0)), vec3(i(1))) {
                let (dx, dz) = (b.x - a.x, b.z - a.z);
                if dx * dx + dz * dz > 1e-12 {
                    // World frame: +X east, +Z south; compass 0 = north = -Z, clockwise (sim_core::math).
                    let mut h = libm::atan2(dx, -dz);
                    if h < 0.0 { h += core::f64::consts::TAU; }
                    set(out, 0, scalar_out(h));
                }
            }
        }
        MathOp::RangeTo => if let (Some(a), Some(b)) = (vec3(i(0)), vec3(i(1))) { let (dx, dz) = (b.x - a.x, b.z - a.z); set(out, 0, scalar_out((dx * dx + dz * dz).sqrt())) },
    }
}

// ---------------------------------------------------------------------------------------------------------
// Value conversion.

/// Stored constant to wire value. Unit: `sys-graph-lib`.
pub fn port_value_from(v: &Value) -> PortValue {
    match v {
        Value::None => PortValue::None,
        Value::Bool(b) => PortValue::Bool(*b),
        Value::Scalar(x) => scalar_out(*x),
        Value::Vec3(a) => vec_out(DVec3::new(a[0], a[1], a[2])),
        Value::Text(s) => PortValue::Text(Arc::from(s.as_str())),
        Value::Stance(s) => PortValue::Stance(*s),
        Value::Emcon(e) => PortValue::Emcon(*e),
        Value::Preset(p) => PortValue::Preset(*p),
        Value::Rung(r) => PortValue::Rung(*r),
    }
}
/// Wire value to stored constant (lists become `Value::None`). Tracks and parts print as their number.
/// Unit: `sys-graph-lib`.
pub fn port_value_to(v: &PortValue) -> Value {
    match v {
        PortValue::None | PortValue::TrackList(_) | PortValue::Detections(_) | PortValue::Orders => Value::None,
        PortValue::Bool(b) => Value::Bool(*b),
        PortValue::Scalar(x) => if x.is_finite() { Value::Scalar(*x) } else { Value::None },
        PortValue::Vec3(x) => if x.is_finite() { Value::Vec3([x.x, x.y, x.z]) } else { Value::None },
        PortValue::Text(s) => Value::Text(s.to_string()),
        PortValue::Stance(s) => Value::Stance(*s),
        PortValue::Emcon(e) => Value::Emcon(*e),
        PortValue::Preset(p) => Value::Preset(*p),
        PortValue::Rung(r) => Value::Rung(*r),
        PortValue::Track(t) => Value::Scalar(t.0 as f64),
        PortValue::Part(p) => Value::Scalar(p.0 as f64),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Palette and text keys.

pub(crate) const ALL_STAGES: [OfficerStage; 11] = [
    OfficerStage::HelmAutopilot, OfficerStage::StationKeeping, OfficerStage::Evasion, OfficerStage::TrackFusion,
    OfficerStage::ThreatRanking, OfficerStage::MountAssignment, OfficerStage::BallisticSolver, OfficerStage::PnGuidance,
    OfficerStage::LoadShedder, OfficerStage::DamageControlPlanner, OfficerStage::EmissionController,
];
const ALL_MATH: [MathOp; 36] = [
    MathOp::Add, MathOp::Sub, MathOp::Mul, MathOp::Div, MathOp::Min, MathOp::Max, MathOp::Abs, MathOp::Neg, MathOp::Clamp,
    MathOp::Compare(Cmp::Lt), MathOp::Compare(Cmp::Le), MathOp::Compare(Cmp::Gt), MathOp::Compare(Cmp::Ge), MathOp::Compare(Cmp::Eq), MathOp::Compare(Cmp::Ne),
    MathOp::And, MathOp::Or, MathOp::Not, MathOp::Select, MathOp::Latch, MathOp::Timer, MathOp::Counter, MathOp::Pid,
    MathOp::LowPass, MathOp::AlphaBeta, MathOp::VecMake, MathOp::VecSplit, MathOp::VecAdd, MathOp::VecSub, MathOp::VecScale,
    MathOp::VecDot, MathOp::VecCross, MathOp::VecLength, MathOp::VecNormalize, MathOp::BearingTo, MathOp::RangeTo,
];

/// Translation key of a node kind's name (`graph.node.<name>`).
pub fn node_key(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Source(s) => match s {
            SourceKind::OwnShip => "graph.node.own_ship",
            SourceKind::PartState(_) => "graph.node.part_state",
            SourceKind::Detections(_) => "graph.node.detections",
            SourceKind::TrackList => "graph.node.track_list",
            SourceKind::TheatreOrders => "graph.node.theatre_orders",
            SourceKind::Constant(_) => "graph.node.constant",
        },
        NodeKind::Math(op) => match op {
            MathOp::Add => "graph.node.add", MathOp::Sub => "graph.node.sub", MathOp::Mul => "graph.node.mul", MathOp::Div => "graph.node.div",
            MathOp::Min => "graph.node.min", MathOp::Max => "graph.node.max", MathOp::Abs => "graph.node.abs", MathOp::Neg => "graph.node.neg",
            MathOp::Clamp => "graph.node.clamp",
            MathOp::Compare(Cmp::Lt) => "graph.node.compare_lt", MathOp::Compare(Cmp::Le) => "graph.node.compare_le",
            MathOp::Compare(Cmp::Gt) => "graph.node.compare_gt", MathOp::Compare(Cmp::Ge) => "graph.node.compare_ge",
            MathOp::Compare(Cmp::Eq) => "graph.node.compare_eq", MathOp::Compare(Cmp::Ne) => "graph.node.compare_ne",
            MathOp::And => "graph.node.and", MathOp::Or => "graph.node.or", MathOp::Not => "graph.node.not", MathOp::Select => "graph.node.select",
            MathOp::Latch => "graph.node.latch", MathOp::Timer => "graph.node.timer", MathOp::Counter => "graph.node.counter", MathOp::Pid => "graph.node.pid",
            MathOp::LowPass => "graph.node.low_pass", MathOp::AlphaBeta => "graph.node.alpha_beta", MathOp::VecMake => "graph.node.vec_make",
            MathOp::VecSplit => "graph.node.vec_split", MathOp::VecAdd => "graph.node.vec_add", MathOp::VecSub => "graph.node.vec_sub",
            MathOp::VecScale => "graph.node.vec_scale", MathOp::VecDot => "graph.node.vec_dot", MathOp::VecCross => "graph.node.vec_cross",
            MathOp::VecLength => "graph.node.vec_length", MathOp::VecNormalize => "graph.node.vec_normalize", MathOp::BearingTo => "graph.node.bearing_to",
            MathOp::RangeTo => "graph.node.range_to",
        },
        NodeKind::Stage(st) => match st {
            OfficerStage::HelmAutopilot => "graph.node.helm_autopilot", OfficerStage::StationKeeping => "graph.node.station_keeping",
            OfficerStage::Evasion => "graph.node.evasion", OfficerStage::TrackFusion => "graph.node.track_fusion",
            OfficerStage::ThreatRanking => "graph.node.threat_ranking", OfficerStage::MountAssignment => "graph.node.mount_assignment",
            OfficerStage::BallisticSolver => "graph.node.ballistic_solver", OfficerStage::PnGuidance => "graph.node.pn_guidance",
            OfficerStage::LoadShedder => "graph.node.load_shedder", OfficerStage::DamageControlPlanner => "graph.node.damage_control_planner",
            OfficerStage::EmissionController => "graph.node.emission_controller",
        },
        NodeKind::Sink(k) => match k {
            SinkKind::HelmHeading => "graph.node.helm_heading", SinkKind::HelmSpeed => "graph.node.helm_speed",
            SinkKind::MountAimPoint(_) => "graph.node.mount_aim_point", SinkKind::MountFirePermission(_) => "graph.node.mount_fire_permission",
            SinkKind::MissileLaunch(_) => "graph.node.missile_launch", SinkKind::MissileTarget(_) => "graph.node.missile_target",
            SinkKind::EmissionState(_) => "graph.node.emission_state", SinkKind::PowerPriority(_) => "graph.node.power_priority",
            SinkKind::Pump(_) => "graph.node.pump", SinkKind::Door(_) => "graph.node.door", SinkKind::FloodValve(_) => "graph.node.flood_valve",
            SinkKind::TrackStanceOverride => "graph.node.track_stance_override", SinkKind::TrackIdOverride => "graph.node.track_id_override",
            SinkKind::ShipPreset => "graph.node.ship_preset", SinkKind::ShipEmcon => "graph.node.ship_emcon",
        },
        NodeKind::Script(_) => "graph.node.script",
    }
}

/// Translation key of a port name (`graph.port.<name>`); `None` for a name outside the library.
pub fn port_key(name: &str) -> Option<&'static str> { PORT_NAMES.iter().position(|n| *n == name).map(|i| PORT_KEYS[i]) }

/// Every node kind the editor's node palette offers at first release, in palette order, with its translation
/// key (`graph.node.<name>`). Part-bound kinds carry slot 0 and compartment 0 as a placeholder the editor
/// rebinds when the node is placed. Script nodes arrive with L4, after the first release. Unit: `sys-graph-lib`.
pub fn palette() -> Vec<(NodeKind, &'static str)> {
    let p = PartSlot(0);
    let mut kinds = vec![
        NodeKind::Source(SourceKind::OwnShip), NodeKind::Source(SourceKind::PartState(p)), NodeKind::Source(SourceKind::Detections(p)),
        NodeKind::Source(SourceKind::TrackList), NodeKind::Source(SourceKind::TheatreOrders), NodeKind::Source(SourceKind::Constant(Value::Scalar(0.0))),
    ];
    kinds.extend(ALL_MATH.iter().map(|op| NodeKind::Math(*op)));
    kinds.extend(ALL_STAGES.iter().map(|s| NodeKind::Stage(*s)));
    kinds.extend([
        SinkKind::HelmHeading, SinkKind::HelmSpeed, SinkKind::MountAimPoint(p), SinkKind::MountFirePermission(p), SinkKind::MissileLaunch(p),
        SinkKind::MissileTarget(p), SinkKind::EmissionState(p), SinkKind::PowerPriority(p), SinkKind::Pump(p), SinkKind::Door(p),
        SinkKind::FloodValve(content::ids::CompartmentId(0)), SinkKind::TrackStanceOverride, SinkKind::TrackIdOverride, SinkKind::ShipPreset, SinkKind::ShipEmcon,
    ].into_iter().map(NodeKind::Sink));
    kinds.into_iter().map(|k| { let key = node_key(&k); (k, key) }).collect()
}

/// Every port name any signature uses, and its key at the same index.
const PORT_NAMES: &[&str] = &[
    "position", "velocity", "heading", "speed_kn", "heel", "power_margin_kw", "fuel_fraction", "hull_health", "inbound_weapons",
    "sortie_time_s", "part", "health", "works", "powered", "ammo_fraction", "train", "elevation", "detections", "count", "sigma_range_m",
    "sigma_bearing", "tracks", "nearest", "nearest_range_m", "inbound_count", "orders", "designated", "weapons_tight", "evade",
    "keep_range_m", "value", "a", "b", "out", "x", "y", "z", "min", "max", "cond", "then", "else", "set", "reset", "q", "run", "elapsed_s",
    "done", "up", "down", "setpoint", "measured", "error", "estimate", "rate", "v", "k", "from", "to", "bearing", "range_m",
    "cross_track_m", "active", "distance_m", "threats", "gate_sigma", "alpha_fire_control", "alpha_search", "alpha_passive", "ranked",
    "top", "target", "fire", "engaged", "aim_point", "time_of_flight_s", "iterations", "residual_m", "converged", "in_range", "launch",
    "n", "priority", "generation_kw", "demand_kw", "shed_count", "pump", "pump_compartment", "door_closed", "teams_busy", "flooding",
    "fires", "setting", "radiate_search", "radiate_fire_control", "radiate_point_defence", "radiate_sonar", "radiate_jammer", "lights",
    "esm_warning", "radiate", "on", "compartment", "closed", "open", "track", "stance", "rung", "preset", "emcon",
];
const PORT_KEYS: &[&str] = &[
    "graph.port.position", "graph.port.velocity", "graph.port.heading", "graph.port.speed_kn", "graph.port.heel", "graph.port.power_margin_kw",
    "graph.port.fuel_fraction", "graph.port.hull_health", "graph.port.inbound_weapons", "graph.port.sortie_time_s", "graph.port.part",
    "graph.port.health", "graph.port.works", "graph.port.powered", "graph.port.ammo_fraction", "graph.port.train", "graph.port.elevation",
    "graph.port.detections", "graph.port.count", "graph.port.sigma_range_m", "graph.port.sigma_bearing", "graph.port.tracks",
    "graph.port.nearest", "graph.port.nearest_range_m", "graph.port.inbound_count", "graph.port.orders", "graph.port.designated",
    "graph.port.weapons_tight", "graph.port.evade", "graph.port.keep_range_m", "graph.port.value", "graph.port.a", "graph.port.b",
    "graph.port.out", "graph.port.x", "graph.port.y", "graph.port.z", "graph.port.min", "graph.port.max", "graph.port.cond",
    "graph.port.then", "graph.port.else", "graph.port.set", "graph.port.reset", "graph.port.q", "graph.port.run", "graph.port.elapsed_s",
    "graph.port.done", "graph.port.up", "graph.port.down", "graph.port.setpoint", "graph.port.measured", "graph.port.error",
    "graph.port.estimate", "graph.port.rate", "graph.port.v", "graph.port.k", "graph.port.from", "graph.port.to", "graph.port.bearing",
    "graph.port.range_m", "graph.port.cross_track_m", "graph.port.active", "graph.port.distance_m", "graph.port.threats",
    "graph.port.gate_sigma", "graph.port.alpha_fire_control", "graph.port.alpha_search", "graph.port.alpha_passive", "graph.port.ranked",
    "graph.port.top", "graph.port.target", "graph.port.fire", "graph.port.engaged", "graph.port.aim_point", "graph.port.time_of_flight_s",
    "graph.port.iterations", "graph.port.residual_m", "graph.port.converged", "graph.port.in_range", "graph.port.launch", "graph.port.n",
    "graph.port.priority", "graph.port.generation_kw", "graph.port.demand_kw", "graph.port.shed_count", "graph.port.pump",
    "graph.port.pump_compartment", "graph.port.door_closed", "graph.port.teams_busy", "graph.port.flooding", "graph.port.fires",
    "graph.port.setting", "graph.port.radiate_search", "graph.port.radiate_fire_control", "graph.port.radiate_point_defence",
    "graph.port.radiate_sonar", "graph.port.radiate_jammer", "graph.port.lights", "graph.port.esm_warning", "graph.port.radiate",
    "graph.port.on", "graph.port.compartment", "graph.port.closed", "graph.port.open", "graph.port.track", "graph.port.stance",
    "graph.port.rung", "graph.port.preset", "graph.port.emcon",
];
/// Group headings of the palette and frame labels of the graph view (generated, kit, pinned, rule, script).
const GROUP_KEYS: &[&str] = &[
    "graph.group.sources", "graph.group.math", "graph.group.stages", "graph.group.sinks", "graph.group.scripts",
    "graph.origin.generated", "graph.origin.kit", "graph.origin.pinned", "graph.origin.replacement", "graph.origin.rule", "graph.origin.script",
];

/// Every `graph.*` key this unit can emit: node names, port names, parameter labels (`game/i18n/graph.csv`).
/// Unit: `sys-graph-lib`.
pub fn text_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = Vec::new();
    for (kind, key) in palette() { keys.push(key); for p in params_of(&kind) { keys.push(p.label); } }
    keys.push(node_key(&NodeKind::Script(0)));
    keys.extend_from_slice(PORT_KEYS);
    keys.extend_from_slice(GROUP_KEYS);
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::enums::{Emcon, Stance};

    fn run(op: MathOp, inputs: &[PortValue], mem: &mut NodeMem, dt: f64) -> Vec<PortValue> {
        let sig = math_sig(op);
        let params: Vec<PortValue> = sig.params.iter().map(|p| PortValue::Scalar(p.default)).collect();
        let mut out = vec![PortValue::None; sig.outputs.len()];
        eval_math(op, inputs, &params, mem, dt, &mut out);
        out
    }
    fn s(x: f64) -> PortValue { PortValue::Scalar(x) }
    fn b(x: bool) -> PortValue { PortValue::Bool(x) }

    #[test]
    fn arithmetic_and_nan() {
        let mut m = NodeMem::None;
        assert_eq!(run(MathOp::Add, &[s(2.0), s(3.0)], &mut m, 0.0)[0], s(5.0));
        assert_eq!(run(MathOp::Div, &[s(1.0), s(0.0)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::Mul, &[s(f64::NAN), s(2.0)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::Add, &[s(f64::MAX), s(f64::MAX)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::Clamp, &[s(5.0), s(2.0), s(0.0)], &mut m, 0.0)[0], s(2.0));
        assert_eq!(run(MathOp::Sub, &[s(1.0), PortValue::None], &mut m, 0.0)[0], PortValue::None);
    }

    #[test]
    fn compare_and_logic() {
        let mut m = NodeMem::None;
        assert_eq!(run(MathOp::Compare(Cmp::Lt), &[s(1.0), s(2.0)], &mut m, 0.0)[0], b(true));
        assert_eq!(run(MathOp::Compare(Cmp::Ge), &[PortValue::Stance(Stance::Hostile), PortValue::Stance(Stance::Suspect)], &mut m, 0.0)[0], b(true));
        assert_eq!(run(MathOp::Compare(Cmp::Eq), &[PortValue::Track(crate::ids::TrackId(3)), PortValue::Track(crate::ids::TrackId(3))], &mut m, 0.0)[0], b(true));
        assert_eq!(run(MathOp::Compare(Cmp::Ne), &[PortValue::Text("a".into()), PortValue::Text("b".into())], &mut m, 0.0)[0], b(true));
        assert_eq!(run(MathOp::Compare(Cmp::Lt), &[PortValue::None, s(2.0)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::Compare(Cmp::Eq), &[PortValue::Emcon(Emcon::Active), s(2.0)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::And, &[PortValue::None, b(false)], &mut m, 0.0)[0], b(false));
        assert_eq!(run(MathOp::And, &[PortValue::None, b(true)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::Or, &[PortValue::None, b(true)], &mut m, 0.0)[0], b(true));
        assert_eq!(run(MathOp::Not, &[PortValue::None], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::Select, &[PortValue::None, s(1.0), s(2.0)], &mut m, 0.0)[0], s(2.0));
        assert_eq!(run(MathOp::Select, &[b(true), PortValue::Track(crate::ids::TrackId(7)), s(2.0)], &mut m, 0.0)[0], PortValue::Track(crate::ids::TrackId(7)));
    }

    #[test]
    fn stateful_nodes() {
        let mut m = NodeMem::None;
        assert_eq!(run(MathOp::Latch, &[b(true), b(false)], &mut m, 0.0)[0], b(true));
        assert_eq!(run(MathOp::Latch, &[b(false), b(false)], &mut m, 0.0)[0], b(true));
        assert_eq!(run(MathOp::Latch, &[b(true), b(true)], &mut m, 0.0)[0], b(false));

        let mut t = NodeMem::None;
        let dt = 1.0 / 30.0;
        let mut done = PortValue::None;
        for _ in 0..149 { done = run(MathOp::Timer, &[b(true), b(false)], &mut t, dt)[1].clone(); }
        assert_eq!(done, b(false));
        for _ in 0..2 { done = run(MathOp::Timer, &[b(true), b(false)], &mut t, dt)[1].clone(); }
        assert_eq!(done, b(true));
        assert_eq!(run(MathOp::Timer, &[b(false), b(false)], &mut t, dt)[0], s(0.0));

        let mut c = NodeMem::None;
        for up in [true, true, false, true, false] { run(MathOp::Counter, &[b(up), b(false), b(false)], &mut c, dt); }
        assert_eq!(run(MathOp::Counter, &[b(false), b(true), b(false)], &mut c, dt)[0], s(1.0));
        assert_eq!(run(MathOp::Counter, &[b(false), b(true), b(true)], &mut c, dt)[0], s(0.0));

        let mut lp = NodeMem::None;
        assert_eq!(run(MathOp::LowPass, &[s(10.0)], &mut lp, dt)[0], s(10.0));
        let PortValue::Scalar(y) = run(MathOp::LowPass, &[s(0.0)], &mut lp, dt)[0].clone() else { panic!() };
        assert!((y - 10.0 * (1.0 - dt / (1.0 + dt))).abs() < 1e-12);
        assert_eq!(run(MathOp::LowPass, &[s(f64::NAN)], &mut lp, dt)[0], PortValue::None);
        assert!(matches!(lp, NodeMem::LowPass(v) if v.is_finite()));

        let mut ab = NodeMem::None;
        let mut rate = 0.0;
        for k in 0..300 { if let PortValue::Scalar(r) = run(MathOp::AlphaBeta, &[s(2.0 * k as f64 * dt)], &mut ab, dt)[1] { rate = r; } }
        assert!((rate - 2.0).abs() < 1e-6);
    }

    #[test]
    fn pid_wraps_and_limits() {
        let params = [s(2.0), s(1.0), s(0.0), s(-0.5), s(0.5), s(1.0)];
        let mut m = NodeMem::None;
        let mut out = vec![PortValue::None; 2];
        // Setpoint 350 degrees, measured 10 degrees: the wrapped error is -20 degrees, not +340.
        eval_math(MathOp::Pid, &[s(350f64.to_radians()), s(10f64.to_radians())], &params, &mut m, 1.0 / 30.0, &mut out);
        let PortValue::Scalar(e) = out[1] else { panic!() };
        assert!((e - (-20f64).to_radians()).abs() < 1e-9);
        assert_eq!(out[0], s(-0.5));
        for _ in 0..10_000 { eval_math(MathOp::Pid, &[s(1.0), s(0.0)], &params, &mut m, 1.0 / 30.0, &mut out); }
        let NodeMem::Pid { integral, .. } = m else { panic!() };
        assert!(integral <= 0.5 + 1e-9);
    }

    #[test]
    fn vectors_and_bearing() {
        let mut m = NodeMem::None;
        let v = |x, y, z| PortValue::Vec3(DVec3::new(x, y, z));
        let PortValue::Scalar(h) = run(MathOp::BearingTo, &[v(0.0, 0.0, 0.0), v(100.0, 0.0, 0.0)], &mut m, 0.0)[0] else { panic!() };
        assert!((h - core::f64::consts::FRAC_PI_2).abs() < 1e-12);
        let PortValue::Scalar(h) = run(MathOp::BearingTo, &[v(0.0, 0.0, 0.0), v(0.0, 0.0, -100.0)], &mut m, 0.0)[0] else { panic!() };
        assert!(h.abs() < 1e-12);
        assert_eq!(run(MathOp::BearingTo, &[v(1.0, 0.0, 1.0), v(1.0, 9.0, 1.0)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::RangeTo, &[v(0.0, 50.0, 0.0), v(3.0, 0.0, 4.0)], &mut m, 0.0)[0], s(5.0));
        assert_eq!(run(MathOp::VecNormalize, &[v(0.0, 0.0, 0.0)], &mut m, 0.0)[0], PortValue::None);
        assert_eq!(run(MathOp::VecCross, &[v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0)], &mut m, 0.0)[0], v(0.0, 0.0, 1.0));
    }

    #[test]
    fn signatures_scale_with_design() {
        let sensors = [PartSlot(1), PartSlot(4)];
        let consumers = [PartSlot(2)];
        let pumps = [PartSlot(5)];
        let doors = [PartSlot(6), PartSlot(7)];
        let f = SigFacts { mounts: &[], sensor_slots: &sensors, consumer_slots: &consumers, pumps: &pumps, doors: &doors };
        assert_eq!(stage_sig(OfficerStage::TrackFusion, &f).inputs.len(), 2);
        assert_eq!(stage_sig(OfficerStage::LoadShedder, &f).outputs.len(), 4);
        let dc = stage_sig(OfficerStage::DamageControlPlanner, &f);
        assert_eq!(dc.outputs.len(), 2 + 2 + 3);
        assert_eq!(dc_door_port(1, 1), Some(3));
        assert_eq!(dc.outputs[3].name, "door_closed");
        assert_eq!(stage_sig(OfficerStage::MountAssignment, &f).outputs.len(), 1);
        // Port lists never exceed what a u8 port index can name.
        let many = vec![PartSlot(0); 400];
        let f = SigFacts { mounts: &[], sensor_slots: &many, consumer_slots: &many, pumps: &many, doors: &many };
        for st in ALL_STAGES { let sig = stage_sig(st, &f); assert!(sig.inputs.len() <= MAX_PORTS && sig.outputs.len() <= MAX_PORTS); }
    }

    #[test]
    fn values_round_trip() {
        for v in [Value::None, Value::Bool(true), Value::Scalar(2.5), Value::Vec3([1.0, 2.0, 3.0]), Value::Text("x".into()), Value::Stance(Stance::Hostile), Value::Emcon(Emcon::Active), Value::Preset(DoctrinePreset::SilentRunning), Value::Rung(IdRung::Class)] {
            assert_eq!(port_value_to(&port_value_from(&v)), v);
        }
        assert_eq!(port_value_from(&Value::Scalar(f64::INFINITY)), PortValue::None);
        assert_eq!(normalize_sink_value(SinkKind::ShipPreset, 0, s(2.0)), PortValue::Preset(DoctrinePreset::SilentRunning));
        assert_eq!(normalize_sink_value(SinkKind::TrackIdOverride, 1, s(9.0)), PortValue::None);
        assert_eq!(normalize_sink_value(SinkKind::Door(PartSlot(1)), 0, s(1.0)), b(true));
        assert_eq!(normalize_sink_value(SinkKind::HelmHeading, 0, PortValue::Track(crate::ids::TrackId(1))), PortValue::None);
    }

    #[test]
    fn palette_and_keys_cover_every_name() {
        let pal = palette();
        assert!(pal.iter().all(|(_, k)| k.starts_with("graph.node.")));
        assert_eq!(PORT_NAMES.len(), PORT_KEYS.len());
        for (n, k) in PORT_NAMES.iter().zip(PORT_KEYS) { assert_eq!(format!("graph.port.{n}"), *k); }
        let keys = text_keys();
        let pumps = [PartSlot(5)];
        let f = SigFacts { mounts: &[], sensor_slots: &pumps, consumer_slots: &pumps, pumps: &pumps, doors: &pumps };
        for (kind, _) in &pal {
            let sig = signature(kind, &f);
            for p in sig.inputs.iter().chain(sig.outputs.iter()) { assert!(port_key(p.name).is_some(), "unlisted port {}", p.name); }
            for p in &sig.params { assert!(keys.contains(&p.label)); }
        }
        let csv = include_str!("../../../../game/i18n/graph.csv");
        assert!(csv.starts_with("keys,?context,en"));
        for k in &keys { assert!(csv.lines().any(|l| l.starts_with(&format!("{k},"))), "graph.csv lacks {k}"); }
    }
}
