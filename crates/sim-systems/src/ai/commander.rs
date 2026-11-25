//! Unit `sys-ai-commander`. The task-group commander level inside a sortie: a belief map of where the player
//! probably is, group tasking and the side's in-sortie alert. DESIGN.md L760 (belief map fed by sightings,
//! radar, radio emissions, ESM and missing convoys; searches its peaks; players can view the overlay after a
//! mission), L749 (what counts as a sighting or evidence), L94 (scheduled reinforcements are committed).
//!
//! The commander reads only its own side's picture and its own losses. It never reads `Track::truth` of an
//! opposing track or any world state of the other side, so decoys, emission control and dark running fool it
//! exactly as they would fool a human plotter. (Own-side Friend tracks are matched to own hulls by entity: the
//! side knows where its own ships are.)
//!
//! Belief model (one update per second, staggered by side):
//! 1. Diffusion: a separable three-tap blur sized so the spread after `T_REF_S` matches the distance a hull at
//!    `ASSUMED_SPEED_MPS` covers in that time. Mass reflects at the map edge.
//! 2. Negative evidence: water inside the assured visual radius of every own hull (Friend surface tracks) that
//!    holds no opposing contact loses probability, more near the hull. Searching a peak and finding nothing
//!    moves the belief elsewhere.
//! 3. Positive evidence, mixed in with weight by quality: opposing surface tracks as Gaussians of their
//!    current uncertainty, bearing-only (ESM, passive sonar) tracks as wedges, opposing weapons as wedges back
//!    along their flight path, opposing aircraft and drones as broad discs, last-known marks dead-reckoned,
//!    and own losses as discs around the loss position.
//! 4. Renormalise with a small uniform floor so no water is ever ruled out for good.
use super::group::GroupState;
use crate::prelude::*;
use crate::protocol::config::GroupOrders;
use crate::tracks::{SidePicture, Track};
use content::campaign::FleetKind;

/// In-sortie alert of an AI side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SortieAlert { #[default] Unaware, Suspicious, Alerted, Engaged }

/// Probability grid over the sortie map (cell edge `cell_m`, row-major from the map's north-west corner).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BeliefGrid { pub cell_m: f64, pub cols: u16, pub rows: u16, pub origin: [f64; 2], pub p: Vec<f32> }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommanderState {
    pub side: SideId,
    pub alert: SortieAlert,
    pub belief: BeliefGrid,
    /// Contacts that held a player-side hull as Suspect or Hostile (Alert +0.5 each, capped per operation by
    /// the theatre, L749). Counted once per opposing hull (server bookkeeping on the track's truth entity, which
    /// no AI decision reads) once a contact has been held for 10 s.
    pub sightings: u16,
    /// Evidence items: wreck found, intercepted emission (once per opposing hull), detected RHIB (+0.25 each).
    pub evidence: u16,
    /// Own losses the side has noticed (observed kills).
    pub losses_known: Vec<EntityId>,
    pub last_contact_tick: Option<u64>,
    pub reinforcements_called: bool,
    /// Private bookkeeping of the commander (unit `sys-ai-commander`). Build states with `..Default::default()`.
    #[serde(default)]
    pub(crate) mem: CommanderMemory,
}

/// What the commander remembers between updates besides the public fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CommanderMemory {
    pub(crate) last_update_tick: Option<u64>,
    /// Opposing hulls already counted as a sighting, ascending ledger keys (see `ledger_key`).
    pub(crate) sighted: Vec<u64>,
    /// Opposing hulls already counted as an intercepted emission, ascending ledger keys.
    pub(crate) emissions: Vec<u64>,
    /// Unobserved own losses not yet noticed: (vessel, tick first reported missing, last position), ascending
    /// vessel.
    pub(crate) pending_losses: Vec<(EntityId, u64, Option<DVec2>)>,
    /// Loss evidence still being folded into the belief: (position, sigma, weight, tick noticed).
    pub(crate) loss_marks: Vec<(DVec2, f64, f64, u64)>,
    /// Orders each re-tasked group had before the commander took it, ascending group.
    pub(crate) standing: Vec<(GroupId, GroupOrders)>,
    /// Last tick the conditions of the current alert level held.
    pub(crate) level_held_tick: u64,
}

// ---- Model constants (the spec gives none; chosen for play, listed in the unit's decisions) ----

/// Top speed the commander assumes for an opposing hull when spreading the belief, m/s (about 29 kn).
const ASSUMED_SPEED_MPS: f64 = 15.0;
/// Time over which the diffusion spread matches `ASSUMED_SPEED_MPS` travel, s.
const T_REF_S: f64 = 600.0;
/// Largest blur side weight per pass.
const MAX_BLUR: f64 = 0.2;
/// Largest grid edge: 128 x 128 cells.
const MAX_CELLS_EDGE: usize = 128;
/// Uniform floor mixed in after each update.
const FLOOR: f64 = 1.0e-4;
/// Assured visual radius as a fraction of the lookout's reference range, and the detection probability at the
/// hull itself.
const NEG_RANGE_FRACTION: f64 = 0.6;
const NEG_PD: f64 = 0.5;
/// Age before a contact counts as a sighting (filters single-scan false tracks), s.
const SIGHTING_MIN_AGE_S: f64 = 10.0;
/// Unobserved losses are noticed when the hull misses this long of datalink check-ins, s.
const LOSS_NOTICE_S: f64 = 180.0;
/// Loss evidence stays in the belief this long, s.
const LOSS_MARK_S: f64 = 300.0;
/// Alert decay: seconds without the conditions of the current level before dropping one level.
const DECAY_ENGAGED_S: f64 = 120.0;
const DECAY_ALERTED_S: f64 = 600.0;
const DECAY_SUSPICIOUS_S: f64 = 900.0;
/// A Suspect contact held this long raises the side to Alerted, s.
const SUSPECT_ALERT_AGE_S: f64 = 60.0;
/// A belief peak is worth searching when its 3 x 3 mass is this many times the uniform 3 x 3 mass.
const PEAK_CONCENTRATION: f64 = 4.0;
/// A new peak replaces a group's current search point only when it is this far away, metres.
const RETASK_HYSTERESIS_M: f64 = 3_000.0;
/// Opposing contacts beyond this from every own hull do not pull groups onto Hunt, metres.
const HUNT_RANGE_M: f64 = 60_000.0;
/// Caps on the wide evidence sources folded in per update, so a salvo or an air raid cannot blow the AI share
/// of the tick: weapons and aircraft (lowest track ids), bearing-only contacts, last-known marks, loss marks.
const MAX_WEAPON_SOURCES: usize = 2;
const MAX_AIR_SOURCES: usize = 2;
const MAX_BEARING_SOURCES: usize = 4;
const MAX_MARK_SOURCES: usize = 4;
const MAX_LOSS_MARKS: usize = 3;
/// Widest Gaussian a source may spread over, metres (3 sigma = 45 km).
const MAX_SOURCE_SIGMA_M: f64 = 15_000.0;

const TICKS_PER_S: f64 = sim_core::time::TICK_HZ as f64;

