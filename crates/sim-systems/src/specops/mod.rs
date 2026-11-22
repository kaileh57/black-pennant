//! Unit `sys-specops`. Special-operations station actions: each is a part plus a timed station action the
//! player starts and the enemy can interrupt. DESIGN.md L786-797; numbers in `SpecialOpsTuning`.
//!
//! | Action | Part | Rule |
//! | Board and capture | boarding kit (RHIB davit + boarding-team berths) | target disabled (no propulsion or colours struck), within 200 m, own speed under 5 kn; 90 s; aborts if either ship takes fire or the target regains propulsion. The prize sails home under its own Helm officer or is stripped on the spot for Salvage and blueprints |
//! | Insert or extract | RHIB davit | hold under 3 kn within 100 m of the shore marker for 60 s; patrols that detect the RHIB raise Alert and can sink it |
//! | Carry passengers | passenger berths | passengers in a flooded or burning berth compartment are lost; losses over the briefing's allowance fail the operation |
//! | Recover survivors | RHIB davit or recovery net | same hold rule at the survivor marker; captured crew pay Intel |
//! | Lay mines | mine rails | draw a line on the plot table; one mine per 50 m while within 5 degrees of the line's course at 6 kn or more; influence radius 10 m; the field persists on the theatre lane and cuts it |
//! | Photograph | camera mast | trainable mount, 10 degree field of view, 6 km range; hold the target in view, in range and in visual line of sight (radar-horizon formula with the target's highest point) for 20 s; range never exceeds weather visibility V |
//!
//! Rules this unit settles where the spec is silent (design decisions, listed so callers can find them):
//! - The mode check (`SortieConfig::actions`) is the caller's: `start` has no config. The Sim refuses a
//!   `StartAction` the sortie does not offer with `NotAllowedInMode` before calling `start`.
//! - `CarryPassengers` is passive (it has no timed hold); `start` refuses it. Losses are counted by
//!   `crate::objectives` from `PhysEvent::PassengersLost`.
//! - A started action whose conditions are not met yet is `Paused` with its reason, so the player can start it
//!   on approach. Hold time already accumulated is kept while paused. Boarding aborts on fire or on the target
//!   regaining propulsion only once progress has begun; before that those conditions pause it.
//! - "Takes fire": weapon damage (shell, missile, torpedo, mine, depth charge), a shell or body impact on
//!   either ship, or a splash from a shooter not on the actor's side within [`UNDER_FIRE_SPLASH_M`] of either
//!   ship, in the previous tick's physical events (`step` runs before physics, so `events` are last tick's).
//! - Player hulls can never be boarded (they never strike, L537).
//! - Mine line: the ship must be within [`MINE_LINE_TOLERANCE_M`] of the line, between its ends, within the
//!   tuned course tolerance and at or above the tuned speed. Drop points are at 0, 50, 100 ... m along the
//!   line; a drop point passed while off the line is skipped and the field gets a gap.
//! - Photograph field of view uses a modelled camera boresight that slews at [`CAMERA_SLEW_DPS`] relative to
//!   the hull, so a turning ship waits for the camera to come back on. Range and field of view come from the
//!   camera part (its gate fallback is 8 km and 15 degrees, L891) and never exceed weather visibility V.
//! - Survivors: an authored survivor marker holds [`SURVIVORS_PER_MARKER`] survivors; every lost hull with
//!   surviving berths leaves its crew as a survivor site where it went down. Crew of a hull from another side
//!   than the rescuer's count as captured crew (Intel).
//! - A prize has reached the exit when it comes within [`EXIT_RADIUS_M`] of an exit marker.
use crate::prelude::*;
use crate::protocol::command::{ActionTarget, PrizeDecision};
use crate::protocol::event::RejectReason;
use crate::protocol::snapshot::{ActionBlock, ActionState, ActionView};
use crate::ship::ShipTable;
use crate::tracks::Pictures;
use content::campaign::{MarkerKind, StationAction};
use content::part::{PartDef, PartFunction, PartTag};
use sim_core::damage::DamageCause;
use sim_core::events::PhysEvent;
use sim_core::parts::{PartRuntime, PartState};
use sim_core::time::{DT, KN};
use sim_core::vessel::{Vessel, VesselStatus};

/// Photograph defaults when the camera part carries no usable values (L796).
pub const PHOTO_FOV_DEG: f64 = 10.0;
pub const PHOTO_RANGE_M: f64 = 6_000.0;
/// DECISION: camera mast training rate relative to the hull, degrees per second.
pub const CAMERA_SLEW_DPS: f64 = 30.0;
/// DECISION: a hostile shell splash this close to either ship counts as taking fire.
pub const UNDER_FIRE_SPLASH_M: f64 = 50.0;
/// DECISION: largest cross-track distance from the drawn mine line that still lays mines.
pub const MINE_LINE_TOLERANCE_M: f64 = 50.0;
/// DECISION: shortest mine line the plot table accepts, metres.
pub const MIN_MINE_LINE_M: f64 = 10.0;
/// DECISION: survivors held at one authored survivor marker.
pub const SURVIVORS_PER_MARKER: u16 = 10;
/// DECISION: a hull within this distance of an exit marker has left by it (prizes, escorts, extraction).
pub const EXIT_RADIUS_M: f64 = 500.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: ActionId,
    pub action: StationAction,
    pub actor: EntityId,
    pub player: Option<PlayerSlot>,
    pub target: ActionTarget,
    /// Truth entity behind a track target, resolved at start (the hold tests need its real state).
    pub target_entity: Option<EntityId>,
    pub state: ActionState,
    pub block: Option<ActionBlock>,
    /// Seconds of hold accumulated, or metres of mine line laid.
    pub progress: f64,
    pub needed: f64,
    /// RHIB body in the water for insert, extract, boarding and recovery.
    pub rhib: Option<EntityId>,
    pub mines_laid: u16,
    pub started_tick: u64,
    /// Part doing the work (davit, boarding kit, recovery net, mine rail, camera mast).
    #[serde(default)]
    pub(crate) part: Option<PartSlot>,
    /// Resolved site in world (x, z): marker, survivor site, the boarding target, or the mine line's start.
    #[serde(default)]
    pub(crate) site: Option<DVec2>,
    /// Marker name for marker targets.
    #[serde(default)]
    pub(crate) site_name: String,
    #[serde(default)]
    pub(crate) rhib_requested: bool,
    /// Sides already given evidence of this action's RHIB (bit per side).
    #[serde(default)]
    pub(crate) evidence_sides: u8,
    /// Modelled camera boresight relative to the bow, radians (Photograph).
    #[serde(default)]
    pub(crate) camera_rel_rad: Option<f64>,
    /// Index of the next mine drop point along the line.
    #[serde(default)]
    pub(crate) next_drop: u16,
    /// Last reported (state, block, percent), so `Changed` goes out only on change.
    #[serde(default)]
    pub(crate) reported: Option<(ActionState, Option<ActionBlock>, u8)>,
}

