//! Unit `sys-engage` (hard: the default fire control every newcomer relies on, gated at L491).
//! Stage 4 of the pipeline at 10 Hz: the Gunnery officer's `ThreatRanking` and `MountAssignment` stages, the
//! inbound test, point defence and decoy control. DESIGN.md L407, L479-494, L310 (deliberate limits: one
//! target per mount, fixed salvo rules), L486 (inbound rule), L353 (every hold and shot carries a reason).
use crate::doctrine::{DoctrineTable, MountInfo, ResolvedEntry};
use crate::prelude::*;
use crate::tracks::Track;
use content::graph::{TargetCategory, WeaponClass};
use content::part::DecoyKind;
use sim_core::networks::LinkState;
use sim_core::parts::PartRuntime;

/// Own-ship facts the gunnery stages read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OwnState { pub vessel: EntityId, pub side: SideId, pub pose: Pose, pub vel: DVec3, pub ang_vel: DVec3, pub heel_rad: f64 }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RankedThreat {
    pub track: TrackId,
    pub category: TargetCategory,
    pub score: f64,
    pub range_m: f64,
    /// Seconds to closest approach for inbound tracks.
    pub time_to_cpa_s: Option<f64>,
    pub inbound: bool,
    pub designated: bool,
}

/// Closest point of approach of a track to a hull within the horizon (L486).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cpa { pub distance_m: f64, pub time_s: f64 }

/// Inbound test: predicted closest approach to any own-side hull at most 500 m within the next 60 s
/// (`DoctrineTuning::inbound_cpa_m`, `inbound_horizon_s`). Unit: `sys-engage`.
///
/// Pure geometry on the track estimate at `tick` (constant velocity, no truth): for each hull (position,
/// velocity) the time of closest approach is clamped to `[0, horizon]` and the 3D miss distance taken there.
/// Returns the most urgent qualifying approach (earliest time, then smallest distance), `None` for a dropped
/// track, non-finite data or no hull inside the CPA radius. Stance is the caller's filter.
pub fn inbound(track: &Track, own_side_hulls: &[(DVec3, DVec3)], tick: u64, tuning: &TuningTable) -> Option<Cpa> {
    let d = &tuning.doctrine;
    let cpa_m = finite_pos(d.inbound_cpa_m).unwrap_or(500.0);
    let horizon = finite_pos(d.inbound_horizon_s).unwrap_or(60.0);
    if !usable(track) { return None; }
    let p = track.predict(tick);
    let v = if track.vel.is_finite() { track.vel } else { DVec3::ZERO };
    if !p.is_finite() { return None; }
    let mut best: Option<Cpa> = None;
    for &(hp, hv) in own_side_hulls {
        if !hp.is_finite() { continue; }
        let hv = if hv.is_finite() { hv } else { DVec3::ZERO };
        let (r, w) = (p - hp, v - hv);
        let w2 = w.length_squared();
        let t = if w2 > 1e-12 { (-r.dot(w) / w2).clamp(0.0, horizon) } else { 0.0 };
        let dist = (r + w * t).length();
        if !(dist.is_finite() && dist <= cpa_m) { continue; }
        let c = Cpa { distance_m: dist, time_s: t };
        if best.is_none_or(|b| (c.time_s, c.distance_m) < (b.time_s, b.distance_m)) { best = Some(c); }
    }
    best
}

/// A live track with a finite position estimate (a non-finite estimate would otherwise predict to the origin).
fn usable(t: &Track) -> bool { t.is_live() && t.pos.is_finite() }
fn finite_pos(v: f64) -> Option<f64> { (v.is_finite() && v >= 0.0).then_some(v) }
fn finite_or(v: DVec3, d: DVec3) -> DVec3 { if v.is_finite() { v } else { d } }

/// Tracks the inbound test applies to: weapons, and air tracks not yet identified to Type (drones and
/// unknown fast movers). Surface and identified air tracks are never inbound.
fn inbound_eligible(t: &Track) -> bool { t.domain == Domain::Weapon || (t.domain == Domain::Air && t.rung < IdRung::Type) }

/// [`crate::doctrine::target_category`] with a freshly computed inbound flag (the track's own flag is last
/// tick's).
fn category_of(t: &Track, inbound: bool) -> TargetCategory {
    if t.domain == Domain::Weapon || inbound && t.domain == Domain::Air && t.rung < IdRung::Type { return TargetCategory::InboundWeapon; }
    match t.domain { Domain::Surface => TargetCategory::Surface, Domain::Subsurface => TargetCategory::Subsurface, Domain::Air => TargetCategory::Air, Domain::Weapon => TargetCategory::InboundWeapon }
}

/// Live track by id: tracks arrive ascending by id from a `SidePicture`; a linear search covers any other
/// order.
fn find_track(tracks: &[Track], id: TrackId) -> Option<&Track> {
    if id.is_none() { return None; }
    let t = match tracks.binary_search_by(|t| t.id.cmp(&id)) { Ok(i) => Some(&tracks[i]), Err(_) => tracks.iter().find(|t| t.id == id) };
    t.filter(|t| usable(t))
}

fn find_part(parts: &[PartRuntime], slot: PartSlot) -> Option<&PartRuntime> {
    parts.get(slot.index()).filter(|p| p.slot == slot).or_else(|| parts.iter().find(|p| p.slot == slot))
}

pub struct RankIn<'a> {
    pub own: OwnState,
    pub tracks: &'a [Track],
    pub designated: TrackId,
    /// L1 `gunnery.aggression` 0..=1 and other sliders by key.
    pub sliders: &'a BTreeMap<String, f64>,
    pub own_side_hulls: &'a [(DVec3, DVec3)],
    pub tick: u64,
}

