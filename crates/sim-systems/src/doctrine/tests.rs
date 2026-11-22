use super::*;
use crate::guidance::gun::{single_round_pk, RangeRow};
use content::graph::MountLimits;
use content::part::weapons::CalibreBand;
use content::part::PartFunction;

pub(crate) fn track(id: u32, stance: Stance, rung: IdRung, grade: TrackGrade, domain: Domain) -> Track {
    Track {
        id: TrackId(id), side: SideId(1), domain, truth: EntityId::NONE, pos: DVec3::ZERO, vel: DVec3::ZERO, r0_m: 10.0, sigma_v: 1.0,
        bearing_only: None, created_tick: 0, last_update_tick: 0, last_lock_tick: None, dropped_tick: None, grade,
        sources: Default::default(), trend: Default::default(), rung, rung_reason: Default::default(),
        candidates: Default::default(), class: None, history: Vec::new(), rcs_m2: 0.1, consistent_ticks: 0, emitters: Vec::new(),
        id_override: None, stance, stance_reason: Default::default(), stance_override: None, fired_on_us: false,
        inbound: false, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
    }
}

pub(crate) fn mount(slot: u16, weapon: WeaponClass, range: f64) -> MountInfo {
    MountInfo {
        slot: PartSlot(slot), part: PartIdx(0), weapon, munition: None, pos_local: DVec3::ZERO, zero_rad: 0.0,
        limits: MountLimits { train_min_deg: -150.0, train_max_deg: 150.0, elevation_min_deg: -10.0, elevation_max_deg: 80.0 },
        effective_range_m: range, director: None, kit: None, group: 0, muzzle_mps: 900.0, point_defence: matches!(weapon, WeaponClass::PointDefenceGun | WeaponClass::PointDefenceMissile),
    }
}

fn find(entries: &[DoctrineEntry], w: WeaponClass, t: TargetCategory) -> DoctrineEntry { *entries.iter().find(|e| e.weapon == w && e.target == t).expect("entry") }

#[test]
fn cautious_defaults_are_the_spec_list() {
    let d = default_entries(DoctrinePreset::Cautious);
    let ashm = find(&d, WeaponClass::AntiShipMissile, TargetCategory::Surface);
    assert_eq!((ashm.min_stance, ashm.min_id, ashm.min_grade, ashm.salvo), (Stance::Hostile, IdRung::Type, TrackGrade::Coarse, 2));
    let gun = find(&d, WeaponClass::Gun, TargetCategory::Surface);
    assert_eq!((gun.min_stance, gun.min_grade), (Stance::Hostile, TrackGrade::Fine));
    let sam = find(&d, WeaponClass::Sam, TargetCategory::Air);
    assert_eq!((sam.min_stance, sam.min_grade, sam.salvo), (Stance::Hostile, TrackGrade::Fine, 1));
    for w in [WeaponClass::PointDefenceGun, WeaponClass::PointDefenceMissile] {
        assert_eq!(find(&d, w, TargetCategory::InboundWeapon).min_grade, TrackGrade::Lock);
    }
    assert!(d.iter().all(|e| e.min_stance >= Stance::Suspect && e.effective_range_m.is_none() && e.weapon != WeaponClass::Decoy));
}

#[test]
fn preset_deltas() {
    let c = default_entries(DoctrinePreset::Cautious);
    let a = default_entries(DoctrinePreset::Aggressive);
    let s = default_entries(DoctrinePreset::SilentRunning);
    assert_eq!(c.len(), a.len());
    assert_eq!(c.len(), s.len());
    for ((c, a), s) in c.iter().zip(&a).zip(&s) {
        if is_point_defence(c.weapon) { assert_eq!((c, c), (a, s)); continue; }
        assert!(a.min_grade <= c.min_grade && a.min_id <= c.min_id && a.salvo >= c.salvo);
        assert_eq!(s.salvo, 0);
    }
    let ashm = find(&a, WeaponClass::AntiShipMissile, TargetCategory::Surface);
    assert_eq!((ashm.min_id, ashm.min_grade, ashm.salvo), (IdRung::Domain, TrackGrade::Bearing, 3));
    assert_eq!(find(&a, WeaponClass::Gun, TargetCategory::Surface).min_grade, TrackGrade::Coarse);
}

