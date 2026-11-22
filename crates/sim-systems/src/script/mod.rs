//! Unit `sys-script`: the Luau host seam (trait, types, the host API registry and the null host).
//! `luau.rs` (unit `sys-script-luau`, Phase::L4, scheduled last, behind the `luau` cargo feature) will hold
//! the mlua-backed implementation; it does not exist yet and nothing else may depend on mlua.
//! DESIGN.md L355-390 (L4 scripting), L2115-2148 (script safety, fuzz targets, kill switch).
//!
//! Contract for any implementation:
//! - one VM per vehicle, set up in this order: `Lua::new_with(StdLib::ALL_SAFE ^ StdLib::OS, ...)`, host
//!   removes or replaces globals, `set_memory_limit`, `set_interrupt(budget counter)`, `sandbox(true)` last;
//!   mlua pinned `=0.12.2` with feature `luau` only, never `luau-jit`;
//! - limits per vehicle: 8 MB memory, 5,000 interrupts per graph tick shared by all its script nodes, and a
//!   2 ms wall-clock backstop per script per graph tick. The backstop is the single place in sim-systems
//!   allowed to read a clock; tripping it disables the script and marks the tick not reproducible;
//! - the interrupt callback counts and returns an error once past the budget (deterministic work count); it
//!   fires only at calls and loop iterations, so every host function must be constant-time or chunked and
//!   declares its bound in [`HostFn::work_units`];
//! - the session has one `ScriptHost` and it is not shared across threads: the Sim ticks ships that have
//!   enabled script nodes serially, in ascending vessel order, after the parallel batch of script-free ships;
//! - scripts run only on the authoritative host; clients receive outputs; replays record the server's output
//!   stream. Luau never runs in a PvP lobby. Guest scripts run only when `SimSettings::guest_scripts` is set
//!   and the fuzz gate has passed; scripts in imported files arrive disabled.
#[cfg(feature = "luau")]
pub mod luau;

use crate::graph::PortValue;
use crate::prelude::*;
use crate::protocol::event::ScriptFault;
use content::graph::{PortType, ScriptSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptLimits { pub memory_bytes: usize, pub interrupts_per_tick: u32, pub backstop_micros: u32 }
impl Default for ScriptLimits { fn default() -> Self { Self { memory_bytes: 8 * 1024 * 1024, interrupts_per_tick: 5_000, backstop_micros: 2_000 } } }

/// Result of running one script node for one graph tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptRun { pub fault: Option<ScriptFault>, pub interrupts: u32 }

/// One function scripts may call. The registry is the fuzz dictionary source (`fuzz/host_api.dict`, L2136).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostFn { pub name: &'static str, pub args: &'static [PortType], pub ret: PortType, /// Declared bound in host work units; the fuzz oracle checks no call exceeds it (L2132).
    pub work_units: u32 }