/// One 1 Hz commander update: diffuse and renormalise the belief, add evidence from the side picture
/// (tracks of the opposing side by grade, ESM bearings as wedges, missing units as negative or positive
/// evidence), raise or decay alert, count sightings and evidence, and re-task groups toward belief peaks or
/// back to standing orders. Unit: `sys-ai-commander`.
///
/// `own_losses` lists this side's lost hulls as (vessel, last position, observed): an observed loss (a
/// surviving unit saw it) is known at once; an unobserved one is noticed `LOSS_NOTICE_S` after it is first
/// listed and then counts as evidence (a wreck found). Only groups of this side are re-tasked: `Hunt` while an
/// opposing contact is held at Alerted or above, `Search` toward distinct belief peaks otherwise, and the
/// group's own standing orders once the side is Unaware again. Orders are returned only when they change.
/// `reinforcements_called` latches when the side first reaches Alerted.
pub fn update(state: &mut CommanderState, picture: &SidePicture, groups: &[GroupState], own_losses: &[(EntityId, DVec3, bool)], ctx: &SimCtx<'_>) -> Vec<(GroupId, GroupOrders)> {
    let now = ctx.tick.0;
    let dt_s = match state.mem.last_update_tick { Some(t) if now > t => ((now - t) as f64 / TICKS_PER_S).min(120.0), Some(_) => 0.0, None => 1.0 };
    state.mem.last_update_tick = Some(now);
    if !grid_ok(&state.belief) { state.belief = repair_grid(&state.belief); }

    let view = PictureView::new(picture, now, &ctx.tuning.tracks);
    let observed_loss = absorb_losses(state, own_losses, now);
    count_contacts(state, &view);

    diffuse(&mut state.belief, dt_s);
    negative_evidence(&mut state.belief, &view, assured_visual_m(ctx));
    let sources = evidence_sources(state, &view, now);
    apply_evidence(&mut state.belief, &sources);
    normalise(&mut state.belief);

    update_alert(state, &view, observed_loss, now);
    if state.alert >= SortieAlert::Alerted { state.reinforcements_called = true; }

    task_groups(state, &view, groups)
}

/// Build an empty belief grid for a map (uniform prior). Unit: `sys-ai-commander`.
///
/// The grid covers the map's `size_m` centred on the origin (world X east, Z south), row 0 at the north
/// edge. A missing, tiny or non-finite `cell_m` is raised so the grid never exceeds 128 x 128 cells; a
/// degenerate map gives one cell.
pub fn new_belief(map: &content::campaign::MapDef, cell_m: f64) -> BeliefGrid {
    let lim = content::campaign::MAX_MAP_M * 4.0;
    let w = if map.size_m[0].is_finite() && map.size_m[0] > 0.0 { map.size_m[0].min(lim) } else { 0.0 };
    let h = if map.size_m[1].is_finite() && map.size_m[1] > 0.0 { map.size_m[1].min(lim) } else { 0.0 };
    let mut cell = if cell_m.is_finite() && cell_m > 0.0 { cell_m } else { 1_000.0 };
    let edge = w.max(h);
    if edge > 0.0 { cell = cell.max(edge / MAX_CELLS_EDGE as f64); }
    let cols = if w > 0.0 { ((w / cell).ceil() as usize).clamp(1, MAX_CELLS_EDGE) } else { 1 };
    let rows = if h > 0.0 { ((h / cell).ceil() as usize).clamp(1, MAX_CELLS_EDGE) } else { 1 };
    let n = cols * rows;
    BeliefGrid { cell_m: cell, cols: cols as u16, rows: rows as u16, origin: [-w * 0.5, -h * 0.5], p: vec![1.0 / n as f32; n] }
}

/// Sample the belief for the after-action overlay: up to `n` (x, z, weight) peaks, highest first (L70).
/// Unit: `sys-ai-commander`.
///
/// A peak is a local maximum of the 3 x 3 neighbourhood mass (ties go to the lower cell index); peaks within
/// two cells of a higher one are suppressed. `x`, `z` are the cell centre in world metres and the weight is
/// the neighbourhood's probability mass.
pub fn belief_samples(belief: &BeliefGrid, n: usize) -> Vec<[f32; 3]> {
    if n == 0 || !grid_ok(belief) { return Vec::new(); }
    peaks(belief, n, 2).into_iter().map(|(i, m)| { let c = cell_centre(belief, i); [c.x as f32, c.y as f32, m as f32] }).collect()
}

// ---------------------------------------------------------------------------------------------------------
// Picture reading
// ---------------------------------------------------------------------------------------------------------

/// The parts of the side picture the commander uses, extracted once per update.
struct PictureView {
    /// Live opposing surface or subsurface contacts (Suspect or Hostile), with their current 1-sigma radius.
    contacts: Vec<ContactView>,
    /// Live opposing weapons in flight: position and velocity.
    weapons: Vec<(DVec2, DVec2)>,
    /// Live opposing aircraft or drones.
    aircraft: Vec<DVec2>,
    /// Dropped opposing contacts still shown as last-known marks.
    marks: Vec<ContactView>,
    /// Own and allied hulls (live Friend surface or subsurface tracks): (own entity, position).
    own_hulls: Vec<(EntityId, DVec2)>,
    fired_on: bool,
}

#[derive(Clone, Copy)]
struct ContactView {
    id: TrackId,
    /// Ledger key for the sighting and emission counters only; never used for a decision.
    ledger: u64,
    pos: DVec2,
    vel: DVec2,
    sigma_m: f64,
    hostile: bool,
    grade: TrackGrade,
    bearing: Option<crate::tracks::BearingState>,
    age_s: f64,
    since_update_s: f64,
    emission: bool,
    last_update_tick: u64,
}

fn finite2(v: DVec2) -> bool { v.x.is_finite() && v.y.is_finite() }
fn xz(v: DVec3) -> DVec2 { DVec2::new(v.x, v.z) }

impl PictureView {
    fn new(picture: &SidePicture, now: u64, tuning: &content::tuning::TrackTuning) -> Self {
        let mut v = PictureView { contacts: Vec::new(), weapons: Vec::new(), aircraft: Vec::new(), marks: Vec::new(), own_hulls: Vec::new(), fired_on: false };
        for t in &picture.tracks {
            let pos = xz(t.pos);
            if !finite2(pos) { continue; }
            let vel = if finite2(xz(t.vel)) { xz(t.vel) } else { DVec2::ZERO };
            let ship_domain = matches!(t.domain, Domain::Surface | Domain::Subsurface);
            if t.stance == Stance::Friend {
                if t.is_live() && ship_domain { v.own_hulls.push((t.truth, pos)); }
                continue;
            }
            if t.stance == Stance::Neutral { continue; }
            if t.is_live() && t.fired_on_us { v.fired_on = true; }
            let cv = ContactView {
                id: t.id,
                ledger: ledger_key(t),
                pos,
                vel,
                sigma_m: track_sigma(t, now, tuning),
                hostile: t.stance == Stance::Hostile,
                grade: t.grade,
                bearing: t.bearing_only.filter(|b| b.origin.x.is_finite() && b.origin.z.is_finite() && b.bearing_rad.is_finite()),
                age_s: now.saturating_sub(t.created_tick) as f64 / TICKS_PER_S,
                since_update_s: now.saturating_sub(t.last_update_tick) as f64 / TICKS_PER_S,
                emission: t.emitting || t.sources.contains(crate::protocol::snapshot::SourceMask::ESM),
                last_update_tick: t.last_update_tick,
            };
            match (t.domain, t.is_live()) {
                (Domain::Surface | Domain::Subsurface, true) => v.contacts.push(cv),
                (Domain::Surface | Domain::Subsurface, false) => if cv.since_update_s <= tuning.last_known_s { v.marks.push(cv) },
                (Domain::Weapon, true) => if t.stance == Stance::Hostile || t.inbound { v.weapons.push((pos, vel)); v.fired_on = true; },
                (Domain::Air, true) => v.aircraft.push(pos),
                _ => {}
            }
        }
        v
    }
}