/// A captured hull awaiting or carrying out the player's decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prize { pub vessel: EntityId, pub captor: PlayerSlot, pub decision: Option<PrizeDecision>, pub reached_exit: bool, pub crew_captured: u16,
    /// `Strip` has been requested (the prize is being scuttled).
    #[serde(default)]
    pub(crate) stripped: bool }

/// A finished station action, kept for objectives and the result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompletedAction { pub action: StationAction, pub actor: EntityId, pub player: Option<PlayerSlot>, pub site: Option<DVec2>, pub site_name: String, pub target: Option<EntityId>, pub tick: u64 }

/// Crew of a lost hull waiting in the water.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LossSite { pub vessel: EntityId, pub side: SideId, pub pos: DVec2, pub crew: u16 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpecOpsState { pub actions: Vec<Action>, pub prizes: Vec<Prize>, pub next_id: u32, pub photos: Vec<(EntityId, EntityId)>, pub survivors_recovered: u16, pub passengers_delivered: u16, pub mines_laid: u16,
    #[serde(default)]
    pub(crate) completed: Vec<CompletedAction>,
    /// Captured crew from recovered survivors (prize crews are on each [`Prize`]).
    #[serde(default)]
    pub(crate) crew_captured: u16,
    /// Survivor markers already emptied.
    #[serde(default)]
    pub(crate) recovered_markers: Vec<String>,
    #[serde(default)]
    pub(crate) loss_sites: Vec<LossSite>,
    /// Vessels that ever ran a mine line (their `MineLaid` events count).
    #[serde(default)]
    pub(crate) layers: Vec<EntityId>,
}

/// What the step asks of the Sim.
#[derive(Clone, Debug, PartialEq)]
pub enum SpecOpsOut {
    Changed(ActionView),
    /// Launch or recover the RHIB from a davit.
    Rhib { vessel: EntityId, slot: PartSlot, launch: bool, toward: DVec2 },
    DropMine { vessel: EntityId, slot: PartSlot },
    /// Boarding succeeded: change the target's side and give it a `Proxy` captain.
    Capture { prize: EntityId, to_side: SideId, by: PlayerSlot },
    /// Photograph completed: raise the track to Exact.
    Photo { by: EntityId, side: SideId, track: TrackId, target: EntityId },
    Recovered { by: EntityId, count: u16, captured_crew: u16 },
    /// Strip on the spot: scuttle the prize and record the payout.
    Strip { prize: EntityId },
    /// Train a camera mast toward the photograph target (10 degree field of view, L796).
    TrainCamera { vessel: EntityId, slot: PartSlot, toward: DVec3 },
    /// Evidence for the enemy commander: a detected RHIB (L749).
    Evidence { side: SideId },
}

// ---------------------------------------------------------------------------------------------------------
// Part and world helpers
// ---------------------------------------------------------------------------------------------------------

fn part_def<'a>(catalogue: &'a Catalogue, part: &PartRuntime) -> Option<&'a PartDef> { catalogue.parts.get_index(part.def.index()).map(|(_, d)| d) }

fn has_tag(def: &PartDef, tag: PartTag) -> bool { def.tags.contains(&tag) }

/// True when a part type can perform an action.
pub(crate) fn serves(action: StationAction, def: &PartDef) -> bool {
    match action {
        StationAction::BoardAndCapture => matches!(def.function, PartFunction::BoardingKit { .. }) || has_tag(def, PartTag::BoardingKit),
        StationAction::InsertOrExtract => matches!(def.function, PartFunction::RhibDavit | PartFunction::BoardingKit { .. }) || has_tag(def, PartTag::RhibDavit) || has_tag(def, PartTag::BoardingKit),
        StationAction::RecoverSurvivors => matches!(def.function, PartFunction::RhibDavit | PartFunction::BoardingKit { .. } | PartFunction::RecoveryNet) || has_tag(def, PartTag::RhibDavit) || has_tag(def, PartTag::BoardingKit) || has_tag(def, PartTag::RecoveryNet),
        StationAction::LayMines => matches!(def.function, PartFunction::MineRail { .. }) || has_tag(def, PartTag::MineRail),
        StationAction::Photograph => matches!(def.function, PartFunction::CameraMast { .. }) || has_tag(def, PartTag::CameraMast),
        StationAction::CarryPassengers => false,
    }
}

