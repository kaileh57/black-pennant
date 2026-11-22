use super::*;
use crate::doctrine::resolve;
use crate::doctrine::tests::{mount, track};
use content::graph::OfficerSettings;
use sim_core::networks::PartLinks;
use sim_core::parts::{MountState, PartState};

fn own() -> OwnState { OwnState { vessel: EntityId::NONE, side: SideId(0), pose: Pose::default(), vel: DVec3::ZERO, ang_vel: DVec3::ZERO, heel_rad: 0.0 } }

fn at(mut t: Track, pos: DVec3, vel: DVec3) -> Track { t.pos = pos; t.vel = vel; t }

fn part(slot: u16, state: PartState) -> PartRuntime {
    PartRuntime {
        slot: PartSlot(slot), def: PartIdx(0), pos: Default::default(), rot: Default::default(), cells: Vec::new(), centre: DVec3::ZERO,
        compartment: None, health: 1.0, level: DamageLevel::Intact, links: PartLinks::default(), priority: 3, state,
    }
}
fn gun_part(slot: u16) -> PartRuntime { part(slot, PartState::Mount(MountState { ready_rounds: 80, ..Default::default() })) }
fn launcher_part(slot: u16, ready: u8) -> PartRuntime { part(slot, PartState::Launcher { ready, reload_ticks: 0, interval_ticks: 0, train_rad: 0.0 }) }

fn rank(tracks: &[Track], designated: TrackId, doctrine: &DoctrineTable, tick: u64) -> Vec<RankedThreat> {
    let sliders = BTreeMap::new();
    let hulls = [(DVec3::ZERO, DVec3::ZERO)];
    threat_ranking(&RankIn { own: own(), tracks, designated, sliders: &sliders, own_side_hulls: &hulls, tick }, doctrine, &TuningTable::default())
}

struct Fx { mounts: Vec<MountInfo>, parts: Vec<PartRuntime>, doctrine: DoctrineTable, state: EngageState }
impl Fx {
    fn new(mounts: Vec<MountInfo>, parts: Vec<PartRuntime>) -> Self {
        let doctrine = resolve(&[], &OfficerSettings::default(), &mounts, &TuningTable::default());
        Fx { mounts, parts, doctrine, state: EngageState::default() }
    }
    fn run(&mut self, tracks: &[Track], tick: u64, held: &[PartSlot], permission: &[Option<bool>]) -> Vec<Assignment> {
        let ranked = rank(tracks, TrackId::NONE, &self.doctrine, tick);
        let input = AssignIn { own: own(), ranked: &ranked, tracks, mounts: &self.mounts, parts: &self.parts, doctrine: &self.doctrine, held, permission, tick, gunnery: None };
        mount_assignment(&input, &mut self.state, &TuningTable::default())
    }
}

#[test]
fn inbound_boundaries() {
    let t = TuningTable::default();
    let hull = [(DVec3::ZERO, DVec3::ZERO)];
    let m = |x: f64, z: f64| at(track(1, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon), DVec3::new(x, 5.0, z), DVec3::new(-250.0, 0.0, 0.0));
    let c = inbound(&m(10_000.0, 0.0), &hull, 0, &t).unwrap();
    assert!((c.time_s - 40.0).abs() < 1e-9 && c.distance_m < 6.0);
    assert!(inbound(&m(10_000.0, 499.0), &hull, 0, &t).is_some());
    assert!(inbound(&m(10_000.0, 501.0), &hull, 0, &t).is_none());
    // 60 s horizon: 15 km out at 250 m/s arrives at 60 s; 15.2 km is still 200 m out at 60 s, 15.6 km is not.
    assert!(inbound(&m(15_000.0, 0.0), &hull, 0, &t).is_some());
    assert!(inbound(&m(15_400.0, 0.0), &hull, 0, &t).is_some());
    assert!(inbound(&m(15_600.0, 0.0), &hull, 0, &t).is_none());
    let mut dropped = m(1_000.0, 0.0);
    dropped.dropped_tick = Some(0);
    assert!(inbound(&dropped, &hull, 0, &t).is_none());
    assert!(inbound(&at(m(0.0, 0.0), DVec3::NAN, DVec3::ZERO), &hull, 0, &t).is_none());
}