/// The host API registry: read-only queries on the ship's own view (own state, part state, tracks by index,
/// detections by index) and math helpers. Scripts act only through their typed output ports.
/// Unit: `sys-script`.
///
/// The registry index of a function is its id on the wire to [`HostApi::call`]; the order is append-only
/// (never reorder or remove a row, only add at the end), and [`host_fn`] names every index. Semantics, which
/// every `HostApi` implementation must follow exactly:
///
/// - Arguments arrive in the declared order and types. A call with the wrong number of arguments or a value
///   of the wrong kind is a script bug: the implementation returns `Err(ScriptFault::Error)`
///   ([`check_args`] does this test). Scalars are `PortValue::Scalar` (a `Bool` is accepted as 0 or 1);
///   angles travel as `Scalar` radians; vectors as `Vec3`; tracks as `Track`.
/// - A well-typed call that names something that does not exist (an index past the end, a slot with no part,
///   a non-finite number, a track that is not in the picture) returns `Ok(PortValue::None)`, "no value",
///   never a fault: scripts must handle missing data the way graph nodes do.
/// - Indices (`i`) are 0-based and truncate toward zero; negative indices are out of range. Part slots are
///   blueprint slot numbers.
/// - "The picture" is the side picture the ship reads over the datalink, live tracks only, ascending track
///   id (the order of `SidePicture::tracks` with dropped tracks skipped). "Detections" are the ship's own
///   detections of the last second in `Ship::detections` order.
/// - Positions are world metres (+X east, +Y up, +Z south as in sim-core); headings and bearings are compass
///   radians in `[0, 2 pi)`, 0 = north, clockwise; speeds are knots where the name says `_kn`, else m/s.
/// - Every function is constant time except where its `work_units` say otherwise; the implementation must
///   not exceed the declared bound (the fuzz oracle counts host work units, L2132).
///
/// | # | name | args | returns |
/// | 0 | `own_position` | | V own grid origin |
/// | 1 | `own_velocity` | | V |
/// | 2 | `own_heading` | | A compass heading |
/// | 3 | `own_speed_kn` | | S speed along the bow |
/// | 4 | `own_heel` | | A signed heel, positive to starboard |
/// | 5 | `sortie_time_s` | | S seconds since the sortie began |
/// | 6 | `hull_health` | | S fraction of solid cells intact, 0..=1 |
/// | 7 | `part_health` | slot S | S 0..=1 |
/// | 8 | `part_works` | slot S | B not destroyed |
/// | 9 | `part_ammo_fraction` | slot S | S ready rounds over capacity (mounts, launchers, decoys) |
/// | 10 | `part_train` | slot S | A mount or sensor training, mount frame |
/// | 11 | `part_elevation` | slot S | A mount elevation |
/// | 12 | `track_count` | | S live tracks in the picture |
/// | 13 | `track_id` | i S | T |
/// | 14 | `track_position` | i S | V predicted to the current tick |
/// | 15 | `track_velocity` | i S | V |
/// | 16 | `track_stance` | i S | St effective stance (override applied) |
/// | 17 | `track_domain` | i S | S 0 surface, 1 subsurface, 2 air, 3 weapon |
/// | 18 | `track_grade` | i S | S 0 bearing, 1 coarse, 2 fine, 3 lock |
/// | 19 | `track_rung` | i S | S 0 unknown .. 4 exact |
/// | 20 | `track_radius95_m` | i S | S 95% radius now |
/// | 21 | `track_inbound` | i S | B |
/// | 22 | `detection_count` | | S |
/// | 23 | `detection_sensor` | i S | S sensor part slot (`None` for a seeker) |
/// | 24 | `detection_bearing` | i S | A compass bearing from the sensor |
/// | 25 | `detection_range_m` | i S | S (`None` for bearing-only sensors) |
/// | 26 | `designated_track` | | T the standing designation (`None` when nothing is designated) |
/// | 27 | `emcon` | | E the ship's EMCON setting |
/// | 28 | `bearing_to` | point V | A compass bearing from own position, horizontal |
/// | 29 | `range_to` | point V | S horizontal distance from own position |
/// | 30 | `wrap_angle` | a A | A wrapped to `[-pi, pi)` |
/// | 31 | `clamp` | x S, lo S, hi S | S (`None` when lo > hi) |
/// | 32 | `intercept_point` | pos V, vel V, speed S | V point where a body leaving own position at `speed` m/s meets a target at `pos` moving at `vel`, earliest positive time; `None` when it cannot |
/// | 33 | `track_index` | track T | S picture index of a track, by binary search (`None` when absent) |
pub fn host_api() -> &'static [HostFn] { &HOST_API }

