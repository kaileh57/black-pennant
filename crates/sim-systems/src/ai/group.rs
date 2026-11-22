//! Unit `sys-ai-group`. Group behaviour: formation by flocking, standing orders, convoy and patrol routes.
//! DESIGN.md L762 (ships hold formation by flocking and keep doctrine-set engagement ranges), L766 (flocking
//! and collision avoidance adjust heading and speed only, never the path), L747 (a task group is 5 hulls).
//!
//! A group reads its own members (own side: the side knows where its ships are), the terrain and its side's
//! picture. It never reads `Track::truth` or any world state of another side. Contacts become actionable to
//! the group only once they have existed for the difficulty's reaction time, as for captains.
//!
//! Implementation notes (choices where the spec is silent):
//! - Leader: the ace's hull while it is active, else the first active member (ascending entity). Disabled
//!   hulls never lead. Under `Escort` orders the charge (the first active own-side hull carrying the group's
//!   tag) is the formation leader and every member takes a slot around it.
//! - Routes are planned for the leader through the shared `PathPlanner`, with the group's widest beam,
//!   deepest draft and largest turning radius (quantised up to 2 m of beam and 0.5 m of draft so classes
//!   share clearance masks). A route is re-requested only when the goal moves past a hysteresis, the leader
//!   changes, the leader strays from it, or (after a back-off) when it came back invalid. While a request is
//!   pending the leader steers straight for the goal.
//! - Goals by orders: `Patrol` cycles its markers (a single marker becomes a circuit around it); `Transit`
//!   and `Flee` run to their marker and hold there; `Guard`, `AirPatrol` and `Search` run a six-point circuit
//!   at the order's radius around their point (`Search` first goes to the point); `Anchor` holds where the
//!   group first stood; `Hunt` runs at the group's contact, else its last-known mark. A point the planner
//!   cannot reach is skipped; an unreachable goal otherwise holds the group where it stopped. Holding is a
//!   one-point route to the hold point at 1 kn (the leader stands on at steerage way past it), with a planned
//!   return once more than 3 turning radii (at least 600 m) away.
//! - Modes: `Withdrawing` under `Flee`, or latched once an engaged group is down to half its peak strength
//!   (never for convoys, garrisons, ace-led groups or under the `hold_ground` card). Convoys `Scatter` for
//!   5 minutes after a hostile comes within 15 km or a member comes under fire. Otherwise a Hostile contact
//!   inside 1.3 times the group's doctrine range is `Engaging`, beyond it `Closing` (anchored and transiting
//!   groups only engage what comes to them); a Suspect contact or `Search` orders give `Investigating`;
//!   `Escort` orders give `Screening`.
//! - Speed: the slowest active member's top speed times 0.6 standing, 0.7 investigating, 0.75 for convoys,
//!   0.85 closing and 0.95 withdrawing, so every member can keep its slot at full power.
//! - Formation spacing is twice the largest member's turning radius, 300 m to 3 km. Slots persist per member;
//!   when membership changes the survivors keep their slot index where it still exists and the rest fill
//!   free slots nearest first.
//! - Directives leave `keep_range_m` and `emcon` to the captain and the doctrine cards (`None`), except
//!   `Silent` while closing under the `silent_approach` card. Weapons are tight until the group's contact is
//!   Hostile; withdrawing and scattered groups are weapons free.
use super::path::{bearing, heading_dir, wrap_pi, PathPlanner, PathRequest, Route};
use super::DoctrineMods;
use crate::prelude::*;
use crate::protocol::config::{GroupInit, GroupOrders};
use crate::ship::ShipTable;
use crate::tracks::{SidePicture, Track};
use content::campaign::{FleetKind, Formation, MarkerKind};
use content::graph::WeaponClass;
use sim_core::terrain::Terrain;
use sim_core::vessel::VesselStatus;

/// DECISION (q5): a scattering convoy spreads its hulls this many formation spacings apart across the route.
const SCATTER_FAN_FACTOR: f64 = 2.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupMode { #[default] Standing, Investigating, Closing, Engaging, Screening, Withdrawing, Scattered }

/// What a group tells one of its captains.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Directive {
    pub mode: GroupMode,
    /// Formation slot offset from the leader in the leader's frame (x ahead, z starboard), metres.
    pub slot: DVec2,
    pub leader: Option<EntityId>,
    /// Route for the leader (members follow in formation).
    pub route: Vec<DVec2>,
    pub speed_kn: f64,
    /// Track the group is prosecuting, in the side picture.
    pub contact: TrackId,
    pub keep_range_m: Option<f64>,
    pub weapons_tight: bool,
    pub emcon: Option<Emcon>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupState {
    pub id: GroupId,
    pub side: SideId,
    pub kind: Option<FleetKind>,
    pub formation: Formation,
    pub orders: Option<GroupOrders>,
    /// Members ascending; the leader is the first live member unless an ace leads.
    pub members: Vec<EntityId>,
    pub leader: Option<EntityId>,
    pub mode: GroupMode,
    pub route: Route,
    pub contact: TrackId,
    pub ace: Option<AceId>,
    pub tag: String,
    pub theatre_group: Option<u32>,
    /// Private bookkeeping of the group (unit `sys-ai-group`). Build states with [`new_group`] or
    /// `..Default::default()`.
    #[serde(default)]
    pub(crate) mem: GroupMemory,
}

/// What a group remembers between updates besides the public fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct GroupMemory {
    /// Orders the current leg plan was made for; a change restarts the legs.
    pub(crate) orders_seen: Option<GroupOrders>,
    /// Leg of the patrol or circuit being run.
    pub(crate) leg: u32,
    /// Hold point (anchorage, end of a transit, start of a patrol with no usable markers).
    pub(crate) hold: Option<DVec2>,
    /// Where the leader stopped making way because its goal was unreachable.
    pub(crate) idle_at: Option<DVec2>,
    /// Goal the newest route request was made for, the leader it was planned for and when.
    pub(crate) req_goal: Option<DVec2>,
    pub(crate) req_leader: EntityId,
    pub(crate) req_tick: u64,
    /// The newest request was for a patrol or circuit leg.
    pub(crate) req_cyclic: bool,
    pub(crate) waiting: bool,
    /// Goal `GroupState::route` was planned for.
    pub(crate) route_goal: Option<DVec2>,
    /// No new request for an unreachable goal before this tick.
    pub(crate) retry_tick: u64,
    /// Most live members seen at once.
    pub(crate) peak: u16,
    pub(crate) engaged: bool,
    pub(crate) withdraw_latched: bool,
    pub(crate) scatter_until_tick: u64,
    /// Slot index per member, ascending member, and the slot count they were assigned for.
    pub(crate) slots: Vec<(EntityId, u16)>,
    pub(crate) slot_count: u16,
}

/// Create a group from its config entry and member list. Unit: `sys-ai-group`.
pub fn new_group(init: &GroupInit, members: Vec<EntityId>) -> GroupState {
    let mut members: Vec<EntityId> = members.into_iter().filter(|m| !m.is_none()).collect();
    members.sort();
    members.dedup();
    let leader = members.first().copied();
    GroupState {
        id: init.id,
        side: init.side,
        kind: Some(init.kind),
        formation: init.formation,
        orders: Some(init.orders.clone()),
        members,
        leader,
        mode: if matches!(init.orders, GroupOrders::Flee { .. }) { GroupMode::Withdrawing } else { GroupMode::Standing },
        route: Route::default(),
        contact: TrackId::NONE,
        ace: init.ace.clone(),
        tag: init.tag.clone(),
        theatre_group: init.theatre_group,
        mem: GroupMemory::default(),
    }
}

/// Formation slot offsets for `n` members at a spacing scaled to the largest member's turning radius.
/// Unit: `sys-ai-group`.
///
/// Offsets are in the leader's frame (x ahead, z starboard); slot 0 is the leader at the origin and every
/// other slot is distinct. Column: astern in line. Line abreast: alternately starboard and port. Wedge:
/// stepped back to either side. Screen: a ring at 1.5 spacings starting ahead and working aft alternately
/// starboard and port (eight stations per ring, the next ring one spacing further out). Box: centred columns
/// (about the square root of `n` wide) in rows astern of the leader. A bad spacing falls back to 500 m.
pub fn formation_slots(formation: Formation, n: usize, spacing_m: f64) -> Vec<DVec2> {
    let s = if spacing_m.is_finite() && spacing_m > 0.0 { spacing_m.min(50_000.0) } else { 500.0 };
    let n = n.min(4_096);
    // Lateral step k of an alternating sequence: 0, +1, -1, +2, -2, ...
    let alt = |k: usize| -> f64 { if k == 0 { 0.0 } else if k % 2 == 1 { k.div_ceil(2) as f64 } else { -((k / 2) as f64) } };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let p = match formation {
            Formation::Column => DVec2::new(-(i as f64) * s, 0.0),
            Formation::LineAbreast => DVec2::new(0.0, alt(i) * s),
            Formation::Wedge => {
                let step = i.div_ceil(2) as f64;
                let side = if i % 2 == 1 { 1.0 } else { -1.0 };
                if i == 0 { DVec2::ZERO } else { DVec2::new(-step * s, side * step * s) }
            }
            Formation::Screen => {
                if i == 0 {
                    DVec2::ZERO
                } else {
                    const ANGLES_DEG: [f64; 8] = [0.0, 45.0, -45.0, 90.0, -90.0, 135.0, -135.0, 180.0];
                    let ring = (i - 1) / ANGLES_DEG.len();
                    // Each further ring is turned half a station so its hulls sit between the inner ones.
                    let a = (ANGLES_DEG[(i - 1) % ANGLES_DEG.len()] + if ring % 2 == 1 { 22.5 } else { 0.0 }) * core::f64::consts::PI / 180.0;
                    let r = (1.5 + ring as f64) * s;
                    DVec2::new(r * libm::cos(a), r * libm::sin(a))
                }
            }
            Formation::Box => {
                let cols = box_columns(n);
                DVec2::new(-((i / cols) as f64) * s, alt(i % cols) * s)
            }
        };
        out.push(p);
    }
    out
}

