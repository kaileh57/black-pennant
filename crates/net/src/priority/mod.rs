//! Unit `net-priority`. The priority accumulator and per-item update rates. DESIGN.md L924 (objects that
//! miss a packet keep their priority and go first next time), L937 (over the cap, distant tracks drop to
//! 1 Hz first, then poses of untracked vessels), L1423-1425 (distant radar tracks 1 Hz, 2 Hz for weapons in
//! flight and tracks closing faster than 30 kn; guided missile state 5 Hz), L931 (own hull every snapshot),
//! L935 (fallback 1: other vessels at 20 Hz).
//!
//! Each snapshot interval the session calls [`PriorityAccumulator::order`] with the viewer's snapshot; every
//! item gains priority at its rate (from its `Relevance`), and the result lists items due, highest first. The
//! delta encoder writes items in that order until the client's byte budget for the frame is spent and reports
//! what it wrote; [`PriorityAccumulator::sent`] resets those. Items that did not fit keep their priority.
use crate::delta::{ItemKey, Section};
use serde::{Deserialize, Serialize};
use sim_systems::protocol::snapshot::{Relevance, Snapshot};

/// Update rates in Hz by relevance and kind. Defaults are spec values; `other_vessel_hz` drops to 20 under
/// fallback 1 (L935), with the client's interpolation delay set to 3 x 50 ms.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RateTuning {
    pub snapshot_hz: u32,
    pub other_vessel_hz: u32,
    pub distant_hz: u32,
    pub distant_fast_hz: u32,
    pub guided_body_hz: u32,
    /// Own ship status section (`Section::Ship`: speed, heading, power, mounts, held roles). The spec gives
    /// it no rate; 5 Hz, half the track rate. Compartments and the other slow lists ride `Section::ShipSlow`
    /// at `slow_section_hz` (they change at most at 2 Hz, L640).
    pub ship_status_hz: u32,
    /// Allies, alerts, objectives, pacing: sent on change, at most this often.
    pub slow_section_hz: u32,
    /// Air and Surface tracks inside 10 km but beyond [`CLOSE_TRACK_M`] of the viewer's hull that are not
    /// engaged (`Relevance::Near`): sent at most this often. The spec sets client rates only for distant
    /// tracks (L1423); the sim's 10 Hz track rate (L1418) stays for close and engaged tracks.
    #[serde(default = "near_track_hz_default")]
    pub near_track_hz: u32,
}
fn near_track_hz_default() -> u32 { 5 }
impl Default for RateTuning {
    fn default() -> Self { Self { snapshot_hz: 30, other_vessel_hz: 30, distant_hz: 1, distant_fast_hz: 2, guided_body_hz: 5, ship_status_hz: 5, slow_section_hz: 2, near_track_hz: near_track_hz_default() } }
}
impl RateTuning {
    /// Fallback 1 of L935.
    pub fn fallback_20hz() -> Self { Self { other_vessel_hz: 20, ..Self::default() } }
}

/// Over-cap shedding stage (L937), set by the session from the client's byte budget pressure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Shedding {
    #[default] None,
    /// Distant tracks at 1 Hz regardless of closing speed.
    DistantTracksSlow,
    /// Also skip poses of vessels the viewer holds no track on.
    UntrackedPoses,
}

pub struct PriorityAccumulator {
    pub(crate) tuning: RateTuning,
    pub(crate) acc: std::collections::BTreeMap<ItemKey, f32>,
    pub(crate) last_sent_tick: std::collections::BTreeMap<ItemKey, u64>,
    pub(crate) shedding: Shedding,
    /// Priority gained per interval by each item at its last `order` (its rate over the snapshot rate).
    pub(crate) step: std::collections::BTreeMap<ItemKey, f32>,
}

/// Tracks change at most at the sim's 10 Hz track rate (Rates table, L1418), so near and engaged tracks are
/// due at 10 Hz rather than at the vessel rate.
pub const TRACK_HZ: u32 = sim_core::time::TRACK_HZ;
/// Range from the viewer's hull inside which a near (not engaged) Air or Surface track keeps the 10 Hz track
/// rate; beyond it, out to the 10 km distant line, it takes `near_track_hz`. The figure is this module's
/// choice: the spec sets the 10 Hz track rate in the sim (L1418) and client rates only beyond 10 km (L1423).
pub const CLOSE_TRACK_M: f64 = 3_000.0;
/// Accumulated priority at which an item is due (one interval of its own rate). The epsilon absorbs f32
/// rounding of repeated `rate / snapshot_hz` sums, so a 10 Hz item is due exactly every third interval.
const DUE: f32 = 1.0 - 1e-3;
/// Ceiling of accumulated priority, so an item that never fits cannot grow without bound.
const ACC_MAX: f32 = 1.0e6;