/// The `ThreatRanking` stage: inbound weapons first by time to closest approach, then the designated track,
/// then Hostile by range and closing rate, then Suspect inside self-defence range. Neutral and Friend never
/// rank, nor does a contact out of the fight (`Track::out_of_action`) unless it is the designated track. Deterministic: ties break by ascending track id. Also sets `Track::inbound` through the returned list
/// (the caller writes it back). Unit: `sys-engage`.
///
/// Tiers, most urgent first: (0) inbound weapons by time to closest approach; (1) the designated track (any
/// Suspect or Hostile track); (2) Hostile tracks by predicted range `lookahead` seconds ahead, `range - closing
/// rate x lookahead` with `lookahead = 60 s x (1 - gunnery.aggression)` (an aggressive officer takes the nearest
/// target, a cautious one the fastest closer); (3) Suspect tracks inside self-defence range, same key. Weapon
/// tracks that are not inbound never rank (they threaten no own-side hull). `score` is
/// `(4 - tier) x 1e7 - key` so it falls monotonically down the list. Self-defence range: slider
/// `gunnery.self_defence_range_m` when set, else the doctrine table's.
pub fn threat_ranking(input: &RankIn<'_>, doctrine: &DoctrineTable, tuning: &TuningTable) -> Vec<RankedThreat> {
    let slider = |k: &str| input.sliders.get(k).copied().filter(|v| v.is_finite());
    let aggression = slider("gunnery.aggression").unwrap_or(0.5).clamp(0.0, 1.0);
    let sd = slider("gunnery.self_defence_range_m").filter(|v| *v > 0.0)
        .or_else(|| finite_pos(doctrine.self_defence_range_m).filter(|v| *v > 0.0))
        .or_else(|| finite_pos(tuning.doctrine.self_defence_range_m)).unwrap_or(4_000.0);
    let lookahead = 60.0 * (1.0 - aggression);
    let own_pos = finite_or(input.own.pose.pos, DVec3::ZERO);
    let own_vel = finite_or(input.own.vel, DVec3::ZERO);
    let mut rows: Vec<(u8, f64, RankedThreat)> = Vec::new();
    for t in input.tracks {
        if !usable(t) || t.stance < Stance::Suspect { continue; }
        // Out of the fight (struck, disabled or sinking): ranked only while the player designates it.
        if t.out_of_action && (input.designated.is_none() || t.id != input.designated) { continue; }
        let p = t.predict(input.tick);
        let rel = p - own_pos;
        let range = rel.length();
        if !range.is_finite() { continue; }
        let w = finite_or(t.vel, DVec3::ZERO) - own_vel;
        let closing = if range > 1e-6 { -rel.dot(w) / range } else { 0.0 };
        let closing = if closing.is_finite() { closing } else { 0.0 };
        let cpa = if inbound_eligible(t) { inbound(t, input.own_side_hulls, input.tick, tuning) } else { None };
        let is_inbound = cpa.is_some();
        let category = category_of(t, is_inbound);
        let designated = !input.designated.is_none() && t.id == input.designated;
        let r_pred = (range - closing * lookahead).max(0.0);
        let (tier, key) = if category == TargetCategory::InboundWeapon && is_inbound {
            (0u8, cpa.map(|c| c.time_s).unwrap_or(0.0))
        } else if designated {
            (1, range)
        } else if t.domain == Domain::Weapon {
            continue;
        } else if t.stance == Stance::Hostile {
            (2, r_pred)
        } else if range <= sd {
            (3, r_pred)
        } else {
            continue;
        };
        let key = key.clamp(0.0, 9.99e6);
        rows.push((tier, key, RankedThreat {
            track: t.id, category, score: (4 - tier) as f64 * 1e7 - key, range_m: range,
            time_to_cpa_s: cpa.map(|c| c.time_s), inbound: is_inbound, designated,
        }));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.track.cmp(&b.2.track)));
    rows.into_iter().map(|r| r.2).collect()
}

/// Per-mount engagement memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MountEngage {
    pub slot: PartSlot,
    pub status: EngageStatus,
    /// Rounds or missiles left in the current salvo.
    pub salvo_left: u8,
    /// Tick the last salvo at the current track left, and whether its outcome is known (SAM second shot).
    pub last_salvo_tick: u64,
    pub awaiting_result: bool,
    /// Track of the current salvo (guided) or engagement (guns).
    pub(crate) target: TrackId,
    /// Decoy launchers: first tick the next salvo may fire (decoy control's own reload clock, L486).
    pub(crate) ready_tick: u64,
    /// Decoy launchers: why the launcher cannot act, from its part state at the last assignment tick.
    pub(crate) decoy_block: Option<HoldReason>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngageState {
    /// One per mount in `ShipDesign::mounts` order.
    pub mounts: Vec<MountEngage>,
    pub ranked: Vec<RankedThreat>,
    /// Decoy salvos already spent per inbound track (1 salvo per missile, L486).
    pub decoyed: Vec<TrackId>,
    pub weapons_in_flight: u16,
    /// L1 `gunnery.open_fire_range` of the `MountAssignment` stage: the fraction (0.5..=1.0) of each
    /// non-point-defence entry's effective range at which mounts open fire. `AssignIn` carries no stage
    /// parameters, so the stage runner writes this before calling [`mount_assignment`]; `None` = 1.0.
    pub open_fire_range: Option<f64>,
    /// Guided salvos launched and not yet resolved, ascending launch order.
    pub(crate) salvos: Vec<Salvo>,
    /// Tracks of this side's picture that are shore sites (a site objective's hulls, `VesselRole::Site`). An
    /// anti-ship missile is never assigned to one: [`HoldReason::ShoreSite`]. `AssignIn` carries no such
    /// list, so, as with `open_fire_range`, whoever runs the stage writes it before [`mount_assignment`];
    /// empty = no track is a site.
    #[serde(default)]
    pub shore_sites: Vec<TrackId>,
}

/// One guided salvo in flight: the target is not offered to another salvo of the same weapon class until the
/// expected impact time passes or the track drops (SAM: "a second after a miss", L485).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Salvo {
    pub slot: PartSlot, pub weapon: WeaponClass, pub track: TrackId, pub count: u8, pub result_tick: u64,
    /// Size of the salvo this launch belongs to. A salvo its launcher could not finish (emptied, reloading,
    /// destroyed or retargeted) is completed by another launcher of the same class (0 = never reopened).
    #[serde(default)]
    pub want: u8,
}

/// What the open-fire test of officer-aimed guns reads (L489, see [`hit_chance`]): the mounts' range tables,
/// the catalogue (gun dispersion), the officers' accuracy multiplier, the sea state and the hit probability
/// a default entry needs. `None` in an `AssignIn` skips the test (unit fixtures without a design).
#[derive(Clone, Copy)]
pub struct GunneryFacts<'a> {
    pub tables: &'a BTreeMap<PartSlot, std::sync::Arc<Vec<crate::guidance::gun::RangeRow>>>,
    pub catalogue: &'a Catalogue,
    pub accuracy_mult: f64,
    pub sea: content::weather::SeaState,
}

pub struct AssignIn<'a> {
    pub own: OwnState,
    pub ranked: &'a [RankedThreat],
    pub tracks: &'a [Track],
    pub mounts: &'a [MountInfo],
    pub parts: &'a [PartRuntime],
    pub doctrine: &'a DoctrineTable,
    /// Mounts the player holds (status becomes `Holding { PlayerControl }` with an advisory solution).
    pub held: &'a [PartSlot],
    /// Fire permission per mount written by rules, pinned nodes or scripts; `None` = officer decides.
    pub permission: &'a [Option<bool>],
    pub tick: u64,
    /// The open-fire test's facts; `None` skips it.
    pub gunnery: Option<GunneryFacts<'a>>,
}

/// One mount's assignment this engagement tick.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Assignment { pub slot: PartSlot, pub status: EngageStatus, /// Guided weapons: launch this many now.
    pub launch: u8 }