/// The part works by launching a RHIB (davits and boarding kits; a recovery net does not).
pub(crate) fn uses_rhib(def: &PartDef) -> bool {
    match def.function {
        PartFunction::RhibDavit | PartFunction::BoardingKit { .. } => true,
        PartFunction::RecoveryNet => false,
        _ => has_tag(def, PartTag::RhibDavit) || has_tag(def, PartTag::BoardingKit),
    }
}

fn rail_mines(part: &PartRuntime) -> u16 { match part.state { PartState::MineRail { mines, .. } => mines, _ => 0 } }

/// Best working part for an action: a recovery net before a davit (no RHIB exposed), a mine rail that still
/// holds mines, else the lowest slot.
fn find_part(vessel: &Vessel, catalogue: &Catalogue, action: StationAction) -> Option<PartSlot> {
    let mut best: Option<(u8, PartSlot)> = None;
    for p in &vessel.parts {
        if !p.works() { continue; }
        let Some(def) = part_def(catalogue, p) else { continue };
        if !serves(action, def) { continue; }
        let rank = match action {
            StationAction::RecoverSurvivors => u8::from(uses_rhib(def)),
            StationAction::LayMines => u8::from(rail_mines(p) == 0),
            _ => 0,
        };
        if best.map_or(true, |(r, _)| rank < r) { best = Some((rank, p.slot)); }
    }
    best.map(|(_, s)| s)
}

fn part_at(vessel: &Vessel, slot: PartSlot) -> Option<&PartRuntime> { vessel.parts.get(slot.index()).filter(|p| p.slot == slot).or_else(|| vessel.parts.iter().find(|p| p.slot == slot)) }

fn pos2(v: &Vessel) -> DVec2 { DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z) }
fn vel2(v: &Vessel) -> DVec2 { DVec2::new(v.kin.lin_vel.x, v.kin.lin_vel.z) }
fn speed_kn(v: &Vessel) -> f64 { vel2(v).length() / KN }
fn finite2(p: DVec2) -> bool { p.x.is_finite() && p.y.is_finite() }

fn is_gone(status: VesselStatus) -> bool { matches!(status, VesselStatus::Lost(_) | VesselStatus::Wreck) }

fn marker_ok_for_hold(kind: MarkerKind) -> bool { !matches!(kind, MarkerKind::PlayerSpawn | MarkerKind::EnemySpawn | MarkerKind::ConvoyEntry | MarkerKind::ConvoyExit | MarkerKind::Exit | MarkerKind::Survivors) }

/// Wrap an angle into [-pi, pi).
pub(crate) fn wrap_pi(a: f64) -> f64 { let tau = core::f64::consts::TAU; a - tau * ((a + core::f64::consts::PI) / tau).floor() }

/// Live track's truth entity in a side's picture.
fn resolve_track(pictures: &Pictures, side: SideId, track: TrackId) -> Option<EntityId> {
    let t = pictures.side(side)?.get(track)?;
    if !t.is_live() || t.truth.is_none() { None } else { Some(t.truth) }
}

// ---------------------------------------------------------------------------------------------------------
// Start, cancel, prizes
// ---------------------------------------------------------------------------------------------------------