#[test]
fn resolve_inherits_fleet_ship_mount() {
    let t = TuningTable::default();
    let mounts = [mount(2, WeaponClass::Gun, 7_500.0), mount(3, WeaponClass::Gun, 7_500.0), mount(4, WeaponClass::Decoy, 0.0)];
    let base = find(&default_entries(DoctrinePreset::Cautious), WeaponClass::Gun, TargetCategory::Surface);
    let fleet = [DoctrineEntry { min_grade: TrackGrade::Coarse, ..base }];
    let mut settings = OfficerSettings::default();
    let table = resolve(&fleet, &settings, &mounts, &t);
    let e = table.get(PartSlot(2), TargetCategory::Surface).unwrap();
    assert_eq!((e.level, e.entry.min_grade, e.effective_range_m), (0, TrackGrade::Coarse, 7_500.0));
    assert_eq!(table.get(PartSlot(2), TargetCategory::Air).unwrap().level, 1);
    assert!(table.entries.keys().all(|(s, _)| *s != PartSlot(4)), "decoys get no entries");
    assert_eq!(table.self_defence_range_m, 4_000.0);

    settings.doctrine.push(DoctrineEntry { min_grade: TrackGrade::Lock, ..base });
    settings.mount_doctrine.insert(PartSlot(3), vec![DoctrineEntry { weapon: WeaponClass::Sam, effective_range_m: Some(5_000.0), salvo: 200, ..base }]);
    settings.self_defence_range_m = Some(f64::NAN);
    let table = resolve(&fleet, &settings, &mounts, &t);
    let e2 = table.get(PartSlot(2), TargetCategory::Surface).unwrap();
    assert_eq!((e2.level, e2.entry.min_grade), (1, TrackGrade::Lock));
    let e3 = table.get(PartSlot(3), TargetCategory::Surface).unwrap();
    assert_eq!((e3.level, e3.effective_range_m, e3.entry.weapon, e3.entry.salvo), (2, 5_000.0, WeaponClass::Gun, MAX_SALVO));
    assert_eq!(table.self_defence_range_m, 4_000.0);

    // Bad override ranges fall back to the mount's solver range.
    settings.mount_doctrine.insert(PartSlot(3), vec![DoctrineEntry { effective_range_m: Some(f64::INFINITY), ..base }]);
    assert_eq!(resolve(&[], &settings, &mounts, &t).get(PartSlot(3), TargetCategory::Surface).unwrap().effective_range_m, 7_500.0);
}

#[test]
fn silent_ship_with_cautious_mount_preset() {
    let t = TuningTable::default();
    let mounts = [mount(2, WeaponClass::Gun, 8_000.0), mount(3, WeaponClass::Gun, 8_000.0), mount(5, WeaponClass::PointDefenceGun, f64::NAN)];
    let mut settings = OfficerSettings { preset: DoctrinePreset::SilentRunning, ..Default::default() };
    settings.mount_presets.insert(PartSlot(3), DoctrinePreset::Cautious);
    // A fleet entry must not leak through the ship's hold.
    let fleet = [find(&default_entries(DoctrinePreset::Aggressive), WeaponClass::Gun, TargetCategory::Surface)];
    let table = resolve(&fleet, &settings, &mounts, &t);
    let hostile = track(1, Stance::Hostile, IdRung::Type, TrackGrade::Fine, Domain::Surface);
    // Fleet beats the ship preset's defaults, so slot 2 follows the fleet card; slot 3 is Cautious.
    assert_eq!(table.get(PartSlot(2), TargetCategory::Surface).unwrap().level, 0);
    assert_eq!(check(table.get(PartSlot(3), TargetCategory::Surface).unwrap(), &hostile, 5_000.0, 4_000.0), Ok(FireReason::Doctrine));
    let no_fleet = resolve(&[], &settings, &mounts, &t);
    assert_eq!(check(no_fleet.get(PartSlot(2), TargetCategory::Surface).unwrap(), &hostile, 5_000.0, 4_000.0), Err(HoldReason::PermissionWithheld));
    let pd = no_fleet.get(PartSlot(5), TargetCategory::InboundWeapon).unwrap();
    assert_eq!(pd.effective_range_m, 2_000.0, "PD gun with no solver range uses the PD cap");
    let missile = track(9, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, Domain::Weapon);
    assert_eq!(check(pd, &missile, 1_500.0, 4_000.0), Ok(FireReason::PointDefence));
}

