//! Tests of unit `sys-ident` on hand-built pools.
use super::stance::{self, StanceFacts};
use super::*;
use crate::protocol::config::{Faction, SideDef};
use crate::sensors::Detection;
use crate::tracks::{Pictures, SidePicture};
use content::campaign::DifficultySliders;
use content::enums::TimeOfDay;
use content::ids::TextKey;
use content::part::{PartCategory, PartDef, Placement, ProcModel, SensorDef, Flammable};
use sim_core::Tick;

struct Env { cat: Catalogue, tuning: TuningTable, weather: WeatherState, diff: DifficultySliders }
impl Env {
    fn new() -> Self { Env { cat: Catalogue::default(), tuning: TuningTable::default(), weather: WeatherState::default(), diff: DifficultySliders::NORMAL } }
    fn ctx(&self, tick: u64) -> SimCtx<'_> {
        SimCtx { seed: 1, tick: Tick(tick), catalogue: &self.cat, tuning: &self.tuning, weather: &self.weather, time_of_day: TimeOfDay::Day, layer_depth_m: 50.0, difficulty: &self.diff }
    }
}

fn cls(name: &str, role: ClassRole, emitters: &[u16], rcs: [f32; 2], speed: f32, acoustic: u8, civilian: bool) -> Candidate {
    Candidate { class: Some(ClassId::new(name)), munition: None, domain: Domain::Surface, role: Some(role), civilian, own_side: false, emitters: emitters.to_vec(), rcs_band_m2: rcs, max_speed_mps: speed, acoustic }
}
fn mun(i: u16, domain: Domain, role: Option<ClassRole>, emitters: &[u16], rcs: [f32; 2], speed: f32) -> Candidate {
    Candidate { class: None, munition: Some(MunitionIdx(i)), domain, role, civilian: false, own_side: false, emitters: emitters.to_vec(), rcs_band_m2: rcs, max_speed_mps: speed, acoustic: 0 }
}

/// 0 frigate, 1 corvette, 2 patrol boat, 3 civilian freighter, 4 missile, 5 drone.
fn lib() -> IdentLibrary {
    IdentLibrary {
        pool: vec![
            cls("frigate", ClassRole::Frigate, &[1], [500.0, 15_000.0], 17.0, 2, false),
            cls("corvette", ClassRole::Corvette, &[1, 2], [100.0, 3_000.0], 17.0, 1, false),
            cls("patrol", ClassRole::PatrolBoat, &[3], [15.0, 450.0], 19.0, 1, false),
            cls("freighter", ClassRole::Freighter, &[0], [50.0, 2_000.0], 9.0, 1, true),
            mun(0, Domain::Weapon, None, &[4], [0.03, 0.3], 300.0),
            mun(1, Domain::Air, Some(ClassRole::Drone), &[], [0.15, 1.5], 50.0),
        ],
        emitters: vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
        combine_emitters: vec![1, 2],
        type_keys: Vec::new(),
    }
}

fn track() -> Track {
    Track {
        id: TrackId(1), side: SideId(0), domain: Domain::Surface, truth: EntityId::NONE, pos: DVec3::ZERO, vel: DVec3::ZERO, r0_m: 50.0, sigma_v: 0.5,
        bearing_only: None, created_tick: 0, last_update_tick: 0, last_lock_tick: None, dropped_tick: None, grade: TrackGrade::Coarse,
        sources: SourceMask::SEARCH_RADAR, trend: Default::default(), rung: IdRung::Unknown, rung_reason: RungReason::None, candidates: Default::default(),
        class: None, history: Vec::new(), rcs_m2: 0.0, consistent_ticks: 0, emitters: Vec::new(), id_override: None, stance: Stance::Suspect,
        stance_reason: StanceReason::Default, stance_override: None, fired_on_us: false, inbound: false, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
    }
}

fn det(kind: SensorKind, evidence: Evidence, tick: u64) -> Detection {
    Detection {
        tick, sensor: SensorRef::default(), side: SideId(0), kind, sensor_pos: DVec3::ZERO, target: EntityId { index: 7, generation: 0 },
        measurement: Measurement::BearingOnly { bearing_rad: 0.0 }, sigma_range_m: 0.0, sigma_bearing_rad: 0.01, evidence, emitter: u16::MAX, rcs_m2: 0.0, domain_hint: None,
    }
}
fn esm(emitter: u16) -> Detection { let mut d = det(SensorKind::Esm, Evidence::EMITTER, 10); d.emitter = emitter; d }
fn remaining(t: &Track) -> Vec<u16> { t.candidates.remaining.clone() }

