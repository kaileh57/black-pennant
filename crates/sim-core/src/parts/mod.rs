//! Unit `sim-core-parts`. Runtime state of placed parts and the per-tick control inputs they consume.
//! DESIGN.md L561 (part health and output), L283 (ammunition inside mount kits), L394 (fallbacks),
//! L412 (rotating antennas), L638-648 (pumps, valves).
//!
//! sim-core moves parts physically (slew mounts, spool engines, feed hoists, rotate antennas, run pumps).
//! Decisions (what to aim at, when to fire, what to shed) arrive from sim-systems as [`VesselControls`].
//!
//! # Conventions (shared with hydro, sensors, gunnery and the client)
//! - Signs follow the controls: positive throttle is ahead, positive rudder and jet deflection are to
//!   starboard (the bow swings to starboard).
//! - Mount frame (guns and trainable launchers): the vessel frame turned about +Y by the mount's zero bearing
//!   `rot.yaw * pi / 2` ([`mount_zero_rad`]). Training is a right-handed turn about +Y, so positive training
//!   swings the muzzle from the bow toward port; elevation is measured up from the deck plane. This is the
//!   convention of `sim_systems::guidance::gun::mount_dir_to_world`, ported here as [`mount_dir_local`].
//! - Sensor angles (`bearing_rad`, `train_rad`) are compass-sense: clockwise from the part's facing, seen
//!   from above, as the sensor and signature units read them.
//! - A shell leaves at the mount's footprint centre (L2489) with world velocity `dir * muzzle_mps +
//!   kin.lin_vel`, the same law as `sim_systems::guidance::gun::shell_velocity`.
//!
//! # Implementation decisions
//! - Output factor (L561): engines, hoists, slew rates, rate of fire, reload and recharge timers all run at
//!   `output_factor()` (1.0, 0.5 at Heavy, 0 when destroyed) times the network supply fraction
//!   (`links.power_supplied`; `fuel_supplied` for diesels and gas turbines). Parts with no Power port get 1.0
//!   from the network solve, so only consumers stall when power is shed.
//! - Engines share one throttle. Each spools toward `controls.throttle` at `1 / spool_s` per second
//!   (reversal passes through zero). Their signed shaft output is pooled and shared by the working
//!   propulsors in proportion to their absorbable power (`max_shaft_kw * output_factor`), each capped there.
//! - Rudders slew at `rate_dps` toward `controls.rudder * max_deg`; with no power they stay where they are.
//!   Waterjet nozzles deflect to `controls.rudder * steer_max_deg` and cross their whole range in 2 s.
//! - Mount training takes the shortest way round unless the blueprint hand-sets a limited arc
//!   (`OfficerSettings::mount_limits`), in which case orders are clamped into the arc and the slew stays
//!   inside it. Auto-set arcs live in sim-systems, which clamps its orders to them. A mount is on target when
//!   both remaining errors are within max(1 mrad, its 1-sigma dispersion) and the order lies within its limits;
//!   it fires only then, and only with ready rounds, fire permission and power.
//! - Firing runs a continuous cycle clock, so rates above 60 rounds per second per barrel and 30 Hz hulls keep
//!   their exact average rate. Every barrel fires once per cycle; `MountOrder::max_rounds` caps rounds per
//!   tick (0 means no cap). Dispersion is isotropic about the barrel: two normal deviates per round from
//!   `Stream::GunDispersion` keyed (vessel id, mount slot) with draws `4k` and `4k + 2` for round `k`.
//! - Ammunition feed (L283, L207): a gun draws from the magazines of its own calibre in its kit and through
//!   the hoists in its kit, at the hoists' combined rate (shared in slot order when one hoist serves several
//!   guns). A magazine that touches the gun's footprint feeds it directly at the gun's cyclic rate. A kit with
//!   no magazine of the right calibre, or a gun placed outside any kit, uses the nearest magazines and hoists
//!   placed outside any kit (up to [`MAX_FEED`] each). A launcher with an at-sea reload draws from its kit's
//!   magazines no gun uses, else from the nearest such loose magazines. Drawing a round takes the per-round share of
//!   the magazine's ammunition mass. A flooded or destroyed magazine feeds nothing.
//! - One-shot orders (`Launch`, `DropMine`, `FireDecoy`, `LaunchOffboard`, `RecoverOffboard`) are consumed:
//!   the order is cleared from `vessel.controls` on the tick it acts. A launch, decoy salvo or offboard launch
//!   that waits on its interval, reload, recharge or power stays pending; one that can never act (no rounds, part destroyed, unknown
//!   munition) is dropped. A mine that cannot drop now is dropped, because a late mine lands in the wrong place.
//!   Persistent orders (`Mount`, `Emit`, `Pump`, `Door`, `Ballast`, `StreamDecoy`) hold until replaced.
//! - Launch geometry: munitions leave the part centre with the hull's velocity plus a short ejection along the
//!   launch axis (missiles 25 m/s pitched 30 degrees up from the launcher's facing, or straight along it when
//!   the launcher stands vertical; torpedoes 12 m/s level; depth charges 3 m/s; mines 1 m/s astern of the
//!   direction of travel (astern of the bow when stopped); drones and helicopters lift at 5 m/s; RHIBs are lowered with no extra speed). Trainable
//!   launchers train toward a `Point` or `Bearing` aim at their rate and launch within 2 degrees of it.
//! - Decoy salvos put one decoy object per round on a fan of 15 degree steps along the launcher's facing:
//!   chaff 40 m out and 15 m up, flares 30 m out and 10 m up, smoke 20 m out at sea level. A towed decoy is
//!   spawned once when streaming starts; `streamed` going false (order, or the part destroyed) means the
//!   world should remove it.
//! - Sensors: search and navigation radars turn at the part's rpm (24 when unset or invalid, L412) while
//!   powered; other sensors stare along `train_rad`, which `Emit` sets at once. Only search, navigation and
//!   fire-control radars and active sonar can radiate, and only while working, enabled and powered.
//! - Pumps run while ordered, working, enabled and powered; a target must be the pump's own compartment or an
//!   adjacent one, otherwise the pump drains its own. Ballast moves toward the ordered load at the large pump's
//!   0.15 m^3/s of sea water (L638), scaled by power.
//! - Timers in whole ticks (`reload_ticks`, `interval_ticks`, `recharge_ticks`) count base ticks of
//!   [`crate::time::DT`], `round(dt / DT)` per call, slowed by the factor above through a stateless dither on the
//!   tick counter, so every machine counts the same ticks.
//! - Lost hulls and wrecks shut every part down (engines, emitters, pumps) and keep their loads.
//! - `refresh_health` never raises health: the cell mean (L561) caps the stored value, so fire burn-down and
//!   repairs, which set health directly, are kept. Owners are found through `PartRuntime::cells`.
//! - Recovered offboard bodies return through [`recover_offboard`], called by the world on
//!   `BodyOutcome::Recovered`.
mod build;
mod step;
#[cfg(test)]
mod tests;