#[test]
fn check_order_and_reasons() {
    let t = TuningTable::default();
    let table = resolve(&[], &OfficerSettings::default(), &[mount(2, WeaponClass::Gun, 8_000.0), mount(3, WeaponClass::AntiShipMissile, 24_000.0), mount(4, WeaponClass::Gun, 3_000.0)], &t);
    let gun = table.get(PartSlot(2), TargetCategory::Surface).unwrap();
    let ashm = table.get(PartSlot(3), TargetCategory::Surface).unwrap();
    let sd = table.self_defence_range_m;
    // Stance first, even when everything else fails too.
    let neutral = track(1, Stance::Neutral, IdRung::Unknown, TrackGrade::Bearing, Domain::Surface);
    assert_eq!(check(gun, &neutral, 99_000.0, sd), Err(HoldReason::Stance { have: Stance::Neutral, need: Stance::Hostile }));
    // ID before grade before range.
    let h = |rung, grade| track(2, Stance::Hostile, rung, grade, Domain::Surface);
    assert_eq!(check(ashm, &h(IdRung::Domain, TrackGrade::Bearing), 90_000.0, sd), Err(HoldReason::Id { have: IdRung::Domain, need: IdRung::Type }));
    assert_eq!(check(ashm, &h(IdRung::Type, TrackGrade::Bearing), 90_000.0, sd), Err(HoldReason::Grade { have: TrackGrade::Bearing, need: TrackGrade::Coarse }));
    assert_eq!(check(ashm, &h(IdRung::Type, TrackGrade::Coarse), 90_000.0, sd), Err(HoldReason::OutOfRange { range_m: 90_000.0, limit_m: 24_000.0 }));
    assert_eq!(check(ashm, &h(IdRung::Type, TrackGrade::Coarse), 20_000.0, sd), Ok(FireReason::Doctrine));
    assert_eq!(check(gun, &h(IdRung::Unknown, TrackGrade::Fine), f64::NAN, sd), Err(HoldReason::OutOfRange { range_m: f64::MAX, limit_m: 8_000.0 }));
    // Suspect: guns only inside self-defence range; missiles never.
    let s = track(3, Stance::Suspect, IdRung::Unknown, TrackGrade::Fine, Domain::Surface);
    assert_eq!(check(gun, &s, 3_900.0, sd), Ok(FireReason::SelfDefence));
    assert_eq!(check(gun, &s, 6_000.0, sd), Err(HoldReason::SuspectOutsideSelfDefence { range_m: 6_000.0, limit_m: 4_000.0 }));
    assert_eq!(check(ashm, &s, 3_000.0, sd), Err(HoldReason::Stance { have: Stance::Suspect, need: Stance::Hostile }));
    // A mount with a shorter effective range uses it as its self-defence range.
    let short = table.get(PartSlot(4), TargetCategory::Surface).unwrap();
    assert_eq!(check(short, &s, 3_500.0, sd), Err(HoldReason::SuspectOutsideSelfDefence { range_m: 3_500.0, limit_m: 3_000.0 }));
    // Designation lifts Suspect, never Neutral.
    assert_eq!(check_facts(ashm, Stance::Suspect, IdRung::Type, TrackGrade::Coarse, 10_000.0, sd, true), Ok(FireReason::Designated));
    assert!(check_facts(ashm, Stance::Neutral, IdRung::Exact, TrackGrade::Lock, 10.0, sd, true).is_err());
}

