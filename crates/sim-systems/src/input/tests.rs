//! Tests of unit `sys-input`, and the hand-built ship fixture `ship::controls` tests share.
use super::*;
use crate::doctrine::MountInfo;
use crate::protocol::command::{Command, CommandEnvelope, ControlRole, ShotDesignation};
use crate::protocol::event::ShotBinding;
use crate::ship::{ShipDesign, ShipOrders, TurnModel};
use content::campaign::DifficultySliders;
use content::cell::CellGrid;
use content::graph::MountLimits;
use content::part::{DecoyKind, Flammable};
use content::ruleset::Ruleset;
use sim_core::parts::{MountState, PartRuntime, PartState};
use sim_core::vessel::{Vessel, VesselPrefab};
use sim_core::Tick;

pub(crate) struct Env { pub cat: Catalogue, pub tuning: TuningTable, pub weather: WeatherState, pub diff: DifficultySliders }
impl Env {
    pub(crate) fn new() -> Self { Env { cat: Catalogue::default(), tuning: TuningTable::default(), weather: WeatherState::default(), diff: DifficultySliders::NORMAL } }
    pub(crate) fn ctx(&self, tick: u64) -> SimCtx<'_> {
        SimCtx { seed: 7, tick: Tick(tick), catalogue: &self.cat, tuning: &self.tuning, weather: &self.weather, time_of_day: TimeOfDay::Day, layer_depth_m: 50.0, difficulty: &self.diff }
    }
}

pub(crate) fn track(id: u32, pos: DVec3, stance: Stance, domain: Domain) -> Track { crate::timeflow::tests::track(id, pos, stance, domain) }

pub(crate) fn picture(tracks: Vec<Track>) -> SidePicture { SidePicture { side: SideId(0), tracks, next_id: 100 } }

pub(crate) fn mount(slot: u16, weapon: WeaponClass, group: u8) -> MountInfo {
    MountInfo {
        slot: PartSlot(slot), part: PartIdx(0), weapon, munition: None, pos_local: DVec3::new(10.0, 3.0, 0.0), zero_rad: 0.0,
        limits: MountLimits { train_min_deg: -150.0, train_max_deg: 150.0, elevation_min_deg: -10.0, elevation_max_deg: 80.0 },
        effective_range_m: 8_000.0, director: None, kit: None, group, muzzle_mps: 800.0, point_defence: false,
    }
}

pub(crate) fn part(slot: u16, state: PartState) -> PartRuntime {
    PartRuntime {
        slot: PartSlot(slot), def: PartIdx(0), pos: Default::default(), rot: Default::default(), cells: Vec::new(), centre: DVec3::ZERO,
        compartment: None, health: 1.0, level: Default::default(), links: Default::default(), priority: 0, state,
    }
}

pub(crate) fn ready_mount() -> PartState { PartState::Mount(MountState { ready_rounds: 10, on_target: true, ..Default::default() }) }

/// Slots: 0 engine (inert), 1 gun (group 0), 2 missile launcher (group 1), 3 chaff launcher, 4 pump, 5 door.
pub(crate) fn parts() -> Vec<PartRuntime> {
    vec![
        part(0, PartState::Inert),
        part(1, ready_mount()),
        part(2, PartState::Launcher { ready: 4, reload_ticks: 0, interval_ticks: 0, train_rad: 0.0 }),
        part(3, PartState::Decoy { rounds: 6, reload_ticks: 0, streamed: false }),
        part(4, PartState::Pump { running: false, target: None }),
        part(5, PartState::Door { closed: false }),
    ]
}

fn grid() -> sim_core::grid::VesselGrid { sim_core::grid::VesselGrid::build(Arc::new(CellGrid::new([1, 1, 1])), &[], &|_| (false, Flammable::No)) }