/// Registry indices of [`host_api`], one constant per row. Append-only.
pub mod host_fn {
    pub const OWN_POSITION: u16 = 0;
    pub const OWN_VELOCITY: u16 = 1;
    pub const OWN_HEADING: u16 = 2;
    pub const OWN_SPEED_KN: u16 = 3;
    pub const OWN_HEEL: u16 = 4;
    pub const SORTIE_TIME_S: u16 = 5;
    pub const HULL_HEALTH: u16 = 6;
    pub const PART_HEALTH: u16 = 7;
    pub const PART_WORKS: u16 = 8;
    pub const PART_AMMO_FRACTION: u16 = 9;
    pub const PART_TRAIN: u16 = 10;
    pub const PART_ELEVATION: u16 = 11;
    pub const TRACK_COUNT: u16 = 12;
    pub const TRACK_ID: u16 = 13;
    pub const TRACK_POSITION: u16 = 14;
    pub const TRACK_VELOCITY: u16 = 15;
    pub const TRACK_STANCE: u16 = 16;
    pub const TRACK_DOMAIN: u16 = 17;
    pub const TRACK_GRADE: u16 = 18;
    pub const TRACK_RUNG: u16 = 19;
    pub const TRACK_RADIUS95_M: u16 = 20;
    pub const TRACK_INBOUND: u16 = 21;
    pub const DETECTION_COUNT: u16 = 22;
    pub const DETECTION_SENSOR: u16 = 23;
    pub const DETECTION_BEARING: u16 = 24;
    pub const DETECTION_RANGE_M: u16 = 25;
    pub const DESIGNATED_TRACK: u16 = 26;
    pub const EMCON: u16 = 27;
    pub const BEARING_TO: u16 = 28;
    pub const RANGE_TO: u16 = 29;
    pub const WRAP_ANGLE: u16 = 30;
    pub const CLAMP: u16 = 31;
    pub const INTERCEPT_POINT: u16 = 32;
    pub const TRACK_INDEX: u16 = 33;
}

const fn hf(name: &'static str, args: &'static [PortType], ret: PortType, work_units: u32) -> HostFn { HostFn { name, args, ret, work_units } }

use PortType as Pt;
static HOST_API: [HostFn; 34] = [
    hf("own_position", &[], Pt::Vec3, 1),
    hf("own_velocity", &[], Pt::Vec3, 1),
    hf("own_heading", &[], Pt::Angle, 1),
    hf("own_speed_kn", &[], Pt::Scalar, 1),
    hf("own_heel", &[], Pt::Angle, 1),
    hf("sortie_time_s", &[], Pt::Scalar, 1),
    hf("hull_health", &[], Pt::Scalar, 1),
    hf("part_health", &[Pt::Scalar], Pt::Scalar, 1),
    hf("part_works", &[Pt::Scalar], Pt::Bool, 1),
    hf("part_ammo_fraction", &[Pt::Scalar], Pt::Scalar, 2),
    hf("part_train", &[Pt::Scalar], Pt::Angle, 1),
    hf("part_elevation", &[Pt::Scalar], Pt::Angle, 1),
    hf("track_count", &[], Pt::Scalar, 1),
    hf("track_id", &[Pt::Scalar], Pt::Track, 1),
    hf("track_position", &[Pt::Scalar], Pt::Vec3, 1),
    hf("track_velocity", &[Pt::Scalar], Pt::Vec3, 1),
    hf("track_stance", &[Pt::Scalar], Pt::Stance, 1),
    hf("track_domain", &[Pt::Scalar], Pt::Scalar, 1),
    hf("track_grade", &[Pt::Scalar], Pt::Scalar, 1),
    hf("track_rung", &[Pt::Scalar], Pt::Scalar, 1),
    hf("track_radius95_m", &[Pt::Scalar], Pt::Scalar, 2),
    hf("track_inbound", &[Pt::Scalar], Pt::Bool, 1),
    hf("detection_count", &[], Pt::Scalar, 1),
    hf("detection_sensor", &[Pt::Scalar], Pt::Scalar, 1),
    hf("detection_bearing", &[Pt::Scalar], Pt::Angle, 1),
    hf("detection_range_m", &[Pt::Scalar], Pt::Scalar, 1),
    hf("designated_track", &[], Pt::Track, 1),
    hf("emcon", &[], Pt::Emcon, 1),
    hf("bearing_to", &[Pt::Vec3], Pt::Angle, 2),
    hf("range_to", &[Pt::Vec3], Pt::Scalar, 1),
    hf("wrap_angle", &[Pt::Angle], Pt::Angle, 1),
    hf("clamp", &[Pt::Scalar, Pt::Scalar, Pt::Scalar], Pt::Scalar, 1),
    hf("intercept_point", &[Pt::Vec3, Pt::Vec3, Pt::Scalar], Pt::Vec3, 4),
    // Binary search over at most 2^16 tracks: 17 comparisons.
    hf("track_index", &[Pt::Track], Pt::Scalar, 17),
];