/// The `MountAssignment` stage: give each mount at most one target (highest-ranked threat its doctrine,
/// arc, range and ammunition allow), deconflict arcs so two mounts do not mask each other, size salvos, and
/// record `Engaging` or `Holding: reason` for every mount. Emits a reason for every hold and every shot.
/// Unit: `sys-engage`.
///
/// Per mount, in `AssignIn::mounts` order (one [`Assignment`] each; `state.mounts` is realigned to it by slot):
/// 1. Gates, first that applies: player holds the mount (`PlayerControl`, with the officer's target as the
///    advisory), part destroyed or missing (`Damaged`), shed, unpowered, fire permission withheld, launcher
///    reloading or out of munitions, gun out of ammunition.
/// 2. Target: the highest-ranked threat (at most [`MAX_THREATS_PER_MOUNT`] examined) that passes the mount's
///    doctrine entry for its category ([`crate::doctrine::check`], with the open-fire fraction applied to
///    non-point-defence ranges and designation lifting Suspect to Hostile), then for guns the training and
///    line-of-sight elevation limits, masking by another gun mount and own-side units near the line of fire;
///    for anti-ship missiles a shore site (`EngageState::shore_sites`: held whatever the stance, a designation
///    or a written permission says, and the mount goes on to the next ranked threat) and a Neutral (or Friend)
///    track inside the seeker basket; for guided weapons a salvo
///    of the same class already in flight at, or assigned this tick to, that track. A salvo whose launcher
///    stopped short of its size (emptied, reloading, destroyed, retargeted) stays open: the next launcher of
///    the class that passes the checks launches the remainder. Inbound-weapon category threats must be inbound
///    (or designated). Permission written `Some(true)` by a rule, node or script fires on range and geometry
///    alone (`FireReason::Graph`), never at Neutral or Friend (held with a stance reason if one is offered).
/// 3. Hold reason: the failure for the highest-ranked threat the mount has an entry for; with none ranked, the
///    nearest unranked Suspect track the mount could otherwise engage (so "Suspect, not inside self-defence
///    range" shows); else `NoTarget`.
///
/// Guided weapons launch one munition per assignment tick while their salvo lasts and the launcher is ready
/// (`Assignment::launch`); guns fire continuously while `Engaging` (`launch` 0). Decoy launchers keep the
/// status decoy control gives them; their part state is recorded for it.
pub fn mount_assignment(input: &AssignIn<'_>, state: &mut EngageState, tuning: &TuningTable) -> Vec<Assignment> {
    realign(state, input.mounts);
    let tick = input.tick;
    state.salvos.retain(|s| tick < s.result_tick && find_track(input.tracks, s.track).is_some());
    for me in state.mounts.iter_mut() {
        if me.salvo_left == 0 && !state.salvos.iter().any(|s| s.slot == me.slot) { me.awaiting_result = false; }
    }
    let ctx = AssignCtx::new(input, state, tuning);
    let mut out = Vec::with_capacity(input.mounts.len());
    let mut taken: Vec<(WeaponClass, TrackId)> = Vec::new();
    for (i, m) in input.mounts.iter().enumerate() {
        let part = find_part(input.parts, m.slot);
        if m.weapon == WeaponClass::Decoy {
            state.mounts[i].decoy_block = decoy_block(part);
            out.push(Assignment { slot: m.slot, status: state.mounts[i].status, launch: 0 });
            continue;
        }
        let me = state.mounts[i];
        let permission = input.permission.get(i).copied().flatten();
        let forced = permission == Some(true);
        let pick = ctx.select(m, &me, &state.mounts, &state.salvos, &taken, forced);
        let target_id = pick.as_ref().ok().map(|p| p.track).unwrap_or_else(|| pick.as_ref().err().map(|h| h.0).unwrap_or(TrackId::NONE));
        let gate = if input.held.contains(&m.slot) { Some(HoldReason::PlayerControl) } else { hardware_hold(part, input.parts, permission) };
        let mut launch = 0u8;
        let status = match (gate, pick) {
            (Some(reason), _) => EngageStatus::Holding { track: target_id, reason },
            (None, Err((track, reason))) => EngageStatus::Holding { track, reason },
            (None, Ok(p)) => {
                let reason = if part.is_some_and(|p| p.links.data == LinkState::LocalControl) && p.reason != FireReason::PointDefence { FireReason::LocalControl } else { p.reason };
                if is_guided(m.weapon) { launch = step_salvo(&mut state.mounts[i], &mut state.salvos, m, part, &p, tick); taken.push((m.weapon, p.track)); }
                EngageStatus::Engaging { track: p.track, reason }
            }
        };
        let me = &mut state.mounts[i];
        match status {
            EngageStatus::Engaging { track, .. } => { if !is_guided(m.weapon) { me.salvo_left = 0; } me.target = track; }
            _ => { me.salvo_left = 0; me.target = TrackId::NONE; }
        }
        me.awaiting_result = state.salvos.iter().any(|s| s.slot == m.slot);
        me.status = status;
        out.push(Assignment { slot: m.slot, status, launch });
    }
    state.weapons_in_flight = state.salvos.iter().fold(0u16, |n, s| n.saturating_add(s.count as u16));
    out
}

/// Ranked threats one mount examines per assignment tick (bounds the mount-threat pair work).
pub const MAX_THREATS_PER_MOUNT: usize = 64;
/// Seeker basket for anti-ship missiles when the launcher's munition is not visible to the stage: the spec's
/// missile seeker reference range (L432); the half field of view comes from `GuidanceTuning`.
pub const DEFAULT_SEEKER_RANGE_M: f64 = 12_000.0;
/// A Friend track this close to the firing ship is the ship itself (own-side hulls are datalink tracks).
pub const OWN_TRACK_RADIUS_M: f64 = 60.0;
/// Friend-in-line test for guns: a friend from this far out to `FRIEND_OVERSHOOT_M` beyond the target, within
/// `FRIEND_LATERAL_M` (or 2% of its distance, if wider) of the line of fire, holds the mount.
pub const FRIEND_MIN_ALONG_M: f64 = 100.0;
pub const FRIEND_OVERSHOOT_M: f64 = 500.0;
pub const FRIEND_LATERAL_M: f64 = 150.0;
/// DECISION (owner ruling, 22 November 2026, reading L487 "nothing auto-engages a Neutral or Friend track" for
/// guns as L483 reads it for missiles): an officer-fired gun holds while a live Neutral or Friend surface track's
/// estimate lies this close (horizontally) to the target's predicted position.
pub const GUN_CLEAR_OF_NEUTRAL_M: f64 = 500.0;
/// Masking by another gun mount: a mount pivot within this lateral distance of the bore line, ahead by at most
/// `MASK_REACH_M`, and less than `MASK_CLEARANCE_M` below the bore there, masks the shot.
pub const MASK_RADIUS_M: f64 = 2.0;
pub const MASK_REACH_M: f64 = 80.0;
pub const MASK_CLEARANCE_M: f64 = 1.0;
/// Margin added to a guided salvo's expected flight time before a miss is assumed, seconds.
pub const SALVO_RESULT_MARGIN_S: f64 = 3.0;
const TICK_S: f64 = 1.0 / sim_core::TICK_HZ as f64;

fn is_guided(w: WeaponClass) -> bool { matches!(w, WeaponClass::AntiShipMissile | WeaponClass::Sam | WeaponClass::PointDefenceMissile | WeaponClass::Torpedo | WeaponClass::DepthCharge) }
fn is_gun(w: WeaponClass) -> bool { matches!(w, WeaponClass::Gun | WeaponClass::PointDefenceGun) }

fn realign(state: &mut EngageState, mounts: &[MountInfo]) {
    if state.mounts.len() == mounts.len() && state.mounts.iter().zip(mounts).all(|(a, b)| a.slot == b.slot) { return; }
    let old = core::mem::take(&mut state.mounts);
    state.mounts = mounts.iter().map(|m| old.iter().find(|o| o.slot == m.slot).copied()
        .unwrap_or(MountEngage { slot: m.slot, status: EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoTarget }, ..Default::default() })).collect();
}

