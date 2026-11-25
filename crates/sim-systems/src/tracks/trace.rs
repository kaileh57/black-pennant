//! Test-only trace of the association pass, for the L467 duplicate measurement (`probe::duplicate_trace`):
//! why a detection of an object that already holds a track started another, and why two tracks of one object
//! were not merged. It reads `Track::truth` and `Detection::target`, which association itself never does.
//! Nothing here is compiled outside tests.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ON: AtomicBool = AtomicBool::new(false);
static LOG: Mutex<Log> = Mutex::new(Log { births: Vec::new(), pairs: BTreeMap::new() });

/// Why a detection did not join an existing track of its own object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NoJoin {
    /// The detection's domain hint and the track's domain disagree (air against surface).
    Domain,
    /// A lock-kind fix with a sea-level height against an air or weapon track.
    LockHeight,
    /// A bearing whose ray points away from the track.
    Behind,
    /// Outside the association gate; the distance in sigma, rounded down (capped at 99).
    Beyond(u8),
    /// Inside the gate, but this sensor already gave that track a report this instant (ranged fixes only).
    Blocked,
    /// Inside the gate and free, yet the spatial index did not offer it.
    IndexMiss,
    /// Inside the gate and free: another track was nearer (the detection joined that one, so no birth).
    Nearer,
}

/// One track born of a detection whose object already held a live track in the picture.
#[derive(Clone, Debug)]
pub(crate) struct Birth { pub side: SideId, pub tick: u64, pub id: TrackId, pub target: EntityId, pub kind: SensorKind, pub ranged: bool, pub hint: Option<Domain>, pub existing: Vec<(TrackId, Domain, bool, NoJoin)> }

/// Why a pair of live tracks of one object was left unmerged at one update.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NoMerge { NeitherUpdated, Domain, Class, SameSensorInstant, Beyond(u8), Singular, WouldMerge }

pub(crate) struct Log { pub births: Vec<Birth>, pub pairs: BTreeMap<(u8, EntityId, NoMerge), u64> }

pub(crate) fn set(on: bool) { ON.store(on, Ordering::SeqCst); }
pub(crate) fn on() -> bool { ON.load(Ordering::Relaxed) }
pub(crate) fn take() -> Log { core::mem::replace(&mut *LOG.lock().expect("trace log"), Log { births: Vec::new(), pairs: BTreeMap::new() }) }

fn sigma_bucket(d2: f64) -> u8 { d2.max(0.0).sqrt().floor().min(99.0) as u8 }

/// Called where a detection starts a track (before the track is pushed).
pub(super) fn birth(picture: &SidePicture, id: TrackId, m: &Meas<'_>, cand: &[usize], taken: &[Vec<(SensorRef, u64)>], tt: &TrackTuning, gate2: f64) {
    let key = (m.det.sensor, m.det.tick);
    let mut existing = Vec::new();
    for (ti, t) in picture.tracks.iter().enumerate() {
        if !t.is_live() || t.is_friend() || t.truth != m.det.target { continue; }
        let why = if !compatible(m.det.domain_hint, t.domain) { NoJoin::Domain }
            else if m.height.is_some() && m.det.domain_hint.is_none() && air_like(t.domain) && is_lock_kind(m.det.kind) { NoJoin::LockHeight }
            else {
                match gate_d2(m, t, tt, 1.0e8) {
                    None => NoJoin::Behind,
                    Some(d2) if d2 > gate2 => NoJoin::Beyond(sigma_bucket(d2)),
                    Some(_) if taken.get(ti).is_some_and(|k| k.contains(&key)) => NoJoin::Blocked,
                    Some(_) if !cand.contains(&ti) => NoJoin::IndexMiss,
                    Some(_) => NoJoin::Nearer,
                }
            };
        existing.push((t.id, t.domain, t.bearing_only.is_some(), why));
    }
    if existing.is_empty() { return; }
    LOG.lock().expect("trace log").births.push(Birth { side: picture.side, tick: m.det.tick, id, target: m.det.target, kind: m.det.kind, ranged: m.ranged, hint: m.det.domain_hint, existing });
}

/// Called before the merge pass: every pair of live tracks holding one object, with the reason the pass will
/// leave it (or `WouldMerge`).
pub(super) fn pairs(picture: &SidePicture, updated: &[bool], taken: &[Vec<(SensorRef, u64)>], tick: u64, tt: &TrackTuning, gate2: f64, fresh_gate2: f64) {
    let n = picture.tracks.len();
    let mut out: Vec<(EntityId, NoMerge)> = Vec::new();
    for i in 0..n {
        let a = &picture.tracks[i];
        if !a.is_live() || a.is_friend() || a.truth.is_none() { continue; }
        for j in (i + 1)..n {
            let b = &picture.tracks[j];
            if !b.is_live() || b.is_friend() || b.truth != a.truth { continue; }
            let why = if !(updated[i] || updated[j]) { NoMerge::NeitherUpdated }
                else if air_like(a.domain) != air_like(b.domain) { NoMerge::Domain }
                else if matches!((&a.class, &b.class), (Some(x), Some(y)) if x != y) { NoMerge::Class }
                else if taken[i].iter().any(|k| taken[j].contains(k)) { NoMerge::SameSensorInstant }
                else {
                    let ghost = (updated[i] && tick.saturating_sub(b.last_update_tick) > GHOST_TICKS) || (updated[j] && tick.saturating_sub(a.last_update_tick) > GHOST_TICKS);
                    let limit2 = if ghost { fresh_gate2 } else { gate2 };
                    let (pa, pb) = (a.predict(tick), b.predict(tick));
                    match a.cov_h(tick, tt).add(b.cov_h(tick, tt)).inv_quad(pa.x - pb.x, pa.z - pb.z) {
                        None => NoMerge::Singular,
                        Some(d2) if d2 > limit2 => NoMerge::Beyond(sigma_bucket(d2)),
                        Some(_) => NoMerge::WouldMerge,
                    }
                };
            out.push((a.truth, why));
        }
    }
    if out.is_empty() { return; }
    let mut log = LOG.lock().expect("trace log");
    for (truth, why) in out { *log.pairs.entry((picture.side.0, truth, why)).or_default() += 1; }
}