pub(crate) fn design() -> ShipDesign {
    let prefab = VesselPrefab {
        hash: Default::default(), name: "test".into(), grid: grid(), parts: parts(), compartments: Default::default(), networks: Default::default(),
        dry_mass: Default::default(), cuboids: Vec::new(), blocks: Default::default(), root_cell: Default::default(), berths: 4, waterline_y: 0.0,
        dims: [40.0, 6.0, 8.0], work_counts: [0; 4],
    };
    ShipDesign {
        hash: Default::default(), prefab: Arc::new(prefab), settings: Default::default(), rules: Vec::new(), overrides: Default::default(),
        graph: Default::default(),
        mounts: vec![mount(1, WeaponClass::Gun, 0), mount(2, WeaponClass::AntiShipMissile, 1), mount(3, WeaponClass::Decoy, 2)],
        emitters: Vec::new(), sensor_slots: Vec::new(), consumer_slots: vec![PartSlot(1), PartSlot(2)],
        decoys: vec![(PartSlot(3), DecoyKind::Chaff)], pumps: vec![PartSlot(4)], doors: vec![PartSlot(5)], bays: Vec::new(),
        range_tables: BTreeMap::new(),
        turn: TurnModel { top_speed_mps: 15.0, turn_radius_m: 150.0, accel_mps2: 0.5, stop_distance_m: 300.0, beam_m: 8.0, draft_m: 3.0, length_m: 40.0 },
        cost: 1_000, tonnage_t: 150.0,
        blueprint: Arc::new(content::blueprint::io::new_blueprint("test", 40.0)), compiled_under: Default::default(), auto_limits: BTreeMap::new(),
    }
}

pub(crate) const OWN: EntityId = EntityId { index: 1, generation: 0 };

pub(crate) fn ship() -> Ship {
    Ship {
        vessel: OWN, side: SideId(0), player: Some(PlayerSlot(0)), design: Arc::new(design()), settings: Default::default(), rules: Vec::new(),
        graph: Default::default(), graph_state: Default::default(), fleet_doctrine: Vec::new(), doctrine: Default::default(), orders: ShipOrders::default(),
        control: ControlState::default(), stages: Default::default(), offboard: Default::default(), proficiency: Default::default(), captain: None,
        sinks: Default::default(), part_command: vec![Default::default(); 6], board: Default::default(), detections: Vec::new(), tag: String::new(),
    }
}

pub(crate) fn vessel() -> Vessel {
    Vessel {
        id: OWN, side: SideId(0), player: Some(PlayerSlot(0)), class: None, blueprint: Default::default(), name: "test".into(), status: Default::default(),
        fidelity: Default::default(), grid: grid(), parts: parts(), compartments: Default::default(), networks: Default::default(), crew: Default::default(),
        controls: Default::default(), dry_mass: Default::default(), mass: Default::default(), body: Default::default(), cuboids: Vec::new(),
        blocks: Default::default(), kin: Default::default(), bounds: Default::default(), root_cell: Default::default(), dims: [40.0, 6.0, 8.0],
        waterline_y: 0.0, last_fired_tick: 0, capsize_ticks: 0, struck: false, pending_destroyed: Vec::new(), parent: None, spawn_tick: 0,
        submersible: false, tonnage_t: 150.0,
    }
}

fn env(seq: u32, command: Command) -> CommandEnvelope { CommandEnvelope { player: PlayerSlot(0), seq, client_render_tick: 95, rtt_ms: 120, command } }

fn apply(s: &mut Ship, seq: u32, c: Command, pic: &SidePicture, mode: GameMode) -> Applied { apply_command(s, &env(seq, c), pic, mode, &Ruleset::default(), 100) }