use crate::networks::PartLinks;
use content::enums::DamageLevel;
use content::ids::{CellIndex, CompartmentId, MunitionIdx, PartIdx, PartSlot};
use content::kit::PartRot;
use serde::{Deserialize, Serialize};

/// Kind-specific runtime state. One variant per `content::part::PartFunction` that has state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum PartState {
    #[default] Inert,
    /// `throttle` is the spooled actual throttle, -1.0 (full astern) ..= 1.0; `output_kw` the shaft power
    /// delivered now (non-negative).
    Engine { throttle: f64, output_kw: f64 },
    /// Written by the network solve.
    Generator { output_kw: f64 },
    /// Written by the network solve.
    Battery { kwh: f64 },
    /// Written by the network solve.
    FuelTank { litres: f64 },
    /// `shaft_kw` is the absorbed shaft power (non-negative); `thrust_n` the static thrust at zero speed along
    /// the propulsor's +X axis, negative astern (hydro applies the speed law and the out-of-water test);
    /// `steer_rad` the waterjet deflection, positive to starboard.
    Propulsor { shaft_kw: f64, thrust_n: f64, steer_rad: f64 },
    /// Rudder angle, positive to starboard.
    Rudder { angle_rad: f64 },
    Ballast { kg: f64 },
    Mount(MountState),
    Magazine { rounds: u32, ammo_kg: f64, flooded: bool, cookoff_ticks: u32 },
    /// `reload_ticks`: base ticks left on the cell being reloaded (0 = none); `interval_ticks`: base ticks
    /// until the next launch; `train_rad`: mount-frame training of a trainable launcher.
    Launcher { ready: u8, reload_ticks: u32, interval_ticks: u32, train_rad: f64 },
    /// `since_last_m`: horizontal distance run since the last mine (`f64::MAX` before the first).
    MineRail { mines: u16, since_last_m: f64 },
    /// `reload_ticks`: base ticks until the next salvo; `streamed`: a towed decoy is out.
    Decoy { rounds: u16, reload_ticks: u32, streamed: bool },
    /// Radar, sonar, ESM, camera: rotation phase for rotating antennas and whether it radiates this tick.
    Sensor { bearing_rad: f64, swept_from_rad: f64, radiating: bool, train_rad: f64 },
    Jammer { radiating: bool },
    Pump { running: bool, target: Option<CompartmentId> },
    Door { closed: bool },
    /// `recharge_ticks`: base ticks until a recovered drone can launch again.
    DroneBay { aboard: u8, recharge_ticks: u32 },
    HeliDeck { aboard: bool },
    Davit { rhib_aboard: bool },
}