#[test]
fn neutral_and_friend_never_pass() {
    let t = TuningTable::default();
    let weapons = [WeaponClass::Gun, WeaponClass::AntiShipMissile, WeaponClass::Sam, WeaponClass::PointDefenceGun, WeaponClass::PointDefenceMissile, WeaponClass::Torpedo, WeaponClass::DepthCharge];
    let mounts: Vec<MountInfo> = weapons.iter().enumerate().map(|(i, w)| mount(i as u16, *w, 10_000.0)).collect();
    let mut settings = OfficerSettings::default();
    // A hostile user entry asking for Friend stance and nothing else.
    for w in weapons { for c in [TargetCategory::Surface, TargetCategory::Air, TargetCategory::InboundWeapon, TargetCategory::Subsurface] {
        settings.doctrine.push(DoctrineEntry { weapon: w, target: c, min_stance: Stance::Friend, min_id: IdRung::Unknown, min_grade: TrackGrade::Bearing, salvo: 1, effective_range_m: None });
    } }
    for preset in [DoctrinePreset::Aggressive, DoctrinePreset::Cautious, DoctrinePreset::SilentRunning] {
        for user in [false, true] {
            let s = OfficerSettings { preset, doctrine: if user { settings.doctrine.clone() } else { Vec::new() }, ..Default::default() };
            let table = resolve(&[], &s, &mounts, &t);
            for e in table.entries.values() {
                for stance in [Stance::Friend, Stance::Neutral] {
                    for designated in [false, true] {
                        assert!(check_facts(e, stance, IdRung::Exact, TrackGrade::Lock, 1.0, 4_000.0, designated).is_err(), "{e:?} {stance:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn range_caps_by_class() {
    let cat = content::catalogue::catalogue();
    let t = TuningTable::default();
    let gun_idx = cat.part_idx(&content::ids::PartId::new("gun.76mm.single")).expect("76 mm gun");
    let mut c = cat.clone();
    let mut caps = Vec::new();
    for band in CalibreBand::ALL {
        let mut def = c.parts[gun_idx.index()].clone();
        if let PartFunction::Gun(g) = &mut def.function { g.band = band; }
        c.parts[gun_idx.index()] = def;
        let m = MountInfo { part: gun_idx, ..mount(2, WeaponClass::Gun, 0.0) };
        caps.push(range_cap_m(&m, &c, &t));
    }
    assert_eq!(caps, vec![3_000.0, 8_000.0, 12_000.0, 18_000.0]);
    let pdg = MountInfo { part: gun_idx, ..mount(2, WeaponClass::PointDefenceGun, 0.0) };
    assert_eq!(range_cap_m(&pdg, &c, &t), 2_000.0);
    let ashm = cat.munition_idx(&content::ids::MunitionId::new("missile.ashm.light")).expect("anti-ship missile");
    let m = MountInfo { munition: Some(ashm), ..mount(3, WeaponClass::AntiShipMissile, 0.0) };
    assert_eq!(range_cap_m(&m, cat, &t), 24_000.0);
    assert_eq!(effective_range_m(&m, cat, &t), 24_000.0);
    assert_eq!(range_cap_m(&MountInfo { munition: Some(ashm), ..mount(3, WeaponClass::PointDefenceMissile, 0.0) }, cat, &t), 8_000.0);
    assert_eq!(range_cap_m(&mount(3, WeaponClass::PointDefenceMissile, 0.0), cat, &t), 8_000.0);
    // Bad indices never panic.
    let bad = MountInfo { part: PartIdx(9_999), munition: Some(MunitionIdx(9_999)), ..mount(3, WeaponClass::Gun, 0.0) };
    assert_eq!(range_cap_m(&bad, cat, &t), 0.0);
    assert_eq!(effective_range_m(&bad, cat, &t), 0.0);
    assert_eq!(range_cap_m(&MountInfo { weapon: WeaponClass::Sam, ..bad }, cat, &t), 0.0);
}

/// A synthetic 76 mm-like table: time of flight and fall angle grow with range.
fn table(to_m: f64) -> Vec<RangeRow> {
    let mut rows = Vec::new();
    let mut r = 250.0;
    while r <= to_m + 1e-9 {
        let tof = r / 800.0 * (1.0 + r / 20_000.0);
        rows.push(RangeRow { range_m: r, elevation_rad: r / 1e5, time_of_flight_s: tof, impact_mps: 900.0 - r / 40.0, fall_angle_rad: (r / 1e5) * 1.5 });
        r += 250.0;
    }
    rows
}

#[test]
fn gun_range_is_the_ten_percent_point() {
    let rows = table(18_000.0);
    let pk = |r: f64, disp: f64| single_round_pk(&rows, r, disp, REFERENCE_HULL_LENGTH_M, REFERENCE_HULL_HEIGHT_M);
    // An inaccurate gun reaches 10% short of the cap: the result sits on the crossing.
    let disp = 4.0;
    let r = gun_range_from_table(&rows, disp, 0.10, 18_000.0);
    assert!(r > 250.0 && r < 18_000.0, "{r}");
    assert!(pk(r, disp) >= 0.10 && pk(r + 2.0, disp) < 0.10, "{r} {} {}", pk(r, disp), pk(r + 2.0, disp));
    // An accurate gun is clamped to the cap; a hopeless one gets 0.
    assert_eq!(gun_range_from_table(&rows, 0.3, 0.10, 3_000.0), 3_000.0);
    let r2 = gun_range_from_table(&rows, 0.3, 0.10, 1e9);
    assert!(r2 > r && r2 <= 18_000.0 && pk(r2, 0.3) >= 0.10 && (r2 == 18_000.0 || pk(r2 + 2.0, 0.3) < 0.10), "{r2}");
    assert_eq!(gun_range_from_table(&rows, 1e6, 0.10, 18_000.0), 0.0);
    assert_eq!(gun_range_from_table(&[], 1.0, 0.10, 18_000.0), 0.0);
    // Raising the criterion (the gate fallback, 15%) never lengthens the range.
    assert!(gun_range_from_table(&rows, disp, 0.15, 18_000.0) <= r);
}