fn box_columns(n: usize) -> usize {
    let mut c = 1usize;
    while c * c < n { c += 1; }
    c.max(1)
}

/// One 2 Hz group update: pick the leader, request or follow a route for the standing orders or the
/// commander's tasking, choose the mode from the side picture (contact inside doctrine range, losses),
/// and issue one directive per member in ascending member order. Unit: `sys-ai-group`.
pub fn update(state: &mut GroupState, ships: &ShipTable, world: &World, picture: &SidePicture, planner: &mut PathPlanner, mods: &DoctrineMods, ctx: &SimCtx<'_>) -> Vec<(EntityId, Directive)> {
    let members: Vec<Member> = state.members.iter().filter_map(|&m| member_view(m, state, ships, world, ctx.tick.0)).collect();
    let charge = match &state.orders { Some(GroupOrders::Escort { tag }) => find_charge(tag, state, ships, world), _ => None };
    let env = Env {
        picture,
        terrain: world.terrain(),
        mods,
        now: ctx.tick.0,
        reaction_ticks: reaction_ticks(ctx),
        last_known_s: ctx.tuning.tracks.last_known_s,
        charge,
    };
    update_core(state, &members, &env, planner)
}

/// Flocking adjustment for one member: separation, alignment and cohesion around its slot plus collision
/// avoidance against hulls and land ahead; returns (heading change in radians, speed factor). Never edits the
/// path. Unit: `sys-ai-group`.
///
/// The heading change is relative to the hull's current heading, positive to starboard (clockwise), at most
/// 60 degrees either way; the speed factor multiplies the ordered speed, 0.2 to 1.2. Alone, on its slot and in
/// open water a hull gets (0, 1). Rules: separation pushes away from hulls closer than 0.75 turning radii
/// (80 to 500 m); alignment pulls toward the mean course of hulls within four turning radii; cohesion pulls
/// toward the slot, harder the further off it is, and trims speed by how far ahead of or behind it the hull
/// is. Collision avoidance looks 90 s ahead at the closest point of approach of every neighbour: a pass closer
/// than 1.5 separation radii turns the hull to starboard (both hulls of a head-on pair turn the same way, so
/// they open) and slows it when the other hull will be ahead. Land and shoal water (under 4 m) on a probe
/// along the heading out to 20 s of travel (at least two turning radii) turns the hull toward the nearest clear
/// heading, starboard first on a tie, and slows it as the shore nears. Non-finite input gives (0, 1).
pub fn flock_adjust(me: &Kinematics, slot_world: DVec2, neighbours: &[Kinematics], turn_radius_m: f64, terrain: &sim_core::terrain::Terrain) -> (f64, f64) {
    let p = DVec2::new(me.pose.pos.x, me.pose.pos.z);
    let h = me.pose.heading_rad();
    if !p.is_finite() || !h.is_finite() { return (0.0, 1.0); }
    let v = { let v = DVec2::new(me.lin_vel.x, me.lin_vel.z); if v.is_finite() { v } else { DVec2::ZERO } };
    let u = v.length();
    let r = if turn_radius_m.is_finite() && turn_radius_m >= 10.0 { turn_radius_m.min(5_000.0) } else { 200.0 };
    let fwd = heading_dir(h);
    let stbd = heading_dir(h + core::f64::consts::FRAC_PI_2);
    let r_sep = (0.75 * r).clamp(80.0, 500.0);
    let r_ali = 4.0 * r;
    let d_safe = 1.5 * r_sep;

    let mut steer = fwd;
    let mut speed = 1.0f64;

    // Cohesion toward the slot.
    let to_slot = slot_world - p;
    if to_slot.is_finite() {
        let d = to_slot.length();
        if d > 1.0 {
            steer += to_slot / d * (0.6 * (d / (4.0 * r)).min(1.0));
            let along = to_slot.dot(fwd);
            speed *= 1.0 + (along / (4.0 * r)).clamp(-0.5, 0.2);
        }
    }

    let mut align = DVec2::ZERO;
    let mut align_n = 0u32;
    let mut sep = DVec2::ZERO;
    let mut avoid = DVec2::ZERO;
    let mut slow = 0.0f64;
    for n in neighbours {
        let q = DVec2::new(n.pose.pos.x, n.pose.pos.z);
        if !q.is_finite() { continue; }
        let vq = { let w = DVec2::new(n.lin_vel.x, n.lin_vel.z); if w.is_finite() { w } else { DVec2::ZERO } };
        let rel = q - p;
        let d = rel.length();
        if d > r_ali.max(u * 90.0 + d_safe + vq.length() * 90.0) { continue; }
        // Separation.
        if d < r_sep {
            let away = if d > 1e-6 { -rel / d } else { -stbd };
            let k = 1.0 - d / r_sep;
            sep += away * (k * k);
        }
        // Alignment.
        if d <= r_ali && vq.length() > 0.5 {
            align += vq.normalize();
            align_n += 1;
        }
        // Collision avoidance at the closest point of approach.
        let w = vq - v;
        let w2 = w.length_squared();
        if w2 > 1e-6 {
            let tcpa = -rel.dot(w) / w2;
            if tcpa > 0.0 && tcpa < 90.0 {
                let miss = rel + w * tcpa;
                let dcpa = miss.length();
                if dcpa < d_safe {
                    let urgency = (1.0 - tcpa / 90.0) * (1.0 - dcpa / d_safe);
                    // Open the miss distance; a dead-on pass (or one already passing to port) turns starboard.
                    let open = if dcpa > 1.0 && miss.dot(stbd) < 0.0 { -miss / dcpa } else { stbd };
                    avoid += (open + stbd * 0.5).normalize_or_zero() * urgency;
                    if rel.dot(fwd) > 0.0 { slow = slow.max(urgency); }
                }
            }
        }
    }
    if align_n > 0 { steer += (align / align_n as f64).normalize_or_zero() * 0.3; }
    steer += sep * 2.0 + avoid * 3.0;
    speed *= 1.0 - 0.6 * slow.clamp(0.0, 1.0);
    if sep.length() > 0.0 {
        // Something inside separation range ahead: ease off.
        let ahead = sep.dot(fwd);
        if ahead < 0.0 { speed *= 1.0 - 0.5 * (-ahead).min(1.0); }
    }

    let mut dh = if steer.length() > 1e-9 { wrap_pi(bearing(steer) - h) } else { 0.0 };

    // Land and shoal water ahead override everything but keep the turn as small as they allow.
    let look = (u * 20.0).max(2.0 * r).min(10_000.0);
    if let Some(hit) = shoal_ahead(terrain, p, h, look) {
        let urgency = 1.0 - hit / look;
        let mut best: Option<f64> = None;
        'search: for step in 1..=6 {
            for sgn in [1.0, -1.0] {
                let off = sgn * step as f64 * core::f64::consts::PI / 12.0;
                if shoal_ahead(terrain, p, h + off, look).is_none() { best = Some(off); break 'search; }
            }
        }
        let turn = best.unwrap_or(core::f64::consts::FRAC_PI_2 * if dh < 0.0 { -1.0 } else { 1.0 });
        // Blend: the closer the shore, the more the clear heading wins.
        let w = (0.4 + 0.6 * urgency).clamp(0.0, 1.0);
        dh = dh * (1.0 - w) + turn * w;
        speed *= 1.0 - 0.7 * urgency;
    }

    let max = core::f64::consts::PI / 3.0;
    let dh = if dh.is_finite() { dh.clamp(-max, max) } else { 0.0 };
    let speed = if speed.is_finite() { speed.clamp(0.2, 1.2) } else { 1.0 };
    (dh, speed)
}

/// Water shallower than this counts as shoal for the flocking probe, metres.
const FLOCK_SHOAL_M: f64 = 4.0;

/// Distance along a heading at which the first shoal sample lies, if any (eight samples to `len`).
fn shoal_ahead(terrain: &Terrain, p: DVec2, h: f64, len: f64) -> Option<f64> {
    let d = heading_dir(h);
    (1..=8).map(|i| len * i as f64 / 8.0).find(|&s| {
        let q = p + d * s;
        let depth = terrain.depth_at(q.x, q.y);
        !depth.is_finite() || depth < FLOCK_SHOAL_M
    })
}

// ---------------------------------------------------------------------------------------------------------
// Constants (the spec fixes none of these; listed in the unit's decisions)
// ---------------------------------------------------------------------------------------------------------

const TICKS_PER_S: u64 = sim_core::time::TICK_HZ as u64;
const KN: f64 = sim_core::time::KN;
/// A hostile inside this many doctrine ranges of the leader means the group is engaging.
const ENGAGE_FACTOR: f64 = 1.3;
/// Contacts further than this from the group (and more than two doctrine ranges) are ignored unless hunting.
const INTEREST_M: f64 = 25_000.0;
/// Convoys scatter when a hostile comes this close.
const SCATTER_M: f64 = 15_000.0;
const SCATTER_S: u64 = 300;
/// Re-request a route when the goal moves this far (a moving contact: `HUNT_REPLAN_M`).
const REPLAN_M: f64 = 250.0;
const HUNT_REPLAN_M: f64 = 1_000.0;
/// Shortest gap between two requests of one group, and the back-off after an unreachable goal, s.
const MIN_REQUEST_S: u64 = 10;
const RETRY_S: u64 = 30;
/// The leader is off its route beyond this, metres.
const STRAY_M: f64 = 1_500.0;
/// Holding creep speed, knots.
const HOLD_KN: f64 = 1.0;
/// Circuit points around a guarded or searched point.
const CIRCUIT_POINTS: u32 = 6;
/// Dead reckoning limit for a contact and for a last-known mark, s.
const CONTACT_DR_S: f64 = 60.0;
const MARK_DR_S: f64 = 600.0;
/// A captain under fire this recently counts for the scatter rule, s.
const UNDER_FIRE_S: u64 = 30;

