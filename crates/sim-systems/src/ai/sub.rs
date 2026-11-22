//! Unit `sys-ai-sub` (Phase::Act3: schedule with Act 3; cut first if a milestone gate is missed, with depth
//! charges). AI-only enemy submarines. DESIGN.md L451 (AI-only submarines and depth charges arrive in Act 3 on
//! the same sonar model; player-built submarines are out of scope), L455 (layer and shadow zone), L618
//! (depth-charge gate against the Act 3 reference submarine), L866-875.
//!
//! A submarine is an ordinary cell hull built in enemy-class mode with `EnemyClassDef::submersible`; it holds
//! the depth in `ShipOrders::depth_m` (sim-core's `hydro::depth_keeping`). This unit is its behaviour tree:
//! choose depth relative to the layer to stay in the hunter's shadow zone, close on passive bearings, fire
//! torpedoes inside their effective range, and evade after detection (go deep, turn away, slow below
//! cavitation speed).
//!
//! The tree, in priority order:
//! - Lost: adrift. Struck (surrender rule): surfaces and stops. Disabled: holds depth and fights where it lies.
//! - Hurt (the shared hurt rule, latched) or group withdrawing: goes deep, turns away, creeps out.
//! - Detected: an inbound weapon, fire on the hull, a hostile that fired on it, a hostile closing inside
//!   1.5 km, or a hostile emitting inside active sonar reach. For 120 s after the last cue it goes below the
//!   layer, turns away from the cue (30 degrees off the reciprocal so the hunter's bearing drifts), runs just
//!   below cavitation speed and asks the Helm to evade. It shoots back only at a hostile in torpedo range that
//!   is already shooting.
//! - After a salvo: 90 s repositioning leg, deep, off the line of the torpedo tracks.
//! - Hostile contact: closes below the layer. The layer is two-sided (hull sonars sit above it, and the boat's
//!   own sonar sits at its depth), so below the layer the boat is in the hunter's shadow zone and the hunter is
//!   in the boat's. When the track has not been refreshed for 45 s the boat comes up above the layer for a
//!   listening leg, then goes deep again once it hears the target. On a bearing-only track it closes on
//!   alternating 35-degree legs either side of the bearing (target motion analysis) instead of a lead course.
//!   Inside torpedo effective range it slows, goes weapons free and lets the Weapons officer launch; it pings
//!   in a burst only while the track is still bearing-only.
//! - Suspect contact, or nothing to shoot with: stalks just outside reach, weapons tight.
//! - Nothing perceived: patrols the group route (or hovers where it is) at 6 kn at listening depth above the
//!   layer, Silent.
//!
//! Emission control is Silent except the ranging ping. Speeds stay at or below the lower of 75% of the
//! propeller's cavitation speed and 12 kn (own speed above 15 kn halves passive range, L453), so a hunting boat
//! is quiet. The ordered keel depth never exceeds the shallowest water within 800 m less 6 m of clearance, and
//! never 150 m, the deepest depth-charge setting (L595), so a submarine can always be reached by the weapons
//! meant to kill it.
//!
//! Captain memory (`CaptainMemory`, owned by `sys-ai-unit`) is reused with these meanings for a submarine:
//! `flee_until_tick` = end of the evasion latch, `opening_until_tick` = end of the post-salvo leg,
//! `attack_start_tick` = start of the current attack (salvo detection), `next_zig_tick` and `beam_sign` = the
//! target-motion-analysis leg timer and the side for legs and beam courses, `anchor` = hover point when there
//! is no route, `threat_bearing` = bearing of the last threat seen.
use super::group::GroupMode;
use super::unit::{build_out, wants_withdraw, Behaviour, CaptainMode, CaptainOut, CaptainState, Contact, Course, OwnState, OwnStatus, Plan, Speed};
use super::DoctrineMods;
use crate::prelude::*;
use crate::ship::Ship;
use crate::tracks::SidePicture;
use content::part::PartFunction;
use core::f64::consts::{FRAC_PI_2, PI, TAU};

/// The submarine captain's decision; called by `unit::decide` when the behaviour is `Submarine`.
/// Unit: `sys-ai-sub`.
///
/// `unit::decide` has already absorbed matured percepts and schedules the next decision after this returns.
pub fn decide(captain: &mut CaptainState, ship: &Ship, world: &World, picture: &SidePicture, mods: &DoctrineMods, ctx: &SimCtx<'_>) -> CaptainOut {
    let now = ctx.tick.0;
    let Some(vessel) = world.vessel(ship.vessel) else {
        captain.mode = CaptainMode::Adrift;
        return CaptainOut::default();
    };
    let own = super::unit::own_state(ship, vessel, world, captain, ctx);
    let (threats, inbound) = super::unit::perceived(captain, picture, &own, now);
    let info = |c: &Contact| {
        let t = picture.get(c.id);
        ContactInfo {
            c: *c,
            subsurface: t.is_some_and(|t| t.domain == Domain::Subsurface),
            bearing_only: t.is_some_and(|t| t.bearing_only.is_some()) || c.grade == TrackGrade::Bearing,
            emitting: t.is_some_and(|t| t.emitting),
            stale_s: t.map_or(f64::INFINITY, |t| now.saturating_sub(t.last_update_tick) as f64 / TICKS_PER_S as f64),
        }
    };
    let threats: Vec<ContactInfo> = threats.iter().map(info).collect();
    let inbound: Vec<ContactInfo> = inbound.iter().map(info).collect();

    let terrain = world.terrain();
    let depth = |p: DVec2| terrain.depth_at(p.x, p.y);
    let water_min_m = shallowest_near(own.pos, &depth);
    let hull_h = if vessel.dims[1].is_finite() && vessel.dims[1] > 0.0 { vessel.dims[1] } else { own.draft_m };
    let sensors = &ctx.tuning.sensors;
    let draw = sim_core::rng::uniform(ctx.key(sim_core::rng::Stream::AiDecision, ship.vessel.to_u64(), DRAW_SIDE));
    let sit = SubSit {
        own: &own,
        threats: &threats,
        inbound: &inbound,
        mods,
        now,
        layer_m: ctx.layer_depth_m,
        water_min_m,
        hull_height_m: hull_h,
        cavitation_kn: cavitation_kn(vessel, world.catalogue()),
        active_ref_m: pos_or(sensors.active_sonar_r_ref_m, 4_000.0),
        shadow_m: pos_or(sensors.shadow_zone_m, 5_000.0),
        side_draw: draw,
    };
    let sp = think_sub(captain, &sit);
    finish(&sp, ship, &own, &depth)
}