/// Start an action for a ship if its part exists and works and the mode offers it. Unit: `sys-specops`.
///
/// The mode check against `SortieConfig::actions` is the caller's (this function has no config). Refusals:
/// `HullLost` (actor gone), `NoSuchPart`, `NoSuchTrack`, `InvalidValue` (wrong target kind, bad line,
/// `CarryPassengers`), `ConditionsNotMet` (no site left, a player hull or own-side target, no player to take
/// a prize), `Duplicate` (same action, or a second RHIB action, already running on this ship).
#[allow(clippy::too_many_arguments)]
pub fn start(state: &mut SpecOpsState, actor: EntityId, player: Option<PlayerSlot>, action: StationAction, target: ActionTarget, world: &World, ships: &ShipTable, pictures: &Pictures, tick: u64) -> Result<ActionId, RejectReason> {
    if action == StationAction::CarryPassengers { return Err(RejectReason::InvalidValue); }
    let vessel = world.vessel(actor).ok_or(RejectReason::HullLost)?;
    if is_gone(vessel.status) { return Err(RejectReason::HullLost); }
    let catalogue = world.catalogue();
    let slot = find_part(vessel, catalogue, action).ok_or(RejectReason::NoSuchPart)?;
    let rhib_part = part_at(vessel, slot).and_then(|p| part_def(catalogue, p)).map_or(false, uses_rhib);
    for a in state.actions.iter().filter(|a| a.actor == actor && matches!(a.state, ActionState::Running | ActionState::Paused)) {
        if a.action == action { return Err(RejectReason::Duplicate); }
        let other_rhib = a.part.and_then(|s| part_at(vessel, s)).and_then(|p| part_def(catalogue, p)).map_or(false, uses_rhib);
        if rhib_part && other_rhib { return Err(RejectReason::Duplicate); }
    }
    let side = vessel.side;
    let here = pos2(vessel);
    let tuning = &world.tuning().special_ops;
    let player = player.or(vessel.player).or_else(|| ships.get(actor).and_then(|s| s.player));
    let mut a = Action {
        id: ActionId(0), action, actor, player, target: target.clone(), target_entity: None,
        state: ActionState::Paused, block: None, progress: 0.0, needed: 1.0, rhib: None, mines_laid: 0, started_tick: tick,
        part: Some(slot), site: None, site_name: String::new(), rhib_requested: false, evidence_sides: 0, camera_rel_rad: None, next_drop: 0, reported: None,
    };
    match action {
        StationAction::BoardAndCapture => {
            let ActionTarget::Track(t) = target else { return Err(RejectReason::InvalidValue) };
            let e = resolve_track(pictures, side, t).ok_or(RejectReason::NoSuchTrack)?;
            let tv = world.vessel(e).filter(|v| !is_gone(v.status)).ok_or(RejectReason::NoSuchTrack)?;
            if tv.side == side || tv.player.is_some() || state.prizes.iter().any(|p| p.vessel == e) || a.player.is_none() { return Err(RejectReason::ConditionsNotMet); }
            a.target_entity = Some(e);
            a.site = Some(pos2(tv));
            a.needed = tuning.board_s;
        }
        StationAction::InsertOrExtract => {
            let terrain = world.terrain();
            let marker = match &target {
                ActionTarget::Marker(name) => terrain.marker(name).filter(|m| marker_ok_for_hold(m.kind)).ok_or(RejectReason::InvalidValue)?,
                ActionTarget::None => terrain.markers_of(MarkerKind::ShoreLanding).filter(|m| finite2(DVec2::from(m.pos_m)))
                    .min_by(|x, y| here.distance_squared(DVec2::from(x.pos_m)).total_cmp(&here.distance_squared(DVec2::from(y.pos_m))))
                    .ok_or(RejectReason::ConditionsNotMet)?,
                _ => return Err(RejectReason::InvalidValue),
            };
            let p = DVec2::from(marker.pos_m);
            if !finite2(p) { return Err(RejectReason::InvalidValue); }
            a.site = Some(p);
            a.site_name = marker.name.clone();
            a.needed = tuning.hold_s;
        }
        StationAction::RecoverSurvivors => {
            let site = resolve_survivor_site(state, world, pictures, side, here, &target)?;
            a.site = Some(site.pos);
            a.site_name = site.name;
            a.target_entity = site.vessel;
            a.needed = tuning.hold_s;
        }
        StationAction::LayMines => {
            let ActionTarget::Line { a: la, b: lb } = target else { return Err(RejectReason::InvalidValue) };
            if !finite2(la) || !finite2(lb) { return Err(RejectReason::InvalidValue); }
            let len = la.distance(lb);
            if !(MIN_MINE_LINE_M..=content::campaign::MAX_MAP_M * 1.5).contains(&len) { return Err(RejectReason::InvalidValue); }
            a.site = Some(la);
            a.needed = len;
            if !state.layers.contains(&actor) { state.layers.push(actor); }
        }
        StationAction::Photograph => {
            let ActionTarget::Track(t) = target else { return Err(RejectReason::InvalidValue) };
            let e = resolve_track(pictures, side, t).ok_or(RejectReason::NoSuchTrack)?;
            let tv = world.vessel(e).filter(|v| !is_gone(v.status)).ok_or(RejectReason::NoSuchTrack)?;
            if tv.side == side { return Err(RejectReason::ConditionsNotMet); }
            a.target_entity = Some(e);
            a.needed = tuning.photo_hold_s;
        }
        StationAction::CarryPassengers => return Err(RejectReason::InvalidValue),
    }
    if !(a.needed.is_finite() && a.needed > 0.0) { a.needed = DT; }
    if state.next_id == 0 { state.next_id = 1; }
    let id = ActionId(state.next_id);
    state.next_id = state.next_id.saturating_add(1);
    a.id = id;
    state.actions.push(a);
    Ok(id)
}

struct SurvivorSite { pos: DVec2, name: String, vessel: Option<EntityId> }

fn resolve_survivor_site(state: &SpecOpsState, world: &World, pictures: &Pictures, side: SideId, here: DVec2, target: &ActionTarget) -> Result<SurvivorSite, RejectReason> {
    let terrain = world.terrain();
    match target {
        ActionTarget::Marker(name) => {
            let m = terrain.marker(name).filter(|m| m.kind == MarkerKind::Survivors).ok_or(RejectReason::InvalidValue)?;
            if state.recovered_markers.contains(&m.name) { return Err(RejectReason::ConditionsNotMet); }
            let p = DVec2::from(m.pos_m);
            if !finite2(p) { return Err(RejectReason::InvalidValue); }
            Ok(SurvivorSite { pos: p, name: m.name.clone(), vessel: None })
        }
        ActionTarget::Track(t) => {
            // A dropped track still names the wreck it last held (the plot keeps a last-known mark).
            let e = pictures.side(side).and_then(|p| p.get(*t)).map(|t| t.truth).ok_or(RejectReason::NoSuchTrack)?;
            let s = state.loss_sites.iter().find(|s| s.vessel == e).ok_or(RejectReason::NoSuchTrack)?;
            Ok(SurvivorSite { pos: s.pos, name: String::new(), vessel: Some(s.vessel) })
        }
        ActionTarget::None => {
            let mut best: Option<(f64, SurvivorSite)> = None;
            for m in terrain.markers_of(MarkerKind::Survivors) {
                let p = DVec2::from(m.pos_m);
                if !finite2(p) || state.recovered_markers.contains(&m.name) { continue; }
                let d = here.distance_squared(p);
                if best.as_ref().map_or(true, |(bd, _)| d < *bd) { best = Some((d, SurvivorSite { pos: p, name: m.name.clone(), vessel: None })); }
            }
            for s in &state.loss_sites {
                let d = here.distance_squared(s.pos);
                if best.as_ref().map_or(true, |(bd, _)| d < *bd) { best = Some((d, SurvivorSite { pos: s.pos, name: String::new(), vessel: Some(s.vessel) })); }
            }
            best.map(|(_, s)| s).ok_or(RejectReason::ConditionsNotMet)
        }
        ActionTarget::Line { .. } => Err(RejectReason::InvalidValue),
    }
}

/// Cancel a running or paused action. The next `step` reports it and recalls its RHIB. Unit: `sys-specops`.
pub fn cancel(state: &mut SpecOpsState, id: ActionId) -> bool {
    match state.actions.iter_mut().find(|a| a.id == id && matches!(a.state, ActionState::Running | ActionState::Paused)) {
        Some(a) => { a.state = ActionState::Aborted; a.block = Some(ActionBlock::Cancelled); true }
        None => false,
    }
}