#[test]
fn ranking_tiers_and_ties() {
    let doctrine = DoctrineTable { self_defence_range_m: 4_000.0, ..Default::default() };
    let w = |id, x: f64| at(track(id, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon), DVec3::new(x, 5.0, 0.0), DVec3::new(-250.0, 0.0, 0.0));
    let s = |id, stance, x: f64| at(track(id, stance, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(x, 0.0, 0.0), DVec3::ZERO);
    let tracks = vec![
        s(1, Stance::Hostile, 9_000.0), s(2, Stance::Hostile, 6_000.0), s(3, Stance::Suspect, 3_000.0), s(4, Stance::Suspect, 5_000.0),
        s(5, Stance::Neutral, 1_000.0), s(6, Stance::Friend, 500.0), w(7, 10_000.0), w(8, 5_000.0),
        // A missile flying away: not inbound, never ranked.
        at(track(9, Stance::Hostile, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon), DVec3::new(2_000.0, 5.0, 0.0), DVec3::new(250.0, 0.0, 0.0)),
        s(10, Stance::Hostile, 6_000.0), s(11, Stance::Suspect, 7_000.0),
    ];
    let r = rank(&tracks, TrackId(11), &doctrine, 0);
    let ids: Vec<u32> = r.iter().map(|x| x.track.0).collect();
    assert_eq!(ids, vec![8, 7, 11, 2, 10, 1, 3]);
    assert!(r[0].inbound && r[0].category == TargetCategory::InboundWeapon && r[0].time_to_cpa_s.unwrap() < r[1].time_to_cpa_s.unwrap());
    assert!(r[2].designated);
    assert!(r.windows(2).all(|p| p[0].score >= p[1].score));
    // Same input, same output; input order does not matter.
    let mut rev = tracks.clone();
    rev.reverse();
    assert_eq!(rank(&rev, TrackId(11), &doctrine, 0), r);
}

#[test]
fn gun_engages_and_reports_holds() {
    let mut fx = Fx::new(vec![mount(2, WeaponClass::Gun, 8_000.0)], vec![gun_part(2)]);
    let hostile = |grade, x: f64, z: f64| at(track(1, Stance::Hostile, IdRung::Type, grade, Domain::Surface), DVec3::new(x, 0.0, z), DVec3::ZERO);
    let a = fx.run(&[hostile(TrackGrade::Fine, 5_000.0, 0.0)], 0, &[], &[]);
    assert_eq!(a[0].status, EngageStatus::Engaging { track: TrackId(1), reason: FireReason::Doctrine });
    assert_eq!(a[0].launch, 0);
    assert_eq!(fx.run(&[hostile(TrackGrade::Coarse, 5_000.0, 0.0)], 6, &[], &[])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::Grade { have: TrackGrade::Coarse, need: TrackGrade::Fine } });
    assert!(matches!(fx.run(&[hostile(TrackGrade::Fine, 9_000.0, 0.0)], 12, &[], &[])[0].status, EngageStatus::Holding { reason: HoldReason::OutOfRange { .. }, .. }));
    // Port is positive training: a target dead abeam to port needs +90 degrees against a +-70 arc.
    fx.mounts[0].limits.train_min_deg = -70.0;
    fx.mounts[0].limits.train_max_deg = 70.0;
    match fx.run(&[hostile(TrackGrade::Fine, 0.0, -5_000.0)], 18, &[], &[])[0].status {
        EngageStatus::Holding { reason: HoldReason::TrainLimit { need_deg, limit_deg }, .. } => assert!((need_deg - 90.0).abs() < 1e-6 && limit_deg == 70.0, "{need_deg} {limit_deg}"),
        s => panic!("{s:?}"),
    }
    match fx.run(&[hostile(TrackGrade::Fine, 0.0, 5_000.0)], 24, &[], &[])[0].status {
        EngageStatus::Holding { reason: HoldReason::TrainLimit { need_deg, limit_deg }, .. } => assert!((need_deg + 90.0).abs() < 1e-6 && limit_deg == -70.0),
        s => panic!("{s:?}"),
    }
    // Suspect tracks: self-defence inside 4 km, a reason outside it, nothing for Neutral.
    let suspect = |x: f64| at(track(3, Stance::Suspect, IdRung::Unknown, TrackGrade::Fine, Domain::Surface), DVec3::new(x, 0.0, 0.0), DVec3::ZERO);
    fx.mounts[0].limits.train_min_deg = -150.0;
    fx.mounts[0].limits.train_max_deg = 150.0;
    assert_eq!(fx.run(&[suspect(3_000.0)], 30, &[], &[])[0].status, EngageStatus::Engaging { track: TrackId(3), reason: FireReason::SelfDefence });
    assert_eq!(fx.run(&[suspect(6_000.0)], 36, &[], &[])[0].status, EngageStatus::Holding { track: TrackId(3), reason: HoldReason::SuspectOutsideSelfDefence { range_m: 6_000.0, limit_m: 4_000.0 } });
    let neutral = at(track(4, Stance::Neutral, IdRung::Type, TrackGrade::Lock, Domain::Surface), DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO);
    assert_eq!(fx.run(&[neutral], 42, &[], &[])[0].status, EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoTarget });
}