#[test]
fn rung_of_agreement_levels() {
    let l = lib();
    let set = |v: &[u16]| CandidateSet { remaining: v.to_vec() };
    assert_eq!(rung_of(&set(&[]), &l), IdRung::Unknown, "empty = whole pool, which disagrees on domain");
    assert_eq!(rung_of(&set(&[0, 1]), &l), IdRung::Domain);
    assert_eq!(rung_of(&set(&[0]), &l), IdRung::Class);
    assert_eq!(rung_of(&set(&[4]), &l), IdRung::Class);
    assert_eq!(rung_of(&set(&[0, 4]), &l), IdRung::Unknown);
    assert_eq!(rung_of(&set(&[999]), &l), IdRung::Unknown, "bad indices are ignored");
    // Two hulls of one role agree on Type.
    let mut l2 = l.clone();
    l2.pool[1].role = Some(ClassRole::Frigate);
    assert_eq!(rung_of(&set(&[0, 1]), &l2), IdRung::Type);
    assert_eq!(rung_of(&set(&[]), &IdentLibrary::default()), IdRung::Unknown);
}

#[test]
fn emitter_matches_narrow_and_name_the_reason() {
    let env = Env::new();
    let l = lib();
    let mut t = track();
    assert_eq!(apply_evidence(&mut t, &[esm(1)], &l, &env.ctx(10)), Some((IdRung::Domain, RungReason::EmitterMatch)));
    assert_eq!(remaining(&t), vec![0, 1]);
    assert_eq!(t.emitters, vec![1]);
    // An emitter fitted to one class only: Class, and the class is set.
    assert_eq!(apply_evidence(&mut t, &[esm(2)], &l, &env.ctx(20)), Some((IdRung::Class, RungReason::EmitterUniqueToClass)));
    assert_eq!(t.class, Some(ClassId::new("corvette")));
    assert_eq!(t.history.iter().map(|s| s.rung).collect::<Vec<_>>(), vec![IdRung::Domain, IdRung::Class]);
    // Contradicting evidence (an emitter no remaining candidate has) is ignored; the rung never falls.
    assert_eq!(apply_evidence(&mut t, &[esm(3)], &l, &env.ctx(30)), None);
    assert_eq!((t.rung, remaining(&t)), (IdRung::Class, vec![1]));
    // Out-of-library emitter ids are ignored.
    let mut u = track();
    assert_eq!(apply_evidence(&mut u, &[esm(u16::MAX), esm(77)], &l, &env.ctx(10)), None);
    assert!(u.emitters.is_empty());
}

#[test]
fn altitude_and_depth_give_the_domain() {
    let env = Env::new();
    let l = lib();
    let mut t = track();
    t.domain = Domain::Air;
    let mut d = det(SensorKind::FireControlRadar, Evidence::ALTITUDE | Evidence::FIRE_CONTROL, 5);
    d.measurement = Measurement::RangeBearing { range_m: 9_000.0, bearing_rad: 0.0, height_m: Some(3_000.0) };
    assert_eq!(apply_evidence(&mut t, &[d], &l, &env.ctx(5)), Some((IdRung::Class, RungReason::AltitudeMeasured)));
    assert_eq!(remaining(&t), vec![5]);
    // A low reading on a surface track keeps surface candidates and drops the air one.
    let mut s = track();
    let mut d = det(SensorKind::FireControlRadar, Evidence::ALTITUDE, 5);
    d.measurement = Measurement::RangeBearing { range_m: 9_000.0, bearing_rad: 0.0, height_m: Some(3.0) };
    assert_eq!(apply_evidence(&mut s, &[d], &l, &env.ctx(5)), Some((IdRung::Domain, RungReason::AltitudeMeasured)));
    assert_eq!(remaining(&s), vec![0, 1, 2, 3]);
    // The filter later finds it is a sea-skimmer: the missile entry is reachable again.
    s.domain = Domain::Weapon;
    assert_eq!(apply_evidence(&mut s, &[], &l, &env.ctx(6)), Some((IdRung::Class, RungReason::None)));
    assert_eq!(remaining(&s), vec![4]);
}