// ---------------------------------------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------------------------------------

/// What the group knows about one of its own hulls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Member {
    pub(crate) vessel: EntityId,
    pub(crate) pos: DVec2,
    pub(crate) heading: f64,
    /// Can steer (Active); a Disabled hull still gets a directive but never leads or holds a slot.
    pub(crate) active: bool,
    pub(crate) top_speed_mps: f64,
    pub(crate) turn_radius_m: f64,
    pub(crate) beam_m: f64,
    pub(crate) draft_m: f64,
    /// Doctrine range estimate from working weapons (as the captain derives it), metres; 0 = unarmed.
    pub(crate) engage_m: f64,
    pub(crate) ace: bool,
    pub(crate) under_fire: bool,
}

/// The charge of an escort group: (vessel, position, heading).
pub(crate) type Charge = (EntityId, DVec2, f64);

/// Everything besides members that one group update reads.
pub(crate) struct Env<'a> {
    pub(crate) picture: &'a SidePicture,
    pub(crate) terrain: &'a Terrain,
    pub(crate) mods: &'a DoctrineMods,
    pub(crate) now: u64,
    pub(crate) reaction_ticks: u64,
    pub(crate) last_known_s: f64,
    pub(crate) charge: Option<Charge>,
}

fn finite_pos(v: DVec3) -> Option<DVec2> { let p = DVec2::new(v.x, v.z); if p.is_finite() { Some(p) } else { None } }

fn reaction_ticks(ctx: &SimCtx<'_>) -> u64 {
    let r = ctx.difficulty.clamped().ai_reaction_s;
    (if r.is_finite() { r.clamp(0.0, 10.0) } else { 2.0 } * TICKS_PER_S as f64).round() as u64
}

pub(crate) fn member_view(m: EntityId, state: &GroupState, ships: &ShipTable, world: &World, now: u64) -> Option<Member> {
    let v = world.vessel(m)?;
    if v.side != state.side { return None; }
    // A hull that struck its colours is out of the group's war.
    if v.struck { return None; }
    let active = match v.status { VesselStatus::Active => true, VesselStatus::Disabled => false, _ => return None };
    let pos = finite_pos(v.kin.pose.pos)?;
    let heading = { let h = v.kin.pose.heading_rad(); if h.is_finite() { h } else { 0.0 } };
    let ship = ships.get(m);
    let turn = ship.map(|s| s.design.turn).unwrap_or_default();
    let pos_or = |x: f64, d: f64| if x.is_finite() && x > 0.0 { x } else { d };
    let mut gun = 0.0f64;
    let mut missile = 0.0f64;
    if let Some(s) = ship {
        for mt in &s.design.mounts {
            if mt.point_defence { continue; }
            let working = v.parts.get(mt.slot.index()).is_some_and(|p| p.level != DamageLevel::Destroyed && p.health > 0.05);
            let r = mt.effective_range_m;
            if !working || !r.is_finite() || r <= 0.0 { continue; }
            match mt.weapon {
                WeaponClass::Gun => gun = gun.max(r),
                WeaponClass::AntiShipMissile | WeaponClass::Torpedo => missile = missile.max(r),
                _ => {}
            }
        }
    }
    let engage_m = if gun > 0.0 { 0.7 * gun } else if missile > 0.0 { 0.85 * missile } else { 0.0 };
    let captain = ship.and_then(|s| s.captain.as_ref());
    let ace = state.ace.is_some() && captain.is_some_and(|c| c.ace == state.ace);
    let under_fire = captain.is_some_and(|c| c.mem.under_fire_tick.is_some_and(|t| now.saturating_sub(t) < UNDER_FIRE_S * TICKS_PER_S)
        || c.pending.iter().any(|p| p.kind == super::unit::PerceptKind::UnderFire));
    Some(Member {
        vessel: m,
        pos,
        heading,
        active,
        top_speed_mps: pos_or(turn.top_speed_mps, 0.0),
        turn_radius_m: pos_or(turn.turn_radius_m, 200.0),
        beam_m: pos_or(turn.beam_m, 6.0),
        draft_m: pos_or(turn.draft_m, 3.0),
        engage_m,
        ace,
        under_fire,
    })
}

/// The first active own-side hull (ascending entity) carrying the escort tag that is not a group member.
fn find_charge(tag: &str, state: &GroupState, ships: &ShipTable, world: &World) -> Option<Charge> {
    if tag.is_empty() { return None; }
    ships.ships.iter().filter(|s| s.tag == tag && s.side == state.side && state.members.binary_search(&s.vessel).is_err()).find_map(|s| {
        let v = world.vessel(s.vessel)?;
        if v.side != state.side || v.status != VesselStatus::Active || v.struck { return None; }
        let h = v.kin.pose.heading_rad();
        Some((s.vessel, finite_pos(v.kin.pose.pos)?, if h.is_finite() { h } else { 0.0 }))
    })
}

/// An opposing contact as the group sees it now.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GContact { id: TrackId, pos: DVec2, vel: DVec2, hostile: bool, fired_on_us: bool }

fn dead_reckon(t: &Track, now: u64, limit_s: f64) -> Option<(DVec2, DVec2)> {
    let p = finite_pos(t.pos)?;
    let vel = finite_pos(t.vel).unwrap_or(DVec2::ZERO);
    let dt = (now.saturating_sub(t.last_update_tick) as f64 / TICKS_PER_S as f64).min(limit_s);
    let q = p + vel * dt;
    Some((if q.is_finite() { q } else { p }, vel))
}

fn opposing(t: &Track) -> bool { matches!(t.stance, Stance::Suspect | Stance::Hostile) && matches!(t.domain, Domain::Surface | Domain::Subsurface) }

/// Live opposing surface and subsurface contacts that have existed for the reaction time.
fn contacts(env: &Env<'_>) -> Vec<GContact> {
    env.picture.tracks.iter().filter(|t| t.is_live() && opposing(t) && t.created_tick.saturating_add(env.reaction_ticks) <= env.now).filter_map(|t| {
        let (pos, vel) = dead_reckon(t, env.now, CONTACT_DR_S)?;
        Some(GContact { id: t.id, pos, vel, hostile: t.stance == Stance::Hostile, fired_on_us: t.fired_on_us })
    }).collect()
}

/// Newest last-known mark of a dropped opposing contact, dead-reckoned.
fn last_known(env: &Env<'_>) -> Option<DVec2> {
    let lk = if env.last_known_s.is_finite() { env.last_known_s.max(0.0) } else { 0.0 };
    env.picture.tracks.iter()
        .filter(|t| !t.is_live() && opposing(t) && (env.now.saturating_sub(t.last_update_tick) as f64 / TICKS_PER_S as f64) <= lk)
        .max_by(|a, b| a.last_update_tick.cmp(&b.last_update_tick).then(b.id.cmp(&a.id)))
        .and_then(|t| dead_reckon(t, env.now, MARK_DR_S).map(|x| x.0))
}

// ---------------------------------------------------------------------------------------------------------
// The update
// ---------------------------------------------------------------------------------------------------------

/// Hull numbers the group's routes are planned for: (beam, draft, turn radius), quantised up.
fn route_hull(members: &[Member]) -> (f64, f64, f64) {
    let mut beam = 2.0f64;
    let mut draft = 1.0f64;
    let mut radius = 50.0f64;
    for m in members.iter().filter(|m| m.active) {
        beam = beam.max(m.beam_m.min(100.0));
        draft = draft.max(m.draft_m.min(50.0));
        radius = radius.max(m.turn_radius_m.min(5_000.0));
    }
    ((beam / 2.0).ceil() * 2.0, (draft * 2.0).ceil() / 2.0, radius)
}

/// The group's hull numbers for planning (for `sys-ai` to pre-build clearance masks).
pub(crate) fn mask_numbers(members: &[Member]) -> (f64, f64) { let (b, d, _) = route_hull(members); (b, d) }