impl PriorityAccumulator {
    pub fn new(tuning: RateTuning) -> Self { Self { tuning, acc: Default::default(), last_sent_tick: Default::default(), shedding: Shedding::None, step: Default::default() } }
    pub fn set_tuning(&mut self, tuning: RateTuning) { self.tuning = tuning; }
    pub fn set_shedding(&mut self, s: Shedding) { self.shedding = s; }
    pub fn shedding(&self) -> Shedding { self.shedding }
    /// Accumulate one snapshot interval and return the items due, highest priority first, ties by key. The
    /// viewer's own vessel (`Relevance::Own`) is always first and always due. Items absent from `snap` are
    /// forgotten (the delta frame reports their removal). Deterministic for its inputs. Unit: `net-priority`.
    ///
    /// Each interval an item gains `rate / snapshot_hz`; it is due once it has gained a full interval of its
    /// own rate. A new item starts due. Due items sort by accumulated priority times a relevance weight
    /// (engaged and own-side items before near ones, near before distant), so an item that keeps missing
    /// rises until it goes first. Rate-0 items (untracked poses while shedding) are never due.
    pub fn order(&mut self, snap: &Snapshot) -> Vec<ItemKey> {
        let items = items_of(snap, self.shedding, &self.tuning);
        let hz = self.tuning.snapshot_hz.max(1) as f32;
        let mut own = Vec::new();
        let mut due: Vec<(f32, ItemKey)> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for (key, rel, shed, cap) in items {
            if !seen.insert(key) { continue; }
            let mut rate = if shed { 0.0 } else { rate_hz(&key, rel, &self.tuning, self.shedding) };
            // A weapon in flight is guided-missile state: 5 Hz (L926), never faster than its own body item; a
            // near Air or Surface track beyond close range takes `near_track_hz`.
            if let Some(cap) = cap { rate = rate.min(cap as f32); }
            let step = rate / hz;
            self.step.insert(key, step);
            if is_own(&key, rel) {
                self.acc.insert(key, 1.0);
                own.push(key);
                continue;
            }
            let acc = match self.acc.get(&key) {
                Some(&a) => (a + step).min(ACC_MAX),
                None if step > 0.0 => 1.0,
                None => 0.0,
            };
            self.acc.insert(key, acc);
            if step > 0.0 && acc >= DUE { due.push((acc * weight(&key, rel), key)); }
        }
        // Forget items that left the snapshot.
        self.acc.retain(|k, _| seen.contains(k));
        self.step.retain(|k, _| seen.contains(k));
        self.last_sent_tick.retain(|k, _| seen.contains(k));
        due.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        own.sort();
        own.extend(due.into_iter().map(|(_, k)| k));
        own
    }
    /// Reset the items the encoder actually wrote at `tick`. Unit: `net-priority`.
    ///
    /// A sent item keeps the fraction of an interval it had gained past due when that fraction is smaller
    /// than one step, so a 20 Hz item at a 30 Hz snapshot rate goes out on two intervals of three; an item
    /// that had been waiting restarts from zero instead of bursting.
    pub fn sent(&mut self, items: &[ItemKey], tick: u64) {
        for key in items {
            let step = self.step.get(key).copied().unwrap_or(1.0);
            if let Some(a) = self.acc.get_mut(key) {
                let rem = *a - 1.0;
                *a = if rem > 0.0 && rem < step - 1e-3 { rem } else { 0.0 };
            }
            if self.acc.contains_key(key) { self.last_sent_tick.insert(*key, tick); }
        }
    }
    /// Forget everything (new sortie, rejoin). Real code.
    pub fn clear(&mut self) { self.acc.clear(); self.last_sent_tick.clear(); self.step.clear(); }
}