/// Record the player's decision for a prize. Unit: `sys-specops`.
///
/// A sail-home prize may still be stripped later; a stripped prize cannot change its mind. The strip itself is
/// requested by the next `step` (`SpecOpsOut::Strip`).
pub fn decide_prize(state: &mut SpecOpsState, prize: EntityId, decision: PrizeDecision) -> bool {
    match state.prizes.iter_mut().find(|p| p.vessel == prize) {
        Some(p) if !p.stripped => { p.decision = Some(decision); true }
        _ => false,
    }
}

/// Current view of an action for snapshots and `ActionChanged`. Track targets carry no position (the plot
/// draws the track itself, so no truth leaks).
pub fn view(a: &Action) -> ActionView {
    let progress = if a.needed > 0.0 { (a.progress / a.needed).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let at = match a.target { ActionTarget::Track(_) => None, _ => a.site };
    ActionView { id: a.id, action: a.action, actor: a.actor, state: a.state, progress, block: a.block, at }
}

// ---------------------------------------------------------------------------------------------------------
// Pure rule cores (tested on hand-built facts)
// ---------------------------------------------------------------------------------------------------------

/// What one tick does to an action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Step { Advance, Pause(ActionBlock), Abort(ActionBlock), Finish(Option<ActionBlock>) }

/// Hold rule: within `range_m` of the site and under `max_kn`.
pub(crate) fn hold_rule(dist_m: f64, speed_kn: f64, range_m: f64, max_kn: f64) -> Step {
    if !(dist_m <= range_m) { Step::Pause(ActionBlock::TooFar) } else if !(speed_kn < max_kn) { Step::Pause(ActionBlock::TooFast) } else { Step::Advance }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TargetCond { Disabled, UnderWay, Lost }

/// Boarding rule (L792).
pub(crate) fn board_rule(progress: f64, dist_m: f64, speed_kn: f64, under_fire: bool, target: TargetCond, t: &content::tuning::SpecialOpsTuning) -> Step {
    let started = progress > 0.0;
    match target {
        TargetCond::Lost => return Step::Abort(ActionBlock::TargetLost),
        TargetCond::UnderWay => return if started { Step::Abort(ActionBlock::TargetUnderWay) } else { Step::Pause(ActionBlock::TargetUnderWay) },
        TargetCond::Disabled => {}
    }
    if under_fire { return if started { Step::Abort(ActionBlock::UnderFire) } else { Step::Pause(ActionBlock::UnderFire) }; }
    hold_rule(dist_m, speed_kn, t.board_range_m, t.board_max_speed_kn)
}

/// Mine-line facts of one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MineFacts { pub pos: DVec2, pub vel: DVec2, pub rail_mines: u16 }
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MineTick { pub step: Step, pub drop: bool, pub along_m: f64, pub next_drop: u16 }

/// Mine-line rule (L795). Drop point k sits at `k x spacing` metres along the line; `next_drop` is the next
/// unused point and `laid` the mines dropped so far.
pub(crate) fn mine_rule(a: DVec2, b: DVec2, f: MineFacts, next_drop: u16, laid: u16, t: &content::tuning::SpecialOpsTuning) -> MineTick {
    let d = b - a;
    let len = d.length();
    let points = mines_for_line(len, t);
    let along = if len > 0.0 { (f.pos - a).dot(d) / len } else { 0.0 };
    let hold = |step| MineTick { step, drop: false, along_m: along, next_drop };
    if points == 0 || next_drop >= points { return hold(Step::Finish(None)); }
    if f.rail_mines == 0 { return hold(if laid > 0 { Step::Finish(Some(ActionBlock::NoMinesLeft)) } else { Step::Abort(ActionBlock::NoMinesLeft) }); }
    let spacing = t.mine_spacing_m;
    if along > len + 0.5 * spacing { return hold(Step::Finish(None)); }
    let speed = f.vel.length();
    if !(speed >= t.mine_min_speed_kn * KN) || speed <= 0.0 { return hold(Step::Pause(ActionBlock::TooSlow)); }
    let u = if len > 0.0 { d / len } else { DVec2::X };
    let cross = (f.pos - a).perp_dot(u).abs();
    let cos_tol = libm::cos(t.mine_course_tolerance_deg.to_radians());
    let on_course = f.vel.dot(u) / speed >= cos_tol;
    if !on_course || !(cross <= MINE_LINE_TOLERANCE_M) || along < 0.0 { return hold(Step::Pause(ActionBlock::OffLine)); }
    if along >= next_drop as f64 * spacing {
        let passed = ((along / spacing).floor() as u64).saturating_add(1).min(points as u64) as u16;
        return MineTick { step: Step::Advance, drop: true, along_m: along, next_drop: passed.max(next_drop + 1) };
    }
    hold(Step::Advance)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PhotoFacts { pub range_m: f64, pub horizon_m: f64, pub line_of_sight: bool, pub visibility_m: f64, pub photo_range_m: f64, pub fov_rad: f64, pub off_boresight_rad: f64 }

/// Photograph rule (L796): range capped by visibility V, visual line of sight by the radar-horizon formula
/// and terrain, then the field of view.
pub(crate) fn photo_rule(f: PhotoFacts) -> Step {
    let limit = f.photo_range_m.min(f.visibility_m);
    if !(f.range_m <= limit) { return Step::Pause(if f.visibility_m < f.photo_range_m && f.range_m > f.visibility_m { ActionBlock::BeyondVisibility } else { ActionBlock::TooFar }); }
    if !(f.range_m <= f.horizon_m) || !f.line_of_sight { return Step::Pause(ActionBlock::NoLineOfSight); }
    if !(f.off_boresight_rad.abs() <= 0.5 * f.fov_rad) { return Step::Pause(ActionBlock::OutsideFieldOfView); }
    Step::Advance
}

/// Slew a modelled boresight toward a desired relative bearing by at most `max_step` radians.
pub(crate) fn slew(current: f64, desired: f64, max_step: f64) -> f64 {
    let diff = wrap_pi(desired - current);
    if diff.abs() <= max_step { wrap_pi(desired) } else { wrap_pi(current + max_step * diff.signum()) }
}

fn weapon_cause(c: DamageCause) -> bool { matches!(c, DamageCause::Shell | DamageCause::Missile | DamageCause::Torpedo | DamageCause::Mine | DamageCause::DepthCharge) }

/// "Either ship takes fire" from one tick's events. `hostile_shooter` says whether a splash's shooter is not
/// on the actor's side; `positions` are the two ships' (x, z).
pub(crate) fn under_fire(events: &[PhysEvent], ships: [Option<EntityId>; 2], positions: [Option<DVec2>; 2], hostile_shooter: &dyn Fn(EntityId) -> bool) -> bool {
    let is_ours = |v: EntityId| ships.iter().any(|s| *s == Some(v));
    events.iter().any(|e| match e {
        PhysEvent::CellsDamaged { vessel, cause, .. } => is_ours(*vessel) && weapon_cause(*cause),
        PhysEvent::ShellImpact { vessel, .. } | PhysEvent::BodyImpact { vessel, .. } => is_ours(*vessel),
        PhysEvent::Splash { pos, shooter, .. } => !is_ours(*shooter) && hostile_shooter(*shooter) && positions.iter().flatten().any(|p| p.distance(DVec2::new(pos.x, pos.z)) <= UNDER_FIRE_SPLASH_M),
        _ => false,
    })
}

// ---------------------------------------------------------------------------------------------------------
// Step
// ---------------------------------------------------------------------------------------------------------

/// One tick of every running action, ascending id: test the rule's conditions, accumulate or pause progress
/// with the blocking reason, abort on interruption, complete. Passenger losses are counted from
/// `PhysEvent::PassengersLost` by `crate::objectives`. Unit: `sys-specops`.
///
/// `Changed` goes out when an action's state, block or whole-percent progress changes; finished actions are
/// reported once, have their RHIB recalled and leave the list (objectives read `completed`).
pub fn step(state: &mut SpecOpsState, world: &World, ships: &ShipTable, pictures: &Pictures, events: &[sim_core::events::PhysEvent], ctx: &SimCtx<'_>, out: &mut Vec<SpecOpsOut>) {
    let tick = ctx.tick.0;
    note_events(state, world, events);
    let mut actions = core::mem::take(&mut state.actions);
    actions.sort_by_key(|a| a.id);
    let mut keep = Vec::with_capacity(actions.len());
    for mut a in actions {
        if matches!(a.state, ActionState::Running | ActionState::Paused) {
            run_action(state, &mut a, world, ships, pictures, events, ctx, out);
        }
        let pct = (view(&a).progress * 100.0).floor().clamp(0.0, 100.0) as u8;
        let now = (a.state, a.block, pct);
        if a.reported != Some(now) { a.reported = Some(now); out.push(SpecOpsOut::Changed(view(&a))); }
        if matches!(a.state, ActionState::Done | ActionState::Aborted) {
            if a.rhib.is_some() || a.rhib_requested {
                if let (Some(slot), Some(v)) = (a.part, world.vessel(a.actor)) {
                    if !is_gone(v.status) { out.push(SpecOpsOut::Rhib { vessel: a.actor, slot, launch: false, toward: pos2(v) }); }
                }
            }
            if a.state == ActionState::Done {
                state.completed.push(CompletedAction { action: a.action, actor: a.actor, player: a.player, site: a.site, site_name: a.site_name.clone(), target: a.target_entity, tick });
            }
        } else {
            keep.push(a);
        }
    }
    state.actions = keep;
    step_prizes(state, world, out);
}

/// Bookkeeping from last tick's events: survivor sites of lost hulls, mines laid.
fn note_events(state: &mut SpecOpsState, world: &World, events: &[PhysEvent]) {
    for e in events {
        match e {
            PhysEvent::VesselLost { vessel, side, .. } => {
                if state.loss_sites.iter().any(|s| s.vessel == *vessel) { continue; }
                if let Some(v) = world.vessel(*vessel) {
                    if v.parent.is_some() { continue; }
                    let crew = v.crew.surviving_berths;
                    let p = pos2(v);
                    if crew > 0 && finite2(p) { state.loss_sites.push(LossSite { vessel: *vessel, side: *side, pos: p, crew }); }
                }
            }
            PhysEvent::MineLaid { layer, .. } => { if state.layers.contains(layer) { state.mines_laid = state.mines_laid.saturating_add(1); } }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_action(state: &mut SpecOpsState, a: &mut Action, world: &World, ships: &ShipTable, pictures: &Pictures, events: &[PhysEvent], ctx: &SimCtx<'_>, out: &mut Vec<SpecOpsOut>) {
    let tuning = &ctx.tuning.special_ops;
    let catalogue = ctx.catalogue;
    let Some(vessel) = world.vessel(a.actor).filter(|v| !is_gone(v.status)) else { apply(a, Step::Abort(ActionBlock::PartLost), false); return };
    let side = vessel.side;
    // RHIB bookkeeping: bind the body the davit launched, abort if it is sunk.
    for e in events {
        match e {
            PhysEvent::BodySpawned { body, launcher, slot, .. } if *launcher == a.actor && Some(*slot) == a.part && a.rhib.is_none() && a.rhib_requested => a.rhib = Some(*body),
            PhysEvent::BodyDestroyed { body, .. } if Some(*body) == a.rhib => { a.rhib = None; apply(a, Step::Abort(ActionBlock::PartLost), false); return; }
            PhysEvent::BodyRecovered { body, .. } if Some(*body) == a.rhib => a.rhib = None,
            _ => {}
        }
    }
    // The part; a mine line hands over to another rail that still holds mines.
    let mut part = a.part.and_then(|s| part_at(vessel, s)).filter(|p| p.works());
    if a.action == StationAction::LayMines && part.map_or(true, |p| rail_mines(p) == 0) {
        if let Some(p) = find_part(vessel, catalogue, StationAction::LayMines).and_then(|s| part_at(vessel, s)).filter(|p| rail_mines(p) > 0) {
            a.part = Some(p.slot);
            part = Some(p);
        }
    }
    let Some(part) = part else { apply(a, Step::Abort(ActionBlock::PartLost), false); return };
    let slot = part.slot;
    let here = pos2(vessel);
    let speed = speed_kn(vessel);
    let rhib = part_def(catalogue, part).map_or(false, uses_rhib);

    let mut timed = true;
    let step = match a.action {
        StationAction::BoardAndCapture => {
            let target = a.target_entity.and_then(|e| world.vessel(e));
            let cond = match target {
                None => TargetCond::Lost,
                Some(t) if is_gone(t.status) || t.side == side => TargetCond::Lost,
                Some(t) if t.status == VesselStatus::Disabled || t.struck => TargetCond::Disabled,
                Some(_) => TargetCond::UnderWay,
            };
            let tpos = target.map(pos2);
            if let Some(p) = tpos { a.site = Some(p); }
            let dist = tpos.map_or(f64::INFINITY, |p| here.distance(p));
            let hostile = |shooter: EntityId| world.vessel(shooter).map_or(true, |s| s.side != side);
            let fire = under_fire(events, [Some(a.actor), a.target_entity], [Some(here), tpos], &hostile);
            board_rule(a.progress, dist, speed, fire, cond, tuning)
        }
        StationAction::InsertOrExtract => {
            let dist = a.site.map_or(f64::INFINITY, |p| here.distance(p));
            hold_rule(dist, speed, tuning.hold_range_m, tuning.hold_max_speed_kn)
        }
        StationAction::RecoverSurvivors => {
            let still_there = match a.target_entity {
                Some(v) => state.loss_sites.iter().any(|s| s.vessel == v),
                None => !state.recovered_markers.contains(&a.site_name),
            };
            if !still_there { Step::Abort(ActionBlock::TargetLost) } else {
                let dist = a.site.map_or(f64::INFINITY, |p| here.distance(p));
                hold_rule(dist, speed, tuning.hold_range_m, tuning.hold_max_speed_kn)
            }
        }
        StationAction::LayMines => {
            timed = false;
            match a.target {
                ActionTarget::Line { a: la, b: lb } => {
                    let m = mine_rule(la, lb, MineFacts { pos: here, vel: vel2(vessel), rail_mines: rail_mines(part) }, a.next_drop, a.mines_laid, tuning);
                    if m.drop {
                        out.push(SpecOpsOut::DropMine { vessel: a.actor, slot });
                        a.mines_laid = a.mines_laid.saturating_add(1);
                    }
                    a.next_drop = m.next_drop;
                    a.progress = a.progress.max(m.along_m.clamp(0.0, a.needed));
                    m.step
                }
                _ => Step::Abort(ActionBlock::Cancelled),
            }
        }
        StationAction::Photograph => photo_step(a, vessel, part, world, ctx, out),
        StationAction::CarryPassengers => Step::Abort(ActionBlock::Cancelled),
    };
    apply(a, step, timed);
    // RHIB: launch once the ship is on station; any other side that tracks it gets evidence once.
    if rhib && !a.rhib_requested && a.state == ActionState::Running {
        if let Some(toward) = a.site { out.push(SpecOpsOut::Rhib { vessel: a.actor, slot, launch: true, toward }); a.rhib_requested = true; }
    }
    if let Some(r) = a.rhib {
        for p in &pictures.sides {
            let bit = 1u8 << (p.side.0 & 7);
            if p.side == side || a.evidence_sides & bit != 0 { continue; }
            if p.track_of(r).map_or(false, |t| t.stance != Stance::Friend) {
                a.evidence_sides |= bit;
                out.push(SpecOpsOut::Evidence { side: p.side });
            }
        }
    }
    if timed && a.state == ActionState::Running && a.progress >= a.needed { complete_action(state, a, vessel, ships, world, pictures, out); }
}

/// Apply a tick's verdict. Timed actions accumulate one tick of hold on `Advance`.
fn apply(a: &mut Action, s: Step, timed: bool) {
    match s {
        Step::Advance => { a.state = ActionState::Running; a.block = None; if timed { a.progress = (a.progress + DT).min(a.needed); } }
        Step::Pause(b) => { a.state = ActionState::Paused; a.block = Some(b); }
        Step::Abort(b) => { a.state = ActionState::Aborted; a.block = Some(b); }
        Step::Finish(b) => { a.state = ActionState::Done; a.block = b; if b.is_none() { a.progress = a.needed; } }
    }
}

fn photo_step(a: &mut Action, vessel: &Vessel, part: &PartRuntime, world: &World, ctx: &SimCtx<'_>, out: &mut Vec<SpecOpsOut>) -> Step {
    let Some(target) = a.target_entity.and_then(|e| world.vessel(e)).filter(|t| !is_gone(t.status)) else { return Step::Abort(ActionBlock::TargetLost) };
    let (fov_deg, range_m) = match part_def(ctx.catalogue, part).map(|d| &d.function) {
        Some(PartFunction::CameraMast { fov_deg, photo_range_m }) => (*fov_deg, *photo_range_m),
        _ => (PHOTO_FOV_DEG, PHOTO_RANGE_M),
    };
    let fov_deg = if fov_deg.is_finite() && fov_deg > 0.0 { fov_deg.min(180.0) } else { PHOTO_FOV_DEG };
    let range_m = if range_m.is_finite() && range_m > 0.0 { range_m } else { PHOTO_RANGE_M };
    let cam = vessel.to_world(part.centre);
    let top = target.top_height_m().max(0.0);
    let aim = DVec3::new(target.kin.pose.pos.x, top, target.kin.pose.pos.z);
    out.push(SpecOpsOut::TrainCamera { vessel: a.actor, slot: part.slot, toward: aim });
    let local = vessel.kin.pose.to_local(aim);
    let desired = libm::atan2(local.z, local.x);
    let cur = slew(a.camera_rel_rad.unwrap_or(desired), desired, CAMERA_SLEW_DPS.to_radians() * DT);
    a.camera_rel_rad = Some(cur);
    let range = DVec2::new(cam.x, cam.z).distance(DVec2::new(aim.x, aim.z));
    let horizon = crate::sensors::radar::horizon_m(cam.y.max(0.0), top);
    let facts = PhotoFacts { range_m: range, horizon_m: horizon, line_of_sight: true, visibility_m: ctx.weather.visibility_m, photo_range_m: range_m, fov_rad: fov_deg.to_radians(), off_boresight_rad: wrap_pi(desired - cur) };
    // The terrain and smoke ray is the costly test: only once everything else passes.
    match photo_rule(facts) {
        Step::Advance if !world.line_of_sight(cam, aim, true, [a.actor, target.id]) => Step::Pause(ActionBlock::NoLineOfSight),
        s => s,
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_action(state: &mut SpecOpsState, a: &mut Action, vessel: &Vessel, ships: &ShipTable, world: &World, pictures: &Pictures, out: &mut Vec<SpecOpsOut>) {
    a.state = ActionState::Done;
    a.block = None;
    a.progress = a.needed;
    match a.action {
        StationAction::BoardAndCapture => {
            let Some(e) = a.target_entity else { return };
            let by = a.player.or(vessel.player).or_else(|| ships.get(a.actor).and_then(|s| s.player)).unwrap_or(PlayerSlot(0));
            let crew = world.vessel(e).map_or(0, |t| t.crew.surviving_berths);
            if !state.prizes.iter().any(|p| p.vessel == e) {
                state.prizes.push(Prize { vessel: e, captor: by, decision: None, reached_exit: false, crew_captured: crew, stripped: false });
            }
            out.push(SpecOpsOut::Capture { prize: e, to_side: vessel.side, by });
        }
        StationAction::RecoverSurvivors => {
            let (count, captured) = match a.target_entity {
                Some(v) => match state.loss_sites.iter().position(|s| s.vessel == v) {
                    Some(i) => { let s = state.loss_sites.remove(i); (s.crew, if s.side != vessel.side { s.crew } else { 0 }) }
                    None => (0, 0),
                },
                None => { state.recovered_markers.push(a.site_name.clone()); (SURVIVORS_PER_MARKER, 0) }
            };
            state.survivors_recovered = state.survivors_recovered.saturating_add(count);
            state.crew_captured = state.crew_captured.saturating_add(captured);
            out.push(SpecOpsOut::Recovered { by: a.actor, count, captured_crew: captured });
        }
        StationAction::Photograph => {
            let Some(e) = a.target_entity else { return };
            let started_on = match a.target { ActionTarget::Track(t) => t, _ => TrackId::NONE };
            // Prefer the live track now on the target (a merge may have renumbered it).
            let track = pictures.side(vessel.side).and_then(|p| p.track_of(e)).map_or(started_on, |t| t.id);
            if !state.photos.contains(&(a.actor, e)) { state.photos.push((a.actor, e)); }
            out.push(SpecOpsOut::Photo { by: a.actor, side: vessel.side, track, target: e });
        }
        StationAction::InsertOrExtract | StationAction::LayMines | StationAction::CarryPassengers => {}
    }
}

/// Prize upkeep: request a strip once, and note sail-home prizes that reach an exit marker.
fn step_prizes(state: &mut SpecOpsState, world: &World, out: &mut Vec<SpecOpsOut>) {
    let exits: Vec<DVec2> = world.terrain().map.markers.iter().filter(|m| matches!(m.kind, MarkerKind::Exit | MarkerKind::ConvoyExit)).map(|m| DVec2::from(m.pos_m)).filter(|p| finite2(*p)).collect();
    for p in state.prizes.iter_mut() {
        if p.stripped { continue; }
        let Some(v) = world.vessel(p.vessel).filter(|v| !is_gone(v.status)) else { continue };
        match p.decision {
            Some(PrizeDecision::StripOnSpot) => { p.stripped = true; out.push(SpecOpsOut::Strip { prize: p.vessel }); }
            Some(PrizeDecision::SailHome) | None => {
                if !p.reached_exit && exits.iter().any(|e| e.distance(pos2(v)) <= EXIT_RADIUS_M) { p.reached_exit = true; }
            }
        }
    }
}

/// The mine-line rule alone, for the plot table preview: number of mines a line of this length takes at the
/// tuned spacing. Real code.
pub fn mines_for_line(length_m: f64, tuning: &content::tuning::SpecialOpsTuning) -> u16 { if tuning.mine_spacing_m <= 0.0 { 0 } else { (length_m / tuning.mine_spacing_m).floor() as u16 + 1 } }

#[cfg(test)]
mod tests;
