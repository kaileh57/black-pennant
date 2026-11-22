use super::*;
use crate::doctrine::tests::{mount, track};
use crate::graph::library::{stage_sig, SigFacts};
use crate::ids::SensorRef;
use crate::sensors::Evidence;
use crate::signature::EmitterKind;
use content::catalogue::catalogue;
use content::graph::MountLimits;
use sim_core::networks::PartLinks;
use sim_core::parts::{MountState, TeamOrder};

fn part(slot: u16, def: u16, state: PartState) -> PartRuntime {
    PartRuntime {
        slot: PartSlot(slot), def: PartIdx(def), pos: Default::default(), rot: Default::default(), cells: Vec::new(), centre: DVec3::ZERO,
        compartment: None, health: 1.0, level: DamageLevel::Intact, links: PartLinks { power_supplied: 1.0, ..Default::default() }, priority: 3, state,
    }
}

/// Index of the first main gun in the embedded catalogue.
fn gun_def() -> u16 {
    catalogue().parts.values().position(|d| matches!(d.function, PartFunction::Gun(ref g) if !g.point_defence)).expect("a gun in the catalogue") as u16
}

fn at(mut t: Track, pos: DVec3, vel: DVec3) -> Track { t.pos = pos; t.vel = vel; t }

struct Fx {
    tick: u64,
    tuning: TuningTable,
    kin: Kinematics,
    parts: Vec<PartRuntime>,
    mounts: Vec<MountInfo>,
    emitters: Vec<EmitterInfo>,
    turn: TurnModel,
    settings: OfficerSettings,
    orders: ShipOrders,
    doctrine: DoctrineTable,
    tracks: Vec<Track>,
    detections: Vec<Detection>,
    hulls: Vec<(DVec3, DVec3)>,
    control: ControlState,
    st: StageStates,
}
impl Fx {
    fn new() -> Self {
        Fx {
            tick: 600, tuning: TuningTable::default(), kin: Kinematics::default(), parts: Vec::new(), mounts: Vec::new(), emitters: Vec::new(),
            turn: TurnModel::default(), settings: OfficerSettings::default(), orders: ShipOrders::default(), doctrine: DoctrineTable::default(),
            tracks: Vec::new(), detections: Vec::new(), hulls: vec![(DVec3::ZERO, DVec3::ZERO)], control: ControlState::default(), st: StageStates::default(),
        }
    }
    /// A gun (slot 0) and an anti-ship missile launcher (slot 1), with the cautious doctrine.
    fn armed() -> Self {
        let mut fx = Fx::new();
        fx.mounts = vec![mount(0, WeaponClass::Gun, 8_000.0), mount(1, WeaponClass::AntiShipMissile, 20_000.0)];
        fx.parts = vec![
            part(0, gun_def(), PartState::Mount(MountState { ready_rounds: 80, ..Default::default() })),
            part(1, 0, PartState::Launcher { ready: 4, reload_ticks: 0, interval_ticks: 0, train_rad: 0.0 }),
        ];
        fx.doctrine = crate::doctrine::resolve(&[], &fx.settings, &fx.mounts, &fx.tuning);
        fx
    }
    fn env(&self) -> StageEnv<'_> {
        StageEnv {
            tick: self.tick, tuning: &self.tuning, catalogue: catalogue(), vessel_id: EntityId::NONE, side: SideId(0), kin: self.kin, heel_rad: 0.0,
            power_margin_kw: 100.0, hull_health: 1.0, parts: &self.parts, mounts: &self.mounts, emitters: &self.emitters, consumer_slots: &[],
            pumps: &[], doors: &[], turn: &self.turn, settings: &self.settings, orders: &self.orders, doctrine: &self.doctrine, tracks: &self.tracks,
            detections: &self.detections, own_side_hulls: &self.hulls, esm_sites: &[], control: &self.control, world: None, vessel: None,
            range_tables: None, accuracy_mult: 1.0, sea: Default::default(),
        }
    }
    /// Run one stage with its default parameters; the output buffer starts dirty, as the VM's pool may hand it.
    fn stage(&mut self, stage: OfficerStage, part: Option<PartSlot>, inputs: &[PortValue]) -> Vec<PortValue> {
        let params: Vec<PortValue> = crate::graph::library::stage_params_spec(stage).iter().map(|p| PortValue::Scalar(p.default)).collect();
        let facts = SigFacts { mounts: &self.mounts, sensor_slots: &[], consumer_slots: &[], pumps: &[], doors: &[] };
        let n = stage_sig(stage, &facts).outputs.len();
        let mut out = vec![PortValue::Bool(true); n];
        let mut st = std::mem::take(&mut self.st);
        let mut work = Work::default();
        eval_stage_env(&self.env(), &mut st, &mut work, stage, part, inputs, &params, &mut out);
        self.st = st;
        out
    }
}

fn ids(v: &PortValue) -> Vec<u32> { match v { PortValue::TrackList(l) => l.iter().map(|t| t.0).collect(), other => panic!("not a list: {other:?}") } }

#[test]
fn slow_stages_run_once_per_period_and_after_a_rewind() {
    assert!(slow_due(None, 0));
    assert!(!slow_due(Some(0), 4), "same 100 ms period");
    assert!(slow_due(Some(0), 6));
    assert!(slow_due(Some(5), 7), "clock went back");
    // Graph ticks on odd ticks still get exactly one run per period.
    let mut last = None;
    let runs = (1..60u64).step_by(2).filter(|t| { let due = slow_due(last, *t); if due { last = Some(slow_period(*t)); } due }).count();
    assert_eq!(runs, 10);
}