/// Rate in Hz of one item under the tuning and shedding stage. Unit: `net-priority`.
///
/// - Vessels: the own hull at the snapshot rate; every other drawn vessel at `other_vessel_hz` whatever its
///   distance (the bandwidth gate budgets 41 hulls at 30 Hz, L930); under [`Shedding::UntrackedPoses`] other
///   vessels the viewer holds no track on (and that are not own-side) get 0 and keep their last pose.
/// - Bodies: `guided_body_hz` (5 Hz, L1421), or the distant rates for bodies beyond 10 km. Decoys:
///   `guided_body_hz`.
/// - Tracks: distant 1 Hz, distant-fast 2 Hz (1 Hz while shedding), otherwise the 10 Hz track rate capped
///   by `other_vessel_hz`. [`PriorityAccumulator::order`] caps tracks of weapons in flight at
///   `guided_body_hz` (5 Hz, L926): a missile's track updates no faster than its replicated state, where
///   the spec is silent on near weapon tracks.
/// - Sections: `ship_status_hz` for the own ship status, `slow_section_hz` for the rest.
pub fn rate_hz(key: &ItemKey, relevance: Relevance, tuning: &RateTuning, shedding: Shedding) -> f32 {
    let distant = |fast: bool| if fast { tuning.distant_fast_hz } else { tuning.distant_hz };
    let hz = match key {
        ItemKey::Vessel(_) => match relevance {
            Relevance::Own => tuning.snapshot_hz,
            _ => tuning.other_vessel_hz,
        },
        ItemKey::Body(_) => match relevance {
            Relevance::Distant => tuning.distant_hz,
            Relevance::DistantFast => tuning.distant_fast_hz,
            _ => tuning.guided_body_hz,
        },
        ItemKey::Decoy(_) => tuning.guided_body_hz,
        ItemKey::Track(_) => match relevance {
            Relevance::Distant => distant(false),
            Relevance::DistantFast => distant(shedding < Shedding::DistantTracksSlow),
            _ => TRACK_HZ.min(tuning.other_vessel_hz.max(1)),
        },
        ItemKey::Section(Section::Ship) => tuning.ship_status_hz,
        ItemKey::Section(_) => tuning.slow_section_hz,
    };
    hz as f32
}

/// Under [`Shedding::UntrackedPoses`], the pose of a vessel the viewer holds no track on is skipped (L937).
/// Own and own-side hulls are never shed: they are the viewer's own and its partners'.
fn shed_pose(v: &sim_systems::protocol::snapshot::VesselView, shedding: Shedding) -> bool {
    shedding >= Shedding::UntrackedPoses && v.track.is_none() && !matches!(v.relevance, Relevance::Own | Relevance::OwnSide)
}

fn is_own(key: &ItemKey, rel: Relevance) -> bool { matches!(key, ItemKey::Vessel(_)) && rel == Relevance::Own }

/// Ordering weight among due items: how much the viewer cares, per L924 and the `Relevance` docs.
fn weight(key: &ItemKey, rel: Relevance) -> f32 {
    match key {
        ItemKey::Section(Section::Ship) => 4.0,
        ItemKey::Section(Section::ShipSlow) => 3.0,
        ItemKey::Section(Section::Pacing) => 3.0,
        ItemKey::Section(_) => 2.0,
        _ => match rel {
            Relevance::Own => 8.0,
            Relevance::Engaged => 4.0,
            Relevance::OwnSide => 3.0,
            Relevance::Near => 2.0,
            Relevance::DistantFast => 1.5,
            Relevance::Distant => 1.0,
        },
    }
}

/// Every item of a snapshot with its relevance and whether its pose is shed. Decoys carry no relevance and
/// rank as near items; sections rank by their own weights.
/// The last value caps a track's rate: tracks of weapons (`Domain::Weapon`) take the guided-missile rate, and
/// near Air and Surface tracks beyond [`CLOSE_TRACK_M`] take `near_track_hz` ([`track_cap_hz`]).
fn items_of(snap: &Snapshot, shedding: Shedding, tuning: &RateTuning) -> Vec<(ItemKey, Relevance, bool, Option<u32>)> {
    let mut out = Vec::with_capacity(snap.vessels.len() + snap.bodies.len() + snap.decoys.len() + snap.tracks.len() + 6);
    let own = snap.vessels.iter().find(|v| v.relevance == Relevance::Own).map(|v| v.kin.pose.pos);
    out.extend(snap.vessels.iter().map(|v| (ItemKey::Vessel(v.entity), v.relevance, shed_pose(v, shedding), None)));
    out.extend(snap.bodies.iter().map(|b| (ItemKey::Body(b.entity), b.relevance, false, None)));
    out.extend(snap.decoys.iter().map(|d| (ItemKey::Decoy(d.entity), Relevance::Near, false, None)));
    out.extend(snap.tracks.iter().map(|t| (ItemKey::Track(t.id), t.relevance, false, track_cap_hz(t, own, tuning))));
    for s in Section::ALL { out.push((ItemKey::Section(s), Relevance::OwnSide, false, None)); }
    out
}