#[test]
fn gates_hold_with_reasons() {
    let mut fx = Fx::new(vec![mount(2, WeaponClass::Gun, 8_000.0)], vec![gun_part(2)]);
    let t = [at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(4_000.0, 0.0, 0.0), DVec3::ZERO)];
    assert_eq!(fx.run(&t, 0, &[PartSlot(2)], &[])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::PlayerControl });
    assert_eq!(fx.run(&t, 6, &[], &[Some(false)])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::PermissionWithheld });
    fx.parts[0].links.shed = true;
    assert_eq!(fx.run(&t, 12, &[], &[])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::Shed });
    fx.parts[0].level = DamageLevel::Destroyed;
    assert_eq!(fx.run(&t, 18, &[], &[])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::Damaged });
    fx.parts.clear();
    assert_eq!(fx.run(&t, 24, &[], &[])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::Damaged });
    // Empty ready rack with no loaded magazine aboard.
    fx.parts = vec![part(2, PartState::Mount(MountState::default()))];
    assert_eq!(fx.run(&t, 30, &[], &[])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::NoAmmunition });
    fx.parts.push(part(7, PartState::Magazine { rounds: 100, ammo_kg: 600.0, flooded: false, cookoff_ticks: 0 }));
    assert!(matches!(fx.run(&t, 36, &[], &[])[0].status, EngageStatus::Engaging { .. }));
    // A rule's permission fires past doctrine (Suspect outside self-defence) on range alone.
    let s = [at(track(5, Stance::Suspect, IdRung::Unknown, TrackGrade::Bearing, Domain::Surface), DVec3::new(6_000.0, 0.0, 0.0), DVec3::ZERO)];
    assert_eq!(fx.run(&s, 42, &[], &[Some(true)])[0].status, EngageStatus::Engaging { track: TrackId(5), reason: FireReason::Graph });
}

#[test]
fn masking_and_friends_in_line() {
    let mut fwd = mount(2, WeaponClass::Gun, 8_000.0);
    fwd.pos_local = DVec3::new(10.0, 3.0, 0.0);
    let mut aft = mount(3, WeaponClass::Gun, 8_000.0);
    aft.pos_local = DVec3::new(0.0, 3.0, 0.0);
    let mut fx = Fx::new(vec![fwd, aft], vec![gun_part(2), gun_part(3)]);
    let ahead = at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(5_000.0, 0.0, 0.0), DVec3::ZERO);
    let a = fx.run(core::slice::from_ref(&ahead), 0, &[], &[]);
    assert!(matches!(a[0].status, EngageStatus::Engaging { .. }));
    assert_eq!(a[1].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::Masked });
    // Superfiring: raise the aft mount 3 m and it clears.
    fx.mounts[1].pos_local.y = 6.0;
    assert!(matches!(fx.run(core::slice::from_ref(&ahead), 6, &[], &[])[1].status, EngageStatus::Engaging { .. }));
    // A friend 2 km down the line of fire holds both; our own track at the ship never does.
    let friend = at(track(2, Stance::Friend, IdRung::Exact, TrackGrade::Lock, Domain::Surface), DVec3::new(3_000.0, 0.0, 50.0), DVec3::ZERO);
    let me = at(track(3, Stance::Friend, IdRung::Exact, TrackGrade::Lock, Domain::Surface), DVec3::ZERO, DVec3::ZERO);
    let a = fx.run(&[ahead.clone(), friend, me.clone()], 12, &[], &[]);
    assert_eq!(a[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::FriendInLine { friend: TrackId(2) } });
    assert!(matches!(fx.run(&[ahead.clone(), me.clone()], 18, &[], &[])[0].status, EngageStatus::Engaging { .. }));
    // Our own missile on its way down the same line is not a friend in the line.
    let missile = at(track(4, Stance::Friend, IdRung::Exact, TrackGrade::Lock, Domain::Weapon), DVec3::new(2_000.0, 5.0, 0.0), DVec3::new(250.0, 0.0, 0.0));
    assert!(matches!(fx.run(&[ahead, missile, me], 24, &[], &[])[0].status, EngageStatus::Engaging { .. }));
}