/// 1-sigma position radius of a track now: the growth law of L463 with the domain's sigma_a.
fn track_sigma(t: &Track, now: u64, tuning: &content::tuning::TrackTuning) -> f64 {
    let dt = now.saturating_sub(t.last_update_tick) as f64 / TICKS_PER_S;
    let sa = if t.bearing_only.is_some() { tuning.sigma_a[3] } else { match t.domain { Domain::Surface | Domain::Subsurface => tuning.sigma_a[0], Domain::Air => tuning.sigma_a[1], Domain::Weapon => tuning.sigma_a[2] } };
    let r0 = if t.r0_m.is_finite() { t.r0_m.max(0.0) } else { 0.0 };
    let sv = if t.sigma_v.is_finite() { t.sigma_v.max(0.0) } else { 0.0 };
    let a = 0.5 * sa * dt * dt;
    let r = (r0 * r0 + (sv * dt) * (sv * dt) + a * a).sqrt();
    if r.is_finite() { r } else { 1.0e6 }
}

// ---------------------------------------------------------------------------------------------------------
// Counters and losses
// ---------------------------------------------------------------------------------------------------------

/// Key the theatre ledger counts by: the opposing hull behind a track when the server knows it, else the
/// track number. Bookkeeping for the Regional Alert inputs only (L749 counts per contact, "holds LANTERN").
fn ledger_key(t: &Track) -> u64 { if t.truth.is_none() { (1u64 << 63) | t.id.0 as u64 } else { t.truth.to_u64() & !(1u64 << 63) } }

fn insert_sorted(v: &mut Vec<u64>, id: u64) -> bool {
    match v.binary_search(&id) { Ok(_) => false, Err(i) => { v.insert(i, id); true } }
}

/// Count sightings (a contact held as Suspect or Hostile for `SIGHTING_MIN_AGE_S`) and intercepted emissions,
/// once per opposing hull; refresh the last contact tick.
fn count_contacts(state: &mut CommanderState, view: &PictureView) {
    for c in &view.contacts {
        if c.age_s >= SIGHTING_MIN_AGE_S && insert_sorted(&mut state.mem.sighted, c.ledger) { state.sightings = state.sightings.saturating_add(1); }
        if c.emission && insert_sorted(&mut state.mem.emissions, c.ledger) { state.evidence = state.evidence.saturating_add(1); }
        let t = c.last_update_tick;
        state.last_contact_tick = Some(state.last_contact_tick.map_or(t, |l| l.max(t)));
    }
}

/// Fold the loss list in; returns true when an observed loss became known this update. Unobserved losses are
/// promoted from `pending_losses` once `LOSS_NOTICE_S` has passed, whether or not the caller lists them again.
fn absorb_losses(state: &mut CommanderState, own_losses: &[(EntityId, DVec3, bool)], now: u64) -> bool {
    let mut observed_now = false;
    let mut sorted: Vec<&(EntityId, DVec3, bool)> = own_losses.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for &&(vessel, pos, observed) in &sorted {
        if state.losses_known.binary_search(&vessel).is_ok() { continue; }
        let p = if finite2(xz(pos)) { Some(xz(pos)) } else { None };
        let pending = state.mem.pending_losses.binary_search_by(|e| e.0.cmp(&vessel));
        if observed {
            observed_now = true;
            if let Some(p) = p { state.mem.loss_marks.push((p, 8_000.0, 0.5, now)); }
            if let Ok(i) = pending { state.mem.pending_losses.remove(i); }
            let i = state.losses_known.partition_point(|v| *v < vessel);
            state.losses_known.insert(i, vessel);
        } else {
            match pending {
                Ok(i) => { if p.is_some() { state.mem.pending_losses[i].2 = p; } }
                Err(i) => state.mem.pending_losses.insert(i, (vessel, now, p)),
            }
        }
    }
    let mut i = 0;
    while i < state.mem.pending_losses.len() {
        let (vessel, since, p) = state.mem.pending_losses[i];
        if (now.saturating_sub(since) as f64) / TICKS_PER_S >= LOSS_NOTICE_S {
            state.mem.pending_losses.remove(i);
            state.evidence = state.evidence.saturating_add(1);
            if let Some(p) = p { state.mem.loss_marks.push((p, 12_000.0, 0.3, now)); }
            if let Err(k) = state.losses_known.binary_search(&vessel) { state.losses_known.insert(k, vessel); }
        } else {
            i += 1;
        }
    }
    state.mem.loss_marks.retain(|m| (now.saturating_sub(m.3) as f64) / TICKS_PER_S < LOSS_MARK_S);
    // Keep the newest few; older marks are already in the belief.
    if state.mem.loss_marks.len() > MAX_LOSS_MARKS { let n = state.mem.loss_marks.len() - MAX_LOSS_MARKS; state.mem.loss_marks.drain(..n); }
    observed_now
}

// ---------------------------------------------------------------------------------------------------------
// Belief grid
// ---------------------------------------------------------------------------------------------------------

fn grid_ok(b: &BeliefGrid) -> bool {
    b.cols > 0 && b.rows > 0 && b.p.len() == b.cols as usize * b.rows as usize && b.cell_m.is_finite() && b.cell_m > 0.0 && b.origin[0].is_finite() && b.origin[1].is_finite()
}

/// Rebuild a malformed grid (bad keyframe or default state) as a uniform one over the same area.
fn repair_grid(b: &BeliefGrid) -> BeliefGrid {
    let cell = if b.cell_m.is_finite() && b.cell_m > 0.0 { b.cell_m } else { 1_000.0 };
    let map = content::campaign::MapDef { size_m: [b.cols as f64 * cell, b.rows as f64 * cell], ..Default::default() };
    let mut g = new_belief(&map, cell);
    if b.origin[0].is_finite() && b.origin[1].is_finite() && b.cols > 0 && b.rows > 0 { g.origin = b.origin; }
    g
}

fn cell_centre(b: &BeliefGrid, i: usize) -> DVec2 {
    let c = (i % b.cols as usize) as f64;
    let r = (i / b.cols as usize) as f64;
    DVec2::new(b.origin[0] + (c + 0.5) * b.cell_m, b.origin[1] + (r + 0.5) * b.cell_m)
}

/// Spread the belief for `dt_s` seconds of opposing movement: per axis, each pass moves a fraction `a` of a
/// cell's mass to each neighbour, so the variance grows by 2 a cell^2 per pass and per axis.
fn diffuse(b: &mut BeliefGrid, dt_s: f64) {
    if !(dt_s > 0.0) { return; }
    let total = ASSUMED_SPEED_MPS * ASSUMED_SPEED_MPS * T_REF_S * dt_s / (2.0 * b.cell_m * b.cell_m);
    if !(total > 0.0) || !total.is_finite() { return; }
    let passes = ((total / MAX_BLUR).ceil() as u32).clamp(1, 64);
    let a = (total / passes as f64).min(MAX_BLUR) as f32;
    let (cols, rows) = (b.cols as usize, b.rows as usize);
    let mut tmp = vec![0.0f32; b.p.len()];
    for _ in 0..passes {
        for r in 0..rows {
            for c in 0..cols {
                let i = r * cols + c;
                let l = if c > 0 { b.p[i - 1] } else { b.p[i] };
                let rr = if c + 1 < cols { b.p[i + 1] } else { b.p[i] };
                // Reflecting edge: a missing neighbour returns the outflow to the cell itself.
                tmp[i] = (1.0 - 2.0 * a) * b.p[i] + a * l + a * rr;
            }
        }
        for r in 0..rows {
            for c in 0..cols {
                let i = r * cols + c;
                let up = if r > 0 { tmp[i - cols] } else { tmp[i] };
                let dn = if r + 1 < rows { tmp[i + cols] } else { tmp[i] };
                b.p[i] = (1.0 - 2.0 * a) * tmp[i] + a * up + a * dn;
            }
        }
    }
}