/// Hardware and permission gates of a weapon mount, in order.
fn hardware_hold(part: Option<&PartRuntime>, parts: &[PartRuntime], permission: Option<bool>) -> Option<HoldReason> {
    use sim_core::parts::PartState;
    let Some(p) = part else { return Some(HoldReason::Damaged) };
    if !p.works() { return Some(HoldReason::Damaged); }
    if p.links.shed { return Some(HoldReason::Shed); }
    if p.priority > 0 && p.links.power == LinkState::Dead { return Some(HoldReason::Unpowered); }
    if permission == Some(false) { return Some(HoldReason::PermissionWithheld); }
    match p.state {
        PartState::Launcher { ready: 0, reload_ticks, .. } if reload_ticks > 0 => Some(HoldReason::Reloading { seconds: reload_ticks as f64 * TICK_S }),
        PartState::Launcher { ready: 0, .. } => Some(HoldReason::NoAmmunition),
        // Ready rounds run out between hoist feeds; only an empty ship (no magazine with rounds) holds the
        // gun. Mount info does not link a gun to its own kit's magazine, so any loaded magazine counts.
        PartState::Mount(ms) if ms.ready_rounds == 0 && !parts.iter().any(|q| q.works() && matches!(q.state, PartState::Magazine { rounds, flooded: false, .. } if rounds > 0)) => Some(HoldReason::NoAmmunition),
        _ => None,
    }
}

/// A weapon mount can still fire: not destroyed, powered, and with rounds (a reloading launcher can). The
/// surrender rule's "no weapon left that works" (`ai::unit::surrender_due`). Shedding is the Engineer's
/// choice, not a loss, so a shed mount still counts.
pub(crate) fn weapon_can_fire(part: Option<&PartRuntime>, parts: &[PartRuntime]) -> bool {
    !matches!(hardware_hold(part, parts, None), Some(HoldReason::Damaged | HoldReason::Unpowered | HoldReason::NoAmmunition))
}

/// Decoy launcher availability for decoy control.
fn decoy_block(part: Option<&PartRuntime>) -> Option<HoldReason> {
    use sim_core::parts::PartState;
    let Some(p) = part else { return Some(HoldReason::Damaged) };
    if !p.works() { return Some(HoldReason::Damaged); }
    if p.links.shed { return Some(HoldReason::Shed); }
    if p.priority > 0 && p.links.power == LinkState::Dead { return Some(HoldReason::Unpowered); }
    match p.state {
        PartState::Decoy { reload_ticks, .. } if reload_ticks > 0 => Some(HoldReason::Reloading { seconds: reload_ticks as f64 * TICK_S }),
        _ => None,
    }
}

/// Advance a guided mount's salvo against `p.track`; returns munitions to launch now (0 or 1).
fn step_salvo(me: &mut MountEngage, salvos: &mut Vec<Salvo>, m: &MountInfo, part: Option<&PartRuntime>, p: &Pick, tick: u64) -> u8 {
    use sim_core::parts::PartState;
    if me.target != p.track || me.salvo_left == 0 {
        me.target = p.track;
        me.salvo_left = p.salvo.max(1);
    }
    let want = p.want.max(p.salvo);
    let ready = match part.map(|q| &q.state) { Some(PartState::Launcher { ready, interval_ticks, .. }) => *ready > 0 && *interval_ticks == 0, _ => true };
    if !ready { return 0; }
    me.salvo_left -= 1;
    let speed = if m.muzzle_mps.is_finite() && m.muzzle_mps > 1.0 { m.muzzle_mps } else { default_speed_mps(m.weapon) };
    let flight_s = (p.range_m / speed + SALVO_RESULT_MARGIN_S).clamp(SALVO_RESULT_MARGIN_S, 900.0);
    let result_tick = tick.saturating_add((flight_s / TICK_S) as u64);
    match salvos.iter_mut().find(|s| s.slot == m.slot && s.track == p.track) {
        Some(s) => { s.count = s.count.saturating_add(1); s.result_tick = s.result_tick.max(result_tick); s.want = s.want.max(want); }
        None => salvos.push(Salvo { slot: m.slot, weapon: m.weapon, track: p.track, count: 1, result_tick, want }),
    }
    if me.salvo_left == 0 { me.last_salvo_tick = tick; }
    1
}

/// Typical speeds for the flight-time estimate when the mount carries none, m/s.
fn default_speed_mps(w: WeaponClass) -> f64 {
    match w { WeaponClass::AntiShipMissile => 270.0, WeaponClass::Sam => 800.0, WeaponClass::PointDefenceMissile => 900.0, WeaponClass::Torpedo => 20.0, WeaponClass::DepthCharge => 3.0, _ => 300.0 }
}

/// A chosen target: `salvo` munitions to launch from this mount, completing a salvo of `want`.
struct Pick { track: TrackId, reason: FireReason, salvo: u8, want: u8, range_m: f64 }

/// Read-only facts shared by every mount this tick.
struct AssignCtx<'a> {
    input: &'a AssignIn<'a>,
    tuning: &'a TuningTable,
    open_fire: f64,
    fov_half_rad: f64,
    /// Friend tracks (id, predicted position), the firing ship itself excluded.
    friends: Vec<(TrackId, DVec3)>,
    friend_ids: Vec<TrackId>,
    /// Live Suspect tracks that did not rank, nearest first: hold-reason fallback.
    unranked_suspects: Vec<(TrackId, f64)>,
    /// The tracks a seeker-basket test looks for, with their predictions at the tick: live Neutral tracks and
    /// the `friends`, in track order. Computed once here, not once per mount and candidate target.
    basket: Vec<(Stance, TrackId, DVec3)>,
    /// Live Neutral surface tracks and Friend surface tracks of `friends`, predicted at the tick: the gun hold
    /// near a target ([`GUN_CLEAR_OF_NEUTRAL_M`]).
    surface_clear: Vec<(Stance, TrackId, DVec3)>,
    /// `EngageState::shore_sites` at the tick.
    shore_sites: Vec<TrackId>,
}

impl<'a> AssignCtx<'a> {
    fn new(input: &'a AssignIn<'a>, state: &EngageState, tuning: &'a TuningTable) -> Self {
        let open_fire = state.open_fire_range.filter(|v| v.is_finite()).map(|v| v.clamp(0.5, 1.0)).unwrap_or(1.0);
        let fov = tuning.guidance.seeker_fov_half_deg;
        let fov_half_rad = if fov.is_finite() && fov > 0.0 { fov.min(89.0).to_radians() } else { 30f64.to_radians() };
        let own = finite_or(input.own.pose.pos, DVec3::ZERO);
        // Own-side weapons in flight are not friends in the line (DECISION, q5): the first missile of a
        // salvo held the second and the guns on "Friend in the line of fire" for its whole flight.
        let friends: Vec<(TrackId, DVec3)> = input.tracks.iter().filter(|t| usable(t) && t.stance == Stance::Friend && t.domain != Domain::Weapon)
            .map(|t| (t.id, t.predict(input.tick))).filter(|(_, p)| p.is_finite() && (*p - own).length() > OWN_TRACK_RADIUS_M).collect();
        let mut unranked_suspects: Vec<(TrackId, f64)> = input.tracks.iter()
            .filter(|t| usable(t) && t.stance == Stance::Suspect && t.domain != Domain::Weapon && !t.out_of_action && !input.ranked.iter().any(|r| r.track == t.id))
            .map(|t| (t.id, (t.predict(input.tick) - own).length())).filter(|(_, r)| r.is_finite()).collect();
        unranked_suspects.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        let friend_ids: Vec<TrackId> = friends.iter().map(|f: &(TrackId, DVec3)| f.0).collect();
        let basket = input.tracks.iter()
            .filter(|t| usable(t) && (t.stance == Stance::Neutral || (t.stance == Stance::Friend && friend_ids.contains(&t.id))))
            .map(|t| (t.stance, t.id, t.predict(input.tick))).filter(|b| b.2.is_finite()).collect();
        let surface_clear = input.tracks.iter()
            .filter(|t| usable(t) && t.domain == Domain::Surface && (t.stance == Stance::Neutral || (t.stance == Stance::Friend && friend_ids.contains(&t.id))))
            .map(|t| (t.stance, t.id, t.predict(input.tick))).filter(|b| b.2.is_finite()).collect();
        Self { input, tuning, open_fire, fov_half_rad, friends, friend_ids, unranked_suspects, basket, surface_clear, shore_sites: state.shore_sites.clone() }
    }