/// Trainable gun mount state. Angles are relative to the mount's zero (bow for yaw 0), radians.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MountState {
    pub train_rad: f64,
    pub elevation_rad: f64,
    pub ready_rounds: u32,
    /// Ticks until each barrel may fire again (`u32::MAX` while the mount has no power).
    pub cooldown_ticks: u32,
    /// Rounds fired in total, for the report card.
    pub fired: u32,
    /// True on ticks the mount is within tolerance of the ordered angles.
    pub on_target: bool,
}

/// Private mechanics of one part (feed wiring, firing clock, hand-set arc). It lives in `PartLinks::mech`
/// because `MountState`, `PartState` and `PartRuntime` are built by literal outside this crate, where private
/// fields would break them. Outside sim-core it is only ever `Default`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PartMech {
    pub(crate) feed: FeedLinks,
    /// Seconds of the firing cycle accumulated, capped at one cycle (a loaded, idle gun fires at once).
    pub(crate) cycle_s: f64,
    /// Fraction of the next round already moved up the feed.
    pub(crate) feed_progress: f64,
    /// Hand-set training and elevation limits from the blueprint, radians.
    pub(crate) arc: Option<HandArc>,
    /// Magazines only: seconds of fire-main flooding received so far. The flooding unit marks the magazine
    /// flooded once this reaches `FloodingTuning::magazine_flood_s` (L648).
    #[serde(default)]
    pub(crate) flood_s: f64,
}

/// Hand-set mount limits in radians (`OfficerSettings::mount_limits`), with the full-circle flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct HandArc { pub t_min: f64, pub t_max: f64, pub e_min: f64, pub e_max: f64, pub full: bool }

/// Most magazines and most hoists one feed chain lists.
pub const MAX_FEED: usize = 4;