pub(crate) fn update_core(state: &mut GroupState, members: &[Member], env: &Env<'_>, planner: &mut PathPlanner) -> Vec<(EntityId, Directive)> {
    let now = env.now;
    let active: Vec<&Member> = members.iter().filter(|m| m.active).collect();
    state.mem.peak = state.mem.peak.max(active.len().min(u16::MAX as usize) as u16);

    // Orders changed (commander tasking or stand-down): restart the legs.
    if state.mem.orders_seen != state.orders {
        state.mem.orders_seen = state.orders.clone();
        state.mem.leg = u32::MAX;
        state.mem.hold = None;
        state.mem.retry_tick = 0;
    }

    // Leader.
    let old_leader = state.leader;
    let leader = active.iter().find(|m| m.ace).or_else(|| active.first()).copied();
    state.leader = leader.map(|m| m.vessel);
    if old_leader != state.leader {
        if let Some(o) = old_leader { let _ = planner.take(o); }
        state.mem.waiting = false;
    }
    let Some(lead) = leader else {
        state.mode = GroupMode::Standing;
        state.contact = TrackId::NONE;
        // Nobody can steer: disabled hulls fight where they lie.
        return members.iter().map(|m| (m.vessel, Directive { mode: GroupMode::Standing, leader: None, weapons_tight: false, ..Default::default() })).collect();
    };

    // Doctrine range of the group, from its best-armed hull.
    let engage = group_engage_range(&active, env.mods);

    // Contact.
    let cs = contacts(env);
    let hunting = matches!(state.orders, Some(GroupOrders::Hunt));
    let centre = env.charge.map_or(lead.pos, |c| c.1);
    let interest = if hunting { f64::INFINITY } else { INTEREST_M.max(2.0 * engage) };
    let score = |c: &GContact| {
        let mut s = if c.hostile { 2.0 } else { 1.0 };
        if c.fired_on_us { s *= 1.5; }
        if c.id == state.contact { s *= 1.3; }
        s / (1.0 + c.pos.distance(centre) / 10_000.0)
    };
    // A Hostile always outranks a Suspect (an unidentified merchant close by must not mute the fight); the
    // score orders contacts of one stance.
    let contact = cs.iter().filter(|c| c.pos.distance(centre) <= interest)
        .max_by(|a, b| a.hostile.cmp(&b.hostile).then(score(a).total_cmp(&score(b))).then(b.id.cmp(&a.id))).copied();
    state.contact = contact.map_or(TrackId::NONE, |c| c.id);
    let hostile_range = contact.filter(|c| c.hostile).map(|c| c.pos.distance(centre));

    // Mode.
    let convoy = matches!(state.kind, Some(FleetKind::Convoy | FleetKind::DecoyConvoy));
    let holds_ground = matches!(state.kind, Some(FleetKind::Garrison)) || active.iter().any(|m| m.ace) || env.mods.flags.contains("hold_ground");
    if !state.mem.withdraw_latched && state.mem.engaged && !convoy && !holds_ground && state.mem.peak >= 2 && active.len() * 2 <= state.mem.peak as usize {
        state.mem.withdraw_latched = true;
    }
    // An anchored group (a prize at its berth, a defector waiting for the rendezvous) holds its anchorage
    // until its orders change: it never scatters.
    let anchored = matches!(state.orders, Some(GroupOrders::Anchor));
    if convoy && !anchored && (hostile_range.is_some_and(|d| d <= SCATTER_M) || active.iter().any(|m| m.under_fire)) {
        state.mem.scatter_until_tick = now + SCATTER_S * TICKS_PER_S;
    }
    let stays = matches!(state.orders, Some(GroupOrders::Anchor | GroupOrders::Transit { .. }));
    let mode = if matches!(state.orders, Some(GroupOrders::Flee { .. })) || state.mem.withdraw_latched {
        GroupMode::Withdrawing
    } else if convoy {
        if now < state.mem.scatter_until_tick { GroupMode::Scattered } else { GroupMode::Standing }
    } else if let Some(d) = hostile_range {
        if matches!(state.orders, Some(GroupOrders::Escort { .. })) && env.charge.is_some() {
            if d <= engage * ENGAGE_FACTOR + screen_radius(&active) { GroupMode::Engaging } else { GroupMode::Screening }
        } else if d <= engage * ENGAGE_FACTOR {
            GroupMode::Engaging
        } else if stays {
            GroupMode::Standing
        } else {
            GroupMode::Closing
        }
    } else if matches!(state.orders, Some(GroupOrders::Escort { .. })) && env.charge.is_some() {
        GroupMode::Screening
    } else if (contact.is_some() && !stays) || matches!(state.orders, Some(GroupOrders::Search { .. })) {
        GroupMode::Investigating
    } else {
        GroupMode::Standing
    };
    if matches!(mode, GroupMode::Engaging | GroupMode::Closing) { state.mem.engaged = true; }
    state.mode = mode;

    // Goal and route.
    let (beam, draft, radius) = route_hull(members);
    let chase = contact.filter(|_| matches!(mode, GroupMode::Closing | GroupMode::Engaging | GroupMode::Investigating) && env.charge.is_none() && !stays);
    let (goal, cyclic) = if mode == GroupMode::Withdrawing {
        (withdraw_goal(state, lead, contact.map(|c| c.pos), env.terrain), false)
    } else if let Some(c) = chase {
        (Some(intercept(lead.pos, lead.top_speed_mps, c)), false)
    } else {
        let cyclic = matches!(state.orders, Some(GroupOrders::Patrol { .. } | GroupOrders::Guard { .. } | GroupOrders::AirPatrol { .. } | GroupOrders::Search { .. }));
        (orders_goal(state, lead, radius, env), cyclic)
    };
    let moving_goal = chase.is_some() || hunting;
    let mut route = route_for(state, lead, goal, moving_goal, cyclic, (beam, draft, radius), planner, now);
    let holding = goal.is_none() || route.is_empty();
    let charge_led = env.charge.is_some() && matches!(state.orders, Some(GroupOrders::Escort { .. }));
    if holding && !charge_led && mode != GroupMode::Withdrawing {
        // Hold: a one-point route to the hold point (or to where the leader stopped making way) replaces any
        // old route, so the leader creeps there and stands on at steerage way instead of finishing a stale
        // route.
        let at = match state.mem.hold.filter(|_| goal.is_none()) { Some(h) => h, None => *state.mem.idle_at.get_or_insert(lead.pos) };
        route = vec![at];
    } else {
        state.mem.idle_at = None;
    }

    // Speed.
    let slowest = active.iter().map(|m| m.top_speed_mps).filter(|s| *s > 0.0).fold(f64::INFINITY, f64::min);
    let factor = match mode {
        GroupMode::Withdrawing => 0.95,
        GroupMode::Closing => 0.85,
        GroupMode::Engaging => 0.85,
        GroupMode::Investigating => 0.7,
        _ if convoy => 0.75,
        _ => 0.6,
    };
    let speed_kn = if !slowest.is_finite() { 0.0 } else if holding && !charge_led { HOLD_KN } else { slowest * factor / KN };
    let speed_kn = (speed_kn * 10.0).round() / 10.0;

    // Formation.
    let spacing = (2.0 * active.iter().map(|m| m.turn_radius_m).fold(0.0, f64::max)).clamp(300.0, 3_000.0);
    let escort = env.charge.filter(|_| matches!(state.orders, Some(GroupOrders::Escort { .. })));
    let slots = assign_slots(state, &active, escort, spacing);
    let formation_leader = match (mode, escort) {
        (GroupMode::Withdrawing | GroupMode::Scattered, _) => None,
        (_, Some(c)) => Some(c.0),
        _ => Some(lead.vessel),
    };

    let hostile_contact = contact.is_some_and(|c| c.hostile);
    let weapons_tight = match mode { GroupMode::Withdrawing | GroupMode::Scattered => false, _ => !hostile_contact };
    let emcon = if mode == GroupMode::Closing && env.mods.flags.contains("silent_approach") { Some(Emcon::Silent) } else { None };
    let base = Directive {
        mode,
        slot: DVec2::ZERO,
        leader: formation_leader,
        route,
        speed_kn,
        contact: state.contact,
        keep_range_m: None,
        weapons_tight,
        emcon,
    };
    // A scattering convoy fans out: each hull runs the route shifted sideways by its place in the line,
    // instead of every hull steering for the same points (they rammed each other).
    let fan = |k: usize, n: usize, route: &[DVec2]| -> Vec<DVec2> {
        let ahead = route.first().map_or(DVec2::ZERO, |w| *w - lead.pos);
        let side = if ahead.length() > 1.0 { ahead.normalize().perp() } else { DVec2::X };
        let off = side * ((k as f64 - (n as f64 - 1.0) * 0.5) * SCATTER_FAN_FACTOR * spacing);
        route.iter().map(|w| *w + off).collect()
    };
    members.iter().enumerate().map(|(k, m)| {
        let mut d = base.clone();
        d.slot = slots.iter().find(|(e, _)| *e == m.vessel).map_or(DVec2::ZERO, |s| s.1);
        if formation_leader.is_none() { d.slot = DVec2::ZERO; }
        if mode == GroupMode::Scattered && members.len() > 1 { d.route = fan(k, members.len(), &base.route); }
        (m.vessel, d)
    }).collect()
}

fn group_engage_range(active: &[&Member], mods: &DoctrineMods) -> f64 {
    let weapon = active.iter().map(|m| m.engage_m).fold(0.0, f64::max);
    let weapon = if weapon > 0.0 { weapon } else { 4_000.0 };
    let f = if mods.engagement_range_factor.is_finite() { mods.engagement_range_factor.clamp(0.25, 4.0) } else { 1.0 };
    let mut r = weapon * f;
    if mods.flags.contains("close_in") { r *= 0.7; }
    if mods.flags.contains("stand_off") { r *= 1.3; }
    r.clamp(800.0, 100_000.0)
}

/// How far escorts stand from their charge (as the escort tree screens).
fn screen_radius(active: &[&Member]) -> f64 { (0.5 * active.iter().map(|m| m.engage_m / 0.7).fold(0.0, f64::max)).clamp(1_500.0, 4_000.0) }

/// Lead point for running at a contact: where it will be when the leader gets there (at most 5 minutes).
fn intercept(from: DVec2, speed: f64, c: GContact) -> DVec2 {
    let t = (c.pos.distance(from) / speed.max(3.0)).min(300.0);
    let p = c.pos + c.vel * t;
    if p.is_finite() { p } else { c.pos }
}

fn marker_pos(terrain: &Terrain, name: &str) -> Option<DVec2> {
    terrain.marker(name).map(|m| DVec2::new(m.pos_m[0], m.pos_m[1])).filter(|p| p.is_finite())
}

/// Circuit point `k` around a centre (compass bearings 0, 60, ... degrees).
fn circuit_point(centre: DVec2, radius: f64, k: u32) -> DVec2 {
    centre + heading_dir((k % CIRCUIT_POINTS) as f64 * core::f64::consts::TAU / CIRCUIT_POINTS as f64) * radius
}

/// The circuit leg nearest a position.
fn nearest_leg(centre: DVec2, radius: f64, pos: DVec2) -> u32 {
    (0..CIRCUIT_POINTS).min_by(|a, b| circuit_point(centre, radius, *a).distance(pos).total_cmp(&circuit_point(centre, radius, *b).distance(pos)).then(a.cmp(b))).unwrap_or(0)
}

fn arrive_radius(radius: f64) -> f64 { (2.0 * radius).max(400.0) }