/// Depth that puts the layer between the submarine and a hunter with hull sonar, or the deepest safe depth
/// when the water is shallower than the layer. Unit: `sys-ai-sub`.
///
/// Depths are keel depths below the surface in metres; `draft_m` is the keel depth at which the hull is just
/// awash (its full height for a submarine). Below the layer means strictly deeper than `layer_depth_m`
/// (`sensors::sonar::layer_factor`); the boat goes `LAYER_MARGIN_M` under it. The result never comes closer
/// than `BOTTOM_CLEARANCE_M` to the bottom and never exceeds `MAX_DEPTH_M`; when even that does not reach below
/// the layer, it is the deepest safe depth. Bad input (NaN, negative) yields a finite depth, 0 when there is no
/// water to dive in.
pub fn evasion_depth_m(water_depth_m: f64, layer_depth_m: f64, draft_m: f64) -> f64 {
    let floor = deepest_safe_m(water_depth_m);
    if floor <= 0.0 { return 0.0; }
    let draft = if draft_m.is_finite() && draft_m > 0.0 { draft_m } else { 0.0 };
    let submerged = (draft + SUBMERGE_MARGIN_M).min(floor);
    let has_layer = layer_depth_m.is_finite() && layer_depth_m > 0.0 && water_depth_m > layer_depth_m;
    if !has_layer { return floor.max(submerged); }
    let want = (layer_depth_m + LAYER_MARGIN_M).max(submerged);
    want.min(floor)
}

// ---------------------------------------------------------------------------------------------------------
// Constants (play choices; the spec fixes only the sonar model)
// ---------------------------------------------------------------------------------------------------------

const TICKS_PER_S: u64 = sim_core::time::TICK_HZ as u64;
const KN: f64 = sim_core::time::KN;
/// Decision code for the side draw (`unit::decide` uses 1 to 3).
const DRAW_SIDE: u64 = 10;
/// Depth below the layer the boat goes to, metres.
const LAYER_MARGIN_M: f64 = 12.0;
/// Depth above the layer for a listening leg, metres.
const LISTEN_MARGIN_M: f64 = 10.0;
/// Water kept under the keel, metres.
const BOTTOM_CLEARANCE_M: f64 = 6.0;
/// Keel depth beyond the hull height that keeps the top under water, metres.
const SUBMERGE_MARGIN_M: f64 = 3.0;
/// Deepest ordered keel depth: the deepest depth-charge setting (L595).
const MAX_DEPTH_M: f64 = 150.0;
/// Radius around the boat within which the shallowest water limits the ordered depth, metres.
const DEPTH_PROBE_M: f64 = 800.0;
/// Ordered depth changes smaller than this are not sent, metres.
const DEPTH_DEADBAND_M: f64 = 3.0;
/// Evasion latch after the last detection cue, s.
const EVADE_S: u64 = 120;
/// Repositioning leg after a salvo, s.
const REPOSITION_S: u64 = 90;
/// A track not refreshed for this long sends the boat up for a listening leg, s.
const STALE_S: f64 = 45.0;
/// Target-motion-analysis leg length and offset from the bearing.
const TMA_LEG_S: u64 = 180;
const TMA_OFFSET_DEG: f64 = 35.0;
/// A hostile closer than this and closing counts as a detection cue, metres.
const CLOSE_HUNTER_M: f64 = 1_500.0;
/// Patrol, creep and in-range speed caps, knots, and the fraction of cavitation speed a hunting boat keeps under.
const PATROL_KN: f64 = 6.0;
const CREEP_MAX_KN: f64 = 12.0;
const ATTACK_KN: f64 = 5.0;
const CAVITATION_FRACTION: f64 = 0.75;
/// Evasion speed as a fraction of cavitation speed: as fast as it can go without cavitating.
const EVADE_CAVITATION_FRACTION: f64 = 0.9;
const DEFAULT_CAVITATION_KN: f64 = 20.0;
/// Fraction of torpedo effective range kept as the doctrine range.
const KEEP_FRACTION: f64 = 0.85;
/// Under this fraction of the firing range the boat opens away instead of holding.
const TOO_CLOSE_FRACTION: f64 = 0.35;
/// An under-fire percept keeps the boat "under fire" this long, s (as `unit.rs`).
const UNDER_FIRE_S: u64 = 30;

// ---------------------------------------------------------------------------------------------------------
// Situation
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct ContactInfo { c: Contact, subsurface: bool, bearing_only: bool, emitting: bool, stale_s: f64 }

struct SubSit<'a> {
    own: &'a OwnState,
    threats: &'a [ContactInfo],
    inbound: &'a [ContactInfo],
    mods: &'a DoctrineMods,
    now: u64,
    layer_m: f64,
    /// Shallowest water within `DEPTH_PROBE_M`, metres (0 near land).
    water_min_m: f64,
    hull_height_m: f64,
    cavitation_kn: f64,
    active_ref_m: f64,
    shadow_m: f64,
    side_draw: f64,
}