#[test]
fn sam_second_shot_after_a_miss() {
    let mut fx = Fx::new(vec![mount(4, WeaponClass::Sam, 20_000.0)], vec![launcher_part(4, 8)]);
    fx.mounts[0].muzzle_mps = 800.0;
    let air = [at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Air), DVec3::new(8_000.0, 3_000.0, 0.0), DVec3::ZERO)];
    let a = fx.run(&air, 0, &[], &[]);
    assert_eq!((a[0].launch, a[0].status), (1, EngageStatus::Engaging { track: TrackId(1), reason: FireReason::Doctrine }));
    assert_eq!(fx.state.weapons_in_flight, 1);
    // Range 8.5 km at 800 m/s plus the margin: about 13.7 s. Until then the target is not offered again.
    let a = fx.run(&air, 60, &[], &[]);
    assert_eq!((a[0].launch, a[0].status), (0, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::SalvoInFlight }));
    assert!(fx.state.mounts[0].awaiting_result);
    let a = fx.run(&air, 60 * 14, &[], &[]);
    assert_eq!(a[0].launch, 1, "second missile after the first missed");
    // A second aircraft draws its own missile while the first salvo flies.
    let two = [air[0].clone(), at(track(2, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Air), DVec3::new(9_000.0, 3_000.0, 0.0), DVec3::ZERO)];
    let a = fx.run(&two, 60 * 15, &[], &[]);
    assert_eq!((a[0].launch, a[0].status), (1, EngageStatus::Engaging { track: TrackId(2), reason: FireReason::Doctrine }));
    assert_eq!(fx.state.weapons_in_flight, 2);
    // Kill the first target: its salvo resolves.
    let a = fx.run(&two[1..], 60 * 16, &[], &[]);
    assert_eq!(a[0].launch, 0);
    assert_eq!(fx.state.weapons_in_flight, 1);
}

#[test]
fn ashm_salvo_and_seeker_basket() {
    let mut fx = Fx::new(vec![mount(4, WeaponClass::AntiShipMissile, 24_000.0), mount(5, WeaponClass::AntiShipMissile, 24_000.0)], vec![launcher_part(4, 4), launcher_part(5, 4)]);
    let target = at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Coarse, Domain::Surface), DVec3::new(20_000.0, 0.0, 0.0), DVec3::ZERO);
    let a = fx.run(core::slice::from_ref(&target), 0, &[], &[]);
    assert_eq!((a[0].launch, a[1].launch), (1, 0), "one salvo per target");
    assert_eq!(a[1].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::SalvoInFlight });
    let a = fx.run(core::slice::from_ref(&target), 6, &[], &[]);
    assert_eq!(a[0].launch, 1, "second of two");
    let a = fx.run(core::slice::from_ref(&target), 12, &[], &[]);
    assert_eq!((a[0].launch, a[1].launch), (0, 0));
    assert_eq!(fx.state.weapons_in_flight, 2);
    // A Neutral near the target holds a fresh launcher.
    let mut fx = Fx::new(vec![mount(4, WeaponClass::AntiShipMissile, 24_000.0)], vec![launcher_part(4, 4)]);
    let fisher = at(track(2, Stance::Neutral, IdRung::Type, TrackGrade::Coarse, Domain::Surface), DVec3::new(21_000.0, 0.0, 800.0), DVec3::ZERO);
    let a = fx.run(&[target.clone(), fisher], 0, &[], &[]);
    assert_eq!((a[0].launch, a[0].status), (0, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::NeutralInSeekerBasket { neutral: TrackId(2) } }));
}

#[test]
fn basket_geometry() {
    let target = at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(20_000.0, 0.0, 0.0), DVec3::ZERO);
    let n = |id, x: f64, z: f64| at(track(id, Stance::Neutral, IdRung::Type, TrackGrade::Fine, Domain::Surface), DVec3::new(x, 0.0, z), DVec3::ZERO);
    let fov = 30f64.to_radians();
    let hit = |ts: &[Track]| neutral_in_basket(&target, ts, 12_000.0, fov, DVec3::ZERO, 0);
    assert_eq!(hit(&[n(2, 22_000.0, 1_000.0)]), Some(TrackId(2)));
    assert_eq!(hit(&[n(2, 10_000.0, 0.0)]), None, "short of the seeker switch-on point");
    assert_eq!(hit(&[n(2, 20_000.0, 6_000.0)]), None, "outside the half-angle");
    assert_eq!(hit(&[n(2, 27_000.0, 0.0)]), None, "beyond the seeker's reach");
    assert_eq!(hit(&[n(3, 20_500.0, 0.0), n(2, 21_000.0, 0.0)]), Some(TrackId(3)), "nearest the target");
    assert_eq!(neutral_in_basket(&target, &[n(2, 20_100.0, 0.0)], f64::NAN, fov, DVec3::ZERO, 0), None);
}