#[test]
fn class_fills_later_and_odd_emitters_do_not_block() {
    let env = Env::new();
    let l = lib();
    let mut t = track();
    apply_evidence(&mut t, &[det(SensorKind::Visual, Evidence::VISUAL_HALF, 1)], &l, &env.ctx(1));
    assert_eq!((t.rung, t.class.clone()), (IdRung::Class, None));
    assert_eq!(apply_evidence(&mut t, &[esm(2)], &l, &env.ctx(2)), None, "already at Class");
    assert_eq!(t.class, Some(ClassId::new("corvette")));
    let mut u = track();
    apply_evidence(&mut u, &[esm(3)], &l, &env.ctx(1));
    apply_evidence(&mut u, &[esm(2)], &l, &env.ctx(2));
    assert_eq!(u.emitters, vec![2, 3]);
    assert_eq!(remaining(&u), vec![2], "the first emitter narrowed to the patrol boat; the second fits nothing left");
    let mut w = track();
    w.emitters = vec![4, 2];
    w.emitters.sort();
    apply_evidence(&mut w, &[esm(2)], &l, &env.ctx(3));
    assert_eq!(remaining(&w), vec![1], "emitter 2 narrows; emitter 4 then fits nothing and is skipped");
}

#[test]
fn sightings_raise_the_rung_and_narrow_with_a_resolver() {
    let env = Env::new();
    let l = lib();
    let truth = |_: EntityId| Some(2u16);
    let mut t = track();
    let half = det(SensorKind::Visual, Evidence::VISUAL_HALF, 40);
    assert_eq!(apply_evidence_with(&mut t, &[half], &l, &env.ctx(40), &truth), Some((IdRung::Class, RungReason::VisualSighting)));
    assert_eq!((remaining(&t), t.class.clone()), (vec![2], Some(ClassId::new("patrol"))));
    let close = det(SensorKind::Camera, Evidence::VISUAL_HALF | Evidence::VISUAL_QUARTER, 50);
    assert_eq!(apply_evidence_with(&mut t, &[close], &l, &env.ctx(50), &truth), Some((IdRung::Exact, RungReason::CloseVisualSighting)));
    // Without a resolver the rung still rises but the list keeps every candidate.
    let mut u = track();
    assert_eq!(apply_evidence(&mut u, &[half], &l, &env.ctx(40)), Some((IdRung::Class, RungReason::VisualSighting)));
    assert_eq!(remaining(&u).len(), 6);
    assert_eq!(u.class, None);
    // Acoustic: the propulsion key of what was heard.
    let mut a = track();
    let heard = det(SensorKind::PassiveSonar, Evidence::ACOUSTIC, 60);
    assert_eq!(apply_evidence_with(&mut a, &[heard], &l, &env.ctx(60), &|_| Some(0)), Some((IdRung::Class, RungReason::AcousticMatch)));
    assert_eq!(remaining(&a), vec![0]);
}

#[test]
fn rcs_and_speed_needs_thirty_seconds() {
    let env = Env::new();
    let l = lib();
    let mut t = track();
    t.vel = DVec3::new(6.0, 0.0, 0.0);
    let mut r = det(SensorKind::SearchRadar, Evidence::empty(), 100);
    r.measurement = Measurement::RangeBearing { range_m: 9_000.0, bearing_rad: 0.0, height_m: None };
    r.rcs_m2 = 5_000.0;
    assert_eq!(apply_evidence(&mut t, &[r], &l, &env.ctx(100)), None);
    assert_eq!(t.consistent_ticks, 101);
    assert_eq!(apply_evidence(&mut t, &[], &l, &env.ctx(100 + 1_799)), None);
    assert_eq!(apply_evidence(&mut t, &[], &l, &env.ctx(100 + 1_800)), Some((IdRung::Class, RungReason::RcsAndSpeed)));
    assert_eq!(t.class, Some(ClassId::new("frigate")));
    // Too fast for every hull: no consistency, the timer resets.
    let mut f = track();
    f.vel = DVec3::new(40.0, 0.0, 0.0);
    r.rcs_m2 = 5_000.0;
    apply_evidence(&mut f, &[r], &l, &env.ctx(100));
    assert_eq!(f.consistent_ticks, 0);
    // A bad RCS reading is ignored.
    r.rcs_m2 = f32::NAN;
    let mut g = track();
    apply_evidence(&mut g, &[r], &l, &env.ctx(100));
    assert_eq!(g.rcs_m2, 0.0);
}