#[test]
fn track_list_filters_and_sorts_nearest_first() {
    let mut fx = Fx::new();
    let h = |id, stance, grade, domain, x: f64| at(track(id, stance, IdRung::Unknown, grade, domain), DVec3::new(x, 0.0, 0.0), DVec3::ZERO);
    let mut dropped = h(5, Stance::Hostile, TrackGrade::Fine, Domain::Surface, 100.0);
    dropped.dropped_tick = Some(1);
    fx.tracks = vec![
        h(1, Stance::Hostile, TrackGrade::Fine, Domain::Surface, 5_000.0),
        h(2, Stance::Suspect, TrackGrade::Coarse, Domain::Air, 2_000.0),
        h(3, Stance::Friend, TrackGrade::Fine, Domain::Surface, 1_000.0),
        h(4, Stance::Neutral, TrackGrade::Fine, Domain::Surface, 500.0),
        dropped,
        at(track(6, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon), DVec3::new(3_000.0, 5.0, 0.0), DVec3::new(-250.0, 0.0, 0.0)),
    ];
    let tick = fx.tick;
    fx.tracks.iter_mut().for_each(|t| t.last_update_tick = tick);
    let env = fx.env();
    let f = filter_tracks(&env, &TrackFilter::from_params(&[]));
    assert_eq!(f.ids, vec![TrackId(2), TrackId(6), TrackId(1)]);
    assert_eq!(f.nearest_range_m, Some(2_000.0));
    assert_eq!(f.inbound, 1, "only the closing weapon is inbound");
    let all = filter_tracks(&env, &TrackFilter::from_params(&[PortValue::Scalar(0.0)]));
    assert_eq!(all.ids.iter().map(|t| t.0).collect::<Vec<_>>(), vec![4, 3, 2, 6, 1]);
    let surf = TrackFilter::from_params(&[PortValue::Scalar(3.0), PortValue::Scalar(0.0)]);
    assert_eq!(filter_tracks(&env, &surf).ids, vec![TrackId(1)]);
    let lock = TrackFilter::from_params(&[PortValue::Scalar(2.0), PortValue::Scalar(-1.0), PortValue::Scalar(3.0)]);
    assert_eq!(filter_tracks(&env, &lock).ids, vec![TrackId(6)]);
    // Junk parameters fall back to the defaults or clamp.
    let junk = TrackFilter::from_params(&[PortValue::Scalar(f64::NAN), PortValue::Scalar(1e300), PortValue::Text("x".into())]);
    assert_eq!(junk, TrackFilter { min_stance: Stance::Suspect, domain: Some(Domain::Weapon), min_grade: TrackGrade::Bearing });

    let mut out = vec![PortValue::None; 5];
    read_source_env(&env, None, &SourceKind::TrackList, &[], &mut out);
    assert_eq!(ids(&out[0]), vec![2, 6, 1]);
    assert_eq!(&out[1..], &[PortValue::Scalar(3.0), PortValue::Track(TrackId(2)), PortValue::Scalar(2_000.0), PortValue::Scalar(1.0)]);
    // A filter nothing passes reports an empty list and no nearest contact (rules stay silent).
    read_source_env(&env, None, &SourceKind::TrackList, &[PortValue::Scalar(3.0), PortValue::Scalar(1.0)], &mut out);
    assert_eq!(out, vec![PortValue::TrackList(Arc::new(vec![])), PortValue::Scalar(0.0), PortValue::None, PortValue::None, PortValue::Scalar(0.0)]);
}

#[test]
fn orders_and_detection_sources() {
    let mut fx = Fx::new();
    fx.orders.designated = TrackId(7);
    fx.orders.weapons_tight = true;
    fx.orders.speed_kn = Some(12.0);
    let det = |index: u16, tick: u64, m: Measurement| Detection {
        tick, sensor: SensorRef { carrier: EntityId::NONE, index }, side: SideId(0), kind: SensorKind::SearchRadar, sensor_pos: DVec3::ZERO,
        target: EntityId::NONE, measurement: m, sigma_range_m: 30.0, sigma_bearing_rad: 0.01, evidence: Evidence::empty(), emitter: u16::MAX,
        rcs_m2: 100.0, domain_hint: None,
    };
    fx.detections = vec![
        det(3, 10, Measurement::RangeBearing { range_m: 9_000.0, bearing_rad: 1.0, height_m: None }),
        det(3, 20, Measurement::BearingOnly { bearing_rad: 1.1 }),
        det(4, 15, Measurement::RangeBearing { range_m: 4_000.0, bearing_rad: 2.0, height_m: None }),
    ];
    let env = fx.env();
    let mut out = vec![PortValue::None; 6];
    read_source_env(&env, None, &SourceKind::TheatreOrders, &[], &mut out);
    assert_eq!(out, vec![PortValue::Orders, PortValue::Track(TrackId(7)), PortValue::Scalar(12.0), PortValue::Bool(true), PortValue::Bool(false), PortValue::None]);
    let mut out = vec![PortValue::None; 4];
    read_source_env(&env, None, &SourceKind::Detections(PartSlot(3)), &[], &mut out);
    assert!(matches!(&out[0], PortValue::Detections(d) if d.len() == 2));
    assert_eq!(&out[1..], &[PortValue::Scalar(2.0), PortValue::None, PortValue::Scalar(0.01)], "latest detection is bearing-only");
    read_source_env(&env, None, &SourceKind::Detections(PartSlot(9)), &[], &mut out);
    assert_eq!(out[1], PortValue::Scalar(0.0));
    // The host API indexes all of the ship's detections in `Ship::detections` order.
    let mut cache = HostCache::default();
    let mut call = |i: u16, args: &[PortValue]| host_call(&env, &mut cache, i, args);
    use crate::script::host_fn as h;
    assert_eq!(call(h::DETECTION_COUNT, &[]), Ok(PortValue::Scalar(3.0)));
    assert_eq!(call(h::DETECTION_RANGE_M, &[PortValue::Scalar(0.0)]), Ok(PortValue::Scalar(9_000.0)));
    assert_eq!(call(h::DETECTION_RANGE_M, &[PortValue::Scalar(1.0)]), Ok(PortValue::None));
    assert_eq!(call(h::DETECTION_BEARING, &[PortValue::Scalar(2.0)]), Ok(PortValue::Scalar(2.0)));
    assert_eq!(call(h::DETECTION_SENSOR, &[PortValue::Scalar(2.0)]), Ok(PortValue::Scalar(4.0)));
    assert_eq!(call(h::DETECTION_BEARING, &[PortValue::Scalar(1e30)]), Ok(PortValue::None));
}