/// Ammunition feed wiring of a gun or reloading launcher, fixed at build (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedLinks {
    pub(crate) mags: [u16; MAX_FEED],
    pub(crate) n_mags: u8,
    pub(crate) hoists: [u16; MAX_FEED],
    pub(crate) n_hoists: u8,
    /// A magazine touches the gun, so it feeds without a hoist.
    pub(crate) direct: bool,
}
impl FeedLinks {
    /// Magazines feeding this part, in draw order.
    pub fn magazines(&self) -> impl Iterator<Item = PartSlot> + '_ { self.mags[..(self.n_mags as usize).min(MAX_FEED)].iter().map(|&s| PartSlot(s)) }
    /// Hoists lifting rounds to this part, ascending slot.
    pub fn hoists(&self) -> impl Iterator<Item = PartSlot> + '_ { self.hoists[..(self.n_hoists as usize).min(MAX_FEED)].iter().map(|&s| PartSlot(s)) }
    /// True when a magazine touches the gun and feeds it directly.
    pub fn direct(&self) -> bool { self.direct }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartRuntime {
    pub slot: PartSlot,
    pub def: PartIdx,
    /// Minimum-corner cell and rotation, copied from the blueprint.
    pub pos: content::cell::CellPos,
    pub rot: PartRot,
    /// Footprint cells in ascending index.
    pub cells: Vec<CellIndex>,
    /// Centre of the footprint in vessel-local metres (muzzle, antenna and thrust point derive from it).
    pub centre: glam::DVec3,
    pub compartment: Option<CompartmentId>,
    /// Mean health of its cells, 0.0..=1.0, lowered further by fire burn-down and raised by repairs (L561, L665).
    pub health: f64,
    pub level: DamageLevel,
    pub links: PartLinks,
    /// Power priority in effect, 1..=5 (0 = not a consumer).
    pub priority: u8,
    pub state: PartState,
}
impl PartRuntime {
    /// Output factor from damage: 1.0, 0.5 at Heavy, 0 when destroyed (L561).
    pub fn output_factor(&self) -> f64 { self.level.output_factor() }
    pub fn works(&self) -> bool { self.level != DamageLevel::Destroyed }
    /// Ammunition feed wiring (empty for parts that take no ammunition).
    pub fn feed(&self) -> &FeedLinks { &self.links.mech.feed }
}

/// Zero bearing of a mount or trainable launcher relative to the bow, radians: where the part's rotated +X
/// (muzzle) axis points about +Y, in quarter turns 0, pi/2, pi, 3pi/2 (one yaw turn carries +X to -Z, so it is
/// the yaw quarter turns for an untilted part). A part tilted so +X stands vertical falls back to its yaw. This
/// is the zero `sim_systems` lays guns in, so tilted mounts aim and fire in the same frame.
pub fn mount_zero_rad(rot: PartRot) -> f64 {
    use core::f64::consts::{FRAC_PI_2, PI};
    let m = content::kit::part_rot_matrix(rot);
    match (m[0][0], m[0][2]) {
        (1, 0) => 0.0,
        (0, -1) => FRAC_PI_2,
        (-1, 0) => PI,
        (0, 1) => 3.0 * FRAC_PI_2,
        _ => (rot.yaw % content::kit::YAW_COUNT) as f64 * FRAC_PI_2,
    }
}

/// Unit barrel direction in the vessel-local frame for mount-frame (training, elevation) about the mount's zero
/// bearing (see the module docs).
pub fn mount_dir_local(zero_rad: f64, train_rad: f64, elevation_rad: f64) -> glam::DVec3 {
    let (st, ct) = (libm::sin(train_rad), libm::cos(train_rad));
    let (se, ce) = (libm::sin(elevation_rad), libm::cos(elevation_rad));
    let m = glam::DVec3::new(ce * ct, se, -ce * st);
    let (s, c) = (libm::sin(zero_rad), libm::cos(zero_rad));
    glam::DVec3::new(m.x * c + m.z * s, m.y, -m.x * s + m.z * c)
}

/// What a mount is told to do this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MountOrder {
    /// Ordered training and elevation, mount frame, radians.
    pub train_rad: f64,
    pub elevation_rad: f64,
    /// Fire permission: the mount fires when on target, loaded and powered.
    pub fire: bool,
    /// Largest number of rounds to fire this tick per barrel group (salvo rules live in sim-systems).
    /// 0 means no cap.
    pub max_rounds: u8,
    /// Extra aim error in radians applied by the caller's officer or AI accuracy (sim-core adds only dispersion).
    /// `[training, elevation]`, added to the laid angles of every round fired.
    pub aim_error_rad: [f64; 2],
    /// Stabilised laying (DECISION, q5): the vessel rotation the angles were laid against. When set, the mount
    /// holds the ordered line in the world frame as the hull rolls and pitches between two orders, taking
    /// training and elevation afresh each tick. Orders are rewritten only once per graph tick, and at
    /// Beaufort 3 the roll in that time threw 76 mm rounds 400 m long and short at 8 km.
    #[serde(default)]
    pub laid_rot: Option<glam::DQuat>,
}

/// Initial guidance of a launched munition.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum LaunchAim { Point(glam::DVec3), Entity(crate::entity::EntityId), Bearing { heading_rad: f64 }, DepthM(f64) }

/// One order per part. `Launch`, `DropMine`, `FireDecoy`, `LaunchOffboard` and `RecoverOffboard` are one-shot:
/// `step_parts` clears them from the controls on the tick they act (see the module docs). The rest persist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum PartOrder {
    #[default] None,
    Mount(MountOrder),
    /// Launch one munition from a launcher, tube or rack.
    Launch(LaunchAim),
    /// Drop one mine now (the mine-line rule is evaluated by sim-systems, L796).
    DropMine,
    FireDecoy,
    StreamDecoy(bool),
    /// Radar, sonar or jammer radiating on or off; sensor training for cameras and directors.
    Emit { radiating: bool, train_rad: f64 },
    Pump { running: bool, target: Option<CompartmentId> },
    Door { closed: bool },
    LaunchOffboard,
    RecoverOffboard,
    Ballast { kg: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartControl { pub enabled: bool, pub priority: u8, pub order: PartOrder }
impl Default for PartControl { fn default() -> Self { Self { enabled: true, priority: 0, order: PartOrder::None } } }

/// Damage-control orders for one team this tick (planning lives in sim-systems' Damage Control officer).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum TeamOrder { #[default] Idle, FightFire(CompartmentId), Patch { compartment: CompartmentId }, JuryRig { conduit: u16 }, Repair(PartSlot) }

/// Everything sim-systems tells one vessel each tick. Plain data; persists until overwritten.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VesselControls {
    /// Ordered power fraction, -1.0 (full astern) ..= 1.0.
    pub throttle: f64,
    /// Ordered rudder or jet deflection, -1.0 (port) ..= 1.0.
    pub rudder: f64,
    /// Indexed by [`PartSlot`]; shorter than the part list means defaults for the rest.
    /// `priority` 1..=5 overrides the part's default power priority; 0 keeps the default.
    pub parts: Vec<PartControl>,
    /// One order per damage-control team.
    pub teams: Vec<TeamOrder>,
    /// Flood valves to hold open, ascending.
    pub open_valves: Vec<CompartmentId>,
    /// Magazines to flood from the fire main (L648), by part slot.
    pub flood_magazines: Vec<PartSlot>,
    /// Act 3, AI-only submarines: ordered keel depth in metres below the surface; `None` = surfaced. Only
    /// vessels spawned `submersible` obey it (the hydro unit trims to neutral buoyancy and holds depth).
    pub depth_m: Option<f64>,
}