/// Goal for the orders (no contact being prosecuted). Advances patrol legs on arrival; `None` holds where
/// the group is (and plans a return when it drifts).
fn orders_goal(state: &mut GroupState, lead: &Member, radius: f64, env: &Env<'_>) -> Option<DVec2> {
    let arrive = arrive_radius(radius);
    let hold_r = (3.0 * radius).max(600.0);
    let terrain = env.terrain;
    let hold_at = |state: &mut GroupState, p: DVec2| -> Option<DVec2> {
        let h = *state.mem.hold.get_or_insert(p);
        if lead.pos.distance(h) > hold_r { Some(h) } else { None }
    };
    let circuit = |state: &mut GroupState, centre: DVec2, r: f64, first_centre: bool| -> Option<DVec2> {
        let r = r.clamp((3.0 * radius).max(1_000.0), 30_000.0);
        if state.mem.leg == u32::MAX { state.mem.leg = if first_centre { u32::MAX - 1 } else { nearest_leg(centre, r, lead.pos) }; }
        if state.mem.leg == u32::MAX - 1 {
            if lead.pos.distance(centre) > arrive { return Some(centre); }
            state.mem.leg = nearest_leg(centre, r, lead.pos);
        }
        let mut g = circuit_point(centre, r, state.mem.leg);
        if lead.pos.distance(g) <= arrive { state.mem.leg = (state.mem.leg + 1) % CIRCUIT_POINTS; g = circuit_point(centre, r, state.mem.leg); }
        Some(g)
    };
    match state.orders.clone() {
        Some(GroupOrders::Patrol { markers }) => {
            let pts: Vec<DVec2> = markers.iter().filter_map(|m| marker_pos(terrain, m)).collect();
            match pts.len() {
                0 => hold_at(state, lead.pos),
                1 => circuit(state, pts[0], 3_000.0, true),
                n => {
                    if state.mem.leg == u32::MAX {
                        // Start at the nearest marker.
                        state.mem.leg = (0..n).min_by(|a, b| pts[*a].distance(lead.pos).total_cmp(&pts[*b].distance(lead.pos)).then(a.cmp(b))).unwrap_or(0) as u32;
                    }
                    let mut k = state.mem.leg as usize % n;
                    if lead.pos.distance(pts[k]) <= arrive { k = (k + 1) % n; }
                    state.mem.leg = k as u32;
                    Some(pts[k])
                }
            }
        }
        Some(GroupOrders::Transit { to, .. }) | Some(GroupOrders::Flee { exit: to }) => match marker_pos(terrain, &to) {
            Some(p) if state.mem.hold.is_none() && lead.pos.distance(p) > arrive => Some(p),
            Some(p) => hold_at(state, p),
            None => hold_at(state, lead.pos),
        },
        Some(GroupOrders::Guard { marker, radius_m }) | Some(GroupOrders::AirPatrol { marker, radius_m }) => match marker_pos(terrain, &marker) {
            Some(c) if radius_m.is_finite() && radius_m >= 1_000.0 => circuit(state, c, radius_m, false),
            Some(c) => hold_at(state, c),
            None => hold_at(state, lead.pos),
        },
        Some(GroupOrders::Search { at, radius_m }) if at.is_finite() => circuit(state, at, if radius_m.is_finite() { radius_m } else { 3_000.0 }, true),
        Some(GroupOrders::Hunt) => match last_known(env) {
            Some(p) => Some(p),
            None => hold_at(state, lead.pos),
        },
        Some(GroupOrders::Escort { .. }) if env.charge.is_some() => None,
        _ => hold_at(state, lead.pos),
    }
}

/// Where a breaking-off group runs: its flee marker, else the nearest exit or Combine spawn marker that does
/// not lie toward the threat, else 20 km straight away from the threat inside the map; `None` holds.
fn withdraw_goal(state: &mut GroupState, lead: &Member, threat: Option<DVec2>, terrain: &Terrain) -> Option<DVec2> {
    if let Some(GroupOrders::Flee { exit }) = &state.orders {
        if let Some(p) = marker_pos(terrain, exit) { return Some(p); }
    }
    let away = threat.map(|t| (lead.pos - t).normalize_or_zero()).filter(|d| d.length() > 0.5);
    let ok = |p: DVec2| away.is_none_or(|a| (p - lead.pos).normalize_or_zero().dot(a) > -0.3);
    let best = terrain.map.markers.iter().filter(|m| matches!(m.kind, MarkerKind::Exit | MarkerKind::EnemySpawn))
        .map(|m| DVec2::new(m.pos_m[0], m.pos_m[1])).filter(|p| p.is_finite() && ok(*p) && p.distance(lead.pos) > 1_000.0)
        .min_by(|a, b| a.distance(lead.pos).total_cmp(&b.distance(lead.pos)).then(a.x.total_cmp(&b.x)).then(a.y.total_cmp(&b.y)));
    if best.is_some() { return best; }
    let a = away?;
    let half = DVec2::new(terrain.map.size_m[0], terrain.map.size_m[1]) * 0.475;
    let p = lead.pos + a * 20_000.0;
    let p = if half.is_finite() && half.x > 0.0 && half.y > 0.0 { p.clamp(-half, half) } else { p };
    Some(p)
}

/// Keep the leader's route current for `goal`; returns the waypoints to hand out. `moving` widens the
/// replanning hysteresis for a contact; `cyclic` marks a patrol or circuit leg, which is skipped when the
/// planner finds it unreachable.
#[allow(clippy::too_many_arguments)]
fn route_for(state: &mut GroupState, lead: &Member, goal: Option<DVec2>, moving: bool, cyclic: bool, hull: (f64, f64, f64), planner: &mut PathPlanner, now: u64) -> Vec<DVec2> {
    // Collect a finished plan.
    if state.mem.waiting {
        if let Some(r) = planner.take(lead.vessel) {
            state.mem.waiting = false;
            state.mem.route_goal = state.mem.req_goal;
            if !r.valid {
                state.mem.retry_tick = now + RETRY_S * TICKS_PER_S;
                if state.mem.req_cyclic {
                    state.mem.leg = match state.mem.leg { u32::MAX => u32::MAX, x if x == u32::MAX - 1 => 0, x => x.wrapping_add(1) % (u32::MAX - 1) };
                }
            }
            state.route = r;
        }
    }
    let Some(goal) = goal.filter(|g| g.is_finite()) else {
        // Nothing to run at: forget the plan, so a later goal (a return to the hold point) is planned afresh
        // from where the leader is rather than handing back the old route.
        state.mem.req_goal = None;
        state.mem.route_goal = None;
        return Vec::new();
    };
    let hyst = if moving { HUNT_REPLAN_M } else { REPLAN_M };
    let near = |g: Option<DVec2>| g.is_some_and(|g| g.distance(goal) <= hyst);
    let planned_here = near(state.mem.req_goal) && state.mem.req_leader == lead.vessel;
    let since = now.saturating_sub(state.mem.req_tick);
    let want = if !planned_here {
        // A new goal; a moving contact is re-planned at most every `MIN_REQUEST_S`.
        !moving || state.mem.req_goal.is_none() || since >= MIN_REQUEST_S * TICKS_PER_S
    } else if state.mem.waiting {
        false
    } else if !state.route.valid {
        now >= state.mem.retry_tick && since >= MIN_REQUEST_S * TICKS_PER_S
    } else {
        off_route(&state.route.waypoints, lead.pos) > STRAY_M && since >= 2 * MIN_REQUEST_S * TICKS_PER_S
    };
    if want {
        let (beam, draft, radius) = hull;
        planner.request(PathRequest { id: 0, requester: lead.vessel, from: lead.pos, from_heading_rad: lead.heading, to: goal, to_heading_rad: None, beam_m: beam, draft_m: draft, turn_radius_m: radius, issued_tick: now });
        state.mem.req_goal = Some(goal);
        state.mem.req_leader = lead.vessel;
        state.mem.req_tick = now;
        state.mem.req_cyclic = cyclic;
        state.mem.waiting = true;
    }
    if near(state.mem.route_goal) && state.route.valid && !state.route.waypoints.is_empty() {
        state.route.waypoints.iter().copied().filter(|w| w.is_finite()).collect()
    } else if near(state.mem.route_goal) && !state.route.valid && state.mem.req_leader == lead.vessel {
        // Unreachable from here: hold rather than run at the shore.
        Vec::new()
    } else {
        // Not planned yet: steer straight for it while the planner works.
        vec![goal]
    }
}