/// Rate cap of one track. Unit: `net-priority`.
///
/// - A weapon in flight: `guided_body_hz` (L926).
/// - An Air or Surface track that is only `Relevance::Near` (not engaged, not own-side) and lies beyond
///   [`CLOSE_TRACK_M`] of the viewer's own hull, horizontally: `near_track_hz`. With no own hull in the
///   snapshot (a side viewer) nothing is close, so every near track is capped.
/// - Anything else: none (the relevance rate stands). The cap never lowers a rate below the distant floor of
///   L937 because a near track's distant rate is already lower.
pub fn track_cap_hz(t: &sim_systems::protocol::snapshot::TrackView, own: Option<sim_core::math::DVec3>, tuning: &RateTuning) -> Option<u32> {
    use content::enums::Domain;
    match t.domain {
        Domain::Weapon => Some(tuning.guided_body_hz),
        Domain::Air | Domain::Surface if t.relevance == Relevance::Near => {
            let close = own.is_some_and(|o| {
                let (dx, dz) = (t.pos.x - o.x, t.pos.z - o.z);
                dx.is_finite() && dz.is_finite() && dx * dx + dz * dz <= CLOSE_TRACK_M * CLOSE_TRACK_M
            });
            (!close).then_some(tuning.near_track_hz.max(tuning.distant_hz).max(1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::EntityId;
    use sim_systems::ids::TrackId;
    use sim_systems::protocol::snapshot::{TrackView, VesselView};

    fn vessel(i: u32, rel: Relevance, track: u32) -> VesselView {
        VesselView { entity: EntityId { index: i, generation: 1 }, kin: Default::default(), status: Default::default(), flags: Default::default(), turrets: vec![], rudder: 0.0, power: 0.0, track: TrackId(track), relevance: rel }
    }
    fn snap() -> Snapshot {
        Snapshot { vessels: vec![vessel(1, Relevance::Near, 3), vessel(2, Relevance::Own, 0), vessel(3, Relevance::Distant, 0)], ..Default::default() }
    }

    #[test]
    fn rates_follow_the_spec() {
        let t = RateTuning::default();
        let v = ItemKey::Vessel(EntityId::default());
        let tr = ItemKey::Track(TrackId(1));
        assert_eq!(rate_hz(&v, Relevance::Own, &t, Shedding::None), 30.0);
        assert_eq!(rate_hz(&v, Relevance::Distant, &t, Shedding::None), 30.0);
        assert_eq!(rate_hz(&v, Relevance::Near, &RateTuning::fallback_20hz(), Shedding::None), 20.0);
        assert_eq!(rate_hz(&tr, Relevance::Distant, &t, Shedding::None), 1.0);
        assert_eq!(rate_hz(&tr, Relevance::DistantFast, &t, Shedding::None), 2.0);
        assert_eq!(rate_hz(&tr, Relevance::DistantFast, &t, Shedding::DistantTracksSlow), 1.0);
        assert_eq!(rate_hz(&tr, Relevance::Engaged, &t, Shedding::None), 10.0);
        assert_eq!(rate_hz(&ItemKey::Body(EntityId::default()), Relevance::Engaged, &t, Shedding::None), 5.0);
        assert_eq!(rate_hz(&ItemKey::Section(Section::Ship), Relevance::OwnSide, &t, Shedding::None), 5.0);
        assert_eq!(rate_hz(&ItemKey::Section(Section::Pacing), Relevance::OwnSide, &t, Shedding::None), 2.0);
    }

    #[test]
    fn own_first_and_rates_are_exact_over_time() {
        let mut p = PriorityAccumulator::new(RateTuning::default());
        let mut s = snap();
        s.tracks = vec![TrackView { relevance: Relevance::Distant, ..track(9) }, TrackView { relevance: Relevance::Near, ..track(10) }];
        let mut counts = std::collections::BTreeMap::<ItemKey, u32>::new();
        for tick in 0..300u64 {
            let o = p.order(&s);
            assert_eq!(o[0], ItemKey::Vessel(EntityId { index: 2, generation: 1 }));
            for k in &o { *counts.entry(*k).or_default() += 1; }
            p.sent(&o, tick);
        }
        // 300 intervals = 10 s.
        assert_eq!(counts[&ItemKey::Vessel(EntityId { index: 1, generation: 1 })], 300);
        assert_eq!(counts[&ItemKey::Track(TrackId(9))], 10);
        assert_eq!(counts[&ItemKey::Track(TrackId(10))], 100);
        assert_eq!(counts[&ItemKey::Section(Section::Ship)], 50);
        assert_eq!(counts[&ItemKey::Section(Section::Alerts)], 20);
        let mut f = PriorityAccumulator::new(RateTuning::fallback_20hz());
        let mut n = 0;
        for tick in 0..300u64 {
            let o = f.order(&s);
            n += o.contains(&ItemKey::Vessel(EntityId { index: 1, generation: 1 })) as u32;
            f.sent(&o, tick);
        }
        assert_eq!(n, 200);
    }

    #[test]
    fn deferred_items_keep_priority_and_go_first() {
        let mut p = PriorityAccumulator::new(RateTuning::default());
        let s = snap();
        let near = ItemKey::Vessel(EntityId { index: 1, generation: 1 });
        let far = ItemKey::Vessel(EntityId { index: 3, generation: 1 });
        let o = p.order(&s);
        assert_eq!(o, vec![ItemKey::Vessel(EntityId { index: 2, generation: 1 }), ItemKey::Section(Section::Ship), ItemKey::Section(Section::Pacing), ItemKey::Section(Section::ShipSlow), near, ItemKey::Section(Section::Allies), ItemKey::Section(Section::Alerts), ItemKey::Section(Section::Objectives), far]);
        // Send everything but the near vessel: it waits, so next interval it outranks the sections.
        let sent: Vec<_> = o.iter().copied().filter(|k| *k != near).collect();
        p.sent(&sent, 0);
        for _ in 0..3 { let o = p.order(&s); assert_eq!(o[1], near); p.sent(&o[..1], 1); }
        // Items absent from the snapshot are forgotten.
        let mut s2 = s.clone();
        s2.vessels.retain(|v| v.entity.index != 1);
        p.order(&s2);
        assert!(!p.acc.contains_key(&near));
    }

    #[test]
    fn weapon_tracks_take_the_guided_missile_rate() {
        let mut p = PriorityAccumulator::new(RateTuning::default());
        let mut s = snap();
        let weapon = |id, rel| TrackView { domain: content::enums::Domain::Weapon, relevance: rel, ..track(id) };
        s.tracks = vec![weapon(20, Relevance::Engaged), weapon(21, Relevance::Near), weapon(22, Relevance::DistantFast), TrackView { relevance: Relevance::Engaged, ..track(23) }];
        let mut counts = std::collections::BTreeMap::<ItemKey, u32>::new();
        for tick in 0..300u64 {
            let o = p.order(&s);
            for k in &o { *counts.entry(*k).or_default() += 1; }
            p.sent(&o, tick);
        }
        // 10 s: near and engaged weapons at 5 Hz (L926), distant ones keep their 2 Hz, a surface track 10 Hz.
        assert_eq!(counts[&ItemKey::Track(TrackId(20))], 50);
        assert_eq!(counts[&ItemKey::Track(TrackId(21))], 50);
        assert_eq!(counts[&ItemKey::Track(TrackId(22))], 20);
        assert_eq!(counts[&ItemKey::Track(TrackId(23))], 100);
    }

    #[test]
    fn shedding_skips_untracked_poses() {
        let mut p = PriorityAccumulator::new(RateTuning::default());
        p.set_shedding(Shedding::UntrackedPoses);
        let o = p.order(&snap());
        assert!(!o.contains(&ItemKey::Vessel(EntityId { index: 3, generation: 1 })));
        assert!(o.contains(&ItemKey::Vessel(EntityId { index: 1, generation: 1 })));
        assert!(o.contains(&ItemKey::Vessel(EntityId { index: 2, generation: 1 })));
    }

    #[test]
    fn near_tracks_beyond_close_range_take_the_near_track_rate() {
        use content::enums::Domain;
        let mut p = PriorityAccumulator::new(RateTuning::fallback_20hz());
        let mut s = snap();
        let at = |id, domain, rel, x: f64| TrackView { domain, relevance: rel, pos: sim_core::math::DVec3::new(x, 0.0, 0.0), ..track(id) };
        s.tracks = vec![
            at(30, Domain::Surface, Relevance::Near, 5_000.0),
            at(31, Domain::Air, Relevance::Near, 9_000.0),
            at(32, Domain::Surface, Relevance::Near, 1_000.0),
            at(33, Domain::Surface, Relevance::Engaged, 5_000.0),
            at(34, Domain::Surface, Relevance::OwnSide, 5_000.0),
            at(35, Domain::Subsurface, Relevance::Near, 5_000.0),
            at(36, Domain::Weapon, Relevance::Near, 5_000.0),
            at(37, Domain::Surface, Relevance::Distant, 20_000.0),
        ];
        let mut counts = std::collections::BTreeMap::<ItemKey, u32>::new();
        for tick in 0..300u64 {
            let o = p.order(&s);
            for k in &o { *counts.entry(*k).or_default() += 1; }
            p.sent(&o, tick);
        }
        let n = |id| counts[&ItemKey::Track(TrackId(id))];
        // 10 s: near Air and Surface tracks beyond 3 km at 5 Hz; close, engaged, own-side and Subsurface
        // tracks keep 10 Hz; weapons 5 Hz; distant 1 Hz.
        assert_eq!((n(30), n(31), n(32), n(33), n(34), n(35), n(36), n(37)), (50, 50, 100, 100, 100, 100, 50, 10));
        // No own hull in the snapshot (a side viewer): nothing is close.
        let own = None;
        assert_eq!(track_cap_hz(&at(32, Domain::Surface, Relevance::Near, 1_000.0), own, &RateTuning::default()), Some(5));
    }

    #[test]
    fn every_track_keeps_the_distant_floor_under_every_shedding_stage() {
        // L937: over the cap distant tracks drop to 1 Hz, and nothing replicated drops below that. With an
        // unlimited budget, every track kind and relevance at every shedding stage is sent at least once a
        // second, and a near-track rate configured below the floor is raised to it.
        use content::enums::Domain;
        let tuning = RateTuning { near_track_hz: 0, ..RateTuning::fallback_20hz() };
        for stage in [Shedding::None, Shedding::DistantTracksSlow, Shedding::UntrackedPoses] {
            let mut p = PriorityAccumulator::new(tuning);
            p.set_shedding(stage);
            let mut s = snap();
            let mut id = 100;
            for domain in [Domain::Surface, Domain::Subsurface, Domain::Air, Domain::Weapon] {
                for rel in [Relevance::Distant, Relevance::DistantFast, Relevance::Near, Relevance::Engaged, Relevance::OwnSide] {
                    for x in [500.0, 5_000.0, 20_000.0] {
                        s.tracks.push(TrackView { domain, relevance: rel, pos: sim_core::math::DVec3::new(x, 0.0, 0.0), ..track(id) });
                        id += 1;
                    }
                }
            }
            let mut counts = std::collections::BTreeMap::<ItemKey, u32>::new();
            for tick in 0..300u64 {
                let o = p.order(&s);
                for k in &o { *counts.entry(*k).or_default() += 1; }
                p.sent(&o, tick);
            }
            for t in &s.tracks {
                let c = counts.get(&ItemKey::Track(t.id)).copied().unwrap_or(0);
                assert!(c >= 10, "{stage:?} {:?} {:?} at {} m: {c} updates in 10 s", t.domain, t.relevance, t.pos.x);
            }
        }
    }

    fn track(id: u32) -> TrackView {
        use sim_systems::protocol::snapshot::UncertaintyShape;
        TrackView {
            id: TrackId(id), domain: Default::default(), stance: Default::default(), stance_reason: Default::default(), rung: Default::default(), rung_reason: Default::default(), grade: Default::default(),
            pos: Default::default(), vel: Default::default(), shape: UncertaintyShape::Circle { radius_m: 10.0 }, last_update_tick: 0, last_known: false, class: None, candidates: 0,
            sources: Default::default(), trend: Default::default(), inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Relevance::Near, struck: false,
        }
    }
}