/// Radius inside which an own lookout would surely have seen a surface hull, metres.
fn assured_visual_m(ctx: &SimCtx<'_>) -> f64 {
    let s = &ctx.tuning.sensors;
    let night = if ctx.time_of_day == TimeOfDay::Night { s.night_visual_factor } else { 1.0 };
    let vis = if ctx.weather.visibility_m.is_finite() && ctx.weather.visibility_m > 0.0 { ctx.weather.visibility_m } else { s.visual_ref_range_m };
    let r = (s.visual_ref_range_m * night).min(vis) * NEG_RANGE_FRACTION;
    if r.is_finite() { r.max(0.0) } else { 0.0 }
}

/// Lower the belief around own hulls that see nothing: detection probability `NEG_PD` at the hull falling
/// linearly to zero at `see_m`. Water within one sigma of a live opposing contact is left alone.
fn negative_evidence(b: &mut BeliefGrid, view: &PictureView, see_m: f64) {
    if !(see_m > 0.0) { return; }
    for &(_, hull) in &view.own_hulls {
        let Some((c0, c1, r0, r1)) = cell_box(b, hull, see_m) else { continue };
        for r in r0..=r1 {
            for c in c0..=c1 {
                let i = r * b.cols as usize + c;
                let cc = cell_centre(b, i);
                let d = cc.distance(hull);
                if d > see_m { continue; }
                if view.contacts.iter().any(|k| cc.distance(k.pos) <= k.sigma_m.max(b.cell_m)) { continue; }
                b.p[i] *= (1.0 - NEG_PD * (1.0 - d / see_m)) as f32;
            }
        }
    }
}

/// Inclusive cell index box covering a disc, clipped to the grid; `None` when it misses the grid.
fn cell_box(b: &BeliefGrid, centre: DVec2, radius: f64) -> Option<(usize, usize, usize, usize)> {
    if !finite2(centre) || !radius.is_finite() || radius < 0.0 { return None; }
    let fx0 = ((centre.x - radius - b.origin[0]) / b.cell_m).floor();
    let fx1 = ((centre.x + radius - b.origin[0]) / b.cell_m).floor();
    let fz0 = ((centre.y - radius - b.origin[1]) / b.cell_m).floor();
    let fz1 = ((centre.y + radius - b.origin[1]) / b.cell_m).floor();
    let (cols, rows) = (b.cols as f64, b.rows as f64);
    if fx1 < 0.0 || fz1 < 0.0 || fx0 >= cols || fz0 >= rows { return None; }
    Some((fx0.max(0.0) as usize, fx1.min(cols - 1.0) as usize, fz0.max(0.0) as usize, fz1.min(rows - 1.0) as usize))
}

/// One positive evidence source.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// Gaussian blob: centre, 1-sigma radius.
    Blob { at: DVec2, sigma: f64 },
    /// Wedge from an origin along a compass bearing with angular sigma, times a radial Gaussian.
    Wedge { origin: DVec2, bearing: f64, sigma_rad: f64, range: f64, sigma_range: f64 },
}

fn evidence_sources(state: &CommanderState, view: &PictureView, now: u64) -> Vec<(Source, f64)> {
    let mut out = Vec::new();
    let mut bearings = 0usize;
    for c in &view.contacts {
        let stance = if c.hostile { 1.0 } else { 0.7 };
        match c.bearing {
            Some(_) if bearings >= MAX_BEARING_SOURCES => {}
            Some(b) => {
                bearings += 1;
                let range = if b.range_m.is_finite() && b.range_m > 0.0 { b.range_m.min(60_000.0) } else { 20_000.0 };
                let sr = if b.sigma_range_m.is_finite() { b.sigma_range_m } else { range };
                let sb = if b.sigma_bearing_rad.is_finite() { b.sigma_bearing_rad } else { 0.1 };
                out.push((Source::Wedge { origin: xz(b.origin), bearing: b.bearing_rad, sigma_rad: sb.max(0.0175), range, sigma_range: sr.clamp(0.5 * range, MAX_SOURCE_SIGMA_M.max(0.5 * range)) }, 0.25 * stance));
            }
            None => {
                let w = match c.grade { TrackGrade::Lock | TrackGrade::Fine => 0.6, TrackGrade::Coarse => 0.45, TrackGrade::Bearing => 0.3 };
                out.push((Source::Blob { at: c.pos, sigma: c.sigma_m.min(MAX_SOURCE_SIGMA_M) }, w * stance));
            }
        }
    }
    for &(pos, vel) in view.weapons.iter().take(MAX_WEAPON_SOURCES) {
        if vel.length() > 1.0 {
            // Compass bearing of -vel: (sin b, -cos b) = -vel / |vel|.
            let back = libm::atan2(-vel.x, vel.y);
            out.push((Source::Wedge { origin: pos, bearing: back, sigma_rad: 0.14, range: 10_000.0, sigma_range: 6_000.0 }, 0.2));
        }
    }
    for &pos in view.aircraft.iter().take(MAX_AIR_SOURCES) { out.push((Source::Blob { at: pos, sigma: 8_000.0 }, 0.1)); }
    for m in view.marks.iter().take(MAX_MARK_SOURCES) {
        let el = m.since_update_s.min(600.0);
        let at = m.pos + m.vel * el;
        let at = if finite2(at) { at } else { m.pos };
        let stance = if m.hostile { 1.0 } else { 0.7 };
        out.push((Source::Blob { at, sigma: m.sigma_m.max(ASSUMED_SPEED_MPS * el * 0.5).min(MAX_SOURCE_SIGMA_M) }, 0.15 * stance));
    }
    for &(pos, sigma, w, t) in &state.mem.loss_marks {
        let age = now.saturating_sub(t) as f64 / TICKS_PER_S;
        let fade = (1.0 - age / LOSS_MARK_S).max(0.0);
        out.push((Source::Blob { at: pos, sigma: (sigma + ASSUMED_SPEED_MPS * age * 0.5).min(MAX_SOURCE_SIGMA_M) }, w * fade));
    }
    out.retain(|(_, w)| *w > 0.0);
    out
}