/// Distance from a point to a polyline (a single point counts as its own segment).
fn off_route(w: &[DVec2], p: DVec2) -> f64 {
    match w.len() {
        0 => f64::INFINITY,
        1 => w[0].distance(p),
        _ => w.windows(2).map(|s| {
            let ab = s[1] - s[0];
            let l2 = ab.length_squared();
            let t = if l2 > 0.0 { ((p - s[0]).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
            (s[0] + ab * t).distance(p)
        }).fold(f64::INFINITY, f64::min),
    }
}

/// Persistent slot per member. With an escort charge every member takes a slot around it (slots 1..=n);
/// otherwise the leader holds slot 0. Returns (member, offset) for active members.
fn assign_slots(state: &mut GroupState, active: &[&Member], charge: Option<Charge>, spacing: f64) -> Vec<(EntityId, DVec2)> {
    let escort = charge.is_some();
    let lead = if escort { None } else { state.leader };
    let n = active.len() + usize::from(escort);
    let offsets = formation_slots(state.formation, n, spacing);
    let wanted: Vec<EntityId> = active.iter().map(|m| m.vessel).collect();
    let same_set = state.mem.slot_count as usize == n && state.mem.slots.len() == wanted.len() && state.mem.slots.iter().all(|(e, _)| wanted.contains(e))
        && lead.is_none_or(|l| state.mem.slots.iter().any(|&(e, k)| e == l && k == 0));
    if !same_set {
        let mut taken = vec![false; n];
        let mut slots: Vec<(EntityId, u16)> = Vec::with_capacity(wanted.len());
        if let Some(l) = lead { slots.push((l, 0)); taken[0] = true; }
        if escort && n > 0 { taken[0] = true; }
        // Survivors keep their slot index when it still exists.
        for &(e, k) in &state.mem.slots {
            if Some(e) == lead || !wanted.contains(&e) { continue; }
            let k = k as usize;
            if k < n && !taken[k] { taken[k] = true; slots.push((e, k as u16)); }
        }
        // The rest fill free slots, nearest pair first (ties by slot then entity).
        let lead_pos = charge.map(|c| (c.1, c.2)).or_else(|| lead.and_then(|l| active.iter().find(|m| m.vessel == l)).map(|m| (m.pos, m.heading)));
        let mut free: Vec<usize> = (0..n).filter(|k| !taken[*k]).collect();
        let mut rest: Vec<&Member> = active.iter().copied().filter(|m| !slots.iter().any(|s| s.0 == m.vessel)).collect();
        while !rest.is_empty() && !free.is_empty() {
            let mut best: Option<(f64, usize, usize)> = None;
            for (ri, m) in rest.iter().enumerate() {
                for (fi, &k) in free.iter().enumerate() {
                    let d = lead_pos.map_or(k as f64, |(lp, lh)| world_of(lp, lh, offsets[k]).distance(m.pos));
                    let better = best.is_none_or(|(bd, bri, bfi)| d < bd || (d == bd && (free[fi], rest[ri].vessel) < (free[bfi], rest[bri].vessel)));
                    if better { best = Some((d, ri, fi)); }
                }
            }
            let Some((_, ri, fi)) = best else { break };
            slots.push((rest.remove(ri).vessel, free.remove(fi) as u16));
        }
        slots.sort_by_key(|s| s.0);
        state.mem.slots = slots;
        state.mem.slot_count = n.min(u16::MAX as usize) as u16;
    }
    state.mem.slots.iter().filter_map(|&(e, k)| offsets.get(k as usize).map(|o| (e, *o))).collect()
}

/// World position of a slot offset (x ahead, z starboard) from a leader pose.
fn world_of(leader: DVec2, heading: f64, off: DVec2) -> DVec2 {
    leader + heading_dir(heading) * off.x + heading_dir(heading + core::f64::consts::FRAC_PI_2) * off.y
}

// ---------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::commander::tests::track;
    use content::campaign::{Island, MapDef, Marker};
    use sim_core::math::Pose;

    fn kin(x: f64, z: f64, heading: f64, speed: f64) -> Kinematics {
        let rot = DQuat::from_rotation_y(core::f64::consts::FRAC_PI_2 - heading);
        let v = heading_dir(heading) * speed;
        Kinematics { pose: Pose { pos: DVec3::new(x, 0.0, z), rot }, lin_vel: DVec3::new(v.x, 0.0, v.y), ang_vel: DVec3::ZERO }
    }
    fn open() -> Terrain { Terrain::new(MapDef { size_m: [40_000.0, 40_000.0], depth_m: 200.0, ..Default::default() }) }
    fn marker(name: &str, x: f64, z: f64) -> Marker { Marker { kind: MarkerKind::Objective, name: name.into(), pos_m: [x, z], heading_deg: 0.0 } }
    fn map_with(markers: Vec<Marker>) -> Terrain { Terrain::new(MapDef { size_m: [40_000.0, 40_000.0], depth_m: 200.0, markers, ..Default::default() }) }
    fn member(id: u64, x: f64, z: f64) -> Member {
        Member { vessel: EntityId::from_u64(id), pos: DVec2::new(x, z), heading: 0.0, active: true, top_speed_mps: 15.0, turn_radius_m: 250.0, beam_m: 8.0, draft_m: 3.0, engage_m: 7_000.0, ace: false, under_fire: false }
    }
    fn init(orders: GroupOrders, kind: FleetKind) -> GroupInit { GroupInit { id: GroupId(3), side: SideId(1), kind, formation: Formation::Wedge, orders, ace: None, theatre_group: None, tag: "tg".into() } }
    fn pic(tracks: Vec<Track>) -> SidePicture { SidePicture { side: SideId(1), tracks, next_id: 100 } }
    fn env<'a>(picture: &'a SidePicture, terrain: &'a Terrain, mods: &'a DoctrineMods, now: u64) -> Env<'a> {
        Env { picture, terrain, mods, now, reaction_ticks: 120, last_known_s: 300.0, charge: None }
    }
    fn run(g: &mut GroupState, ms: &[Member], e: &Env<'_>, planner: &mut PathPlanner) -> Vec<(EntityId, Directive)> {
        // Two updates with a planner step between, as the 2 Hz cadence gives.
        let _ = update_core(g, ms, e, planner);
        planner.step(2_000, e.now);
        update_core(g, ms, e, planner)
    }

    #[test]
    fn new_group_copies_config() {
        let g = new_group(&init(GroupOrders::Hunt, FleetKind::HunterKiller), vec![EntityId::from_u64(9), EntityId::from_u64(4), EntityId::from_u64(9), EntityId::NONE]);
        assert_eq!(g.members, vec![EntityId::from_u64(4), EntityId::from_u64(9)]);
        assert_eq!(g.leader, Some(EntityId::from_u64(4)));
        assert_eq!((g.id, g.side, g.kind, g.formation, g.orders.clone()), (GroupId(3), SideId(1), Some(FleetKind::HunterKiller), Formation::Wedge, Some(GroupOrders::Hunt)));
        assert_eq!(g.tag, "tg");
        assert_eq!(new_group(&init(GroupOrders::Flee { exit: "x".into() }, FleetKind::Convoy), vec![]).mode, GroupMode::Withdrawing);
    }

    #[test]
    fn formation_shapes() {
        for f in [Formation::Column, Formation::LineAbreast, Formation::Wedge, Formation::Screen, Formation::Box] {
            assert!(formation_slots(f, 0, 500.0).is_empty());
            for n in [1usize, 2, 5, 9, 17] {
                let s = formation_slots(f, n, 500.0);
                assert_eq!(s.len(), n);
                assert_eq!(s[0], DVec2::ZERO, "{f:?}");
                for i in 0..n { for j in 0..i { assert!(s[i].distance(s[j]) > 100.0, "{f:?} n {n} slots {i} {j}"); } }
            }
        }
        assert_eq!(formation_slots(Formation::Column, 3, 400.0), vec![DVec2::ZERO, DVec2::new(-400.0, 0.0), DVec2::new(-800.0, 0.0)]);
        assert_eq!(formation_slots(Formation::LineAbreast, 3, 400.0), vec![DVec2::ZERO, DVec2::new(0.0, 400.0), DVec2::new(0.0, -400.0)]);
        assert_eq!(formation_slots(Formation::Wedge, 3, 400.0), vec![DVec2::ZERO, DVec2::new(-400.0, 400.0), DVec2::new(-400.0, -400.0)]);
        let sc = formation_slots(Formation::Screen, 4, 400.0);
        assert!(sc[1].distance(DVec2::new(600.0, 0.0)) < 1e-9 && (sc[2].length() - 600.0).abs() < 1e-9 && sc[2].x > 0.0 && sc[2].y > 0.0);
        let bx = formation_slots(Formation::Box, 4, 400.0);
        assert_eq!(bx, vec![DVec2::ZERO, DVec2::new(0.0, 400.0), DVec2::new(-400.0, 0.0), DVec2::new(-400.0, 400.0)]);
        assert_eq!(formation_slots(Formation::Column, 2, f64::NAN)[1], DVec2::new(-500.0, 0.0));
    }

    #[test]
    fn patrol_cycles_markers_in_formation() {
        let t = map_with(vec![marker("a", 0.0, -10_000.0), marker("b", 10_000.0, 0.0)]);
        let mods = DoctrineMods::default();
        let p = pic(vec![]);
        let mut g = new_group(&init(GroupOrders::Patrol { markers: vec!["a".into(), "b".into(), "missing".into()] }, FleetKind::Patrol), vec![EntityId::from_u64(1), EntityId::from_u64(2), EntityId::from_u64(3)]);
        let ms = [member(1, 0.0, 0.0), member(2, -300.0, 300.0), member(3, 300.0, 300.0)];
        let mut planner = PathPlanner::default();
        let d = run(&mut g, &ms, &env(&p, &t, &mods, 600), &mut planner);
        assert_eq!(d.len(), 3);
        assert_eq!(g.leader, Some(EntityId::from_u64(1)));
        assert_eq!(g.mode, GroupMode::Standing);
        // Nearest marker first, a straight route on a planner with no grid.
        assert_eq!(d[0].1.route.last().copied(), Some(DVec2::new(0.0, -10_000.0)));
        assert!(d.iter().all(|x| x.1.leader == Some(EntityId::from_u64(1)) && x.1.route == d[0].1.route && x.1.weapons_tight));
        assert_eq!(d[0].1.slot, DVec2::ZERO);
        let others: Vec<DVec2> = d[1..].iter().map(|x| x.1.slot).collect();
        assert!(others.contains(&DVec2::new(-500.0, 500.0)) && others.contains(&DVec2::new(-500.0, -500.0)), "{others:?}");
        // Patrol speed: 0.6 of the slowest top speed.
        assert!((d[0].1.speed_kn - (15.0 * 0.6 / KN * 10.0).round() / 10.0).abs() < 1e-9);
        // Arrive at a: next leg is b.
        let ms2 = [member(1, 0.0, -9_900.0), member(2, -300.0, -9_600.0), member(3, 300.0, -9_600.0)];
        let d2 = run(&mut g, &ms2, &env(&p, &t, &mods, 1_800), &mut planner);
        assert_eq!(d2[0].1.route.last().copied(), Some(DVec2::new(10_000.0, 0.0)));
        // Slots persist across updates.
        assert_eq!(d2[1].1.slot, d[1].1.slot);
    }

    #[test]
    fn hostile_contact_closes_then_engages() {
        let t = open();
        let mods = DoctrineMods::default();
        let mut g = new_group(&init(GroupOrders::Anchor, FleetKind::TaskGroup), vec![EntityId::from_u64(1), EntityId::from_u64(2)]);
        g.orders = Some(GroupOrders::Hunt);
        let ms = [member(1, 0.0, 0.0), member(2, -500.0, 500.0)];
        let mut planner = PathPlanner::default();
        let far = pic(vec![track(7, DVec2::new(0.0, -20_000.0), Stance::Hostile, Domain::Surface)]);
        let d = run(&mut g, &ms, &env(&far, &t, &mods, 600), &mut planner);
        assert_eq!((g.mode, g.contact), (GroupMode::Closing, TrackId(7)));
        assert!(d.iter().all(|x| x.1.contact == TrackId(7) && !x.1.weapons_tight && x.1.mode == GroupMode::Closing));
        assert!(d[0].1.route.last().unwrap().distance(DVec2::new(0.0, -20_000.0)) < 1.0);
        let near = pic(vec![track(7, DVec2::new(0.0, -6_000.0), Stance::Hostile, Domain::Surface)]);
        run(&mut g, &ms, &env(&near, &t, &mods, 1_200), &mut planner);
        assert_eq!(g.mode, GroupMode::Engaging);
        // Suspect only: investigate, weapons tight.
        let mut g2 = new_group(&init(GroupOrders::Patrol { markers: vec![] }, FleetKind::Patrol), vec![EntityId::from_u64(1)]);
        let sus = pic(vec![track(8, DVec2::new(3_000.0, 0.0), Stance::Suspect, Domain::Surface)]);
        let d = run(&mut g2, &ms[..1], &env(&sus, &t, &mods, 600), &mut planner);
        assert_eq!((g2.mode, d[0].1.weapons_tight), (GroupMode::Investigating, true));
        // A contact younger than the reaction time is not yet seen.
        let mut young = track(9, DVec2::new(3_000.0, 0.0), Stance::Hostile, Domain::Surface);
        young.created_tick = 590;
        let mut g3 = new_group(&init(GroupOrders::Patrol { markers: vec![] }, FleetKind::Patrol), vec![EntityId::from_u64(1)]);
        run(&mut g3, &ms[..1], &env(&pic(vec![young]), &t, &mods, 600), &mut planner);
        assert_eq!((g3.mode, g3.contact), (GroupMode::Standing, TrackId::NONE));
    }

    #[test]
    fn hostile_outranks_a_close_suspect() {
        let t = open();
        let mods = DoctrineMods::default();
        let mut g = new_group(&init(GroupOrders::Patrol { markers: vec![] }, FleetKind::TaskGroup), vec![EntityId::from_u64(1)]);
        let p = pic(vec![track(3, DVec2::new(1_000.0, 0.0), Stance::Suspect, Domain::Surface), track(9, DVec2::new(0.0, -15_000.0), Stance::Hostile, Domain::Surface)]);
        let d = run(&mut g, &[member(1, 0.0, 0.0)], &env(&p, &t, &mods, 600), &mut PathPlanner::default());
        assert_eq!((g.contact, g.mode), (TrackId(9), GroupMode::Closing));
        assert!(!d[0].1.weapons_tight);
    }

    #[test]
    fn return_to_hold_is_planned_afresh() {
        let t = map_with(vec![marker("b", 5_000.0, 0.0)]);
        let mods = DoctrineMods::default();
        let p = pic(vec![]);
        let mut g = new_group(&init(GroupOrders::Transit { from: "a".into(), to: "b".into() }, FleetKind::TaskGroup), vec![EntityId::from_u64(1)]);
        let mut planner = PathPlanner::default();
        let d = run(&mut g, &[member(1, 0.0, 0.0)], &env(&p, &t, &mods, 600), &mut planner);
        assert_eq!(d[0].1.route.last().copied(), Some(DVec2::new(5_000.0, 0.0)));
        // Arrived: hold at the marker.
        let d = run(&mut g, &[member(1, 4_900.0, 0.0)], &env(&p, &t, &mods, 1_200), &mut planner);
        assert_eq!((d[0].1.route.clone(), d[0].1.speed_kn), (vec![DVec2::new(5_000.0, 0.0)], HOLD_KN));
        // Drifted off: a new, short plan back from where the leader is.
        let id = planner.next_id;
        let d = run(&mut g, &[member(1, 7_000.0, 0.0)], &env(&p, &t, &mods, 1_800), &mut planner);
        assert!(planner.next_id > id);
        assert_eq!(d[0].1.route.first().copied(), Some(DVec2::new(7_000.0, 0.0)));
        assert_eq!(d[0].1.route.last().copied(), Some(DVec2::new(5_000.0, 0.0)));
        assert!(d[0].1.speed_kn > HOLD_KN);
    }

    #[test]
    fn opposing_truth_never_changes_output() {
        let t = open();
        let mods = DoctrineMods::default();
        let ms = [member(1, 0.0, 0.0), member(2, -500.0, 500.0)];
        let out = |truth: u64| {
            let mut g = new_group(&init(GroupOrders::Hunt, FleetKind::TaskGroup), vec![EntityId::from_u64(1), EntityId::from_u64(2)]);
            let mut tr = track(7, DVec2::new(1_000.0, -9_000.0), Stance::Hostile, Domain::Surface);
            tr.truth = EntityId::from_u64(truth);
            let p = pic(vec![tr]);
            let mut planner = PathPlanner::default();
            let d = run(&mut g, &ms, &env(&p, &t, &mods, 600), &mut planner);
            (d, g.mode, g.contact, g.route.clone())
        };
        assert_eq!(out(5), out(77));
    }

    #[test]
    fn losses_make_an_engaged_group_withdraw() {
        let t = map_with(vec![Marker { kind: MarkerKind::EnemySpawn, name: "home".into(), pos_m: [0.0, 15_000.0], heading_deg: 0.0 }]);
        let mods = DoctrineMods::default();
        let ids: Vec<EntityId> = (1..=4).map(EntityId::from_u64).collect();
        let mut g = new_group(&init(GroupOrders::Hunt, FleetKind::TaskGroup), ids.clone());
        let ms: Vec<Member> = (1..=4).map(|i| member(i, i as f64 * 300.0, 0.0)).collect();
        let p = pic(vec![track(7, DVec2::new(0.0, -6_000.0), Stance::Hostile, Domain::Surface)]);
        let mut planner = PathPlanner::default();
        run(&mut g, &ms, &env(&p, &t, &mods, 600), &mut planner);
        assert_eq!(g.mode, GroupMode::Engaging);
        let survivors = [ms[2], ms[3]];
        let d = run(&mut g, &survivors, &env(&p, &t, &mods, 1_200), &mut planner);
        assert_eq!(g.mode, GroupMode::Withdrawing);
        assert!(d.iter().all(|x| x.1.leader.is_none() && !x.1.weapons_tight));
        assert_eq!(d[0].1.route.last().copied(), Some(DVec2::new(0.0, 15_000.0)));
        // Hold-ground card: fights on.
        let mut hold = DoctrineMods::default();
        hold.flags.insert("hold_ground".into());
        let mut g2 = new_group(&init(GroupOrders::Hunt, FleetKind::TaskGroup), ids);
        run(&mut g2, &ms, &env(&p, &t, &hold, 600), &mut planner);
        run(&mut g2, &survivors, &env(&p, &t, &hold, 1_200), &mut planner);
        assert_eq!(g2.mode, GroupMode::Engaging);
    }

    #[test]
    fn ace_leads_and_convoy_scatters() {
        let t = open();
        let mods = DoctrineMods::default();
        let mut g = new_group(&init(GroupOrders::Patrol { markers: vec![] }, FleetKind::TaskGroup), vec![EntityId::from_u64(1), EntityId::from_u64(2)]);
        let mut ace = member(2, 500.0, 0.0);
        ace.ace = true;
        let d = run(&mut g, &[member(1, 0.0, 0.0), ace], &env(&pic(vec![]), &t, &mods, 600), &mut PathPlanner::default());
        assert_eq!(g.leader, Some(EntityId::from_u64(2)));
        assert_eq!(d[1].1.slot, DVec2::ZERO);
        // Holding with nowhere to go: creep on a one-point route where it stands.
        assert!(d[0].1.route == vec![DVec2::new(500.0, 0.0)] && d[0].1.speed_kn == HOLD_KN, "{:?}", d[0].1);

        let mut c = new_group(&init(GroupOrders::Transit { from: "a".into(), to: "b".into() }, FleetKind::Convoy), vec![EntityId::from_u64(1), EntityId::from_u64(2)]);
        let ms = [member(1, 0.0, 0.0), member(2, 0.0, 500.0)];
        let p = pic(vec![track(7, DVec2::new(0.0, -10_000.0), Stance::Hostile, Domain::Surface)]);
        let d = run(&mut c, &ms, &env(&p, &t, &mods, 600), &mut PathPlanner::default());
        assert_eq!(c.mode, GroupMode::Scattered);
        assert!(d.iter().all(|x| x.1.leader.is_none() && !x.1.weapons_tight));
        // Still scattered a minute later with the threat gone; standing after five minutes.
        run(&mut c, &ms, &env(&pic(vec![]), &t, &mods, 600 + 3_600), &mut PathPlanner::default());
        assert_eq!(c.mode, GroupMode::Scattered);
        run(&mut c, &ms, &env(&pic(vec![]), &t, &mods, 600 + 301 * 60), &mut PathPlanner::default());
        assert_eq!(c.mode, GroupMode::Standing);
    }

    #[test]
    fn anchored_convoy_holds_its_anchorage() {
        // A prize or a defector at anchor (convoy kind, Anchor orders) with a Hostile contact at 5 km and a
        // captain under fire: it stays Standing on a one-point route at its anchorage.
        let t = open();
        let mods = DoctrineMods::default();
        let mut c = new_group(&init(GroupOrders::Anchor, FleetKind::Convoy), vec![EntityId::from_u64(1)]);
        let mut m = member(1, 100.0, 0.0);
        m.under_fire = true;
        let p = pic(vec![track(7, DVec2::new(0.0, -5_000.0), Stance::Hostile, Domain::Surface)]);
        let mut planner = PathPlanner::default();
        let d = run(&mut c, &[m], &env(&p, &t, &mods, 600), &mut planner);
        assert_eq!(c.mode, GroupMode::Standing);
        assert_eq!(c.mem.scatter_until_tick, 0);
        assert!(d[0].1.route.len() == 1 && d[0].1.route[0].distance(DVec2::new(100.0, 0.0)) < 1.0 && d[0].1.speed_kn == HOLD_KN, "{:?}", d[0].1);
        let d = run(&mut c, &[m], &env(&p, &t, &mods, 1_200), &mut planner);
        assert_eq!((c.mode, d[0].1.mode), (GroupMode::Standing, GroupMode::Standing));
    }

    #[test]
    fn escort_screens_its_charge_and_search_circuits() {
        let t = open();
        let mods = DoctrineMods::default();
        let mut g = new_group(&init(GroupOrders::Escort { tag: "convoy".into() }, FleetKind::Escort), vec![EntityId::from_u64(1), EntityId::from_u64(2)]);
        g.formation = Formation::Screen;
        let charge = EntityId::from_u64(50);
        let empty = pic(vec![]);
        let mut e = env(&empty, &t, &mods, 600);
        e.charge = Some((charge, DVec2::new(0.0, 0.0), 0.0));
        let d = run(&mut g, &[member(1, 0.0, -600.0), member(2, 600.0, 0.0)], &e, &mut PathPlanner::default());
        assert_eq!(g.mode, GroupMode::Screening);
        assert!(d.iter().all(|x| x.1.leader == Some(charge) && x.1.slot != DVec2::ZERO && x.1.route.is_empty()));
        assert_ne!(d[0].1.slot, d[1].1.slot);
        // Nearest free slot: member 1 sits ahead of the charge.
        assert_eq!(d[0].1.slot, DVec2::new(750.0, 0.0));

        let at = DVec2::new(8_000.0, 0.0);
        let mut s = new_group(&init(GroupOrders::Search { at, radius_m: 4_000.0 }, FleetKind::TaskGroup), vec![EntityId::from_u64(1)]);
        let mut planner = PathPlanner::default();
        let d = run(&mut s, &[member(1, 0.0, 0.0)], &env(&pic(vec![]), &t, &mods, 600), &mut planner);
        assert_eq!(s.mode, GroupMode::Investigating);
        assert_eq!(d[0].1.route.last().copied(), Some(at));
        let d = run(&mut s, &[member(1, 7_900.0, 0.0)], &env(&pic(vec![]), &t, &mods, 1_800), &mut planner);
        let g1 = d[0].1.route.last().copied().unwrap();
        assert!((g1.distance(at) - 4_000.0).abs() < 1.0, "circuit point {g1}");
    }

    #[test]
    fn unreachable_goal_is_skipped_and_planner_used_once() {
        // Planner over a grid that is all land except the start cell's surroundings.
        let cols = 40u32;
        let mut depth = vec![50.0f32; (cols * cols) as usize];
        for z in 0..cols { depth[(20 + cols * z) as usize] = -5.0; }
        let grid = super::super::path::DepthGridData { cell_m: 50.0, cols, rows: cols, origin: [-1_000.0, -1_000.0], depth };
        let mut planner = PathPlanner::with_grid(grid);
        let t = map_with(vec![marker("west", -800.0, 0.0), marker("east", 800.0, 0.0)]);
        let mods = DoctrineMods::default();
        let mut g = new_group(&init(GroupOrders::Patrol { markers: vec!["east".into(), "west".into()] }, FleetKind::Patrol), vec![EntityId::from_u64(1)]);
        let mut m = member(1, -200.0, 0.0);
        m.turn_radius_m = 60.0;
        let p = pic(vec![]);
        let e = env(&p, &t, &mods, 600);
        let _ = update_core(&mut g, &[m], &e, &mut planner);
        assert_eq!(g.mem.req_goal, Some(DVec2::new(-800.0, 0.0)));
        // Same goal next update: no new request.
        let id = planner.next_id;
        let _ = update_core(&mut g, &[m], &e, &mut planner);
        assert_eq!(planner.next_id, id);
        planner.step(100_000, 630);
        let d = update_core(&mut g, &[m], &env(&p, &t, &mods, 660), &mut planner);
        assert!(g.route.valid);
        assert!(d[0].1.route.last().unwrap().distance(DVec2::new(-800.0, 0.0)) < 60.0);

        // A transit across the land: the planner fails and the group holds where it is instead of running ashore.
        let mut tr = new_group(&init(GroupOrders::Transit { from: "west".into(), to: "east".into() }, FleetKind::TaskGroup), vec![EntityId::from_u64(2)]);
        let mut m2 = member(2, -200.0, 0.0);
        m2.turn_radius_m = 60.0;
        let d = update_core(&mut tr, &[m2], &env(&p, &t, &mods, 600), &mut planner);
        assert_eq!(d[0].1.route, vec![DVec2::new(800.0, 0.0)]);
        planner.step(100_000, 630);
        let d = update_core(&mut tr, &[m2], &env(&p, &t, &mods, 660), &mut planner);
        assert!(!tr.route.valid);
        assert_eq!(d[0].1.route, vec![DVec2::new(-200.0, 0.0)]);
        // No retry inside the back-off.
        let id = planner.next_id;
        let _ = update_core(&mut tr, &[m2], &env(&p, &t, &mods, 900), &mut planner);
        assert_eq!(planner.next_id, id);
    }

    #[test]
    fn flocking_rules() {
        let t = open();
        // Alone on its slot in open water.
        let me = kin(0.0, 0.0, 0.0, 8.0);
        let (dh, sf) = flock_adjust(&me, DVec2::ZERO, &[], 250.0, &t);
        assert!(dh.abs() < 1e-9 && (sf - 1.0).abs() < 1e-9, "{dh} {sf}");
        // Head-on: turn to starboard and slow.
        let other = kin(0.0, -1_200.0, core::f64::consts::PI, 8.0);
        let (dh, sf) = flock_adjust(&me, DVec2::ZERO, &[other], 250.0, &t);
        assert!(dh > 0.05 && sf < 0.95, "head-on {dh} {sf}");
        let (dh2, _) = flock_adjust(&other, DVec2::new(0.0, -1_200.0), &[me], 250.0, &t);
        assert!(dh2 > 0.05, "both turn starboard {dh2}");
        // Crowded on the starboard beam: open to port.
        let close = kin(60.0, 0.0, 0.0, 8.0);
        let (dh, _) = flock_adjust(&me, DVec2::ZERO, &[close], 250.0, &t);
        assert!(dh < -0.05, "beam {dh}");
        // Off slot: steer toward it, faster when it is ahead.
        let (dh, sf) = flock_adjust(&me, DVec2::new(1_000.0, -1_000.0), &[], 250.0, &t);
        assert!(dh > 0.1 && sf > 1.0, "slot {dh} {sf}");
        // Land dead ahead: turn off and slow.
        let isl = Terrain::new(MapDef { size_m: [40_000.0, 40_000.0], depth_m: 200.0, islands: vec![Island { centre_m: [0.0, -650.0], radius_m: 400.0, height_m: 30.0, seed: 1 }], ..Default::default() });
        let (dh, sf) = flock_adjust(&me, DVec2::ZERO, &[], 250.0, &isl);
        assert!(dh.abs() > 0.2 && sf < 0.9, "land {dh} {sf}");
        // Bad input.
        let mut bad = me;
        bad.pose.pos.x = f64::NAN;
        assert_eq!(flock_adjust(&bad, DVec2::ZERO, &[], 250.0, &t), (0.0, 1.0));
        let (dh, sf) = flock_adjust(&me, DVec2::new(f64::NAN, 0.0), &[kin(f64::INFINITY, 0.0, 0.0, 1.0)], f64::NAN, &t);
        assert!(dh.is_finite() && sf.is_finite());
    }

    #[test]
    fn no_leader_and_bad_input() {
        let t = open();
        let mods = DoctrineMods::default();
        let mut g = new_group(&init(GroupOrders::Hunt, FleetKind::TaskGroup), vec![EntityId::from_u64(1)]);
        let mut m = member(1, 0.0, 0.0);
        m.active = false;
        let d = run(&mut g, &[m], &env(&pic(vec![]), &t, &mods, 600), &mut PathPlanner::default());
        assert_eq!((d.len(), g.leader), (1, None));
        assert!(run(&mut g, &[], &env(&pic(vec![]), &t, &mods, 600), &mut PathPlanner::default()).is_empty());
        let mut bad = track(4, DVec2::new(f64::NAN, 0.0), Stance::Hostile, Domain::Surface);
        bad.vel = DVec3::splat(f64::INFINITY);
        let mut g2 = new_group(&init(GroupOrders::Search { at: DVec2::new(f64::NAN, 1.0), radius_m: f64::NAN }, FleetKind::TaskGroup), vec![EntityId::from_u64(1)]);
        let d = run(&mut g2, &[member(1, 0.0, 0.0)], &env(&pic(vec![bad]), &t, &mods, 600), &mut PathPlanner::default());
        assert!(d[0].1.route.iter().all(|w| w.is_finite()) && d[0].1.speed_kn.is_finite());
    }
}