/// Registry index of a host function by name. Unit: `sys-script`.
pub fn host_fn_index(name: &str) -> Option<u16> { host_api().iter().position(|f| f.name == name).map(|i| i as u16) }

/// Arity and type check of a host call against the registry (the first step of every [`HostApi::call`]):
/// `Err(ScriptFault::Error)` for an unknown index, a wrong argument count or an argument of the wrong kind.
/// `PortValue::None` is accepted for any type (a missing value; the function then returns `None`). Scalars and
/// angles accept `Bool` as 0 or 1. Unit: `sys-script`.
pub fn check_args(index: u16, args: &[PortValue]) -> Result<&'static HostFn, ScriptFault> {
    let f = host_api().get(index as usize).ok_or(ScriptFault::Error)?;
    if args.len() != f.args.len() { return Err(ScriptFault::Error); }
    for (a, t) in args.iter().zip(f.args) {
        let ok = match (a, t) {
            (PortValue::None, _) => true,
            (PortValue::Scalar(_) | PortValue::Bool(_), Pt::Scalar | Pt::Angle) => true,
            (PortValue::Bool(_), Pt::Bool) => true,
            (PortValue::Vec3(_), Pt::Vec3) => true,
            (PortValue::Track(_), Pt::Track) => true,
            (PortValue::Stance(_), Pt::Stance) => true,
            (PortValue::Emcon(_), Pt::Emcon) => true,
            (PortValue::Text(_), Pt::Text) => true,
            _ => false,
        };
        if !ok { return Err(ScriptFault::Error); }
    }
    Ok(f)
}