#[test]
fn overrides_and_exact() {
    let env = Env::new();
    let l = lib();
    let mut t = track();
    apply_evidence(&mut t, &[esm(2)], &l, &env.ctx(10));
    assert_eq!(t.rung, IdRung::Class);
    assert_eq!(set_override(&mut t, Some((IdRung::Domain, None)), &l, 20), (IdRung::Domain, RungReason::PlayerOverride));
    assert_eq!(t.class, None);
    // Evidence under an override is kept but does not move the shown rung.
    let close = det(SensorKind::Visual, Evidence::VISUAL_HALF | Evidence::VISUAL_QUARTER, 30);
    assert_eq!(apply_evidence_with(&mut t, &[close], &l, &env.ctx(30), &|_| Some(1)), None);
    assert_eq!(t.rung, IdRung::Domain);
    // Reverting restores the evidence rung, Exact from the history.
    assert_eq!(set_override(&mut t, None, &l, 40), (IdRung::Exact, RungReason::CloseVisualSighting));
    assert_eq!(t.class, Some(ClassId::new("corvette")));
    assert_eq!(set_override(&mut t, None, &l, 41), (IdRung::Exact, RungReason::CloseVisualSighting), "clearing twice is a no-op");
    // Photograph or briefing.
    let mut p = track();
    set_exact(&mut p, Some(ClassId::new("patrol")), RungReason::Photograph, &l, 50);
    assert_eq!((p.rung, p.rung_reason, remaining(&p), p.class.clone()), (IdRung::Exact, RungReason::Photograph, vec![2], Some(ClassId::new("patrol"))));
    // Datalink friends are left alone.
    let mut f = track();
    f.stance = Stance::Friend;
    f.stance_reason = StanceReason::Datalink;
    f.sources = SourceMask::DATALINK;
    f.rung = IdRung::Exact;
    f.rung_reason = RungReason::Datalink;
    assert_eq!(set_override(&mut f, Some((IdRung::Unknown, None)), &l, 1), (IdRung::Exact, RungReason::Datalink));
    assert_eq!(apply_evidence(&mut f, &[esm(1)], &l, &env.ctx(1)), None);
    set_exact(&mut f, None, RungReason::Photograph, &l, 1);
    assert_eq!(f.rung_reason, RungReason::Datalink);
}

fn side(id: u8, faction: Faction, pvp: Option<u8>) -> SideDef { SideDef { id: SideId(id), faction, name: TextKey::new("s"), hostile_to: SideMask::NONE, allied: SideMask::only(SideId(id)), pvp_team: pvp } }