#[test]
fn launch_edges() {
    let mut mem = Vec::new();
    let (s, t1, t2) = (PartSlot(4), TrackId(1), TrackId(2));
    // Officer pulses: true on assignment ticks only, each pulse is a launch.
    assert!(launch_edge(&mut mem, s, true, t1));
    assert!(!launch_edge(&mut mem, s, false, t1));
    assert!(launch_edge(&mut mem, s, true, t1));
    // A held fire (rule or subgraph) launches once per target.
    assert!(!launch_edge(&mut mem, s, true, t1));
    assert!(!launch_edge(&mut mem, s, true, t1));
    assert!(launch_edge(&mut mem, s, true, t2), "new target");
    assert!(!launch_edge(&mut mem, s, true, t2));
    // Launchers keep separate memories, ascending slot.
    assert!(launch_edge(&mut mem, PartSlot(1), true, t2));
    assert_eq!(mem.iter().map(|e| e.0).collect::<Vec<_>>(), vec![PartSlot(1), s]);
}

#[test]
fn officers_engage_hold_between_runs_and_pulse_launches() {
    let mut fx = Fx::armed();
    fx.tracks = vec![at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO)];
    fx.tracks[0].last_update_tick = fx.tick;

    let fused = fx.stage(OfficerStage::TrackFusion, None, &[]);
    assert_eq!(ids(&fused[0]), vec![1]);
    assert_eq!(fused[1], PortValue::Scalar(1.0));
    assert_eq!(fused[3], PortValue::Scalar(0.5), "fire-control alpha");
    let ranked = fx.stage(OfficerStage::ThreatRanking, None, &[fused[0].clone(), PortValue::Orders]);
    assert_eq!(ids(&ranked[0]), vec![1]);
    assert_eq!(ranked[1], PortValue::Track(TrackId(1)));
    let a = fx.stage(OfficerStage::MountAssignment, None, &[ranked[0].clone(), PortValue::Orders]);
    assert_eq!(a.len(), 5);
    assert_eq!(&a[..2], &[PortValue::Track(TrackId(1)), PortValue::Bool(true)], "gun engages: {:?}", fx.st.engage.mounts);
    assert_eq!(&a[2..4], &[PortValue::Track(TrackId(1)), PortValue::Bool(true)], "launcher pulses: {:?}", fx.st.engage.mounts);
    assert_eq!(a[4], PortValue::Scalar(2.0));
    assert_eq!(fx.st.engage.open_fire_range, Some(1.0));
    let g = fx.stage(OfficerStage::PnGuidance, Some(PartSlot(1)), &[a[2].clone(), a[3].clone()]);
    assert_eq!(&g[..3], &[PortValue::Bool(true), PortValue::Track(TrackId(1)), PortValue::Scalar(3.0)]);
    assert_eq!(g[3], PortValue::None, "midcourse aim is the predicted track");

    // Next graph tick, same period: held results, the pulse is over, and a changed input is not re-ranked.
    fx.tick += 2;
    let ranked2 = fx.stage(OfficerStage::ThreatRanking, None, &[PortValue::TrackList(Arc::new(vec![])), PortValue::Orders]);
    assert_eq!(ranked2, ranked);
    let a2 = fx.stage(OfficerStage::MountAssignment, None, &[ranked[0].clone(), PortValue::Orders]);
    assert_eq!(&a2[..2], &a[..2]);
    assert_eq!(a2[3], PortValue::Bool(false));
    let g2 = fx.stage(OfficerStage::PnGuidance, Some(PartSlot(1)), &[a2[2].clone(), a2[3].clone()]);
    assert_eq!(g2[0], PortValue::Bool(false));

    // Next period: the second missile of the salvo of two.
    fx.tick += 4;
    let a3 = fx.stage(OfficerStage::MountAssignment, None, &[ranked[0].clone(), PortValue::Orders]);
    assert_eq!(a3[3], PortValue::Bool(true), "{:?}", fx.st.engage.mounts);
    let g3 = fx.stage(OfficerStage::PnGuidance, Some(PartSlot(1)), &[a3[2].clone(), a3[3].clone()]);
    assert_eq!(g3[0], PortValue::Bool(true));
}

#[test]
fn weapons_tight_and_takeover_gate_the_assignment() {
    let mut fx = Fx::armed();
    fx.tracks = vec![at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO)];
    fx.tracks[0].last_update_tick = fx.tick;
    fx.orders.weapons_tight = true;
    let a = fx.stage(OfficerStage::MountAssignment, None, &[PortValue::TrackList(Arc::new(vec![TrackId(1)])), PortValue::Orders]);
    assert_eq!(a[1], PortValue::Bool(false));
    assert!(matches!(fx.st.engage.mounts[0].status, EngageStatus::Holding { reason: HoldReason::PermissionWithheld, .. }), "{:?}", fx.st.engage.mounts[0]);

    let mut c = ControlState { held: vec![ControlRole::WeaponGroup(0), ControlRole::Helm, ControlRole::Mount(PartSlot(9))], ..Default::default() };
    assert_eq!(held_mounts(&c, &fx.mounts), vec![PartSlot(0), PartSlot(1), PartSlot(9)]);
    c.officers_in_command = true;
    assert!(held_mounts(&c, &fx.mounts).is_empty(), "a disconnected player's hull is run by the officers");
}

/// Weapons tight leaves point defence free; a struck hull holds every mount, point defence included.
#[test]
fn a_struck_hull_withholds_fire_from_every_mount() {
    let mut pd = mount(1, WeaponClass::Gun, 2_000.0);
    pd.point_defence = true;
    let mounts = vec![mount(0, WeaponClass::Gun, 8_000.0), pd];
    assert_eq!(fire_permission(&mounts, false, false), vec![None, None]);
    assert_eq!(fire_permission(&mounts, true, false), vec![Some(false), None]);
    assert_eq!(fire_permission(&mounts, true, true), vec![Some(false), Some(false)]);
    assert_eq!(fire_permission(&mounts, false, true), vec![Some(false), Some(false)]);
}