/// The libFuzzer dictionary `fuzz/host_api.dict` (L2136): every host function name, then every Luau keyword,
/// one quoted token per line. Unit: `sys-script`.
pub fn fuzz_dictionary() -> String {
    const LUAU_KEYWORDS: [&str; 26] = ["and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "continue", "export", "type", "typeof", "self"];
    let mut s = String::new();
    for f in host_api() { s.push('"'); s.push_str(f.name); s.push_str("\"\n"); }
    for k in LUAU_KEYWORDS { s.push('"'); s.push_str(k); s.push_str("\"\n"); }
    s
}

/// Pure maths of [`host_fn::INTERCEPT_POINT`], shared by every `HostApi` implementation: the earliest positive
/// time `t` with `|pos + vel t - origin| = speed t`, returned as the meeting point. Unit: `sys-script`.
pub fn intercept_point(origin: DVec3, pos: DVec3, vel: DVec3, speed: f64) -> Option<DVec3> {
    if !(origin.is_finite() && pos.is_finite() && vel.is_finite() && speed.is_finite() && speed > 0.0) { return None; }
    let d = pos - origin;
    let a = vel.length_squared() - speed * speed;
    let b = 2.0 * d.dot(vel);
    let c = d.length_squared();
    if c == 0.0 { return Some(pos); }
    let t = if a.abs() < 1e-12 {
        if b >= 0.0 { return None; }
        -c / b
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 { return None; }
        let sq = libm::sqrt(disc);
        let (t1, t2) = ((-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a));
        let (lo, hi) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        if lo > 0.0 { lo } else if hi > 0.0 { hi } else { return None; }
    };
    let p = pos + vel * t;
    if t.is_finite() && p.is_finite() { Some(p) } else { None }
}

/// What a host function may read; implemented by the stage runner over the ship's view.
pub trait HostApi {
    /// Call registry function `index` with typed arguments. Must stay within the function's declared bound.
    fn call(&mut self, index: u16, args: &[PortValue]) -> Result<PortValue, ScriptFault>;
}

/// The seam the graph VM talks to. `Send` so a `Sim` owning one can move to the sim thread.
pub trait ScriptHost: Send {
    /// Create or replace the vehicle's VM and compile its scripts. Disabled sources are skipped. Returns one
    /// result per source.
    fn load(&mut self, vessel: EntityId, sources: &[ScriptSource], limits: ScriptLimits) -> Vec<Result<(), ScriptFault>>;
    /// Reset the vehicle's shared interrupt counter at the start of a graph tick.
    fn begin_tick(&mut self, vessel: EntityId);
    /// Run script `index` of the vehicle: inputs in declared port order, outputs written to `out`. On a fault
    /// `out` is left untouched.
    fn run(&mut self, vessel: EntityId, index: u16, inputs: &[PortValue], api: &mut dyn HostApi, out: &mut [PortValue]) -> ScriptRun;
    fn unload(&mut self, vessel: EntityId);
    /// Memory in use by a vehicle's VM, for the editor readout (shipped examples stay under 1 MB, L375).
    fn memory_bytes(&self, vessel: EntityId) -> usize;
}

/// The host used wherever scripts cannot run: every script reports `ScriptFault::Error` at load and never
/// runs, so its sinks stay with the officer stages. Real code: this is the default until L4 ships.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullScriptHost;
impl ScriptHost for NullScriptHost {
    fn load(&mut self, _vessel: EntityId, sources: &[ScriptSource], _limits: ScriptLimits) -> Vec<Result<(), ScriptFault>> { sources.iter().map(|_| Err(ScriptFault::Error)).collect() }
    fn begin_tick(&mut self, _vessel: EntityId) {}
    fn run(&mut self, _vessel: EntityId, _index: u16, _inputs: &[PortValue], _api: &mut dyn HostApi, _out: &mut [PortValue]) -> ScriptRun { ScriptRun { fault: Some(ScriptFault::Error), interrupts: 0 } }
    fn unload(&mut self, _vessel: EntityId) {}
    fn memory_bytes(&self, _vessel: EntityId) -> usize { 0 }
}

/// Whether scripts of a given owner may run in this session (L2119-2121, L388): single-player own scripts
/// yes; co-op, skirmish and sandbox only the host's own unless guest scripts are enabled and unlocked; PvP
/// never. Unit: `sys-script`.
///
/// The function cannot tell a single-player session from a hosted lobby, so it follows the rule that covers
/// both: outside PvP the host's own scripts always run (single-player is always the host); a guest's scripts
/// run only when the lobby allows Luau (`Ruleset::luau`), the host turned guest scripts on
/// (`SimSettings::guest_scripts`) and this build passed the fuzz gate. Scripts never run in a PvP mode, or under
/// a ruleset that bans Luau for a guest.
pub fn scripts_allowed(mode: GameMode, settings: &content::settings::SimSettings, owner_is_host: bool, guest_gate_passed: bool) -> bool {
    if matches!(mode, GameMode::PvpElimination | GameMode::PvpEscort) { return false; }
    if owner_is_host { return true; }
    settings.ruleset.luau && settings.guest_scripts && guest_gate_passed
}

/// Create the session's script host: the Luau host when the `luau` feature is built, else [`NullScriptHost`].
/// Unit: `sys-script`.
///
/// The `luau` build calls `luau::LuauHost::new()`, the constructor unit `sys-script-luau` must provide (a host
/// with no vehicles loaded; VMs are created per vessel in [`ScriptHost::load`]).
pub fn new_host() -> Box<dyn ScriptHost> {
    #[cfg(feature = "luau")]
    { return Box::new(luau::LuauHost::new()); }
    #[cfg(not(feature = "luau"))]
    { Box::new(NullScriptHost) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_well_formed_and_indexed() {
        let api = host_api();
        assert_eq!(api.len(), 34);
        let mut names: Vec<&str> = api.iter().map(|f| f.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), api.len(), "names are unique");
        for (i, f) in api.iter().enumerate() {
            assert_eq!(host_fn_index(f.name), Some(i as u16));
            assert!(f.work_units >= 1 && f.work_units <= 32, "{} declares a small constant bound", f.name);
            assert!(f.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
        }
        assert_eq!(api[host_fn::TRACK_INDEX as usize].name, "track_index");
        assert_eq!(api[host_fn::INTERCEPT_POINT as usize].args.len(), 3);
        assert_eq!(api[host_fn::EMCON as usize].ret, PortType::Emcon);
    }

    #[test]
    fn args_are_checked() {
        assert!(check_args(host_fn::OWN_POSITION, &[]).is_ok());
        assert_eq!(check_args(host_fn::OWN_POSITION, &[PortValue::Scalar(1.0)]), Err(ScriptFault::Error));
        assert_eq!(check_args(999, &[]), Err(ScriptFault::Error));
        assert!(check_args(host_fn::PART_HEALTH, &[PortValue::Scalar(3.0)]).is_ok());
        assert!(check_args(host_fn::PART_HEALTH, &[PortValue::Bool(true)]).is_ok());
        assert!(check_args(host_fn::PART_HEALTH, &[PortValue::None]).is_ok());
        assert_eq!(check_args(host_fn::PART_HEALTH, &[PortValue::Vec3(DVec3::ZERO)]), Err(ScriptFault::Error));
        assert_eq!(check_args(host_fn::TRACK_INDEX, &[PortValue::Scalar(1.0)]), Err(ScriptFault::Error));
        assert!(check_args(host_fn::TRACK_INDEX, &[PortValue::Track(TrackId(4))]).is_ok());
    }

    #[test]
    fn dictionary_lists_every_function_and_keyword() {
        let d = fuzz_dictionary();
        for f in host_api() { assert!(d.contains(&format!("\"{}\"\n", f.name))); }
        assert!(d.contains("\"function\"\n"));
        assert_eq!(d.lines().count(), host_api().len() + 26);
    }

    #[test]
    fn intercept_meets_the_target() {
        let o = DVec3::ZERO;
        let p = DVec3::new(1_000.0, 0.0, 0.0);
        let v = DVec3::new(0.0, 0.0, 10.0);
        let q = intercept_point(o, p, v, 20.0).unwrap();
        let t = (q - p).length() / 10.0;
        assert!(((q - o).length() - 20.0 * t).abs() < 1e-6);
        // A target running away faster than the body cannot be met.
        assert_eq!(intercept_point(o, p, DVec3::new(30.0, 0.0, 0.0), 20.0), None);
        assert_eq!(intercept_point(o, p, v, f64::NAN), None);
        assert_eq!(intercept_point(o, p, v, 0.0), None);
        // Equal speeds, closing: the linear case.
        let q = intercept_point(o, p, DVec3::new(-20.0, 0.0, 0.0), 20.0).unwrap();
        assert!((q.x - 500.0).abs() < 1e-9);
    }

    #[test]
    fn script_policy_follows_the_lobby_table() {
        let mut s = content::settings::SimSettings::default();
        assert!(scripts_allowed(GameMode::Campaign, &s, true, false));
        assert!(scripts_allowed(GameMode::Splash, &s, true, false));
        assert!(!scripts_allowed(GameMode::PvpElimination, &s, true, true));
        assert!(!scripts_allowed(GameMode::PvpEscort, &s, true, true));
        // Guests: off until the host enables Luau and guest scripts and the gate has passed.
        assert!(!scripts_allowed(GameMode::Skirmish, &s, false, true));
        s.guest_scripts = true;
        s.ruleset.luau = true;
        assert!(!scripts_allowed(GameMode::Skirmish, &s, false, false));
        assert!(scripts_allowed(GameMode::Skirmish, &s, false, true));
        s.ruleset.luau = false;
        assert!(!scripts_allowed(GameMode::Sandbox, &s, false, true));
    }

    #[test]
    #[cfg(not(feature = "luau"))]
    fn null_host_is_the_default_build() {
        let mut h = new_host();
        let srcs = vec![ScriptSource { name: "a".into(), source: String::new(), inputs: vec![], outputs: vec![], enabled: true }];
        assert_eq!(h.load(EntityId::default(), &srcs, ScriptLimits::default()), vec![Err(ScriptFault::Error)]);
        assert_eq!(h.memory_bytes(EntityId::default()), 0);
    }
}