/// A plan plus the depth order: `None` keeps the current order, `Some(None)` surfaces.
#[derive(Clone, Debug, PartialEq)]
struct SubPlan { plan: Plan, depth: Option<Option<f64>> }

fn pos_or(x: f64, d: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { d } }

fn deepest_safe_m(water_depth_m: f64) -> f64 {
    if water_depth_m.is_nan() { return 0.0; }
    (water_depth_m - BOTTOM_CLEARANCE_M).clamp(0.0, MAX_DEPTH_M)
}

/// Shallowest water at the boat and on two rings (400 m and 800 m) of eight points around it.
fn shallowest_near(pos: DVec2, depth: &dyn Fn(DVec2) -> f64) -> f64 {
    let sample = |p: DVec2| { let d = depth(p); if d.is_nan() { 0.0 } else { d } };
    let mut m = sample(pos);
    for ring in 1..=2 {
        let r = DEPTH_PROBE_M * ring as f64 / 2.0;
        for k in 0..8 { m = m.min(sample(pos + dir(k as f64 * TAU / 8.0) * r)); }
    }
    m.max(0.0)
}

/// Lowest cavitation speed of the hull's working propulsors, knots.
fn cavitation_kn(vessel: &sim_core::vessel::Vessel, catalogue: &Catalogue) -> f64 {
    let mut kn = f64::INFINITY;
    for p in &vessel.parts {
        if p.level == DamageLevel::Destroyed { continue; }
        let Some((_, def)) = catalogue.parts.get_index(p.def.index()) else { continue };
        if let PartFunction::Propulsor(pd) = &def.function {
            if pd.cavitation_kn.is_finite() && pd.cavitation_kn > 0.0 { kn = kn.min(pd.cavitation_kn); }
        }
    }
    if kn.is_finite() { kn } else { DEFAULT_CAVITATION_KN }
}