#[test]
fn stance_rules_in_order() {
    let l = lib();
    let intel = SortieIntel { declared_hostile: vec![ClassId::new("patrol")], civilian: vec![ClassId::new("freighter")], ..Default::default() };
    let viewer = side(0, Faction::Players, None);
    let sides = [viewer.clone(), side(1, Faction::Combine, None)];
    let facts = |declared| StanceFacts { viewer: &viewer, sides: &sides, intel: &intel, lib: &l, declared };
    let mut t = track();
    assert!(!stance::evaluate(&mut t, &facts(None)));
    assert_eq!((t.stance, t.stance_reason), (Stance::Suspect, StanceReason::Default));
    t.emitters = vec![2];
    assert!(stance::evaluate(&mut t, &facts(None)));
    assert_eq!((t.stance, t.stance_reason), (Stance::Hostile, StanceReason::CombineEmitter));
    // A declared defector radiating Combine radar stays Neutral; firing on us still makes it Hostile.
    stance::evaluate(&mut t, &facts(Some(Stance::Neutral)));
    assert_eq!((t.stance, t.stance_reason), (Stance::Neutral, StanceReason::BriefingNeutral));
    t.fired_on_us = true;
    stance::evaluate(&mut t, &facts(Some(Stance::Neutral)));
    assert_eq!((t.stance, t.stance_reason), (Stance::Hostile, StanceReason::FiredOnOwnSide));
    // Briefing class lists apply once the class is known.
    let mut h = track();
    h.rung = IdRung::Class;
    h.class = Some(ClassId::new("patrol"));
    stance::evaluate(&mut h, &facts(None));
    assert_eq!((h.stance, h.stance_reason), (Stance::Hostile, StanceReason::BriefingHostile));
    h.class = Some(ClassId::new("freighter"));
    stance::evaluate(&mut h, &facts(None));
    assert_eq!((h.stance, h.stance_reason), (Stance::Neutral, StanceReason::BriefingNeutral));
    // Overrides win over rules 2 and 3; a graph cannot undo a player override.
    assert!(stance::set_override(&mut h, Some(Stance::Hostile), false));
    stance::evaluate(&mut h, &facts(None));
    assert_eq!((h.stance, h.stance_reason), (Stance::Hostile, StanceReason::PlayerOverride));
    assert!(!stance::set_override(&mut h, Some(Stance::Neutral), true));
    assert!(stance::set_override(&mut h, None, false));
    stance::evaluate(&mut h, &facts(None));
    assert_eq!(h.stance, Stance::Neutral);
    // Clearing an override that is not there changes nothing.
    let mut k = track();
    k.emitters = vec![2];
    stance::evaluate(&mut k, &facts(None));
    assert!(stance::set_override(&mut k, None, true));
    assert_eq!((k.stance, k.stance_reason), (Stance::Hostile, StanceReason::CombineEmitter));
    // Combine pictures ignore the player briefing's emitter rule.
    let combine = side(1, Faction::Combine, None);
    let mut c = track();
    c.emitters = vec![1];
    stance::evaluate(&mut c, &StanceFacts { viewer: &combine, sides: &sides, intel: &intel, lib: &l, declared: None });
    assert_eq!(c.stance, Stance::Suspect);
    // PvP lobby.
    let a = side(0, Faction::Players, Some(0));
    let pvp = [a.clone(), side(1, Faction::Players, Some(1))];
    let mut p = track();
    stance::evaluate(&mut p, &StanceFacts { viewer: &a, sides: &pvp, intel: &intel, lib: &l, declared: None });
    assert_eq!((p.stance, p.stance_reason), (Stance::Hostile, StanceReason::LobbyOpposingSide));
}

/// The BriefingNeutral latch holds only while the briefing still declares the track's current truth Neutral
/// or Friend: a latched track whose truth moved to a declared-hostile hull (a pursuer) turns Hostile.
#[test]
fn briefing_neutral_latch_follows_the_current_truth() {
    let l = lib();
    let intel = SortieIntel { declared_hostile: vec![ClassId::new("patrol")], ..Default::default() };
    let viewer = side(0, Faction::Players, None);
    let sides = [viewer.clone(), side(1, Faction::Combine, None)];
    let facts = |declared| StanceFacts { viewer: &viewer, sides: &sides, intel: &intel, lib: &l, declared };
    let mut t = track();
    stance::evaluate(&mut t, &facts(Some(Stance::Neutral)));
    assert_eq!((t.stance, t.stance_reason), (Stance::Neutral, StanceReason::BriefingNeutral));
    // A passing Combine-radar association while the defector is still the truth keeps it Neutral.
    t.emitters = vec![2];
    stance::evaluate(&mut t, &facts(Some(Stance::Neutral)));
    assert_eq!((t.stance, t.stance_reason), (Stance::Neutral, StanceReason::BriefingNeutral));
    // The truth moved to a hull the briefing declares Hostile: the latch no longer shields it.
    stance::evaluate(&mut t, &facts(Some(Stance::Hostile)));
    assert_eq!((t.stance, t.stance_reason), (Stance::Hostile, StanceReason::BriefingHostile));
    // A latched track whose truth is undeclared falls through to the Combine-radar rule.
    let mut u = track();
    stance::evaluate(&mut u, &facts(Some(Stance::Neutral)));
    u.emitters = vec![2];
    stance::evaluate(&mut u, &facts(None));
    assert_eq!((u.stance, u.stance_reason), (Stance::Hostile, StanceReason::CombineEmitter));
}