    /// Best target for one mount, or the hold (track, reason) to report.
    fn select(&self, m: &MountInfo, me: &MountEngage, others: &[MountEngage], salvos: &[Salvo], taken: &[(WeaponClass, TrackId)], forced: bool) -> Result<Pick, (TrackId, HoldReason)> {
        let mut first_hold: Option<(TrackId, HoldReason)> = None;
        for r in self.input.ranked.iter().take(MAX_THREATS_PER_MOUNT) {
            let Some(entry) = self.input.doctrine.get(m.slot, r.category) else { continue };
            if r.category == TargetCategory::InboundWeapon && !r.inbound && !r.designated { continue; }
            let Some(t) = find_track(self.input.tracks, r.track) else { continue };
            match self.evaluate(m, me, others, entry, t, r.designated, salvos, taken, forced) {
                Ok(p) => return Ok(p),
                Err(h) => { if first_hold.is_none() { first_hold = Some((t.id, h)); } }
            }
        }
        if let Some(h) = first_hold { return Err(h); }
        for &(id, _) in &self.unranked_suspects {
            let Some(t) = find_track(self.input.tracks, id) else { continue };
            let Some(entry) = self.input.doctrine.get(m.slot, category_of(t, false)) else { continue };
            return match self.evaluate(m, me, others, entry, t, false, salvos, taken, forced) { Ok(p) => Ok(p), Err(h) => Err((id, h)) };
        }
        Err((TrackId::NONE, HoldReason::NoTarget))
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate(&self, m: &MountInfo, me: &MountEngage, others: &[MountEngage], entry: &ResolvedEntry, t: &Track, designated: bool, salvos: &[Salvo], taken: &[(WeaponClass, TrackId)], forced: bool) -> Result<Pick, HoldReason> {
        let input = self.input;
        // DECISION (coordinator ruling, 22 November 2026; L483 and L523 speak of ships and are silent on
        // sites): an anti-ship missile's seeker finds a ship against the sea, not a site against the land, so
        // the officer never gives one a shore site, designated or with fire permission written or not.
        if m.weapon == WeaponClass::AntiShipMissile && self.shore_sites.contains(&t.id) { return Err(HoldReason::ShoreSite); }
        let muzzle = finite_or(input.own.pose.to_world(m.pos_local), finite_or(input.own.pose.pos, DVec3::ZERO));
        let tp = t.predict(input.tick);
        let range = (tp - muzzle).length();
        let pd = matches!(m.weapon, WeaponClass::PointDefenceGun | WeaponClass::PointDefenceMissile) || entry.entry.target == TargetCategory::InboundWeapon;
        let mut e = *entry;
        if !pd { e.effective_range_m *= self.open_fire; }
        let reason = if forced {
            // A rule, node or script never overrides L487: Neutral and Friend are never engaged.
            if t.stance < Stance::Suspect { return Err(HoldReason::Stance { have: t.stance, need: entry.entry.min_stance.max(Stance::Suspect) }); }
            let limit = finite_pos(e.effective_range_m).unwrap_or(0.0);
            if range.is_nan() || range > limit { return Err(HoldReason::OutOfRange { range_m: if range.is_finite() { range } else { f64::MAX }, limit_m: limit }); }
            if pd { FireReason::PointDefence } else { FireReason::Graph }
        } else {
            match crate::doctrine::check_facts(&e, t.stance, t.rung, t.grade, range, input.doctrine.self_defence_range_m, designated) {
                Ok(r) => r,
                Err(HoldReason::Grade { need: TrackGrade::Lock, .. }) if self.director_silent(m) => return Err(HoldReason::EmconSilent),
                Err(h) => return Err(h),
            }
        };
        if is_gun(m.weapon) {
            self.gun_geometry(m, muzzle, tp, range)?;
            // L487: no officer-fired gun shoots at a surface contact with a Neutral or Friend hull beside it.
            if !pd && t.domain == Domain::Surface {
                if let Some(&(stance, other, _)) = self.surface_clear.iter().filter(|b| b.1 != t.id)
                    .find(|b| { let d = DVec2::new(b.2.x - tp.x, b.2.z - tp.z).length(); d.is_finite() && d <= GUN_CLEAR_OF_NEUTRAL_M }) {
                    return Err(if stance == Stance::Neutral { HoldReason::NeutralInSeekerBasket { neutral: other } } else { HoldReason::FriendInLine { friend: other } });
                }
            }
            // The open-fire test of a default entry (DECISION, k2): the entry's range is the gauge's, solved
            // against the 40 m reference hull (L489); inside it the officer fires only while the predicted
            // single-round hit probability against this contact, with the errors its fire control really has,
            // meets the same criterion. Otherwise it holds, reporting the range at which it would.
            if m.weapon == WeaponClass::Gun && !pd && !forced && entry.entry.effective_range_m.is_none() {
                if let Some(g) = &input.gunnery {
                    let pk_min = finite_pos(self.tuning.doctrine.effective_range_pk).unwrap_or(0.10);
                    if let Some(h) = hit_chance(m, t, muzzle, range, input.tick, g, self.tuning) {
                        if h.pk < pk_min {
                            let limit = crate::guidance::gun::pk_range(h.table, &h.errors, &h.target, pk_min, e.effective_range_m);
                            return Err(HoldReason::OutOfRange { range_m: range, limit_m: limit });
                        }
                    }
                }
            }
        }
        if m.weapon == WeaponClass::AntiShipMissile {
            for stance in [Stance::Neutral, Stance::Friend] {
                let near = self.basket.iter().filter(|b| b.0 == stance).map(|b| (b.1, b.2));
                if let Some(other) = basket_scan(t, near, DEFAULT_SEEKER_RANGE_M, self.fov_half_rad, muzzle, input.tick) {
                    return Err(if stance == Stance::Neutral { HoldReason::NeutralInSeekerBasket { neutral: other } } else { HoldReason::FriendInLine { friend: other } });
                }
            }
        }
        let want = if e.entry.salvo == 0 {
            // Designated past a held entry: the cautious default salvo for the pair.
            crate::doctrine::default_entries(DoctrinePreset::Cautious).iter().find(|d| d.weapon == m.weapon && d.target == e.entry.target).map(|d| d.salvo).unwrap_or(1)
        } else { e.entry.salvo };
        let mut salvo = want;
        if is_guided(m.weapon) {
            if taken.contains(&(m.weapon, t.id)) { return Err(HoldReason::SalvoInFlight); }
            let own_salvo = me.target == t.id && me.salvo_left > 0;
            let same = || salvos.iter().filter(|s| s.weapon == m.weapon && s.track == t.id);
            let flying = same().fold(0u8, |n, s| n.saturating_add(s.count));
            if flying > 0 && !own_salvo {
                // The salvo in flight is closed unless its launcher stopped short of its size; then this mount
                // launches the rest (fixed salvo rules, L310, whichever launcher carries them).
                let open_want = same().map(|s| s.want).max().unwrap_or(0);
                let continuing = same().any(|s| s.slot != m.slot && others.iter().any(|o| o.slot == s.slot && o.target == t.id && o.salvo_left > 0));
                if open_want <= flying || continuing { return Err(HoldReason::SalvoInFlight); }
                salvo = open_want - flying;
                return Ok(Pick { track: t.id, reason, salvo, want: open_want, range_m: range });
            }
        }
        if salvo == 0 { salvo = 1; }
        Ok(Pick { track: t.id, reason, salvo, want: want.max(1), range_m: range })
    }

    /// The mount's fire-control director exists and is not radiating (emission control keeps it silent).
    fn director_silent(&self, m: &MountInfo) -> bool {
        let Some(slot) = m.director else { return false };
        matches!(find_part(self.input.parts, slot).map(|p| &p.state), Some(sim_core::parts::PartState::Sensor { radiating: false, .. }))
    }

    /// Training and line-of-sight elevation limits, masking and friends near the line of fire.
    fn gun_geometry(&self, m: &MountInfo, muzzle: DVec3, tp: DVec3, range: f64) -> Result<(), HoldReason> {
        let input = self.input;
        let dir = tp - muzzle;
        if !(range.is_finite() && range > 1e-6) { return Ok(()); }
        let (train, elev) = crate::guidance::gun::world_dir_to_mount(&input.own.pose, m.zero_rad, dir);
        train_check(&m.limits, train)?;
        elevation_check(&m.limits, elev)?;
        // Masking by another gun mount, in the vessel frame.
        let rot = if input.own.pose.rot.is_finite() && input.own.pose.rot.length_squared() > 1e-24 { input.own.pose.rot.normalize() } else { DQuat::IDENTITY };
        let d = rot.inverse() * dir;
        let horiz = libm::sqrt(d.x * d.x + d.z * d.z);
        if horiz > 1e-9 {
            let (hx, hz, slope) = (d.x / horiz, d.z / horiz, d.y / horiz);
            for n in input.mounts.iter().filter(|n| n.slot != m.slot && is_gun(n.weapon)) {
                let q = n.pos_local - m.pos_local;
                if !q.is_finite() { continue; }
                let along = q.x * hx + q.z * hz;
                if along <= 0.5 || along > MASK_REACH_M { continue; }
                let lateral = (q.x * hz - q.z * hx).abs();
                if lateral > MASK_RADIUS_M { continue; }
                if q.y + MASK_CLEARANCE_M > along * slope { return Err(HoldReason::Masked); }
            }
        }
        // Own-side units in or near the line of fire.
        let u = dir / range;
        for &(id, f) in &self.friends {
            let rel = f - muzzle;
            let along = rel.dot(u);
            if along < FRIEND_MIN_ALONG_M || along > range + FRIEND_OVERSHOOT_M { continue; }
            let lateral = (rel - u * along).length();
            if lateral < FRIEND_LATERAL_M.max(0.02 * along) { return Err(HoldReason::FriendInLine { friend: id }); }
        }
        Ok(())
    }
}

/// One gun's predicted chance against one contact, with the inputs that gave it.
pub struct HitChance<'a> {
    pub pk: f64,
    pub table: &'a [crate::guidance::gun::RangeRow],
    pub errors: crate::guidance::gun::ShotErrors,
    pub target: crate::guidance::gun::TargetBox,
}