/// Mix the sources into the belief: p' = (1 - W) p + W E, with W = 1 - prod(1 - w_i) capped at 0.9 and E the
/// weight-averaged normalised source distributions. Returns the cells evaluated (work units).
fn apply_evidence(b: &mut BeliefGrid, sources: &[(Source, f64)]) -> u32 {
    let mut work = 0u32;
    if sources.is_empty() { return work; }
    let mut keep = 1.0f64;
    let mut sum_w = 0.0f64;
    let mut e = vec![0.0f64; b.p.len()];
    let mut scratch: Vec<(usize, f64)> = Vec::new();
    let half = b.cell_m * 0.5;
    for &(src, w) in sources {
        scratch.clear();
        match src {
            Source::Blob { at, sigma } => {
                if !sigma.is_finite() { continue; }
                let s = sigma.max(half);
                let s2 = s * s;
                if let Some((c0, c1, r0, r1)) = cell_box(b, at, 3.0 * s) {
                    for r in r0..=r1 { for c in c0..=c1 {
                        let i = r * b.cols as usize + c;
                        scratch.push((i, libm::exp(-0.5 * cell_centre(b, i).distance_squared(at) / s2)));
                    } }
                }
            }
            Source::Wedge { origin, bearing, sigma_rad, range, sigma_range } => {
                let rmax = (range + 3.0 * sigma_range).min(150_000.0);
                let sr = sigma_range.max(half);
                if let Some((c0, c1, r0, r1)) = cell_box(b, origin, rmax) {
                    for r in r0..=r1 { for c in c0..=c1 {
                        let i = r * b.cols as usize + c;
                        let d = cell_centre(b, i) - origin;
                        let dist = d.length();
                        if dist > rmax { continue; }
                        let db = wrap_pi(libm::atan2(d.x, -d.y) - bearing);
                        // The angular spread is at least the half cell seen from this range.
                        let sa = sigma_rad.max(if dist > 0.0 { half / dist } else { core::f64::consts::PI });
                        let dr = (dist - range) / sr;
                        scratch.push((i, libm::exp(-0.5 * (db / sa) * (db / sa) - 0.5 * dr * dr)));
                    } }
                }
            }
        }
        work = work.saturating_add(scratch.len() as u32);
        let total: f64 = scratch.iter().map(|x| x.1).sum();
        if !(total > 0.0) || !total.is_finite() { continue; }
        for &(i, g) in &scratch { e[i] += w * g / total; }
        keep *= 1.0 - w.clamp(0.0, 1.0);
        sum_w += w;
    }
    if !(sum_w > 0.0) { return work; }
    let big_w = (1.0 - keep).min(0.9);
    for (p, ei) in b.p.iter_mut().zip(e.iter()) { *p = ((1.0 - big_w) * *p as f64 + big_w * ei / sum_w) as f32; }
    work
}

fn wrap_pi(a: f64) -> f64 {
    let t = core::f64::consts::TAU;
    let mut x = a % t;
    if x > core::f64::consts::PI { x -= t; }
    if x < -core::f64::consts::PI { x += t; }
    x
}

/// Renormalise to unit mass with a small uniform floor; a dead or non-finite grid resets to uniform.
fn normalise(b: &mut BeliefGrid) {
    let n = b.p.len();
    if n == 0 { return; }
    let mut sum = 0.0f64;
    for p in &b.p { if p.is_finite() && *p > 0.0 { sum += *p as f64; } }
    if !(sum > 0.0) || !sum.is_finite() { for p in &mut b.p { *p = 1.0 / n as f32; } return; }
    let floor = FLOOR / n as f64;
    for p in &mut b.p {
        let v = if p.is_finite() && *p > 0.0 { *p as f64 / sum } else { 0.0 };
        *p = ((1.0 - FLOOR) * v + floor) as f32;
    }
}