#[test]
fn duplicates_are_dropped_and_rejections_consume_the_number() {
    let mut s = ship();
    let p = picture(vec![]);
    assert_eq!(apply(&mut s, 1, Command::SetSpeed { speed_kn: 12.0 }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(apply(&mut s, 1, Command::SetSpeed { speed_kn: 20.0 }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::Duplicate));
    assert_eq!(s.orders.speed_kn, Some(12.0));
    assert_eq!(apply(&mut s, 2, Command::SetSpeed { speed_kn: f64::NAN }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue));
    assert_eq!(apply(&mut s, 2, Command::SetSpeed { speed_kn: 5.0 }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::Duplicate));
    // Unsequenced in-process commands always apply.
    assert_eq!(apply(&mut s, 0, Command::SetSpeed { speed_kn: 5.0 }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(apply(&mut s, 0, Command::SetSpeed { speed_kn: 6.0 }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(s.orders.speed_kn, Some(6.0));
}

#[test]
fn course_station_and_designation() {
    let mut s = ship();
    let p = picture(vec![track(3, DVec3::new(5_000.0, 0.0, 0.0), Stance::Suspect, Domain::Surface), track(4, DVec3::ZERO, Stance::Friend, Domain::Surface)]);
    let wp = vec![DVec2::new(100.0, 0.0), DVec2::new(200.0, 50.0)];
    assert_eq!(apply(&mut s, 1, Command::SetCourse { waypoints: wp.clone(), speed_kn: Some(18.0), append: false }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(apply(&mut s, 2, Command::SetCourse { waypoints: vec![DVec2::new(300.0, 0.0)], speed_kn: None, append: true }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(s.orders.route.len(), 3);
    assert_eq!(s.orders.speed_kn, Some(18.0));
    assert_eq!(apply(&mut s, 3, Command::SetCourse { waypoints: vec![DVec2::new(f64::INFINITY, 0.0)], speed_kn: None, append: false }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue));
    assert_eq!(s.orders.route.len(), 3);
    assert_eq!(apply(&mut s, 4, Command::SetCourse { waypoints: vec![DVec2::ZERO; MAX_ROUTE + 1], speed_kn: None, append: false }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue));
    assert_eq!(apply(&mut s, 5, Command::HoldStation { at: DVec2::new(10.0, 10.0), heading_rad: Some(-1.0) }, &p, GameMode::Campaign), Applied::Ok);
    assert!(s.orders.route.is_empty());
    assert!(s.orders.station.unwrap().1.unwrap() > 0.0);
    assert_eq!(apply(&mut s, 6, Command::Designate { track: Some(TrackId(3)) }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(s.orders.designated, TrackId(3));
    assert_eq!(apply(&mut s, 7, Command::Designate { track: Some(TrackId(4)) }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::FriendCannotBeEngaged));
    assert_eq!(apply(&mut s, 8, Command::Designate { track: Some(TrackId(9)) }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::NoSuchTrack));
    assert_eq!(apply(&mut s, 9, Command::Designate { track: None }, &p, GameMode::Campaign), Applied::Ok);
    assert!(s.orders.designated.is_none());
}

#[test]
fn takeover_and_manual_fire() {
    let mut s = ship();
    let p = picture(vec![track(3, DVec3::new(5_000.0, 0.0, 0.0), Stance::Hostile, Domain::Surface), track(4, DVec3::ZERO, Stance::Friend, Domain::Surface)]);
    let dir = ShotDesignation::Direction { dir_local: DVec3::new(2.0, 0.0, 0.0) };
    assert_eq!(apply(&mut s, 1, Command::Fire { role: ControlRole::Mount(PartSlot(1)), shot: dir }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::RoleNotHeld));
    assert_eq!(apply(&mut s, 2, Command::TakeControl { role: ControlRole::WeaponGroup(9), on: true }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::NoSuchPart));
    assert_eq!(apply(&mut s, 3, Command::TakeControl { role: ControlRole::WeaponGroup(0), on: true }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(held_mounts(&s), vec![PartSlot(1)]);
    // A mount inside a held group may be fired by its own role.
    assert_eq!(apply(&mut s, 4, Command::Fire { role: ControlRole::Mount(PartSlot(1)), shot: dir }, &p, GameMode::Campaign), Applied::Ok);
    match s.control.pending[0].shot { ShotDesignation::Direction { dir_local } => assert!((dir_local.length() - 1.0).abs() < 1e-12), _ => panic!("direction shot") };
    assert_eq!(s.control.pending[0].client_render_tick, 95);
    assert_eq!(apply(&mut s, 5, Command::Fire { role: ControlRole::WeaponGroup(0), shot: ShotDesignation::Track { track: TrackId(4), offset_m: DVec3::ZERO } }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::FriendCannotBeEngaged));
    assert_eq!(apply(&mut s, 6, Command::Fire { role: ControlRole::Mount(PartSlot(2)), shot: dir }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue), "launchers are not aimed by hand");
    for k in 0..20 { apply(&mut s, 10 + k, Command::Fire { role: ControlRole::WeaponGroup(0), shot: dir }, &p, GameMode::Campaign); }
    assert_eq!(s.control.pending.len(), MAX_PENDING);
    assert_eq!(apply(&mut s, 40, Command::TakeControl { role: ControlRole::WeaponGroup(0), on: false }, &p, GameMode::Campaign), Applied::Ok);
    assert!(s.control.pending.iter().all(|p| p.role != ControlRole::WeaponGroup(0)));
    assert!(held_mounts(&s).is_empty());
}

#[test]
fn launches_decoys_and_damage_control() {
    let mut s = ship();
    let p = picture(vec![track(3, DVec3::new(5_000.0, 0.0, 0.0), Stance::Hostile, Domain::Surface)]);
    assert_eq!(apply(&mut s, 1, Command::Launch { slot: PartSlot(1), track: Some(TrackId(3)), bearing_rad: None, depth_m: None }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::NoSuchPart));
    assert_eq!(apply(&mut s, 2, Command::Launch { slot: PartSlot(2), track: None, bearing_rad: None, depth_m: None }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue));
    assert_eq!(apply(&mut s, 3, Command::Launch { slot: PartSlot(2), track: Some(TrackId(3)), bearing_rad: None, depth_m: None }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(s.orders.launches.len(), 1);
    assert_eq!(apply(&mut s, 4, Command::FireDecoy { slot: PartSlot(3) }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(apply(&mut s, 5, Command::FireDecoy { slot: PartSlot(3) }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(s.orders.decoys, vec![PartSlot(3)]);
    assert_eq!(apply(&mut s, 6, Command::Pump { slot: PartSlot(5), running: true, target: None }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::NoSuchPart));
    assert_eq!(apply(&mut s, 7, Command::Pump { slot: PartSlot(4), running: true, target: None }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(apply(&mut s, 8, Command::Pump { slot: PartSlot(4), running: false, target: None }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(s.stages.dc.manual_pumps, vec![(PartSlot(4), false, None)]);
    assert_eq!(apply(&mut s, 9, Command::AssignTeam { team: 1, order: Some(sim_core::parts::TeamOrder::Repair(PartSlot(1))) }, &p, GameMode::Campaign), Applied::Ok);
    assert_eq!(s.stages.dc.pinned.len(), 1);
    assert_eq!(apply(&mut s, 10, Command::AssignTeam { team: 1, order: None }, &p, GameMode::Campaign), Applied::Ok);
    assert!(s.stages.dc.pinned.is_empty());
    assert_eq!(apply(&mut s, 11, Command::ReorderRepairs(vec![content::graph::RepairPriority::OtherFires, content::graph::RepairPriority::OtherFires]), &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue));
}

#[test]
fn settings_mode_rules_and_handoffs() {
    let mut s = ship();
    let p = picture(vec![track(3, DVec3::ZERO, Stance::Suspect, Domain::Surface), track(4, DVec3::ZERO, Stance::Friend, Domain::Surface)]);
    assert_eq!(apply(&mut s, 1, Command::SetPreset { mount: None, preset: DoctrinePreset::Aggressive }, &p, GameMode::Campaign), Applied::Ok);
    assert!(s.control.settings_dirty);
    s.control.settings_dirty = false;
    assert_eq!(apply(&mut s, 2, Command::SetSlider { key: "no.such.slider".into(), value: 0.5 }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue));
    assert!(!s.control.settings_dirty);
    let key = crate::officers::tuning::sliders()[0].key;
    assert_eq!(apply(&mut s, 3, Command::SetSlider { key: key.into(), value: 1.0e9 }, &p, GameMode::Campaign), Applied::Ok);
    let def = crate::officers::tuning::slider(key).unwrap();
    assert!(s.settings.sliders[key] <= def.max.max(def.min));
    assert_eq!(apply(&mut s, 4, Command::SetRules(vec![]), &p, GameMode::PvpElimination), Applied::Ok);
    assert_eq!(apply(&mut s, 5, Command::SetPowerPriority { slot: PartSlot(1), priority: 9 }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::InvalidValue));
    assert_eq!(apply(&mut s, 6, Command::SetPowerPriority { slot: PartSlot(4), priority: 2 }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::NoSuchPart));
    // Track overrides are validated, then handed to the Sim.
    assert_eq!(apply(&mut s, 7, Command::SetStance { track: TrackId(3), stance: Some(Stance::Hostile) }, &p, GameMode::Campaign), Applied::NotShip);
    assert_eq!(apply(&mut s, 8, Command::SetStance { track: TrackId(4), stance: Some(Stance::Hostile) }, &p, GameMode::Campaign), Applied::Rejected(RejectReason::FriendCannotBeEngaged));
    assert_eq!(apply(&mut s, 9, Command::SetStance { track: TrackId(4), stance: None }, &p, GameMode::Campaign), Applied::NotShip);
    // Graph and script edits only in the splash basin; time commands are the Sim's, never in PvP.
    assert_eq!(apply(&mut s, 10, Command::RearmScripts, &p, GameMode::Campaign), Applied::Rejected(RejectReason::OnlyInDryDockOrSplash));
    assert_eq!(apply(&mut s, 11, Command::RearmScripts, &p, GameMode::Splash), Applied::NotShip);
    assert_eq!(apply(&mut s, 12, Command::SplashRestart, &p, GameMode::Skirmish), Applied::Rejected(RejectReason::NotAllowedInMode));
    assert_eq!(apply(&mut s, 13, Command::Pause(true), &p, GameMode::PvpElimination), Applied::Rejected(RejectReason::NotAllowedInMode));
    assert_eq!(apply(&mut s, 14, Command::Pause(true), &p, GameMode::Campaign), Applied::NotShip);
    assert_eq!(apply(&mut s, 15, Command::Withdraw, &p, GameMode::Campaign), Applied::NotShip);
}

#[test]
fn limits_clamp_into_the_arc() {
    let lim = MountLimits { train_min_deg: -150.0, train_max_deg: 150.0, elevation_min_deg: -10.0, elevation_max_deg: 80.0 };
    let (t, e) = clamp_to_limits(0.5, 2.0, &lim);
    assert!((t - 0.5).abs() < 1e-12 && (e - 80f64.to_radians()).abs() < 1e-12);
    // 170 degrees lies in the dead arc nearer +150; -170 nearer -150.
    assert!((clamp_to_limits(170f64.to_radians(), 0.0, &lim).0 - 150f64.to_radians()).abs() < 1e-9);
    assert!((clamp_to_limits(-170f64.to_radians(), 0.0, &lim).0 + 150f64.to_radians()).abs() < 1e-9);
    // 400 degrees wraps to 40.
    assert!((clamp_to_limits(400f64.to_radians(), 0.0, &lim).0 - 40f64.to_radians()).abs() < 1e-9);
    let full = MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: -10.0, elevation_max_deg: 80.0 };
    assert!((clamp_to_limits(3.5, 0.0, &full).0 - (3.5 - core::f64::consts::TAU)).abs() < 1e-9);
    assert_eq!(clamp_to_limits(f64::NAN, f64::NAN, &lim), (0.0, 0.0));
}

fn table() -> Vec<RangeRow> {
    (1..=8).map(|i| { let r = 250.0 * i as f64; RangeRow { range_m: r, elevation_rad: 1e-5 * r, time_of_flight_s: r / 800.0, impact_mps: 700.0, fall_angle_rad: 0.0 } }).collect()
}

#[test]
fn elevation_comes_from_the_table_plus_sight() {
    let t = table();
    let g = GunFacts { table: Some(&t), def: None, drag_k: 0.0 };
    assert!((gun_elevation(&g, 800.0, 1_000.0, 0.0) - 0.01).abs() < 1e-12);
    assert!((gun_elevation(&g, 800.0, 1_125.0, 0.0) - 0.01125).abs() < 1e-12);
    assert!((gun_elevation(&g, 800.0, 125.0, 0.0) - 0.00125).abs() < 1e-12);
    assert!((gun_elevation(&g, 800.0, 50_000.0, 0.0) - 0.02).abs() < 1e-12, "beyond reach: longest row");
    let up = gun_elevation(&g, 800.0, 1_000.0, 100.0);
    assert!((up - (0.01 + libm::atan2(100.0, 1_000.0))).abs() < 1e-12);
    let none = GunFacts { table: None, def: None, drag_k: 0.0 };
    assert!((gun_elevation(&none, 0.0, 1_000.0, 0.0)).abs() < 1e-12, "no table and no muzzle: line of sight");
}

fn manual<'a>(m: &'a MountInfo, p: &'a PartRuntime, c: &'a ControlState, pic: &'a SidePicture, targets: &'a [AimTarget], t: &'a [RangeRow], tuning: &'a TuningTable) -> ManualIn<'a> {
    ManualIn { mount: m, part: Some(p), gun: GunFacts { table: Some(t), def: None, drag_k: 0.0 }, pose: Pose::default(), own_vel: DVec3::ZERO, control: c, picture: pic, targets, tick: 200, tuning }
}

#[test]
fn direction_shots_are_corrected_to_the_server_tick() {
    let m = mount(1, WeaponClass::Gun, 0);
    let p = part(1, ready_mount());
    let tbl = table();
    let tuning = TuningTable::default();
    let pic = picture(vec![]);
    let muzzle = m.pos_local;
    // The target moves +Z at 10 m/s; the client rendered it 30 ticks (0.5 s) ago, 5 m back along its course.
    let now = DVec3::new(1_000.0, 0.0, 5.0);
    let tgt = AimTarget { entity: EntityId { index: 9, generation: 0 }, pos: now, vel: DVec3::new(0.0, 0.0, 10.0), half_len_m: 20.0, friend: false };
    let seen = now - DVec3::new(0.0, 0.0, 5.0);
    let dir = (seen - muzzle).normalize();
    let mut c = ControlState { held: vec![ControlRole::Mount(PartSlot(1))], ..Default::default() };
    c.pending.push(PendingShot { role: ControlRole::Mount(PartSlot(1)), shot: ShotDesignation::Direction { dir_local: dir }, client_render_tick: 170, rtt_ms: 60, issued_tick: 199 });
    let out = manual_core(&manual(&m, &p, &c, &pic, &[tgt], &tbl, &tuning));
    assert!(out.order.fire);
    assert_eq!(out.order.max_rounds, 1);
    // Training points at the corrected point (bearing to the target now), not where the client saw it.
    let want = libm::atan2(-(now.z - muzzle.z), now.x - muzzle.x);
    assert!((out.order.train_rad - want).abs() < 2e-4, "{} vs {}", out.order.train_rad, want);
    assert!(out.order.elevation_rad > 0.0, "range elevation added");
    assert!((out.target_speed_mps - 10.0).abs() < 1e-6);
    assert_eq!(out.shot.map(|s| (s.1, s.2)), Some((170, 60)));
    // The same shot at a Friend is refused, and a stale shot is not fired.
    let friend = AimTarget { friend: true, ..tgt };
    assert!(!manual_core(&manual(&m, &p, &c, &pic, &[friend], &tbl, &tuning)).order.fire);
    c.pending[0].issued_tick = 0;
    let out = manual_core(&manual(&m, &p, &c, &pic, &[tgt], &tbl, &tuning));
    assert!(!out.order.fire && out.shot.is_none());
}

#[test]
fn trigger_fire_follows_the_input_aim_and_meets_the_sea() {
    let m = mount(1, WeaponClass::Gun, 0);
    let p = part(1, ready_mount());
    let tbl = table();
    let tuning = TuningTable::default();
    let pic = picture(vec![]);
    let mut c = ControlState { held: vec![ControlRole::WeaponGroup(0)], ..Default::default() };
    // Aimed 0.3 degrees below the horizon from 3 m: the ray meets the sea about 573 m out.
    let dip = 0.3f64.to_radians();
    c.input.aim_local = [libm::cos(dip) as f32, -libm::sin(dip) as f32, 0.0];
    let out = manual_core(&manual(&m, &p, &c, &pic, &[], &tbl, &tuning));
    assert!(!out.order.fire, "no trigger, no fire");
    let r = 3.0 / libm::tan(dip);
    let expect = 1e-5 * r + libm::atan2(-3.0, r);
    assert!((out.order.elevation_rad - expect).abs() < 1e-3, "{} vs {}", out.order.elevation_rad, expect);
    c.input.trigger = true;
    let out = manual_core(&manual(&m, &p, &c, &pic, &[], &tbl, &tuning));
    assert!(out.order.fire && out.order.max_rounds == 0, "continuous fire keeps the full cyclic rate");
    // Aimed above the horizon with nothing there: the ray itself.
    c.input.aim_local = [1.0, 0.5, 0.0];
    let out = manual_core(&manual(&m, &p, &c, &pic, &[], &tbl, &tuning));
    assert!((out.order.elevation_rad - libm::atan2(0.5, 1.0)).abs() < 1e-6);
}

#[test]
fn track_shots_use_the_solver_on_the_track_estimate() {
    let m = mount(1, WeaponClass::Gun, 0);
    let p = part(1, ready_mount());
    let tbl = table();
    let tuning = TuningTable::default();
    let pic = picture(vec![track(5, DVec3::new(10.0, 0.0, -2_000.0), Stance::Hostile, Domain::Surface)]);
    let mut c = ControlState { held: vec![ControlRole::Mount(PartSlot(1))], ..Default::default() };
    c.pending.push(PendingShot { role: ControlRole::Mount(PartSlot(1)), shot: ShotDesignation::Track { track: TrackId(5), offset_m: DVec3::ZERO }, client_render_tick: 190, rtt_ms: 40, issued_tick: 199 });
    let out = manual_core(&manual(&m, &p, &c, &pic, &[], &tbl, &tuning));
    assert!(out.order.fire);
    // North of the mount (-Z) from a bow-facing mount: +90 degrees of training (toward port).
    assert!((out.order.train_rad - core::f64::consts::FRAC_PI_2).abs() < 0.01, "{}", out.order.train_rad);
}

fn spawn(id: u32, tick: u64, shooter: EntityId, mount: u16) -> sim_core::events::PhysEvent {
    sim_core::events::PhysEvent::ProjectileSpawned(sim_core::projectiles::ProjectileSpawn {
        id: sim_core::projectiles::ProjectileId(id), tick, origin: DVec3::ZERO, velocity: DVec3::X, calibre_mm: 76, band: content::part::weapons::CalibreBand::ALL[1],
        shooter, mount: PartSlot(mount), rounds: 1, point_defence: false,
    })
}

fn row(tick: u64, mount: u16, shooter: EntityId, rtt: u16, speed: f32) -> ShotLogEntry {
    ShotLogEntry { tick, player: PlayerSlot(0), mount: PartSlot(mount), shot: ShotDesignation::Direction { dir_local: DVec3::X }, rtt_ms: rtt, client_render_tick: tick, target_speed_mps: speed, hit: None, binding: ShotBinding { shooter, ..Default::default() } }
}

#[test]
fn shots_bind_to_their_rounds_and_resolve() {
    use sim_core::events::PhysEvent;
    let other = EntityId { index: 7, generation: 0 };
    let target = EntityId { index: 8, generation: 0 };
    let mut log = vec![row(100, 1, OWN, 50, 12.0), row(100, 1, other, 50, 12.0), row(130, 1, OWN, 50, 12.0)];
    // Twin barrels: two contiguous rounds from our mount; one from the other ship's same slot.
    resolve_shots(&mut log, &[spawn(10, 100, OWN, 1), spawn(11, 100, OWN, 1), spawn(12, 101, other, 1), spawn(13, 100, OWN, 2)]);
    assert_eq!((log[0].binding.first_projectile, log[0].binding.projectiles, log[0].binding.open), (10, 2, 2));
    assert_eq!((log[1].binding.first_projectile, log[1].binding.projectiles), (12, 1));
    assert_eq!(log[2].binding.projectiles, 0, "a later row waits for its own rounds");
    let splash = |id: u32, shooter: EntityId| PhysEvent::Splash { projectile: sim_core::projectiles::ProjectileId(id), pos: DVec3::ZERO, calibre_mm: 76, shooter };
    resolve_shots(&mut log, &[splash(10, OWN)]);
    assert_eq!(log[0].hit, None, "one round still flying");
    resolve_shots(&mut log, &[splash(12, other)]);
    assert_eq!(log[1].hit, Some(false));
    let impact = PhysEvent::ShellImpact { projectile: sim_core::projectiles::ProjectileId(11), vessel: target, cell: Default::default(), outcome: sim_core::armour::HitOutcome::Penetrated, world_point: DVec3::ZERO, shooter: OWN, burst: false };
    resolve_shots(&mut log, &[impact]);
    assert_eq!(log[0].hit, Some(true));
    // Events from rounds nobody logged change nothing.
    resolve_shots(&mut log, &[splash(99, OWN)]);
    assert_eq!(log[2].hit, None);
}

#[test]
fn rewind_trigger_needs_samples_and_a_25_percent_gap() {
    let mut log = Vec::new();
    let mut add = |n: usize, hits: usize, rtt: u16, speed: f32| for i in 0..n { let mut r = row(0, 1, OWN, rtt, speed); r.hit = Some(i < hits); log.push(r); };
    add(200, 100, 20, 12.0);
    add(199, 70, 150, 12.0);
    assert!(!rewind_triggered(&log), "199 high-RTT shots are not enough");
    let mut log2 = log.clone();
    let mut r = row(0, 1, OWN, 150, 12.0);
    r.hit = Some(false);
    log2.push(r);
    assert!(rewind_triggered(&log2), "35% vs 50%: 30% below");
    // Exactly 25% below triggers; slower targets and track shots do not count.
    let mut log3 = Vec::new();
    for i in 0..200 { let mut r = row(0, 1, OWN, 10, 12.0); r.hit = Some(i < 100); log3.push(r); }
    for i in 0..200 { let mut r = row(0, 1, OWN, 100, 12.0); r.hit = Some(i < 75); log3.push(r); }
    assert!(rewind_triggered(&log3));
    if let Some(r) = log3.iter_mut().find(|r| r.rtt_ms == 100 && r.hit == Some(false)) { r.hit = Some(true); }
    assert!(!rewind_triggered(&log3), "76 / 200 is less than 25% below");
    let slow: Vec<ShotLogEntry> = log2.iter().map(|r| ShotLogEntry { target_speed_mps: 5.0, ..*r }).collect();
    assert!(!rewind_triggered(&slow));
}

#[test]
fn pose_history_keeps_twelve_aligned_ticks() {
    let mut h = PoseHistory::default();
    let a = EntityId { index: 1, generation: 0 };
    let b = EntityId { index: 2, generation: 0 };
    let pose = |x: f64| Pose::new(DVec3::new(x, 0.0, 0.0), DQuat::IDENTITY);
    for t in 0..20u64 {
        let mut ps = vec![(a, pose(t as f64))];
        if t >= 15 { ps.push((b, pose(-(t as f64)))); }
        record_core(&mut h, t, &ps);
    }
    assert_eq!(h.ticks, (8..20).collect::<Vec<u64>>());
    assert_eq!(h.poses[&a].len(), 12);
    assert_eq!(h.poses[&a].last().unwrap().pos.x, 19.0);
    assert_eq!(h.poses[&b].len(), 5);
    // Recording the same tick again replaces it; a vanished entity is forgotten.
    record_core(&mut h, 19, &[(a, pose(100.0))]);
    assert_eq!(h.ticks.len(), 12);
    assert_eq!(h.poses[&a].last().unwrap().pos.x, 100.0);
    assert!(!h.poses.contains_key(&b));
}