/// The predicted single-round hit probability of an officer-aimed gun `m` at `track`, `range_m` from its muzzle
/// (L489 with the errors the solver really has). Unit: `sys-engage`.
///
/// - Laying: the gun's dispersion, the officers' laying error times the accuracy multiplier and the sea's
///   (`guidance::gun::laying_sigma_rad`).
/// - Target estimate: the track's fire-control motion fit (`Track::fire_control`), aged from its last fix;
///   without one, the filter's own radius and velocity sigma.
/// - Manoeuvre allowance: the spec's surface sigma_a (L463, `TrackTuning::sigma_a`), combined with the larger
///   of the acceleration of a turn in progress and the target's measured manoeuvring (the fit's checks on its
///   own predictions).
/// - Silhouette: the contact's radar cross-section at the aspect its estimated course shows to the gun
///   (`guidance::gun::silhouette`); the aspect is unknown while the course is slower than 2 m/s or less
///   certain than half the speed.
///
/// `None` when the mount has no range table or its part is not a gun.
pub fn hit_chance<'a>(m: &MountInfo, track: &Track, muzzle: DVec3, range_m: f64, tick: u64, g: &GunneryFacts<'a>, tuning: &TuningTable) -> Option<HitChance<'a>> {
    use crate::guidance::gun::{laying_sigma_rad, predicted_pk, silhouette, ShotErrors};
    let table: &'a [crate::guidance::gun::RangeRow] = g.tables.get(&m.slot)?.as_slice();
    let gun = match g.catalogue.parts.get_index(m.part.index()).map(|(_, d)| &d.function) { Some(content::part::PartFunction::Gun(gd)) => gd, _ => return None };
    let sigma_a = finite_pos(tuning.tracks.sigma_a[0]).unwrap_or(0.3);
    let tick_s = |t: u64| tick.saturating_sub(t) as f64 * TICK_S;
    let angular_rad = laying_sigma_rad(gun.dispersion_mrad, g.accuracy_mult, g.sea);
    let tp = track.predict(tick);
    let los = DVec2::new(tp.x - muzzle.x, tp.z - muzzle.z);
    // (errors without the manoeuvre allowance, course estimate, its velocity variance, measured acceleration)
    let (mut errors, vel, var_vel, accel) = match track.fire_control() {
        Some(e) => {
            // The fit's error is shaped like its fixes' (`MotionEstimate::along_frac`): split it across and along
            // this gun's line of fire by the angle between that and the fixes' line of sight.
            let c2 = if los.length() > 1.0 { let d = los.normalize().dot(e.fix_los); d * d } else { 0.5 };
            let cross = e.cross_frac * c2 + e.along_frac * (1.0 - c2);
            let along = e.along_frac * c2 + e.cross_frac * (1.0 - c2);
            (ShotErrors {
                angular_rad, var_pos: e.var_pos * cross, var_vel: e.var_vel * cross, cov: e.cov * cross,
                var_pos_along: e.var_pos * along, var_vel_along: e.var_vel * along, cov_along: e.cov * along, age_s: tick_s(e.tick), ..Default::default()
            }, DVec2::new(e.vel.x, e.vel.y), e.var_vel, e.accel_mps2.max(e.manoeuvre_mps2))
        }
        None => {
            let (r2, v2) = { let r = track.sigma_m(tick, &tuning.tracks); (r * r, track.sigma_v * track.sigma_v) };
            (ShotErrors { angular_rad, var_pos: r2, var_vel: v2, var_pos_along: r2, var_vel_along: v2, ..Default::default() }, DVec2::new(track.vel.x, track.vel.z), v2, 0.0)
        }
    };
    let speed = vel.length();
    let aspect = if speed.is_finite() && speed > 2.0 && libm::sqrt(var_vel.max(0.0)) < 0.5 * speed && los.length() > 1.0 {
        Some(libm::acos((vel.dot(los) / (speed * los.length())).clamp(-1.0, 1.0)))
    } else { None };
    // A hull manoeuvres by turning: its measured acceleration acts across its course, which the aspect splits
    // into the cross-range and along-range axes. The spec's allowance is per axis.
    let (sc, cc) = match aspect { Some(a) => (libm::sin(a).abs(), libm::cos(a).abs()), None => (core::f64::consts::FRAC_1_SQRT_2, core::f64::consts::FRAC_1_SQRT_2) };
    errors.manoeuvre_mps2 = libm::sqrt(sigma_a * sigma_a + accel * accel * cc * cc);
    errors.manoeuvre_along_mps2 = libm::sqrt(sigma_a * sigma_a + accel * accel * sc * sc);
    let target = crate::guidance::gun::TargetBox { crest_m: crate::guidance::gun::crest_m(g.sea), ..silhouette(track.rcs_m2 as f64, aspect) };
    let pk = predicted_pk(table, range_m, &errors, &target);
    Some(HitChance { pk, table, errors, target })
}