#[test]
fn point_defence_takes_only_inbound_weapons() {
    let mut fx = Fx::new(vec![mount(6, WeaponClass::PointDefenceGun, 2_000.0)], vec![gun_part(6)]);
    let inbound_m = at(track(1, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon), DVec3::new(1_800.0, 5.0, 0.0), DVec3::new(-250.0, 0.0, 0.0));
    let passing = at(track(2, Stance::Hostile, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon), DVec3::new(1_000.0, 5.0, 900.0), DVec3::new(0.0, 0.0, 250.0));
    let a = fx.run(&[inbound_m.clone(), passing.clone()], 0, &[], &[]);
    assert_eq!(a[0].status, EngageStatus::Engaging { track: TrackId(1), reason: FireReason::PointDefence });
    assert_eq!(fx.run(&[passing], 6, &[], &[])[0].status, EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoTarget });
    // Under a silent director the hold names emission control.
    fx.mounts[0].director = Some(PartSlot(9));
    fx.parts.push(part(9, PartState::Sensor { bearing_rad: 0.0, swept_from_rad: 0.0, radiating: false, train_rad: 0.0 }));
    let mut fine = inbound_m;
    fine.grade = TrackGrade::Fine;
    assert_eq!(fx.run(&[fine], 12, &[], &[])[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::EmconSilent });
}

#[test]
fn decoys_one_salvo_per_missile() {
    let t = TuningTable::default();
    let launchers = [(PartSlot(10), DecoyKind::Chaff), (PartSlot(11), DecoyKind::Flare), (PartSlot(12), DecoyKind::TowedDecoy), (PartSlot(13), DecoyKind::Smoke)];
    let mut state = EngageState::default();
    let missile = |id, x: f64| { let mut m = at(track(id, Stance::Suspect, IdRung::Unknown, TrackGrade::Fine, Domain::Weapon), DVec3::new(x, 5.0, 0.0), DVec3::new(-250.0, 0.0, 0.0)); m.inbound = true; m };
    // 7 km out: towed decoy streams, nothing launched.
    let o = decoy_control(&own(), &[missile(1, 7_000.0)], &launchers, &mut state, 0, &t);
    assert_eq!(o, vec![(PartSlot(12), DecoyOrder::StreamOn)]);
    // Inside 6 km: one chaff and one flare salvo, once.
    let o = decoy_control(&own(), &[missile(1, 5_900.0)], &launchers, &mut state, 6, &t);
    assert_eq!(o, vec![(PartSlot(10), DecoyOrder::Fire), (PartSlot(11), DecoyOrder::Fire)]);
    assert!(decoy_control(&own(), &[missile(1, 5_000.0)], &launchers, &mut state, 12, &t).is_empty());
    // A second missile during the reload waits for it.
    let two = [missile(1, 4_000.0), missile(2, 5_000.0)];
    assert!(decoy_control(&own(), &two, &launchers, &mut state, 300, &t).is_empty());
    let chaff = state.mounts.iter().find(|m| m.slot == PartSlot(10)).unwrap();
    assert!(matches!(chaff.status, EngageStatus::Holding { track: TrackId(2), reason: HoldReason::Reloading { .. } }));
    let o = decoy_control(&own(), &two, &launchers, &mut state, 6 + 600, &t);
    assert_eq!(o, vec![(PartSlot(10), DecoyOrder::Fire), (PartSlot(11), DecoyOrder::Fire)]);
    // Threat gone: stream off once; decoyed list pruned of dead tracks.
    assert_eq!(decoy_control(&own(), &[], &launchers, &mut state, 700, &t), vec![(PartSlot(12), DecoyOrder::StreamOff)]);
    assert!(decoy_control(&own(), &[], &launchers, &mut state, 706, &t).is_empty());
    assert!(state.decoyed.is_empty());
    // Friend weapons never trigger anything; torpedoes stream the towed decoy but draw no chaff.
    let mut ours = missile(3, 3_000.0);
    ours.stance = Stance::Friend;
    assert!(decoy_control(&own(), &[ours], &launchers, &mut state, 800, &t).is_empty());
    let mut torp = missile(4, 3_000.0);
    torp.pos.y = -8.0;
    assert_eq!(decoy_control(&own(), &[torp], &launchers, &mut state, 806, &t), vec![(PartSlot(12), DecoyOrder::StreamOn)]);
}