/// Build the runtime part list from a blueprint's placements in slot order, with loads (fuel, rounds) from the
/// placement or the save. Unit: `sim-core-parts`.
///
/// One entry per placement, so `parts[i].slot == PartSlot(i)`. A part the catalogue does not know, or whose
/// footprint misses the grid entirely, is kept as a destroyed inert entry. Placement loads: fuel fraction for
/// tanks, rounds fraction for magazines (ammunition mass follows the rounds), kilograms for ballast; mounts,
/// launchers, rails, decoys and bays start full. `compartment` is left `None` for the caller to link.
pub fn build_parts(bp: &content::blueprint::Blueprint, grid: &content::cell::CellGrid, catalogue: &content::catalogue::Catalogue) -> Vec<PartRuntime> { build::build_parts(bp, grid, catalogue) }

/// Recompute `health` and `level` of parts owning any of the changed cells (mean cell health, L561).
/// Returns slots whose level changed. Unit: `sim-core-parts`.
///
/// Health becomes the lower of its stored value and the cell mean, so burn-down and repairs are kept; the level
/// follows `crew::level_from_health` and never improves here.
pub fn refresh_health(parts: &mut [PartRuntime], grid: &crate::grid::VesselGrid, changed: &[CellIndex]) -> Vec<PartSlot> { build::refresh_health(parts, grid, changed) }

/// What the mechanical step asks the world to spawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpawnRequest {
    /// One round: muzzle position and velocity in the world frame, already including dispersion.
    Shell { slot: PartSlot, origin: glam::DVec3, velocity: glam::DVec3, rounds: u8 },
    Munition { slot: PartSlot, munition: MunitionIdx, origin: glam::DVec3, velocity: glam::DVec3, aim: LaunchAim },
    Decoy { slot: PartSlot, kind: content::part::DecoyKind, origin: glam::DVec3 },
}

/// One tick of part mechanics for a vessel, slots ascending: engines spool, mounts slew at their rates within
/// limits, hoists feed, reload timers run, antennas rotate (24 rpm), launchers and decoys act on their orders,
/// pumps and doors switch. Dispersion draws use `rng::Stream::GunDispersion` keyed (vessel, slot).
/// Appends spawn requests in ascending slot (projectile ids follow (vessel id, mount id), L557).
/// Unit: `sim-core-parts`.
///
/// Also applies control priorities to `PartRuntime::priority` (read by the next network solve), clears
/// one-shot orders it acted on, and stamps `last_fired_tick` when a gun fires or a munition launches.
pub fn step_parts(vessel: &mut crate::vessel::Vessel, ctx: &crate::world::StepContext<'_>, out: &mut Vec<SpawnRequest>) {
    use crate::vessel::VesselStatus;
    let active = matches!(vessel.status, VesselStatus::Active | VesselStatus::Disabled);
    let io = step::PartsIo {
        parts: &mut vessel.parts,
        controls: &mut vessel.controls,
        kin: vessel.kin,
        compartments: &vessel.compartments,
        active,
        last_fired_tick: &mut vessel.last_fired_tick,
    };
    let keys = step::Keys { seed: ctx.seed, tick: ctx.tick.0, vessel: vessel.id.to_u64() };
    step::step(io, keys, ctx.catalogue, ctx.dt, out);
}

/// Return a recovered offboard body (drone, helicopter, RHIB) to a bay. The launching slot is tried first, then
/// every other working bay that takes that body, ascending. Returns the slot that took it, or `None` when no bay
/// can (the body is then lost). A recovered drone needs its bay's `recharge_s` before it launches again.
pub(crate) fn recover_offboard(parts: &mut [PartRuntime], slot: PartSlot, munition: MunitionIdx, catalogue: &content::catalogue::Catalogue) -> Option<PartSlot> { step::recover_offboard(parts, slot, munition, catalogue) }