/// Training check in the limits' representation; reports the needed angle and the nearer limit in degrees.
fn train_check(l: &content::graph::MountLimits, train_rad: f64) -> Result<(), HoldReason> {
    use core::f64::consts::TAU;
    let fin = |v: f64, d: f64| if v.is_finite() { v } else { d };
    let (mut t0, mut t1) = (fin(l.train_min_deg, -180.0), fin(l.train_max_deg, 180.0));
    if t0 > t1 { core::mem::swap(&mut t0, &mut t1); }
    if t1 - t0 >= 360.0 - 1e-9 { return Ok(()); }
    let (a0, a1) = (t0.to_radians(), t1.to_radians());
    let t = a0 + (train_rad - a0).rem_euclid(TAU);
    if t <= a1 + 1e-9 { return Ok(()); }
    // In the dead arc: name the nearer limit, with the need expressed next to it.
    let (to_max, to_min) = (t - a1, a0 + TAU - t);
    let (need, limit) = if to_max <= to_min { (t, a1) } else { (t - TAU, a0) };
    Err(HoldReason::TrainLimit { need_deg: need.to_degrees(), limit_deg: limit.to_degrees() })
}

fn elevation_check(l: &content::graph::MountLimits, elev_rad: f64) -> Result<(), HoldReason> {
    let fin = |v: f64, d: f64| if v.is_finite() { v } else { d };
    let (mut e0, mut e1) = (fin(l.elevation_min_deg, -90.0), fin(l.elevation_max_deg, 90.0));
    if e0 > e1 { core::mem::swap(&mut e0, &mut e1); }
    let e = elev_rad.to_degrees();
    if e < e0 - 1e-9 { return Err(HoldReason::ElevationLimit { need_deg: e, limit_deg: e0 }); }
    if e > e1 + 1e-9 { return Err(HoldReason::ElevationLimit { need_deg: e, limit_deg: e1 }); }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecoyOrder { Fire, StreamOn, StreamOff }

/// Decoy control: chaff or flares when an inbound missile is inside 6 km, one salvo per missile with the
/// launcher's reload; towed decoys stream while a torpedo or anti-ship missile track is inbound; smoke only
/// by player order or the `helm.evasion` pattern. Unit: `sys-engage`.
///
/// Inbound threats are live Suspect or Hostile weapon tracks flagged inbound (on the track or in
/// `state.ranked`); a weapon track below [`TORPEDO_DEPTH_M`] is a torpedo, any other a missile. For each inbound
/// missile inside `decoy_trigger_m` not yet decoyed, nearest first, the lowest-slot ready chaff launcher and the
/// lowest-slot ready flare launcher each fire one salvo; the missile counts as decoyed once any launcher fired
/// for it, so a missile that arrives while every launcher reloads is served when one is ready. A launcher is
/// ready when its part allows (recorded by [`mount_assignment`]) and [`DECOY_RELOAD_S`] has passed since its
/// last salvo. Towed decoys get `StreamOn` when the first torpedo or missile turns inbound and `StreamOff` when
/// none is left; orders are sent on change only. Smoke is never fired here. Launcher statuses are written to
/// `state.mounts`; `state.decoyed` keeps only live tracks.
pub fn decoy_control(own: &OwnState, tracks: &[Track], launchers: &[(PartSlot, DecoyKind)], state: &mut EngageState, tick: u64, tuning: &TuningTable) -> Vec<(PartSlot, DecoyOrder)> {
    let trigger = finite_pos(tuning.doctrine.decoy_trigger_m).unwrap_or(6_000.0);
    let own_pos = finite_or(own.pose.pos, DVec3::ZERO);
    let reload_ticks = (DECOY_RELOAD_S / TICK_S) as u64;
    state.decoyed.retain(|id| find_track(tracks, *id).is_some());
    state.decoyed.sort_unstable();
    state.decoyed.dedup();
    let ranked_inbound = |id: TrackId| state.ranked.iter().any(|r| r.track == id && r.inbound);
    let mut missiles: Vec<(f64, TrackId)> = Vec::new();
    let mut any_inbound: Option<TrackId> = None;
    for t in tracks.iter().filter(|t| usable(t) && t.domain == Domain::Weapon && t.stance >= Stance::Suspect) {
        if !(t.inbound || ranked_inbound(t.id)) { continue; }
        let p = t.predict(tick);
        if !p.is_finite() { continue; }
        if any_inbound.is_none_or(|a| t.id < a) { any_inbound = Some(t.id); }
        if p.y < TORPEDO_DEPTH_M { continue; }
        let r = (p - own_pos).length();
        if r <= trigger && state.decoyed.binary_search(&t.id).is_err() { missiles.push((r, t.id)); }
    }
    missiles.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut order: Vec<(PartSlot, DecoyKind)> = launchers.to_vec();
    order.sort_by_key(|l| l.0);
    order.dedup_by_key(|l| l.0);
    let mut idx = Vec::with_capacity(order.len());
    for &(slot, _) in &order {
        let i = match state.mounts.iter().position(|m| m.slot == slot) {
            Some(i) => i,
            None => { state.mounts.push(MountEngage { slot, status: EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoTarget }, ..Default::default() }); state.mounts.len() - 1 }
        };
        idx.push(i);
    }
    let mut out = Vec::new();
    let mut fired = vec![false; order.len()];
    for &(_, missile) in &missiles {
        let mut any = false;
        for kind in [DecoyKind::Chaff, DecoyKind::Flare] {
            let pick = (0..order.len()).find(|&j| {
                let me = &state.mounts[idx[j]];
                order[j].1 == kind && !fired[j] && me.decoy_block.is_none() && tick >= me.ready_tick
            });
            if let Some(j) = pick {
                let me = &mut state.mounts[idx[j]];
                me.ready_tick = tick.saturating_add(reload_ticks);
                me.last_salvo_tick = tick;
                me.status = EngageStatus::Engaging { track: missile, reason: FireReason::PointDefence };
                fired[j] = true;
                any = true;
                out.push((order[j].0, DecoyOrder::Fire));
            }
        }
        if any { if let Err(at) = state.decoyed.binary_search(&missile) { state.decoyed.insert(at, missile); } }
    }
    let waiting = missiles.iter().map(|m| m.1).find(|id| state.decoyed.binary_search(id).is_err());
    for (j, &(slot, kind)) in order.iter().enumerate() {
        if fired[j] { continue; }
        let me = &mut state.mounts[idx[j]];
        match kind {
            DecoyKind::Chaff | DecoyKind::Flare => {
                me.status = match (me.decoy_block, waiting) {
                    (Some(reason), w) => EngageStatus::Holding { track: w.unwrap_or(TrackId::NONE), reason },
                    (None, w) if tick < me.ready_tick => EngageStatus::Holding { track: w.unwrap_or(TrackId::NONE), reason: HoldReason::Reloading { seconds: (me.ready_tick - tick) as f64 * TICK_S } },
                    _ => EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoTarget },
                };
            }
            DecoyKind::TowedDecoy => {
                let streaming = matches!(me.status, EngageStatus::Engaging { .. });
                match (any_inbound, me.decoy_block) {
                    (_, Some(reason)) if !matches!(reason, HoldReason::Reloading { .. }) => {
                        if streaming { out.push((slot, DecoyOrder::StreamOff)); }
                        me.status = EngageStatus::Holding { track: any_inbound.unwrap_or(TrackId::NONE), reason };
                    }
                    (Some(threat), _) => {
                        if !streaming { out.push((slot, DecoyOrder::StreamOn)); }
                        me.status = EngageStatus::Engaging { track: threat, reason: FireReason::PointDefence };
                    }
                    (None, _) => {
                        if streaming { out.push((slot, DecoyOrder::StreamOff)); }
                        me.status = EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoTarget };
                    }
                }
            }
            DecoyKind::Smoke => {
                if !matches!(me.status, EngageStatus::Holding { .. }) { me.status = EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoTarget }; }
            }
        }
    }
    out
}