#[test]
fn envelope_ignores_decoys_and_dead_mounts() {
    let mounts = vec![mount(2, WeaponClass::Gun, 8_000.0), mount(4, WeaponClass::AntiShipMissile, 24_000.0), mount(10, WeaponClass::Decoy, 6_000.0)];
    let doctrine = resolve(&[], &OfficerSettings::default(), &mounts, &TuningTable::default());
    assert_eq!(longest_envelope_m(&mounts, &doctrine, &[]), 24_000.0);
    let parts = vec![gun_part(2), part(4, PartState::Launcher { ready: 0, reload_ticks: 0, interval_ticks: 0, train_rad: 0.0 })];
    assert_eq!(longest_envelope_m(&mounts, &doctrine, &parts), 8_000.0);
    assert_eq!(longest_envelope_m(&mounts[2..], &doctrine, &parts), 0.0);
}

#[test]
fn salvo_completes_on_a_second_launcher() {
    // Launcher 4 has one ready cell left, launcher 5 a full rack: the two-missile salvo still goes out.
    let mut fx = Fx::new(vec![mount(4, WeaponClass::AntiShipMissile, 24_000.0), mount(5, WeaponClass::AntiShipMissile, 24_000.0)], vec![launcher_part(4, 1), launcher_part(5, 4)]);
    let target = [at(track(1, Stance::Hostile, IdRung::Type, TrackGrade::Coarse, Domain::Surface), DVec3::new(20_000.0, 0.0, 0.0), DVec3::ZERO)];
    let a = fx.run(&target, 0, &[], &[]);
    assert_eq!((a[0].launch, a[1].launch), (1, 0));
    fx.parts[0].state = PartState::Launcher { ready: 0, reload_ticks: 0, interval_ticks: 0, train_rad: 0.0 };
    let a = fx.run(&target, 6, &[], &[]);
    assert_eq!(a[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::NoAmmunition });
    assert_eq!((a[1].launch, a[1].status), (1, EngageStatus::Engaging { track: TrackId(1), reason: FireReason::Doctrine }));
    // Two in flight: the salvo is closed.
    let a = fx.run(&target, 12, &[], &[]);
    assert_eq!((a[1].launch, a[1].status), (0, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::SalvoInFlight }));
    assert_eq!(fx.state.weapons_in_flight, 2);
    // While the first launcher is still working through its salvo, nobody else joins.
    let mut fx = Fx::new(vec![mount(4, WeaponClass::AntiShipMissile, 24_000.0), mount(5, WeaponClass::AntiShipMissile, 24_000.0)], vec![launcher_part(4, 4), launcher_part(5, 4)]);
    fx.run(&target, 0, &[], &[]);
    fx.parts[0].state = PartState::Launcher { ready: 3, reload_ticks: 0, interval_ticks: 60, train_rad: 0.0 };
    let a = fx.run(&target, 6, &[], &[]);
    assert_eq!((a[0].launch, a[0].status), (0, EngageStatus::Engaging { track: TrackId(1), reason: FireReason::Doctrine }));
    assert_eq!(a[1].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::SalvoInFlight });
}

#[test]
fn forced_permission_never_fires_at_neutral() {
    let mut fx = Fx::new(vec![mount(2, WeaponClass::Gun, 8_000.0)], vec![gun_part(2)]);
    let neutral = [at(track(1, Stance::Neutral, IdRung::Type, TrackGrade::Lock, Domain::Surface), DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO)];
    // A replaced ranking stage offers the Neutral anyway.
    let ranked = [RankedThreat { track: TrackId(1), category: TargetCategory::Surface, score: 1.0, range_m: 2_000.0, time_to_cpa_s: None, inbound: false, designated: false }];
    let input = AssignIn { own: own(), ranked: &ranked, tracks: &neutral, mounts: &fx.mounts, parts: &fx.parts, doctrine: &fx.doctrine, held: &[], permission: &[Some(true)], tick: 0, gunnery: None };
    let a = mount_assignment(&input, &mut fx.state, &TuningTable::default());
    assert_eq!(a[0].status, EngageStatus::Holding { track: TrackId(1), reason: HoldReason::Stance { have: Stance::Neutral, need: Stance::Hostile } });
}