fn has_layer(s: &SubSit<'_>) -> bool { s.layer_m.is_finite() && s.layer_m > 0.0 && s.water_min_m > s.layer_m }

/// Keel depth below the layer (or deepest safe without one).
fn deep_m(s: &SubSit<'_>) -> f64 { evasion_depth_m(s.water_min_m, s.layer_m, s.hull_height_m) }

/// Keel depth above the layer where the boat's own sonar shares the hunter's side; deep when there is no layer.
fn listen_m(s: &SubSit<'_>) -> f64 {
    if !has_layer(s) { return deep_m(s); }
    let submerged = s.hull_height_m.max(0.0) + SUBMERGE_MARGIN_M;
    (s.layer_m - LISTEN_MARGIN_M).max(submerged).min(deepest_safe_m(s.water_min_m))
}

fn top_kn(s: &SubSit<'_>) -> f64 { let t = s.own.top_speed_mps / KN; if t.is_finite() && t > 0.0 { t } else { f64::INFINITY } }

fn quiet_kn(s: &SubSit<'_>, cap_kn: f64) -> f64 { (CAVITATION_FRACTION * s.cavitation_kn).min(cap_kn).min(top_kn(s)).max(0.0) }

fn evade_kn(s: &SubSit<'_>) -> f64 { (EVADE_CAVITATION_FRACTION * s.cavitation_kn).min(top_kn(s)).max(0.0) }

fn knots(k: f64) -> Speed { Speed::Knots((k * 10.0).round() / 10.0) }

fn under_fire(c: &CaptainState, now: u64) -> bool { c.mem.under_fire_tick.is_some_and(|t| now.saturating_sub(t) < UNDER_FIRE_S * TICKS_PER_S) }

fn bearing(a: DVec2, b: DVec2) -> f64 { let d = b - a; wrap_tau(libm::atan2(d.x, -d.y)) }
fn dir(h: f64) -> DVec2 { DVec2::new(libm::sin(h), -libm::cos(h)) }
fn wrap_tau(a: f64) -> f64 { let x = a % TAU; if x < 0.0 { x + TAU } else { x } }
fn deg(d: f64) -> f64 { d * PI / 180.0 }
fn finite2(v: DVec2) -> bool { v.x.is_finite() && v.y.is_finite() }

fn plan(mode: CaptainMode) -> Plan {
    Plan { mode, course: Course::Keep, speed: Speed::Keep, designated: None, weapons_tight: None, evade: Some(false), keep_range_m: None, emcon: Some(Emcon::Silent), preset: None, strike: false, at_rest: false }
}

fn side(c: &mut CaptainState, s: &SubSit<'_>) -> f64 {
    if c.mem.beam_sign == 0 { c.mem.beam_sign = if s.side_draw < 0.5 { 1 } else { -1 }; }
    c.mem.beam_sign as f64
}

/// Torpedo (or missile) firing range under the cards: the group's keep range if set, else the weapon range,
/// scaled down by the engagement range factor and the `close_in` flag (never past the weapon's reach). 0 when
/// unarmed.
fn fire_range_m(c: &CaptainState, s: &SubSit<'_>) -> f64 {
    let weapon = s.own.missile_range_m;
    if !(weapon.is_finite() && weapon > 0.0) { return 0.0; }
    if let Some(r) = c.directive.keep_range_m.filter(|r| r.is_finite() && *r > 0.0) { return r.min(weapon); }
    let f = if s.mods.engagement_range_factor.is_finite() { s.mods.engagement_range_factor.clamp(0.25, 1.0) } else { 1.0 };
    let mut r = weapon * f;
    if s.mods.flags.contains("close_in") { r *= 0.7; }
    r.clamp(weapon.min(300.0), weapon)
}

/// Best target: Hostile over Suspect, those that fired on us, the group's contact, nearer first; the current
/// target is kept unless another scores 40% better.
fn pick_target(c: &CaptainState, s: &SubSit<'_>) -> Option<ContactInfo> {
    let score = |t: &ContactInfo| {
        let mut v = if t.c.hostile { 2.0 } else { 1.0 };
        if t.c.fired_on_us { v *= 1.5; }
        if t.c.id == c.directive.contact && !t.c.id.is_none() { v *= 1.5; }
        v / (1.0 + t.c.range / 10_000.0)
    };
    let best = s.threats.iter().max_by(|a, b| score(a).total_cmp(&score(b)).then(b.c.id.cmp(&a.c.id))).copied()?;
    if let Some(cur) = s.threats.iter().find(|t| t.c.id == c.target) {
        if score(cur) * 1.4 >= score(&best) { return Some(*cur); }
    }
    Some(best)
}

/// `Some(bearing of the cue)` when the boat has reason to think it has been found.
fn detection_cue(c: &CaptainState, s: &SubSit<'_>) -> Option<Option<f64>> {
    let own = s.own;
    if let Some(w) = s.inbound.first() { return Some(Some(bearing(own.pos, w.c.pos))); }
    if let Some(t) = s.threats.iter().find(|t| t.c.hostile && t.c.fired_on_us) { return Some(Some(bearing(own.pos, t.c.pos))); }
    let closing = |t: &ContactInfo| (own.pos - t.c.pos).dot(t.c.vel) > 0.0;
    if let Some(t) = s.threats.iter().find(|t| t.c.hostile && !t.subsurface && t.c.range <= CLOSE_HUNTER_M && closing(t)) { return Some(Some(bearing(own.pos, t.c.pos))); }
    if let Some(t) = s.threats.iter().find(|t| t.c.hostile && t.emitting && t.c.range <= s.active_ref_m) { return Some(Some(bearing(own.pos, t.c.pos))); }
    if under_fire(c, s.now) { return Some(c.mem.threat_bearing); }
    None
}

// ---------------------------------------------------------------------------------------------------------
// The tree (pure over its inputs)
// ---------------------------------------------------------------------------------------------------------

fn think_sub(c: &mut CaptainState, s: &SubSit<'_>) -> SubPlan {
    let sp = tree(c, s);
    c.mode = sp.plan.mode;
    if let Some(d) = sp.plan.designated { c.target = d; }
    sp
}

fn tree(c: &mut CaptainState, s: &SubSit<'_>) -> SubPlan {
    let own = s.own;
    if own.status == OwnStatus::Lost {
        let mut p = plan(CaptainMode::Adrift);
        p.evade = None;
        p.emcon = None;
        return SubPlan { plan: p, depth: None };
    }
    if c.mode == CaptainMode::Struck { return struck(c, false); }
    let contacts: Vec<Contact> = s.threats.iter().map(|t| t.c).collect();
    if super::unit::surrender_due(c, own, &contacts, s.now) { return struck(c, true); }
    let target = pick_target(c, s);
    if own.status == OwnStatus::Disabled {
        // No propulsion: hold the depth it has and fight from where it lies.
        let mut p = plan(CaptainMode::Adrift);
        p.course = Course::Stop;
        p.speed = Speed::Knots(0.0);
        p.designated = Some(target.map_or(TrackId::NONE, |t| t.c.id));
        p.weapons_tight = Some(false);
        return SubPlan { plan: p, depth: None };
    }
    if let Some(t) = s.threats.first() { c.mem.threat_bearing = Some(bearing(own.pos, t.c.pos)); }
    if !c.mem.withdraw_latched && !s.mods.flags.contains("hold_ground") && wants_withdraw(&own.health, Behaviour::Submarine, c.ace.is_some()) { c.mem.withdraw_latched = true; }
    let fire_range = fire_range_m(c, s);
    if c.mem.withdraw_latched || c.directive.mode == GroupMode::Withdrawing { return withdraw(c, s, fire_range); }

    if let Some(cue) = detection_cue(c, s) {
        c.mem.flee_until_tick = s.now + EVADE_S * TICKS_PER_S;
        if let Some(b) = cue { c.mem.threat_bearing = Some(b); }
    }
    if s.now < c.mem.flee_until_tick { return evade(c, s, target, fire_range); }
    if s.now < c.mem.opening_until_tick { return reposition(c, s, target); }

    match target {
        Some(t) if t.c.hostile && fire_range > 0.0 => {
            if c.mode != CaptainMode::Attacking { c.mem.attack_start_tick = s.now; }
            let fired = own.last_fired_tick > c.mem.attack_start_tick && own.last_fired_tick <= s.now;
            if fired {
                c.mem.opening_until_tick = s.now + REPOSITION_S * TICKS_PER_S;
                return reposition(c, s, Some(t));
            }
            attack(c, s, &t, fire_range)
        }
        Some(t) => {
            // Suspect, or nothing to shoot with: stalk just outside reach (the shadow range when unarmed).
            let hold = if fire_range > 0.0 { 1.15 * fire_range } else { 1.2 * s.shadow_m };
            stalk(c, s, &t, hold)
        }
        None => patrol(c, s),
    }
}

fn struck(c: &mut CaptainState, now_striking: bool) -> SubPlan {
    let mut p = plan(CaptainMode::Struck);
    p.course = Course::Stop;
    p.speed = Speed::Knots(0.0);
    p.designated = Some(TrackId::NONE);
    p.weapons_tight = Some(true);
    p.strike = now_striking;
    c.target = TrackId::NONE;
    // Surfaces to strike: the hunters must be able to see the boat give up.
    SubPlan { plan: p, depth: Some(None) }
}

/// Away from the weighted threat centroid (weapons included), else from the last threat seen.
fn away_heading(c: &CaptainState, s: &SubSit<'_>) -> Option<f64> {
    let own = s.own;
    let mut sum = DVec2::ZERO;
    for t in s.threats.iter().chain(s.inbound.iter()) {
        let w = if t.c.hostile { 1.0 } else { 0.5 } / (1.0 + t.c.range / 5_000.0);
        sum += (t.c.pos - own.pos).normalize_or_zero() * w;
    }
    let from = if sum.length() > 1e-6 { Some(wrap_tau(libm::atan2(sum.x, -sum.y))) } else { c.mem.threat_bearing };
    from.map(|b| wrap_tau(b + PI))
}

fn withdraw(c: &mut CaptainState, s: &SubSit<'_>, fire_range: f64) -> SubPlan {
    let mut p = plan(CaptainMode::Withdrawing);
    let route: Vec<DVec2> = c.directive.route.iter().copied().filter(|w| finite2(*w)).collect();
    p.course = match away_heading(c, s) {
        Some(h) => Course::Heading(h),
        None if !route.is_empty() => Course::Route(route),
        None => Course::Keep,
    };
    p.speed = knots(if s.inbound.is_empty() { quiet_kn(s, CREEP_MAX_KN) } else { evade_kn(s) });
    p.evade = Some(!s.inbound.is_empty());
    let shooter = s.threats.iter().find(|t| t.c.hostile && t.c.fired_on_us && t.c.range <= fire_range);
    p.designated = Some(shooter.map_or(TrackId::NONE, |t| t.c.id));
    p.weapons_tight = Some(shooter.is_none());
    p.keep_range_m = Some(None);
    p.preset = Some(DoctrinePreset::Cautious);
    SubPlan { plan: p, depth: Some(Some(deep_m(s))) }
}

fn evade(c: &mut CaptainState, s: &SubSit<'_>, target: Option<ContactInfo>, fire_range: f64) -> SubPlan {
    let sign = side(c, s);
    let mut p = plan(CaptainMode::Evading);
    // Turn away from the cue, 30 degrees off the reciprocal so the hunter's bearing keeps drifting.
    let from = s.inbound.first().map(|w| bearing(s.own.pos, w.c.pos)).or(c.mem.threat_bearing);
    p.course = match from { Some(b) => Course::Heading(wrap_tau(b + PI + sign * deg(30.0))), None => Course::Keep };
    p.speed = knots(evade_kn(s));
    p.evade = Some(true);
    // Shoot back only at a hostile in range that is already shooting.
    let shooter = target.filter(|t| t.c.hostile && t.c.range <= fire_range && (t.c.fired_on_us || under_fire(c, s.now)));
    p.designated = Some(shooter.map_or(TrackId::NONE, |t| t.c.id));
    p.weapons_tight = Some(shooter.is_none());
    p.keep_range_m = Some(None);
    p.preset = Some(DoctrinePreset::Cautious);
    SubPlan { plan: p, depth: Some(Some(deep_m(s))) }
}

fn reposition(c: &mut CaptainState, s: &SubSit<'_>, target: Option<ContactInfo>) -> SubPlan {
    let sign = side(c, s);
    let mut p = plan(CaptainMode::Opening);
    let from = target.map(|t| bearing(s.own.pos, t.c.pos)).or(c.mem.threat_bearing);
    // Off the line of the torpedo tracks and opening: 120 degrees off the target bearing.
    p.course = match from { Some(b) => Course::Heading(wrap_tau(b + sign * deg(120.0))), None => Course::Keep };
    p.speed = knots(quiet_kn(s, CREEP_MAX_KN));
    p.designated = Some(target.map_or(TrackId::NONE, |t| t.c.id));
    p.weapons_tight = Some(true);
    p.preset = Some(DoctrinePreset::Cautious);
    SubPlan { plan: p, depth: Some(Some(deep_m(s))) }
}

/// Deep while the track is fresh, up for a listening leg when it has gone stale.
fn hunting_depth(s: &SubSit<'_>, t: &ContactInfo) -> f64 { if has_layer(s) && t.stale_s > STALE_S { listen_m(s) } else { deep_m(s) } }

/// Closing heading: lead course on a ranged track, alternating legs either side of the bearing on a
/// bearing-only one.
fn closing_heading(c: &mut CaptainState, s: &SubSit<'_>, t: &ContactInfo) -> f64 {
    let own = s.own;
    let to_t = bearing(own.pos, t.c.pos);
    if t.bearing_only {
        side(c, s);
        if s.now >= c.mem.next_zig_tick {
            if c.mem.next_zig_tick != 0 { c.mem.beam_sign = -c.mem.beam_sign; }
            c.mem.next_zig_tick = s.now + TMA_LEG_S * TICKS_PER_S;
        }
        return wrap_tau(to_t + c.mem.beam_sign as f64 * deg(TMA_OFFSET_DEG));
    }
    let closing = (quiet_kn(s, CREEP_MAX_KN) * KN).max(2.0);
    let lead_t = (t.c.range / closing).min(600.0);
    let lead = t.c.pos + t.c.vel * lead_t;
    if finite2(lead) { bearing(own.pos, lead) } else { to_t }
}

fn attack(c: &mut CaptainState, s: &SubSit<'_>, t: &ContactInfo, fire_range: f64) -> SubPlan {
    let in_range = t.c.range <= fire_range;
    let mut p = plan(CaptainMode::Attacking);
    let to_t = bearing(s.own.pos, t.c.pos);
    if !in_range {
        p.course = Course::Heading(closing_heading(c, s, t));
        p.speed = knots(quiet_kn(s, CREEP_MAX_KN));
    } else if t.c.range < TOO_CLOSE_FRACTION * fire_range {
        // Too close for the torpedoes to settle and too close to stay hidden: open on a quarter course.
        let sign = side(c, s);
        p.course = Course::Heading(wrap_tau(to_t + PI - sign * deg(40.0)));
        p.speed = knots(quiet_kn(s, CREEP_MAX_KN));
    } else {
        // Firing position: slow, the target on the beam so the boat drifts across its line.
        let sign = side(c, s);
        p.course = Course::Heading(wrap_tau(to_t + sign * FRAC_PI_2));
        p.speed = knots(quiet_kn(s, ATTACK_KN));
    }
    p.designated = Some(t.c.id);
    p.weapons_tight = Some(!in_range);
    p.keep_range_m = Some(Some(KEEP_FRACTION * fire_range));
    // Ranging pings in burst only while the track is bearing-only.
    p.emcon = Some(if in_range && t.bearing_only { Emcon::Burst } else { Emcon::Silent });
    p.preset = Some(if c.ace.is_some() || s.mods.flags.contains("aggressive") { DoctrinePreset::Aggressive } else { DoctrinePreset::Cautious });
    SubPlan { plan: p, depth: Some(Some(hunting_depth(s, t))) }
}

fn stalk(c: &mut CaptainState, s: &SubSit<'_>, t: &ContactInfo, hold_m: f64) -> SubPlan {
    let mut p = plan(CaptainMode::Investigating);
    let to_t = bearing(s.own.pos, t.c.pos);
    if t.c.range > hold_m {
        p.course = Course::Heading(closing_heading(c, s, t));
        p.speed = knots(quiet_kn(s, CREEP_MAX_KN));
    } else {
        let sign = side(c, s);
        let off = if t.c.range < 0.8 * hold_m { deg(135.0) } else { FRAC_PI_2 };
        p.course = Course::Heading(wrap_tau(to_t + sign * off));
        p.speed = knots(quiet_kn(s, ATTACK_KN));
    }
    p.designated = Some(t.c.id);
    p.weapons_tight = Some(true);
    p.keep_range_m = Some(Some(hold_m));
    p.preset = Some(DoctrinePreset::Cautious);
    SubPlan { plan: p, depth: Some(Some(hunting_depth(s, t))) }
}

fn patrol(c: &mut CaptainState, s: &SubSit<'_>) -> SubPlan {
    let mode = if c.directive.mode == GroupMode::Investigating { CaptainMode::Investigating } else { CaptainMode::Following };
    let mut p = plan(mode);
    let route: Vec<DVec2> = c.directive.route.iter().copied().filter(|w| finite2(*w)).collect();
    let quiet = quiet_kn(s, PATROL_KN);
    if route.is_empty() {
        // Nowhere to go: hover where it is, silent and slow.
        let at = *c.mem.anchor.get_or_insert(s.own.pos);
        p.course = Course::Station(at, None);
        p.speed = knots(quiet.min(3.0));
    } else {
        p.course = Course::Route(route);
        let d = c.directive.speed_kn;
        p.speed = knots(if d.is_finite() && d > 0.0 { d.min(quiet_kn(s, CREEP_MAX_KN)) } else { quiet });
    }
    p.designated = Some(TrackId::NONE);
    p.weapons_tight = Some(c.directive.weapons_tight);
    p.keep_range_m = Some(None);
    p.preset = Some(DoctrinePreset::Cautious);
    SubPlan { plan: p, depth: Some(Some(listen_m(s))) }
}

// ---------------------------------------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------------------------------------

/// Turn the plan into order changes: `build_out` for course, speed and fire orders (its land probe sized to the
/// keel depth), then the depth order, filtered through a dead band. A depth of 0 or less surfaces.
fn finish(sp: &SubPlan, ship: &Ship, own: &OwnState, depth: &dyn Fn(DVec2) -> f64) -> CaptainOut {
    let cur = &ship.orders;
    let want_depth = match sp.depth {
        None => cur.depth_m,
        Some(Some(d)) if d.is_finite() && d > 0.0 => {
            let keep = cur.depth_m.is_some_and(|c| c.is_finite() && (c - d).abs() < DEPTH_DEADBAND_M);
            if keep { cur.depth_m } else { Some((d * 10.0).round() / 10.0) }
        }
        Some(_) => None,
    };
    let mut probe = own.clone();
    if let Some(d) = want_depth { probe.draft_m = probe.draft_m.max(d); }
    let mut out = build_out(&sp.plan, cur, ship.settings.emcon, ship.settings.preset, &probe, depth);
    if want_depth != cur.depth_m {
        let mut o = out.orders.take().unwrap_or_else(|| cur.clone());
        o.depth_m = want_depth;
        out.orders = Some(o);
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::unit::{new_captain, HullHealth};
    use content::campaign::SurrenderRule;

    fn own() -> OwnState {
        OwnState {
            vessel: EntityId::from_u64(7), pos: DVec2::ZERO, heading: 0.0, status: OwnStatus::Active,
            health: HullHealth { cells_lost: 0.0, weapons_total: 2, weapons_working: 2, propulsion_total: 1, propulsion_working: 1 },
            top_speed_mps: 10.0, turn_radius_m: 250.0, draft_m: 6.0, gun_range_m: 0.0, missile_range_m: 8_000.0, self_defence_m: 4_000.0,
            visual_ref_m: 10_000.0, last_fired_tick: 0, is_player: false, leader: None, strike: Default::default(),
        }
    }
    fn contact(id: u32, pos: DVec2, hostile: bool) -> ContactInfo {
        ContactInfo { c: Contact { id: TrackId(id), pos, vel: DVec2::ZERO, range: pos.length(), hostile, fired_on_us: false, grade: TrackGrade::Fine }, subsurface: false, bearing_only: false, emitting: false, stale_s: 1.0 }
    }
    fn sit<'a>(own: &'a OwnState, threats: &'a [ContactInfo], inbound: &'a [ContactInfo], mods: &'a DoctrineMods, now: u64) -> SubSit<'a> {
        SubSit { own, threats, inbound, mods, now, layer_m: 50.0, water_min_m: 200.0, hull_height_m: 8.0, cavitation_kn: 20.0, active_ref_m: 4_000.0, shadow_m: 5_000.0, side_draw: 0.25 }
    }
    fn sub() -> CaptainState { new_captain(Behaviour::Submarine, None, None, SurrenderRule::Never) }
    fn depth_of(sp: &SubPlan) -> f64 { sp.depth.flatten().expect("depth order") }
    fn heading_of(p: &Plan) -> f64 { match p.course { Course::Heading(h) => h, ref o => panic!("no heading: {o:?}") } }
    fn ang(a: f64, b: f64) -> f64 { let x = wrap_tau(a - b); x.min(TAU - x) }
    fn kn_of(p: &Plan) -> f64 { match p.speed { Speed::Knots(k) => k, ref o => panic!("no knots: {o:?}") } }

    #[test]
    fn evasion_depth_puts_the_layer_between() {
        let t = content::tuning::SensorTuning::default();
        let d = evasion_depth_m(200.0, 50.0, 8.0);
        assert_eq!(d, 62.0);
        // Against a hull sonar at 4 m the boat is across the layer: 0.3 inside 5 km, shadow beyond.
        assert!((crate::sensors::sonar::layer_factor(4.0, d, 3_000.0, 200.0, 50.0, &t) - 0.3).abs() < 1e-12);
        assert_eq!(crate::sensors::sonar::layer_factor(4.0, d, 6_000.0, 200.0, 50.0, &t), 0.0);
        // Water barely deeper than the layer: the deepest safe depth.
        assert_eq!(evasion_depth_m(60.0, 50.0, 8.0), 54.0);
        assert_eq!(evasion_depth_m(55.0, 50.0, 8.0), 49.0);
        // No layer in shallow water: deepest safe.
        assert_eq!(evasion_depth_m(40.0, 50.0, 8.0), 34.0);
        // Deep water stays within the deepest depth-charge setting.
        assert_eq!(evasion_depth_m(2_000.0, 80.0, 8.0), 92.0);
        assert_eq!(evasion_depth_m(2_000.0, f64::NAN, 8.0), MAX_DEPTH_M);
        assert_eq!(evasion_depth_m(f64::INFINITY, 50.0, 8.0), 62.0);
        // Bad input stays finite.
        for (w, l, dr) in [(f64::NAN, 50.0, 8.0), (-10.0, 50.0, 8.0), (3.0, 50.0, 8.0), (200.0, -1.0, f64::INFINITY), (200.0, 50.0, f64::NAN)] {
            let d = evasion_depth_m(w, l, dr);
            assert!(d.is_finite() && d >= 0.0, "{w} {l} {dr} -> {d}");
        }
        assert_eq!(evasion_depth_m(f64::NAN, 50.0, 8.0), 0.0);
    }

    #[test]
    fn patrols_silent_above_the_layer() {
        let o = own();
        let mods = DoctrineMods::default();
        let mut c = sub();
        c.directive.route = vec![DVec2::new(0.0, -10_000.0)];
        let sp = think_sub(&mut c, &sit(&o, &[], &[], &mods, 100));
        assert_eq!(sp.plan.mode, CaptainMode::Following);
        assert_eq!(sp.plan.emcon, Some(Emcon::Silent));
        assert_eq!(depth_of(&sp), 40.0, "listening depth above the layer");
        assert!(kn_of(&sp.plan) <= PATROL_KN);
        // Without a route it hovers.
        let mut c = sub();
        let sp = think_sub(&mut c, &sit(&o, &[], &[], &mods, 100));
        assert!(matches!(sp.plan.course, Course::Station(..)));
    }

    #[test]
    fn closes_deep_and_fires_in_range() {
        let o = own();
        let mods = DoctrineMods::default();
        let mut c = sub();
        let far = [contact(3, DVec2::new(0.0, -20_000.0), true)];
        let sp = think_sub(&mut c, &sit(&o, &far, &[], &mods, 100));
        assert_eq!(sp.plan.mode, CaptainMode::Attacking);
        assert!(depth_of(&sp) > 50.0, "below the layer");
        assert_eq!(sp.plan.weapons_tight, Some(true));
        assert!(ang(heading_of(&sp.plan), 0.0) < 1e-6);
        assert!(kn_of(&sp.plan) <= 15.0 && kn_of(&sp.plan) <= 0.75 * 20.0 + 1e-9, "quiet {}", kn_of(&sp.plan));
        assert_eq!(sp.plan.emcon, Some(Emcon::Silent));
        // Stale track: up for a listening leg.
        let mut stale = far;
        stale[0].stale_s = 60.0;
        let sp = think_sub(&mut c, &sit(&o, &stale, &[], &mods, 200));
        assert!(depth_of(&sp) < 50.0);
        // In range: weapons free, slow, designated.
        let near = [contact(3, DVec2::new(0.0, -6_000.0), true)];
        let sp = think_sub(&mut c, &sit(&o, &near, &[], &mods, 300));
        assert_eq!(sp.plan.weapons_tight, Some(false));
        assert_eq!(sp.plan.designated, Some(TrackId(3)));
        assert!(kn_of(&sp.plan) <= ATTACK_KN);
        assert_eq!(c.target, TrackId(3));
        // A salvo goes: reposition deep for 90 s.
        let mut o2 = own();
        o2.last_fired_tick = 350;
        let sp = think_sub(&mut c, &sit(&o2, &near, &[], &mods, 360));
        assert_eq!(sp.plan.mode, CaptainMode::Opening);
        assert!(depth_of(&sp) > 50.0);
        let sp = think_sub(&mut c, &sit(&o2, &near, &[], &mods, 360 + 60 * TICKS_PER_S));
        assert_eq!(sp.plan.mode, CaptainMode::Opening);
        let sp = think_sub(&mut c, &sit(&o2, &near, &[], &mods, 360 + 91 * TICKS_PER_S));
        assert_eq!(sp.plan.mode, CaptainMode::Attacking, "the old salvo does not count twice");
    }

    #[test]
    fn bearing_only_closes_on_alternating_legs() {
        let o = own();
        let mods = DoctrineMods::default();
        let mut c = sub();
        let mut t = [contact(3, DVec2::new(0.0, -20_000.0), true)];
        t[0].bearing_only = true;
        let h1 = heading_of(&think_sub(&mut c, &sit(&o, &t, &[], &mods, 100)).plan);
        assert!((ang(h1, 0.0) - deg(TMA_OFFSET_DEG)).abs() < 1e-9);
        let h2 = heading_of(&think_sub(&mut c, &sit(&o, &t, &[], &mods, 100 + TMA_LEG_S * TICKS_PER_S)).plan);
        assert!((ang(h2, 0.0) - deg(TMA_OFFSET_DEG)).abs() < 1e-9);
        assert!(ang(h1, h2) > deg(60.0), "flipped side");
    }

    #[test]
    fn evades_deep_and_away_after_detection() {
        let o = own();
        let mods = DoctrineMods::default();
        let mut c = sub();
        let threats = [contact(3, DVec2::new(0.0, -6_000.0), true)];
        let mut torp = contact(9, DVec2::new(0.0, -2_000.0), true);
        torp.c.vel = DVec2::new(0.0, 20.0);
        let sp = think_sub(&mut c, &sit(&o, &threats, &[torp], &mods, 100));
        assert_eq!(sp.plan.mode, CaptainMode::Evading);
        assert_eq!(sp.plan.evade, Some(true));
        assert!(depth_of(&sp) > 50.0);
        assert!(ang(heading_of(&sp.plan), PI) <= deg(30.0) + 1e-9, "turned away");
        assert!(kn_of(&sp.plan) < 20.0, "below cavitation");
        assert_eq!(sp.plan.weapons_tight, Some(true), "does not shoot back at a hunter that has not fired");
        // The latch holds after the torpedo is gone, then lapses.
        assert_eq!(think_sub(&mut c, &sit(&o, &threats, &[], &mods, 100 + 60 * TICKS_PER_S)).plan.mode, CaptainMode::Evading);
        assert_eq!(think_sub(&mut c, &sit(&o, &threats, &[], &mods, 100 + 121 * TICKS_PER_S)).plan.mode, CaptainMode::Attacking);
        // A hunter emitting close by is a cue too.
        let mut c = sub();
        let mut pinger = contact(4, DVec2::new(0.0, -3_000.0), true);
        pinger.emitting = true;
        assert_eq!(think_sub(&mut c, &sit(&o, &[pinger], &[], &mods, 100)).plan.mode, CaptainMode::Evading);
    }

    #[test]
    fn strikes_by_surfacing_and_withdraws_when_hurt() {
        let mut o = own();
        o.health.cells_lost = 0.6;
        let mods = DoctrineMods::default();
        let mut c = new_captain(Behaviour::Submarine, None, None, SurrenderRule::Strike { cells_lost: 0.5 });
        let near = [contact(3, DVec2::new(0.0, -2_000.0), true)];
        // Under fire and past the fraction: the rule's 5 s confirm, then it strikes.
        c.mem.under_fire_tick = Some(60);
        assert!(!think_sub(&mut c, &sit(&o, &near, &[], &mods, 60)).plan.strike, "the confirm time runs first");
        let sp = think_sub(&mut c, &sit(&o, &near, &[], &mods, 60 + 5 * TICKS_PER_S));
        assert!(sp.plan.strike);
        assert_eq!(sp.depth, Some(None));
        assert_eq!(c.mode, CaptainMode::Struck);
        assert!(!think_sub(&mut c, &sit(&o, &near, &[], &mods, 60 + 6 * TICKS_PER_S)).plan.strike, "strikes once");
        // Hurt but no surrender rule: withdraws deep, straight away from the threat.
        let mut c = sub();
        let mut o = own();
        o.health.cells_lost = 0.2;
        let far = [contact(3, DVec2::new(0.0, -9_000.0), true)];
        let sp = think_sub(&mut c, &sit(&o, &far, &[], &mods, 100));
        assert_eq!(sp.plan.mode, CaptainMode::Withdrawing);
        assert!(depth_of(&sp) > 50.0);
        assert!(ang(heading_of(&sp.plan), PI) < 1e-6);
    }

    #[test]
    fn shallow_water_limits_depth() {
        let o = own();
        let mods = DoctrineMods::default();
        let mut c = sub();
        let mut s = sit(&o, &[], &[], &mods, 100);
        s.water_min_m = 30.0;
        assert_eq!(depth_of(&think_sub(&mut c, &s)), 24.0, "no layer in 30 m of water: deepest safe");
        s.water_min_m = 0.0;
        assert_eq!(think_sub(&mut c, &s).depth, Some(Some(0.0)));
    }

    #[test]
    fn shallowest_near_reads_the_rings() {
        let shoal = |p: DVec2| if p.x > 700.0 { 12.0 } else { 300.0 };
        assert_eq!(shallowest_near(DVec2::ZERO, &shoal), 12.0);
        let nan = |_: DVec2| f64::NAN;
        assert_eq!(shallowest_near(DVec2::ZERO, &nan), 0.0);
    }
}