/// Reload between salvos of one chaff or flare launcher, seconds (L486). Decoy control keeps its own clock
/// because it sees no part state; a launcher whose part reloads slower reports `Reloading` through
/// [`mount_assignment`].
pub const DECOY_RELOAD_S: f64 = 10.0;
/// A weapon track deeper than this (metres, negative down) is a torpedo.
pub const TORPEDO_DEPTH_M: f64 = -2.0;

/// Seeker-basket test for anti-ship missiles: a Neutral track inside the seeker search area around the
/// predicted target position holds the launch (L483). Returns the offending track. Unit: `sys-engage`.
///
/// The basket is a horizontal sector: apex half a seeker range short of the predicted target position along
/// the line from the launch point (or at the launch point when it is nearer), axis along that line, half-angle
/// `fov_half_rad`, out to `seeker_range_m` from the apex, so the target sits in its middle. Positions are
/// track predictions at `tick`. Returns the offending live Neutral track nearest the target (ties by id).
/// Invalid ranges or angles test nothing.
pub fn neutral_in_basket(target: &Track, tracks: &[Track], seeker_range_m: f64, fov_half_rad: f64, launch_pos: DVec3, tick: u64) -> Option<TrackId> {
    let near = tracks.iter().filter(|n| usable(n) && n.stance == Stance::Neutral).map(|n| (n.id, n.predict(tick)));
    basket_scan(target, near, seeker_range_m, fov_half_rad, launch_pos, tick)
}

/// The basket geometry of [`neutral_in_basket`] over candidate (id, predicted position) pairs; the target's
/// own id is skipped. Returns the candidate nearest the target (ties by id), so candidate order is free.
fn basket_scan(target: &Track, near: impl Iterator<Item = (TrackId, DVec3)>, seeker_range_m: f64, fov_half_rad: f64, launch_pos: DVec3, tick: u64) -> Option<TrackId> {
    if !(seeker_range_m.is_finite() && seeker_range_m > 0.0 && fov_half_rad.is_finite() && fov_half_rad > 0.0 && launch_pos.is_finite()) { return None; }
    if !target.pos.is_finite() { return None; }
    let tp = target.predict(tick);
    if !tp.is_finite() { return None; }
    let (tx, tz) = (tp.x, tp.z);
    let (ax, az) = (tx - launch_pos.x, tz - launch_pos.z);
    let dist = libm::sqrt(ax * ax + az * az);
    let cos_fov = libm::cos(fov_half_rad.min(core::f64::consts::PI));
    let (dx, dz, apex) = if dist > 1.0 {
        let (dx, dz) = (ax / dist, az / dist);
        let back = (0.5 * seeker_range_m).min(dist);
        (dx, dz, (tx - dx * back, tz - dz * back))
    } else { (1.0, 0.0, (tx, tz)) };
    let mut best: Option<(f64, TrackId)> = None;
    for (id, p) in near {
        if id == target.id || !p.is_finite() { continue; }
        let (qx, qz) = (p.x - apex.0, p.z - apex.1);
        let len = libm::sqrt(qx * qx + qz * qz);
        let inside = if dist <= 1.0 { len <= 0.5 * seeker_range_m } else if len < 1e-6 { true } else {
            len <= seeker_range_m && (qx * dx + qz * dz) / len >= cos_fov
        };
        if !inside { continue; }
        let to_target = libm::sqrt((p.x - tx) * (p.x - tx) + (p.z - tz) * (p.z - tz));
        if best.is_none_or(|b| (to_target, id) < b) { best = Some((to_target, id)); }
    }
    best.map(|b| b.1)
}

/// Longest weapon envelope of a ship in metres under its doctrine (time-compression safety test and plot
/// ring, L113). Unit: `sys-engage`.
///
/// The largest effective range over the weapon mounts' resolved entries (the mount's own effective range when
/// it has none). Decoy launchers do not count; neither do destroyed mounts or launchers that are empty with no
/// reload at sea. A mount missing from `parts` counts (the test errs toward dropping time compression).
pub fn longest_envelope_m(mounts: &[MountInfo], doctrine: &DoctrineTable, parts: &[PartRuntime]) -> f64 {
    use sim_core::parts::PartState;
    let mut longest = 0.0f64;
    for m in mounts.iter().filter(|m| m.weapon != WeaponClass::Decoy) {
        if let Some(p) = find_part(parts, m.slot) {
            if !p.works() || matches!(p.state, PartState::Launcher { ready: 0, reload_ticks: 0, .. }) { continue; }
        }
        let mut best: Option<f64> = None;
        for (_, e) in doctrine.entries.range((m.slot, TargetCategory::Surface)..=(m.slot, TargetCategory::Subsurface)) {
            if let Some(r) = finite_pos(e.effective_range_m) { best = Some(best.map_or(r, |b: f64| b.max(r))); }
        }
        if let Some(r) = best.or_else(|| finite_pos(m.effective_range_m)) { longest = longest.max(r); }
    }
    longest
}

#[cfg(test)]
mod tests;