/// Peaks of the 3 x 3 mass: (cell index, mass), highest first, suppressing peaks within `suppress` cells of a
/// higher one.
fn peaks(b: &BeliefGrid, n: usize, suppress: usize) -> Vec<(usize, f64)> {
    let (cols, rows) = (b.cols as usize, b.rows as usize);
    let mut s = vec![0.0f64; b.p.len()];
    for r in 0..rows {
        for c in 0..cols {
            let mut m = 0.0f64;
            for rr in r.saturating_sub(1)..=(r + 1).min(rows - 1) {
                for cc in c.saturating_sub(1)..=(c + 1).min(cols - 1) { let v = b.p[rr * cols + cc]; if v.is_finite() && v > 0.0 { m += v as f64; } }
            }
            s[r * cols + c] = m;
        }
    }
    let mut cand: Vec<(usize, f64)> = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let i = r * cols + c;
            if !(s[i] > 0.0) { continue; }
            let mut peak = true;
            'n: for rr in r.saturating_sub(1)..=(r + 1).min(rows - 1) {
                for cc in c.saturating_sub(1)..=(c + 1).min(cols - 1) {
                    let j = rr * cols + cc;
                    if j != i && (s[j] > s[i] || (s[j] == s[i] && j < i)) { peak = false; break 'n; }
                }
            }
            if peak { cand.push((i, s[i])); }
        }
    }
    cand.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out: Vec<(usize, f64)> = Vec::new();
    for (i, m) in cand {
        if out.len() >= n { break; }
        let (r, c) = (i / cols, i % cols);
        if out.iter().any(|&(j, _)| r.abs_diff(j / cols) <= suppress && c.abs_diff(j % cols) <= suppress) { continue; }
        out.push((i, m));
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Alert
// ---------------------------------------------------------------------------------------------------------

/// Raise at once to what the picture shows; drop one level at a time after a quiet spell. Known losses or
/// evidence keep the side at least Suspicious for the rest of the sortie.
fn update_alert(state: &mut CommanderState, view: &PictureView, observed_loss_now: bool, now: u64) {
    let floor = if !state.losses_known.is_empty() || state.evidence > 0 { SortieAlert::Suspicious } else { SortieAlert::Unaware };
    let seen = if view.fired_on || observed_loss_now {
        SortieAlert::Engaged
    } else if view.contacts.iter().any(|c| c.hostile || c.age_s >= SUSPECT_ALERT_AGE_S) {
        SortieAlert::Alerted
    } else if !view.contacts.is_empty() || !view.marks.is_empty() || !view.aircraft.is_empty() {
        SortieAlert::Suspicious
    } else {
        SortieAlert::Unaware
    };
    let target = seen.max(floor);
    if target >= state.alert {
        state.alert = target;
        state.mem.level_held_tick = now;
        return;
    }
    let quiet_s = now.saturating_sub(state.mem.level_held_tick) as f64 / TICKS_PER_S;
    let need = match state.alert { SortieAlert::Engaged => DECAY_ENGAGED_S, SortieAlert::Alerted => DECAY_ALERTED_S, SortieAlert::Suspicious => DECAY_SUSPICIOUS_S, SortieAlert::Unaware => f64::INFINITY };
    if quiet_s >= need {
        let next = match state.alert { SortieAlert::Engaged => SortieAlert::Alerted, SortieAlert::Alerted => SortieAlert::Suspicious, _ => SortieAlert::Unaware };
        state.alert = next.max(target);
        state.mem.level_held_tick = now;
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tasking
// ---------------------------------------------------------------------------------------------------------

/// Groups the commander may send hunting: task groups, patrols, hunter-killers and submarine patrols. Escorts
/// stay with their charge, convoys keep their lane, garrisons and air patrols keep their stations.
fn huntable(g: &GroupState, side: SideId) -> bool {
    if g.side != side || g.members.is_empty() { return false; }
    let orders_ok = !matches!(g.orders, Some(GroupOrders::Escort { .. } | GroupOrders::Anchor | GroupOrders::AirPatrol { .. } | GroupOrders::Flee { .. }));
    let kind_ok = match g.kind {
        Some(k) => matches!(k, FleetKind::TaskGroup | FleetKind::Patrol | FleetKind::HunterKiller | FleetKind::SubmarinePatrol),
        None => matches!(g.orders, Some(GroupOrders::Patrol { .. } | GroupOrders::Guard { .. } | GroupOrders::Hunt | GroupOrders::Search { .. } | GroupOrders::Transit { .. })),
    };
    orders_ok && kind_ok
}

fn is_commander_order(o: &Option<GroupOrders>) -> bool { matches!(o, Some(GroupOrders::Hunt | GroupOrders::Search { .. })) }

/// Where a group is: its leader's Friend track, else the first live member's, else its route start.
fn group_pos(g: &GroupState, own: &[(EntityId, DVec2)]) -> Option<DVec2> {
    let find = |e: EntityId| own.iter().find(|(o, _)| *o == e).map(|(_, p)| *p);
    g.leader.and_then(find).or_else(|| g.members.iter().find_map(|m| find(*m))).or_else(|| g.route.waypoints.first().copied().filter(|p| finite2(*p)))
}

fn task_groups(state: &mut CommanderState, view: &PictureView, groups: &[GroupState]) -> Vec<(GroupId, GroupOrders)> {
    let side = state.side;
    let mut out: Vec<(GroupId, GroupOrders)> = Vec::new();
    let mut gs: Vec<&GroupState> = groups.iter().filter(|g| huntable(g, side)).collect();
    gs.sort_by_key(|g| g.id);
    if gs.is_empty() { return out; }

    // Remember standing orders the first time the commander takes a group; give them back on stand-down.
    fn take(mem: &mut CommanderMemory, g: &GroupState) {
        // The first orders seen are the standing ones, whatever they are (a hunter-killer may start on Hunt).
        if let (Some(o), Err(i)) = (&g.orders, mem.standing.binary_search_by(|(id, _)| id.cmp(&g.id))) { mem.standing.insert(i, (g.id, o.clone())); }
    }
    fn restore(mem: &mut CommanderMemory, g: &GroupState, out: &mut Vec<(GroupId, GroupOrders)>) {
        if let Ok(i) = mem.standing.binary_search_by(|(id, _)| id.cmp(&g.id)) {
            let (_, o) = mem.standing.remove(i);
            if g.orders.as_ref() != Some(&o) { out.push((g.id, o)); }
        }
    }

    if state.alert == SortieAlert::Unaware {
        for g in &gs { restore(&mut state.mem, g, &mut out); }
        return out;
    }

    // An opposing contact (not a bare bearing) near own hulls: every huntable group prosecutes it.
    let near_contact = view.contacts.iter().filter(|c| c.bearing.is_none()).any(|c| view.own_hulls.iter().any(|(_, h)| h.distance(c.pos) <= HUNT_RANGE_M));
    if state.alert >= SortieAlert::Alerted && near_contact {
        for g in &gs {
            if g.orders != Some(GroupOrders::Hunt) { take(&mut state.mem, g); out.push((g.id, GroupOrders::Hunt)); }
        }
        return out;
    }

    // Otherwise search distinct belief peaks: one group while Suspicious, all huntable groups above that.
    let uniform3 = 9.0 / state.belief.p.len().max(1) as f64;
    let want = if state.alert == SortieAlert::Suspicious { 1 } else { gs.len() };
    let pk: Vec<DVec2> = peaks(&state.belief, want, 3).into_iter().filter(|&(_, m)| m >= PEAK_CONCENTRATION * uniform3).map(|(i, _)| cell_centre(&state.belief, i)).collect();
    let radius = 2.0 * state.belief.cell_m + 3_000.0;
    let positions: Vec<Option<DVec2>> = gs.iter().map(|g| group_pos(g, &view.own_hulls)).collect();
    let mut assigned: Vec<Option<DVec2>> = vec![None; gs.len()];
    for &at in &pk {
        // Highest peak first, to the nearest unassigned group; a group already searching near it keeps it.
        let mut best: Option<(usize, f64)> = None;
        for (k, g) in gs.iter().enumerate() {
            if assigned[k].is_some() { continue; }
            let cur = match &g.orders { Some(GroupOrders::Search { at: a, .. }) => Some(*a), _ => None };
            let d = match (cur, positions[k]) {
                (Some(a), _) if a.distance(at) <= RETASK_HYSTERESIS_M => -1.0,
                (_, Some(p)) => p.distance(at),
                _ => 1.0e12 + k as f64,
            };
            if best.map_or(true, |(_, bd)| d < bd) { best = Some((k, d)); }
        }
        if let Some((k, _)) = best { assigned[k] = Some(at); }
    }
    for (k, g) in gs.iter().enumerate() {
        match assigned[k] {
            Some(at) => {
                let keep = matches!(&g.orders, Some(GroupOrders::Search { at: a, .. }) if a.distance(at) <= RETASK_HYSTERESIS_M);
                if !keep { take(&mut state.mem, g); out.push((g.id, GroupOrders::Search { at, radius_m: radius })); }
            }
            None => {
                // No peak for this group: one left hunting a contact that is gone, or any extra searcher while
                // merely Suspicious, goes back to its standing orders.
                if g.orders == Some(GroupOrders::Hunt) || (state.alert == SortieAlert::Suspicious && is_commander_order(&g.orders)) { restore(&mut state.mem, g, &mut out); }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::protocol::snapshot::{SourceMask, VerticalTrend};
    use content::campaign::{DifficultySliders, MapDef};

    pub(crate) fn track(id: u32, pos: DVec2, stance: Stance, domain: Domain) -> Track {
        Track {
            id: TrackId(id), side: SideId(1), domain, truth: EntityId::NONE, pos: DVec3::new(pos.x, 0.0, pos.y), vel: DVec3::ZERO,
            r0_m: 200.0, sigma_v: 1.0, bearing_only: None, created_tick: 0, last_update_tick: 0, last_lock_tick: None, dropped_tick: None,
            grade: TrackGrade::Fine, sources: SourceMask::SEARCH_RADAR, trend: VerticalTrend::default(), rung: IdRung::Unknown, rung_reason: RungReason::default(),
            candidates: Default::default(), class: None, history: Vec::new(), rcs_m2: 0.0, consistent_ticks: 0, emitters: Vec::new(), id_override: None,
            stance, stance_reason: StanceReason::default(), stance_override: None, fired_on_us: false, inbound: false, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
        }
    }

    pub(crate) struct Env { pub cat: Catalogue, pub tuning: TuningTable, pub weather: WeatherState, pub diff: DifficultySliders }
    impl Env {
        pub(crate) fn new() -> Self { Env { cat: Catalogue::default(), tuning: TuningTable::default(), weather: WeatherState::default(), diff: DifficultySliders::NORMAL } }
        pub(crate) fn ctx(&self, tick: u64) -> SimCtx<'_> {
            SimCtx { seed: 7, tick: Tick(tick), catalogue: &self.cat, tuning: &self.tuning, weather: &self.weather, time_of_day: TimeOfDay::Day, layer_depth_m: 50.0, difficulty: &self.diff }
        }
    }

    fn map() -> MapDef { MapDef { size_m: [100_000.0, 100_000.0], depth_m: 200.0, ..Default::default() } }
    fn commander() -> CommanderState { CommanderState { side: SideId(1), belief: new_belief(&map(), 1_000.0), ..Default::default() } }
    fn pic(tracks: Vec<Track>) -> SidePicture { SidePicture { side: SideId(1), tracks, next_id: 100 } }
    fn mass_near(b: &BeliefGrid, at: DVec2, r: f64) -> f64 { (0..b.p.len()).filter(|&i| cell_centre(b, i).distance(at) <= r).map(|i| b.p[i] as f64).sum() }
    fn friend(id: u32, entity: u64, pos: DVec2) -> Track { let mut t = track(id, pos, Stance::Friend, Domain::Surface); t.truth = EntityId::from_u64(entity); t }
    fn group(id: u16, leader: u64) -> GroupState {
        let e = EntityId::from_u64(leader);
        GroupState { id: GroupId(id), side: SideId(1), kind: Some(FleetKind::TaskGroup), orders: Some(GroupOrders::Patrol { markers: vec!["a".into()] }), members: vec![e], leader: Some(e), ..Default::default() }
    }

    #[test]
    fn new_belief_geometry_and_guards() {
        let b = new_belief(&map(), 1_000.0);
        assert_eq!((b.cols, b.rows), (100, 100));
        assert_eq!(b.origin, [-50_000.0, -50_000.0]);
        assert!((b.p.iter().map(|&p| p as f64).sum::<f64>() - 1.0).abs() < 1e-4);
        assert_eq!(cell_centre(&b, 0), DVec2::new(-49_500.0, -49_500.0));
        let small = new_belief(&map(), 1.0);
        assert!(small.cols as usize <= MAX_CELLS_EDGE && small.rows as usize <= MAX_CELLS_EDGE && grid_ok(&small));
        assert!(grid_ok(&new_belief(&map(), f64::NAN)));
        assert_eq!(new_belief(&MapDef::default(), 1_000.0).p.len(), 1);
    }

    #[test]
    fn diffusion_conserves_mass_and_spreads_evenly() {
        let mut b = new_belief(&map(), 1_000.0);
        for p in &mut b.p { *p = 0.0; }
        let mid = 50 * 100 + 50;
        b.p[mid] = 1.0;
        for _ in 0..60 { diffuse(&mut b, 1.0); }
        let sum: f64 = b.p.iter().map(|&p| p as f64).sum();
        assert!((sum - 1.0).abs() < 1e-4, "sum {sum}");
        assert!(b.p[mid] < 0.5 && b.p[mid + 1] > 0.0 && b.p[mid + 100] > 0.0);
        assert!((b.p[mid + 1] - b.p[mid - 1]).abs() < 1e-6 && (b.p[mid + 100] - b.p[mid - 100]).abs() < 1e-6);
    }

    #[test]
    fn hostile_track_pulls_belief_and_raises_alert() {
        let env = Env::new();
        let mut c = commander();
        let at = DVec2::new(20_000.0, -10_000.0);
        let p = pic(vec![track(5, at, Stance::Hostile, Domain::Surface)]);
        for s in 1..=5 { update(&mut c, &p, &[], &[], &env.ctx(s * 60)); }
        assert!(mass_near(&c.belief, at, 3_000.0) > 0.8, "mass {}", mass_near(&c.belief, at, 3_000.0));
        assert_eq!(c.alert, SortieAlert::Alerted);
        assert!(c.reinforcements_called);
        let s = belief_samples(&c.belief, 3);
        assert!(!s.is_empty());
        assert!(DVec2::new(s[0][0] as f64, s[0][1] as f64).distance(at) < 1_500.0);
        assert!(s.windows(2).all(|w| w[0][2] >= w[1][2]));
    }

    #[test]
    fn sightings_and_emissions_count_once() {
        let env = Env::new();
        let mut c = commander();
        let mut t = track(3, DVec2::new(1_000.0, 1_000.0), Stance::Suspect, Domain::Surface);
        t.emitting = true;
        let p = pic(vec![t]);
        update(&mut c, &p, &[], &[], &env.ctx(60));
        assert_eq!((c.sightings, c.evidence), (0, 1));
        for s in 2..20 { update(&mut c, &p, &[], &[], &env.ctx(s * 60)); }
        assert_eq!((c.sightings, c.evidence), (1, 1));
        let mut c2 = commander();
        let p2 = pic(vec![track(1, DVec2::ZERO, Stance::Neutral, Domain::Surface), friend(2, 9, DVec2::ZERO)]);
        for s in 1..30 { update(&mut c2, &p2, &[], &[], &env.ctx(s * 60)); }
        assert_eq!((c2.sightings, c2.evidence, c2.alert), (0, 0, SortieAlert::Unaware));
    }

    #[test]
    fn own_hull_clears_nearby_water() {
        let env = Env::new();
        let mut c = commander();
        let own = DVec2::new(-20_000.0, 0.0);
        let p = pic(vec![friend(1, 1, own)]);
        let before = mass_near(&c.belief, own, 3_000.0);
        for s in 1..=10 { update(&mut c, &p, &[], &[], &env.ctx(s * 60)); }
        assert!(mass_near(&c.belief, own, 3_000.0) < 0.3 * before);
    }

    #[test]
    fn esm_wedge_points_along_bearing() {
        let env = Env::new();
        let mut c = commander();
        let mut t = track(9, DVec2::new(0.0, -20_000.0), Stance::Hostile, Domain::Surface);
        // Bearing 0 (north, world -Z) from the origin, range guess 20 km.
        t.bearing_only = Some(crate::tracks::BearingState { origin: DVec3::ZERO, bearing_rad: 0.0, bearing_rate: 0.0, sigma_bearing_rad: 0.03, range_m: 20_000.0, sigma_range_m: 10_000.0, sensors: 1 });
        t.grade = TrackGrade::Bearing;
        let p = pic(vec![t]);
        for s in 1..=5 { update(&mut c, &p, &[], &[], &env.ctx(s * 60)); }
        let north = mass_near(&c.belief, DVec2::new(0.0, -20_000.0), 8_000.0);
        let south = mass_near(&c.belief, DVec2::new(0.0, 20_000.0), 8_000.0);
        let east = mass_near(&c.belief, DVec2::new(20_000.0, 0.0), 8_000.0);
        assert!(north > 5.0 * south && north > 5.0 * east, "north {north} south {south} east {east}");
    }

    #[test]
    fn losses_observed_and_unobserved() {
        let env = Env::new();
        let mut c = commander();
        let lost = EntityId::from_u64(42);
        let losses = [(lost, DVec3::new(5_000.0, 0.0, 5_000.0), false)];
        update(&mut c, &pic(vec![]), &[], &losses, &env.ctx(60));
        assert!(c.losses_known.is_empty());
        update(&mut c, &pic(vec![]), &[], &losses, &env.ctx(60 + 181 * 60));
        assert_eq!((c.losses_known.clone(), c.evidence), (vec![lost], 1));
        assert!(c.alert >= SortieAlert::Suspicious);
        // Listed once only: still noticed later.
        let mut c3 = commander();
        update(&mut c3, &pic(vec![]), &[], &losses, &env.ctx(60));
        update(&mut c3, &pic(vec![]), &[], &[], &env.ctx(60 + 181 * 60));
        assert_eq!((c3.losses_known.clone(), c3.evidence), (vec![lost], 1));
        let mut c2 = commander();
        update(&mut c2, &pic(vec![]), &[], &[(lost, DVec3::ZERO, true)], &env.ctx(60));
        assert_eq!((c2.alert, c2.evidence), (SortieAlert::Engaged, 0));
    }

    #[test]
    fn alert_decays_after_quiet_spell() {
        let env = Env::new();
        let mut c = CommanderState { side: SideId(1), belief: new_belief(&MapDef { size_m: [10_000.0, 10_000.0], ..Default::default() }, 1_000.0), ..Default::default() };
        let mut t = track(1, DVec2::ZERO, Stance::Hostile, Domain::Surface);
        t.fired_on_us = true;
        update(&mut c, &pic(vec![t]), &[], &[], &env.ctx(60));
        assert_eq!(c.alert, SortieAlert::Engaged);
        let quiet = pic(vec![]);
        let mut tick = 60;
        for _ in 0..121 { tick += 60; update(&mut c, &quiet, &[], &[], &env.ctx(tick)); }
        assert_eq!(c.alert, SortieAlert::Alerted);
        for _ in 0..(600 + 900 + 5) { tick += 60; update(&mut c, &quiet, &[], &[], &env.ctx(tick)); }
        assert_eq!(c.alert, SortieAlert::Unaware);
    }

    #[test]
    fn tasking_hunts_then_searches_then_stands_down() {
        let env = Env::new();
        let mut c = commander();
        let own = friend(1, 1, DVec2::new(-30_000.0, 0.0));
        let g = group(0, 1);
        let p = pic(vec![own.clone(), track(2, DVec2::ZERO, Stance::Hostile, Domain::Surface)]);
        assert_eq!(update(&mut c, &p, &[g.clone()], &[], &env.ctx(60)), vec![(GroupId(0), GroupOrders::Hunt)]);
        // Contact lost: the group searches the belief peak where it was.
        let mut hunting = g.clone();
        hunting.orders = Some(GroupOrders::Hunt);
        let o = update(&mut c, &pic(vec![own.clone()]), &[hunting.clone()], &[], &env.ctx(120));
        let at = match o.as_slice() { [(GroupId(0), GroupOrders::Search { at, .. })] => *at, other => panic!("{other:?}") };
        assert!(at.length() < 5_000.0, "at {at}");
        // Same peak next second: no new order.
        let mut searching = g.clone();
        searching.orders = Some(GroupOrders::Search { at, radius_m: 5_000.0 });
        assert!(update(&mut c, &pic(vec![own.clone()]), &[searching.clone()], &[], &env.ctx(180)).is_empty());
        // Unaware again: standing orders come back.
        c.alert = SortieAlert::Unaware;
        let view = PictureView::new(&pic(vec![]), 0, &env.tuning.tracks);
        assert_eq!(task_groups(&mut c, &view, &[searching]), vec![(GroupId(0), GroupOrders::Patrol { markers: vec!["a".into()] })]);
        // Other sides' groups and escorts are never re-tasked.
        let mut other = group(1, 1);
        other.side = SideId(2);
        let mut esc = group(2, 1);
        esc.kind = Some(FleetKind::Escort);
        c.alert = SortieAlert::Engaged;
        let view = PictureView::new(&p, 0, &env.tuning.tracks);
        assert!(task_groups(&mut c, &view, &[other, esc]).is_empty());
    }

    #[test]
    fn opposing_truth_never_changes_output() {
        let env = Env::new();
        let run = |truth: u64| {
            let mut c = commander();
            let mut t = track(2, DVec2::new(10_000.0, 0.0), Stance::Hostile, Domain::Surface);
            t.truth = EntityId::from_u64(truth);
            let p = pic(vec![friend(1, 1, DVec2::ZERO), t]);
            let o = update(&mut c, &p, &[group(0, 1)], &[], &env.ctx(60));
            (c.alert, c.belief, c.reinforcements_called, o)
        };
        assert_eq!(run(3), run(99));
    }

    #[test]
    fn same_hull_regained_counts_one_sighting() {
        let env = Env::new();
        let mut c = commander();
        let mut a = track(3, DVec2::new(1_000.0, 1_000.0), Stance::Suspect, Domain::Surface);
        a.truth = EntityId::from_u64(77);
        for s in 11..14 { update(&mut c, &pic(vec![a.clone()]), &[], &[], &env.ctx(s * 60)); }
        let mut b = a.clone();
        b.id = TrackId(8);
        b.created_tick = 20 * 60;
        for s in 31..34 { update(&mut c, &pic(vec![b.clone()]), &[], &[], &env.ctx(s * 60)); }
        assert_eq!(c.sightings, 1);
    }

    #[test]
    fn standing_hunt_comes_back() {
        let env = Env::new();
        let mut c = commander();
        let mut g = group(0, 1);
        g.kind = Some(FleetKind::HunterKiller);
        g.orders = Some(GroupOrders::Hunt);
        c.alert = SortieAlert::Alerted;
        // A sharp peak far from the group, no contacts: search it.
        for p in &mut c.belief.p { *p = 0.0; }
        c.belief.p[20 * 100 + 20] = 1.0;
        let view = PictureView::new(&pic(vec![friend(1, 1, DVec2::ZERO)]), 0, &env.tuning.tracks);
        let o = task_groups(&mut c, &view, &[g.clone()]);
        assert!(matches!(o.as_slice(), [(GroupId(0), GroupOrders::Search { .. })]), "{o:?}");
        g.orders = Some(o[0].1.clone());
        c.alert = SortieAlert::Unaware;
        assert_eq!(task_groups(&mut c, &view, &[g]), vec![(GroupId(0), GroupOrders::Hunt)]);
    }

    #[test]
    fn salvo_update_work_is_bounded() {
        let env = Env::new();
        let mut c = commander();
        let mut tracks = vec![friend(1, 1, DVec2::ZERO)];
        for k in 0..8u32 {
            let mut w = track(10 + k, DVec2::new(1_000.0 * k as f64, -20_000.0), Stance::Hostile, Domain::Weapon);
            w.vel = DVec3::new(0.0, 0.0, 280.0);
            w.inbound = true;
            tracks.push(w);
        }
        for k in 0..6u32 { tracks.push(track(30 + k, DVec2::new(-10_000.0, 2_000.0 * k as f64), Stance::Hostile, Domain::Air)); }
        let p = pic(tracks);
        let view = PictureView::new(&p, 60, &env.tuning.tracks);
        let sources = evidence_sources(&c, &view, 60);
        let work = apply_evidence(&mut c.belief, &sources);
        assert!(work <= 15_000, "cells evaluated {work}");
        update(&mut c, &p, &[], &[], &env.ctx(60));
        assert_eq!(c.alert, SortieAlert::Engaged);
    }

    #[test]
    fn bad_input_never_panics() {
        let env = Env::new();
        let mut c = CommanderState::default();
        let mut t = track(1, DVec2::new(f64::NAN, 0.0), Stance::Hostile, Domain::Surface);
        t.r0_m = f64::INFINITY;
        let mut u = track(2, DVec2::new(1.0e12, -1.0e12), Stance::Hostile, Domain::Weapon);
        u.vel = DVec3::new(f64::NAN, 0.0, 1.0);
        let mut w = track(3, DVec2::new(100.0, 0.0), Stance::Suspect, Domain::Surface);
        w.r0_m = f64::NAN;
        w.last_update_tick = u64::MAX;
        update(&mut c, &pic(vec![t, u, w]), &[], &[(EntityId::from_u64(1), DVec3::splat(f64::NAN), true)], &env.ctx(60));
        assert!(grid_ok(&c.belief) && c.belief.p.iter().all(|p| p.is_finite()));
        assert!(belief_samples(&BeliefGrid::default(), 4).is_empty());
        assert!(belief_samples(&c.belief, 0).is_empty());
    }
}