#[test]
fn a_replaced_ranking_keeps_its_order() {
    let mut fx = Fx::armed();
    let h = |id, x: f64| at(track(id, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(x, 0.0, 0.0), DVec3::ZERO);
    fx.tracks = vec![h(1, 2_000.0), h(2, 4_000.0), at(track(3, Stance::Friend, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO)];
    let tick = fx.tick;
    fx.tracks.iter_mut().for_each(|t| t.last_update_tick = tick);
    let env = fx.env();
    let input = RankIn { own: env.own_state(), tracks: env.tracks, designated: TrackId::NONE, sliders: &BTreeMap::new(), own_side_hulls: env.own_side_hulls, tick: env.tick };
    let officer = crate::engage::threat_ranking(&input, env.doctrine, env.tuning);
    assert_eq!(officer.iter().map(|r| r.track.0).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(ranked_rows(&env, &[TrackId(1), TrackId(2)], &officer), officer);
    let rows = ranked_rows(&env, &[TrackId(2), TrackId(3), TrackId(99), TrackId(2), TrackId(1)], &officer);
    assert_eq!(rows.iter().map(|r| r.track.0).collect::<Vec<_>>(), vec![2, 1], "friend, unknown and repeated ids skipped");
    assert!(rows[0].score > rows[1].score);
    // A track the officer did not rank is rebuilt.
    let rows = ranked_rows(&env, &[TrackId(2)], &[]);
    assert_eq!((rows[0].category, rows[0].range_m, rows[0].inbound), (TargetCategory::Surface, 4_000.0, false));
    // The mount assignment uses the replaced order: the gun takes the further track first.
    let a = fx.stage(OfficerStage::MountAssignment, None, &[PortValue::TrackList(Arc::new(vec![TrackId(2), TrackId(1)])), PortValue::Orders]);
    assert_eq!(a[0], PortValue::Track(TrackId(2)));
    assert_eq!(fx.st.engage.ranked.iter().map(|r| r.track.0).collect::<Vec<_>>(), vec![2, 1]);
}

#[test]
fn ballistic_solver_fires_only_on_a_reachable_permitted_solution() {
    let mut fx = Fx::armed();
    fx.mounts[0].limits = MountLimits { train_min_deg: -170.0, train_max_deg: 170.0, elevation_min_deg: -10.0, elevation_max_deg: 60.0 };
    fx.tracks = vec![at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(3_000.0, 0.0, 0.0), DVec3::new(0.0, 0.0, 5.0))];
    fx.tracks[0].last_update_tick = fx.tick;
    let fire = [PortValue::Track(TrackId(1)), PortValue::Bool(true)];
    let s = fx.stage(OfficerStage::BallisticSolver, Some(PartSlot(0)), &fire);
    assert_eq!(s.len(), 9);
    assert!(matches!(s[0], PortValue::Vec3(p) if (p.x - 3_000.0).abs() < 200.0), "{s:?}");
    assert_eq!(s[1], PortValue::Bool(true), "{s:?}");
    assert!(matches!(s[5], PortValue::Scalar(n) if n >= 1.0));
    assert_eq!(s[8], PortValue::Bool(true));
    let held = fx.stage(OfficerStage::BallisticSolver, Some(PartSlot(0)), &[PortValue::Track(TrackId(1)), PortValue::Bool(false)]);
    assert_eq!(held[1], PortValue::Bool(false));
    assert_eq!(held[0], s[0], "the mount still lays on the target");
    // Never at a friend, whatever drives the node.
    fx.tracks[0].stance = Stance::Friend;
    let f = fx.stage(OfficerStage::BallisticSolver, Some(PartSlot(0)), &fire);
    assert_eq!((&f[0], &f[1], &f[5], &f[8]), (&PortValue::None, &PortValue::Bool(false), &PortValue::Scalar(0.0), &PortValue::Bool(false)));
    let g = fx.stage(OfficerStage::PnGuidance, Some(PartSlot(1)), &fire);
    assert_eq!(&g[..2], &[PortValue::Bool(false), PortValue::None]);
    // A slot that is not a mount writes nothing.
    let none = fx.stage(OfficerStage::BallisticSolver, Some(PartSlot(40)), &fire);
    assert!(none.iter().all(|v| *v == PortValue::None));
}

#[test]
fn pn_constant_by_domain() {
    let p = [PortValue::Scalar(3.5), PortValue::Scalar(5.0)];
    assert_eq!(pn_constant(Domain::Surface, &p), 3.5);
    assert_eq!(pn_constant(Domain::Subsurface, &p), 3.5);
    assert_eq!(pn_constant(Domain::Air, &p), 5.0);
    assert_eq!(pn_constant(Domain::Weapon, &[PortValue::Scalar(9.0), PortValue::Scalar(f64::NAN)]), 4.0);
    assert_eq!(pn_constant(Domain::Surface, &[PortValue::Scalar(9.0)]), 6.0);
}

#[test]
fn per_entry_ports_follow_the_signatures() {
    // Damage control: two pumps and a door.
    let (pumps, doors, consumers) = ([PartSlot(5), PartSlot(7)], [PartSlot(9)], [PartSlot(2), PartSlot(3)]);
    let facts = SigFacts { mounts: &[], sensor_slots: &[], consumer_slots: &consumers, pumps: &pumps, doors: &doors };
    let mut out = vec![PortValue::None; stage_sig(OfficerStage::DamageControlPlanner, &facts).outputs.len()];
    let dc = DcOut {
        teams: vec![TeamOrder::Idle, TeamOrder::FightFire(CompartmentId(2))],
        pumps: vec![(PartSlot(5), true, Some(CompartmentId(2))), (PartSlot(7), false, None)],
        doors: vec![(PartSlot(9), true)],
        ..Default::default()
    };
    dc_outputs(&pumps, &doors, &dc, 1, 2, &mut out);
    assert_eq!(out, vec![
        PortValue::Bool(true), PortValue::Scalar(2.0), PortValue::Bool(false), PortValue::None, PortValue::Bool(true),
        PortValue::Scalar(1.0), PortValue::Scalar(1.0), PortValue::Scalar(2.0),
    ]);
    // Load shedder: one priority per consumer, then generation, demand and the shed count.
    let mut out = vec![PortValue::None; stage_sig(OfficerStage::LoadShedder, &facts).outputs.len()];
    let eng = crate::officers::engineer::EngineerState { shed: vec![PartSlot(3)], generation_kw: 900.0, demand_kw: 950.0, fuel_fraction: 1.0 };
    shedder_outputs(&consumers, &[(PartSlot(2), 1), (PartSlot(3), 5)], &eng, &mut out);
    assert_eq!(out, vec![PortValue::Scalar(1.0), PortValue::Scalar(5.0), PortValue::Scalar(900.0), PortValue::Scalar(950.0), PortValue::Scalar(1.0)]);
    // Emission control: emitters of one class share a port.
    let e = |slot: u16, kind, pd| EmitterInfo { slot: PartSlot(slot), kind, r_ref_m: 20_000.0, height_m: 10.0, rotation_s: 2.5, point_defence: pd };
    let emitters = [e(1, EmitterKind::SearchRadar, false), e(2, EmitterKind::FireControlRadar, true), e(3, EmitterKind::FireControlRadar, false), e(4, EmitterKind::Jammer, false)];
    let mut out = vec![PortValue::None; 7];
    emission_outputs(&emitters, &[false, true, false, true], false, true, &mut out);
    assert_eq!(out, [false, false, true, false, true, false, true].map(PortValue::Bool).to_vec());
    // Mount assignment with no mounts still reports the engaged count.
    let mut out = vec![PortValue::None; 1];
    assignment_outputs(&[], &StageStates::default(), 0, &mut out);
    assert_eq!(out, vec![PortValue::Scalar(0.0)]);
}

#[test]
fn helm_readouts() {
    let route = [DVec2::new(0.0, 0.0), DVec2::new(0.0, -1_000.0)]; // due north
    assert_eq!(cross_track_m(&route, 1, false, DVec3::new(50.0, 0.0, -500.0)), Some(50.0), "east of a northbound leg is starboard");
    assert_eq!(cross_track_m(&route, 1, false, DVec3::new(-20.0, 0.0, -500.0)), Some(-20.0));
    assert_eq!(cross_track_m(&route, 0, false, DVec3::ZERO), None);
    assert_eq!(cross_track_m(&route, 1, true, DVec3::ZERO), None);
    assert_eq!(cross_track_m(&route, 7, false, DVec3::ZERO), None);
    // Formation slot 100 m ahead and 50 m to starboard of a leader at the origin heading east.
    let leader = Kinematics { pose: Pose::from_heading(DVec3::ZERO, core::f64::consts::FRAC_PI_2), ..Default::default() };
    let orders = ShipOrders { formation: Some(crate::ship::FormationSlot { leader: EntityId::NONE, offset_m: DVec2::new(100.0, 50.0) }), ..Default::default() };
    let d = station_distance_m(&orders, Some(&leader), DVec3::new(100.0, 0.0, 50.0)).unwrap();
    assert!(d < 1e-6, "{d}");
    let orders = ShipOrders { station: Some((DVec2::new(300.0, 400.0), None)), ..Default::default() };
    assert_eq!(station_distance_m(&orders, None, DVec3::ZERO), Some(500.0));
    assert_eq!(station_distance_m(&ShipOrders::default(), None, DVec3::ZERO), None);
}

#[test]
fn helm_stages_drive_the_heading_and_report_activity() {
    let mut fx = Fx::new();
    fx.turn = TurnModel { top_speed_mps: 15.0, turn_radius_m: 300.0, accel_mps2: 0.2, stop_distance_m: 400.0, beam_m: 10.0, draft_m: 3.0, length_m: 80.0 };
    fx.kin = Kinematics { pose: Pose::from_heading(DVec3::ZERO, 0.0), ..Default::default() };
    fx.orders.route = vec![DVec2::new(0.0, -2_000.0), DVec2::new(2_000.0, -4_000.0)];
    let auto = fx.stage(OfficerStage::HelmAutopilot, None, &[PortValue::Orders]);
    assert!(matches!(auto[0], PortValue::Scalar(h) if h.is_finite()));
    assert!(matches!(auto[1], PortValue::Scalar(s) if s > 0.0), "{auto:?}");
    let station = fx.stage(OfficerStage::StationKeeping, None, &[PortValue::Orders]);
    assert_eq!((&station[0], &station[2], &station[3]), (&PortValue::None, &PortValue::Bool(false), &PortValue::None));
    let ev = fx.stage(OfficerStage::Evasion, None, &[PortValue::Orders, PortValue::TrackList(Arc::new(vec![]))]);
    assert_eq!((&ev[0], &ev[2]), (&PortValue::None, &PortValue::Bool(false)));
    // Cruise speed comes from `helm.cruise_speed`, under the key the helm reads.
    let slow = slider_map(OfficerStage::HelmAutopilot, &[PortValue::Scalar(0.5), PortValue::Scalar(0.3), PortValue::Scalar(1.5)]);
    assert_eq!(slow.get(helm::SLIDER_CRUISE), Some(&0.3));
    assert_eq!(slow.get("helm.cruise_speed"), Some(&0.3));
    let clamped = slider_map(OfficerStage::HelmAutopilot, &[PortValue::Scalar(9.0), PortValue::Scalar(f64::NAN)]);
    assert_eq!((clamped["helm.responsiveness"], clamped["helm.cruise_speed"], clamped["helm.lookahead"]), (1.0, 0.7, 1.5));
}

#[test]
fn host_api_follows_the_registry_and_never_panics() {
    use crate::script::{host_api, host_fn as h};
    use content::graph::PortType;
    let mut fx = Fx::armed();
    fx.kin = Kinematics { pose: Pose::from_heading(DVec3::ZERO, core::f64::consts::FRAC_PI_2), lin_vel: DVec3::new(10.0 * KN, 0.0, 0.0), ..Default::default() };
    let mut dropped = track(3, Stance::Hostile, IdRung::Unknown, TrackGrade::Fine, Domain::Surface);
    dropped.dropped_tick = Some(1);
    let mut overridden = at(track(9, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon), DVec3::new(3_000.0, 5.0, 0.0), DVec3::new(-250.0, 0.0, 0.0));
    overridden.stance_override = Some(Stance::Hostile);
    fx.tracks = vec![at(track(2, Stance::Hostile, IdRung::Class, TrackGrade::Fine, Domain::Surface), DVec3::new(0.0, 0.0, -2_000.0), DVec3::ZERO), dropped, overridden];
    let tick = fx.tick;
    fx.tracks.iter_mut().for_each(|t| t.last_update_tick = tick);
    fx.orders.designated = TrackId(9);
    fx.settings.emcon = Emcon::Silent;
    let det = |carrier: EntityId, index: u16, m: Measurement| Detection {
        tick, sensor: SensorRef { carrier, index }, side: SideId(0), kind: SensorKind::SearchRadar, sensor_pos: DVec3::ZERO,
        target: EntityId::NONE, measurement: m, sigma_range_m: 30.0, sigma_bearing_rad: 0.01, evidence: Evidence::empty(), emitter: u16::MAX,
        rcs_m2: 100.0, domain_hint: None,
    };
    fx.detections = vec![
        det(EntityId::NONE, 5, Measurement::RangeBearing { range_m: 9_000.0, bearing_rad: 1.0, height_m: None }),
        det(EntityId::NONE, SensorRef::SEEKER, Measurement::BearingOnly { bearing_rad: 1.5 }),
    ];
    let env = fx.env();
    let mut cache = HostCache::default();
    let mut call = |i: u16, args: &[PortValue]| host_call(&env, &mut cache, i, args);
    let s = PortValue::Scalar;
    assert!(matches!(call(h::OWN_SPEED_KN, &[]), Ok(PortValue::Scalar(v)) if (v - 10.0).abs() < 1e-9));
    assert!(matches!(call(h::OWN_HEADING, &[]), Ok(PortValue::Scalar(v)) if (v - core::f64::consts::FRAC_PI_2).abs() < 1e-9));
    assert_eq!(call(h::SORTIE_TIME_S, &[]), Ok(s(10.0)));
    assert_eq!(call(h::HULL_HEALTH, &[]), Ok(s(1.0)));
    // Track queries index the live tracks, ascending id: the dropped track 3 is skipped.
    assert_eq!(call(h::TRACK_COUNT, &[]), Ok(s(2.0)));
    assert_eq!(call(h::TRACK_ID, &[s(0.0)]), Ok(PortValue::Track(TrackId(2))));
    assert_eq!(call(h::TRACK_ID, &[s(1.9)]), Ok(PortValue::Track(TrackId(9))), "indices truncate");
    assert_eq!(call(h::TRACK_ID, &[s(2.0)]), Ok(PortValue::None));
    assert_eq!(call(h::TRACK_ID, &[s(-0.5)]), Ok(PortValue::None), "negative indices are out of range");
    assert_eq!(call(h::TRACK_INDEX, &[PortValue::Track(TrackId(9))]), Ok(s(1.0)));
    assert_eq!(call(h::TRACK_INDEX, &[PortValue::Track(TrackId(3))]), Ok(PortValue::None), "dropped");
    assert_eq!(call(h::TRACK_INDEX, &[s(9.0)]), Err(ScriptFault::Error), "a track argument is a Track");
    assert_eq!(call(h::TRACK_POSITION, &[s(0.0)]), Ok(PortValue::Vec3(DVec3::new(0.0, 0.0, -2_000.0))));
    assert_eq!(call(h::TRACK_STANCE, &[s(1.0)]), Ok(PortValue::Stance(Stance::Hostile)), "override applied");
    assert_eq!(call(h::TRACK_DOMAIN, &[s(1.0)]), Ok(s(3.0)));
    assert_eq!(call(h::TRACK_GRADE, &[s(1.0)]), Ok(s(3.0)));
    assert_eq!(call(h::TRACK_RUNG, &[s(0.0)]), Ok(s(3.0)));
    assert_eq!(call(h::TRACK_INBOUND, &[s(0.0)]), Ok(PortValue::Bool(false)));
    assert_eq!(call(h::TRACK_INBOUND, &[s(1.0)]), Ok(PortValue::Bool(true)), "the closing weapon");
    // Parts: slots, ready rounds over capacity, training of mounts only.
    assert_eq!(call(h::PART_WORKS, &[s(1.0)]), Ok(PortValue::Bool(true)));
    assert_eq!(call(h::PART_WORKS, &[s(7.0)]), Ok(PortValue::None));
    assert!(matches!(call(h::PART_AMMO_FRACTION, &[s(0.0)]), Ok(PortValue::Scalar(v)) if v > 0.0 && v <= 1.0));
    assert_eq!(call(h::PART_TRAIN, &[s(0.0)]), Ok(s(0.0)));
    assert_eq!(call(h::PART_TRAIN, &[s(1.0)]), Ok(PortValue::None), "a launcher is not a mount or sensor");
    assert_eq!(call(h::PART_ELEVATION, &[s(0.0)]), Ok(s(0.0)));
    // Detections: one flat index over the ship's own detections.
    assert_eq!(call(h::DETECTION_COUNT, &[]), Ok(s(2.0)));
    assert_eq!(call(h::DETECTION_SENSOR, &[s(0.0)]), Ok(s(5.0)));
    assert_eq!(call(h::DETECTION_SENSOR, &[s(1.0)]), Ok(PortValue::None), "a seeker");
    assert_eq!(call(h::DETECTION_RANGE_M, &[s(0.0)]), Ok(s(9_000.0)));
    assert_eq!(call(h::DETECTION_RANGE_M, &[s(1.0)]), Ok(PortValue::None), "bearing only");
    assert_eq!(call(h::DETECTION_BEARING, &[s(1.0)]), Ok(s(1.5)));
    assert_eq!(call(h::DESIGNATED_TRACK, &[]), Ok(PortValue::Track(TrackId(9))));
    assert_eq!(call(h::EMCON, &[]), Ok(PortValue::Emcon(Emcon::Silent)));
    // Geometry from own position, horizontal and compass.
    assert_eq!(call(h::RANGE_TO, &[PortValue::Vec3(DVec3::new(3.0, 9.0, 4.0))]), Ok(s(5.0)));
    assert_eq!(call(h::BEARING_TO, &[PortValue::Vec3(DVec3::new(0.0, 0.0, -10.0))]), Ok(s(0.0)), "due north");
    assert_eq!(call(h::BEARING_TO, &[PortValue::Vec3(DVec3::ZERO)]), Ok(PortValue::None), "no bearing to own position");
    assert_eq!(call(h::CLAMP, &[s(5.0), s(1.0), s(3.0)]), Ok(s(3.0)));
    assert_eq!(call(h::CLAMP, &[s(5.0), s(3.0), s(1.0)]), Ok(PortValue::None), "lo > hi");
    assert!(matches!(call(h::WRAP_ANGLE, &[s(7.0)]), Ok(PortValue::Scalar(a)) if (a - (7.0 - core::f64::consts::TAU)).abs() < 1e-12));
    assert!(matches!(call(h::WRAP_ANGLE, &[s(core::f64::consts::PI)]), Ok(PortValue::Scalar(a)) if (a + core::f64::consts::PI).abs() < 1e-12), "[-pi, pi)");
    let meet = call(h::INTERCEPT_POINT, &[PortValue::Vec3(DVec3::new(1_000.0, 0.0, 0.0)), PortValue::Vec3(DVec3::ZERO), s(20.0)]);
    assert_eq!(meet, Ok(PortValue::Vec3(DVec3::new(1_000.0, 0.0, 0.0))));

    // Every registry row is answered; edge values never fault, panic or leak NaN; wrong arity and kinds fault.
    assert_eq!(host_api().len(), HANDLED_HOST_ROWS as usize, "a registry row was appended: answer it in host_call");
    let edge = [s(f64::NAN), s(f64::INFINITY), s(-1.0), s(1e300), s(-0.0), PortValue::Bool(true), PortValue::None];
    for (i, f) in host_api().iter().enumerate() {
        let i = i as u16;
        for e in &edge {
            let args: Vec<PortValue> = f.args.iter().map(|t| match t {
                PortType::Vec3 => if *e == PortValue::None { PortValue::None } else { PortValue::Vec3(DVec3::splat(f64::NAN)) },
                PortType::Track => if *e == PortValue::None { PortValue::None } else { PortValue::Track(TrackId(u32::MAX)) },
                _ => e.clone(),
            }).collect();
            match call(i, &args) {
                Ok(v) => assert_eq!(crate::graph::library::sanitize(v.clone()), v, "{} gave {v:?}", f.name),
                Err(err) => panic!("{} faulted on edge values: {err:?}", f.name),
            }
        }
        let mut long = vec![PortValue::None; f.args.len()];
        long.push(PortValue::None);
        assert_eq!(call(i, &long), Err(ScriptFault::Error), "{}", f.name);
        if !f.args.is_empty() { assert_eq!(call(i, &vec![PortValue::Orders; f.args.len()]), Err(ScriptFault::Error), "{}", f.name); }
    }
    assert_eq!(call(host_api().len() as u16, &[]), Err(ScriptFault::Error), "past the registry");
}

#[test]
fn host_cache_is_per_tick_and_survives_an_empty_picture() {
    let fx = Fx::new();
    let env = fx.env();
    let mut cache = HostCache::default();
    assert_eq!(host_call(&env, &mut cache, crate::script::host_fn::TRACK_COUNT, &[]), Ok(PortValue::Scalar(0.0)));
    assert_eq!(host_call(&env, &mut cache, crate::script::host_fn::TRACK_INBOUND, &[PortValue::Scalar(0.0)]), Ok(PortValue::None));
    assert_eq!(host_call(&env, &mut cache, crate::script::host_fn::TRACK_INDEX, &[PortValue::Track(TrackId::NONE)]), Ok(PortValue::None));
}

#[test]
fn hold_survives_a_keyframe_round_trip() {
    let mut st = StageStates::default();
    st.hold.rank_period = Some(3);
    st.hold.rank_ids = vec![TrackId(2)];
    st.hold.launchers = vec![(PartSlot(1), true, TrackId(2))];
    st.hold.dc.teams = vec![TeamOrder::Repair(PartSlot(4))];
    let back: StageStates = bincode::deserialize(&bincode::serialize(&st).unwrap()).unwrap();
    assert_eq!(back, st);
}

/// A host over the plain-data view, so the real VM can run the generated graph against these stage bodies.
struct EnvHost<'a, 'b> { env: StageEnv<'a>, st: &'b mut StageStates }
impl StageHost for EnvHost<'_, '_> {
    fn read_source(&mut self, source: &SourceKind, out: &mut [PortValue]) { read_source_env(&self.env, Some(&OwnFacts::of(&self.env)), source, &[], out); }
    fn read_source_with(&mut self, source: &SourceKind, params: &[PortValue], out: &mut [PortValue]) { read_source_env(&self.env, Some(&OwnFacts::of(&self.env)), source, params, out); }
    fn eval_stage(&mut self, stage: OfficerStage, part: Option<PartSlot>, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
        eval_stage_env(&self.env, self.st, &mut Work::default(), stage, part, inputs, params, out);
    }
}

#[test]
fn the_generated_graph_runs_on_these_stages() {
    use crate::graph::compile::tests::{compile_d, gun_rule};
    use crate::graph::generate::tests::frigate;
    use crate::graph::vm::{init_state, run_tick, VmBudget};
    use crate::graph::{SinkWrites, WriteOrigin};
    use content::graph::{GraphOverrides, SinkKind};

    let mut d = frigate();
    d.mounts[0].pos_local = DVec3::new(0.0, 0.0, -5.0);
    d.mounts[1].pos_local = DVec3::new(0.0, 0.0, 5.0);
    let mut fx = Fx::new();
    fx.mounts = d.mounts.clone();
    fx.emitters = d.emitters.clone();
    fx.parts = (0..12u16).map(|s| match s {
        2 | 3 => part(s, gun_def(), PartState::Mount(MountState { ready_rounds: 80, ..Default::default() })),
        4 => part(s, 0, PartState::Launcher { ready: 4, reload_ticks: 0, interval_ticks: 0, train_rad: 0.0 }),
        _ => part(s, 0, PartState::Inert),
    }).collect();
    fx.parts[2].centre = d.mounts[0].pos_local;
    fx.parts[3].centre = d.mounts[1].pos_local;
    fx.doctrine = crate::doctrine::resolve(&[], &fx.settings, &fx.mounts, &fx.tuning);
    fx.turn = TurnModel { top_speed_mps: 15.0, turn_radius_m: 300.0, accel_mps2: 0.2, stop_distance_m: 400.0, beam_m: 10.0, draft_m: 3.0, length_m: 80.0 };
    fx.tracks = vec![at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO)];
    fx.tracks[0].last_update_tick = fx.tick;
    let budget = VmBudget { node_evals: 100_000, luau_interrupts: 5_000 };
    let tick = |fx: &mut Fx, g: &crate::graph::CompiledGraph, gs: &mut crate::graph::GraphState| -> SinkWrites {
        let mut st = std::mem::take(&mut fx.st);
        let r = { let mut host = EnvHost { env: fx.env(), st: &mut st }; run_tick(EntityId::NONE, g, gs, &mut host, None, budget, GRAPH_DT) };
        fx.st = st;
        assert!(!r.over_budget);
        r.sinks
    };

    let g = compile_d(&d, &GraphOverrides::default(), &[]);
    let mut gs = init_state(&g, None);
    let s = tick(&mut fx, &g, &mut gs);
    assert!(matches!(s.get(SinkKind::HelmHeading).map(|w| &w.values[0]), Some(PortValue::Scalar(_))), "{:?}", s.get(SinkKind::HelmHeading));
    for gun in [2u16, 3] {
        let fire = s.get(SinkKind::MountFirePermission(PartSlot(gun))).unwrap_or_else(|| panic!("gun {gun}: {:?}", fx.st.engage.mounts));
        assert_eq!(fire.values, vec![PortValue::Bool(true), PortValue::Track(TrackId(1))], "gun {gun}");
        assert!(matches!(s.get(SinkKind::MountAimPoint(PartSlot(gun))).unwrap().values[0], PortValue::Vec3(_)));
    }
    let launch = s.get(SinkKind::MissileLaunch(PartSlot(4))).expect("missile launch");
    assert_eq!(launch.values, vec![PortValue::Bool(true), PortValue::Scalar(3.0)]);
    assert_eq!(s.get(SinkKind::MissileTarget(PartSlot(4))).unwrap().values[0], PortValue::Track(TrackId(1)));
    assert!(s.get(SinkKind::EmissionState(PartSlot(6))).is_some());
    // The next graph tick holds the engagement and ends the launch pulse.
    fx.tick += 2;
    let s2 = tick(&mut fx, &g, &mut gs);
    assert_eq!(s2.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap().values[0], PortValue::Bool(true));
    assert_eq!(s2.get(SinkKind::MissileLaunch(PartSlot(4))).unwrap().values[0], PortValue::Bool(false));

    // An L2 rule retargets gun 3 through the filtered track list while weapons are tight for the officers.
    fx.orders.weapons_tight = true;
    fx.st = StageStates::default();
    let g = compile_d(&d, &GraphOverrides::default(), &[gun_rule(3, 4_000.0)]);
    assert!(g.issues.is_empty(), "{:?}", g.issues);
    let mut gs = init_state(&g, None);
    let s = tick(&mut fx, &g, &mut gs);
    let ruled = s.get(SinkKind::MountFirePermission(PartSlot(3))).unwrap();
    assert_eq!(ruled.values, vec![PortValue::Bool(true), PortValue::Track(TrackId(1))]);
    assert!(matches!(ruled.origin, WriteOrigin::Rule(_)), "{:?}", ruled.origin);
    assert_eq!(s.get(SinkKind::MountFirePermission(PartSlot(2))).unwrap().values[0], PortValue::Bool(false), "officer holds: weapons tight");
    // Out of the rule's range the officer's hold returns.
    fx.tracks[0].pos = DVec3::new(5_000.0, 0.0, 0.0);
    fx.tick += 2;
    let s = tick(&mut fx, &g, &mut gs);
    assert_eq!(s.get(SinkKind::MountFirePermission(PartSlot(3))).unwrap().values[0], PortValue::Bool(false));
}

#[test]
fn a_replaced_ranking_stays_current_across_periods() {
    // ThreatRanking is replaced (never runs): the assignment rebuilds its rows every period instead of
    // reusing the ones it wrote itself.
    let mut fx = Fx::armed();
    fx.tracks = vec![at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO)];
    fx.tracks[0].last_update_tick = fx.tick;
    let list = PortValue::TrackList(Arc::new(vec![TrackId(1)]));
    fx.stage(OfficerStage::MountAssignment, None, &[list.clone(), PortValue::Orders]);
    assert_eq!((fx.st.engage.ranked[0].range_m, fx.st.engage.ranked[0].designated), (3_000.0, false));
    fx.tick += 6;
    fx.tracks[0].pos = DVec3::new(2_500.0, 0.0, 0.0);
    fx.tracks[0].last_update_tick = fx.tick;
    fx.orders.designated = TrackId(1);
    fx.stage(OfficerStage::MountAssignment, None, &[list, PortValue::Orders]);
    assert_eq!((fx.st.engage.ranked[0].range_m, fx.st.engage.ranked[0].designated), (2_500.0, true));
}

#[test]
fn an_unpowered_mount_still_solves_finite() {
    let mut fx = Fx::armed();
    fx.parts[0].links.power_supplied = 0.0;
    fx.tracks = vec![at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(3_000.0, 0.0, 1_000.0), DVec3::new(-10.0, 0.0, 0.0))];
    fx.tracks[0].last_update_tick = fx.tick;
    let s = fx.stage(OfficerStage::BallisticSolver, Some(PartSlot(0)), &[PortValue::Track(TrackId(1)), PortValue::Bool(true)]);
    for v in &s { assert_eq!(crate::graph::library::sanitize(v.clone()), *v); }
    assert!(matches!(s[0], PortValue::Vec3(_)), "{s:?}");
}