#[test]
fn friends_and_fired_on() {
    let mut f = track();
    f.truth = EntityId { index: 3, generation: 0 };
    f.stance = Stance::Friend;
    f.stance_reason = StanceReason::Datalink;
    f.sources = SourceMask::DATALINK;
    assert!(!stance::set_override(&mut f, Some(Stance::Hostile), false));
    let mut e = track();
    e.id = TrackId(2);
    e.truth = EntityId { index: 4, generation: 0 };
    let mut held = track();
    held.id = TrackId(3);
    held.truth = EntityId { index: 4, generation: 0 };
    held.stance_override = Some(Stance::Neutral);
    held.stance = Stance::Neutral;
    let mut pics = Pictures { sides: vec![SidePicture { side: SideId(0), tracks: vec![f, e, held], next_id: 4 }] };
    stance::note_fired_on(&mut pics, SideId(0), EntityId { index: 3, generation: 0 });
    stance::note_fired_on(&mut pics, SideId(0), EntityId { index: 4, generation: 0 });
    stance::note_fired_on(&mut pics, SideId(0), EntityId::NONE);
    stance::note_fired_on(&mut pics, SideId(5), EntityId { index: 4, generation: 0 });
    let t = &pics.sides[0].tracks;
    assert!(!t[0].fired_on_us && t[0].stance == Stance::Friend);
    assert!(t[1].fired_on_us && t[1].stance == Stance::Hostile && t[1].stance_reason == StanceReason::FiredOnOwnSide);
    assert!(t[2].fired_on_us && t[2].stance == Stance::Neutral);
}

fn sensor_part(id: &str, kind: SensorKind, emitter: &str) -> PartDef {
    PartDef {
        id: PartId::new(id), name: TextKey::new("n"), description: TextKey::new("d"), category: PartCategory::Sensor, tier: Default::default(),
        phase: Default::default(), size: [1, 1, 1], placement: Placement::Anywhere, mass_kg: 1.0, cost: 1, ports: vec![], power_priority: 0,
        buffer_s: None, watertight: false, flammable: Flammable::No, rcs_factor: 1.0, tags: vec![],
        function: PartFunction::Sensor(SensorDef { kind, r_ref_m: 20_000.0, rate_hz: 5.0, rpm: None, fov_half_deg: 180.0, emitter: emitter.into() }),
        model: ProcModel::default(), icon: Default::default(), replaced_by: None, standard_limit: None,
    }
}

#[test]
fn build_core_orders_pool_and_picks_combine_radars() {
    let mut cat = Catalogue::default();
    cat.parts.insert(PartId::new("a"), sensor_part("a", SensorKind::SearchRadar, "mil_search"));
    cat.parts.insert(PartId::new("b"), sensor_part("b", SensorKind::NavRadar, "nav"));
    cat.parts.insert(PartId::new("c"), sensor_part("c", SensorKind::SearchRadar, "shared_search"));
    let keys = crate::signature::emitter_keys(&cat);
    assert_eq!(keys, vec!["mil_search".to_string(), "nav".into(), "shared_search".into()]);
    let facts: BTreeMap<ClassId, ClassFacts> = [
        (ClassId::new("frigate"), ClassFacts { role: Some(ClassRole::Frigate), emitters: vec![("mil_search".into(), SensorKind::SearchRadar), ("nav".into(), SensorKind::NavRadar), ("shared_search".into(), SensorKind::SearchRadar)], tonnage_t: Some(1_800.0), acoustic: 2, ..Default::default() }),
        (ClassId::new("liner"), ClassFacts { role: Some(ClassRole::Freighter), emitters: vec![("shared_search".into(), SensorKind::SearchRadar)], ..Default::default() }),
    ].into_iter().collect();
    let intel = SortieIntel { order_of_battle: vec![ClassId::new("frigate"), ClassId::new("frigate")], civilian: vec![ClassId::new("liner")], ..Default::default() };
    let l = build_core(&intel, &[ClassId::new("mine")], &facts, &cat);
    assert_eq!(l.emitters, keys);
    assert_eq!(l.pool.len(), 3);
    assert_eq!(l.pool[0].class, Some(ClassId::new("frigate")));
    assert_eq!(l.pool[0].emitters, vec![0, 1, 2]);
    assert!((l.pool[0].rcs_band_m2[0] - 700.0).abs() < 1.0 && (l.pool[0].rcs_band_m2[1] - 21_000.0).abs() < 5.0);
    assert!(l.pool[1].civilian && l.pool[2].own_side);
    assert_eq!(l.pool[2].rcs_band_m2, [0.0, f32::MAX]);
    assert_eq!(l.combine_emitters, vec![0], "navigation and civilian-shared radars are not Combine radars");
    assert_eq!(l.emitter_index("nav"), 1);
    assert_eq!(l.class_entry(&ClassId::new("liner")), Some(1));
    // RCS anchors of L432.
    assert!((hull_rcs_m2(150.0) - 150.0).abs() < 1e-6 && (hull_rcs_m2(1_800.0) - 7_000.0).abs() < 1e-6);
}
