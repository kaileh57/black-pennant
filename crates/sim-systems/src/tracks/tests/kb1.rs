//! Regression tests and measurements from verifier kb1's review of the b1 tracker changes: a hull's track is
//! never taken by the initiation pass, an intermittent hull keeps one track, a wedge keeps its number and
//! stance when radar converts it, and the by-position label score the hull rule in `update_track` was chosen
//! on. The measurements are `#[ignore]`d and print; the rest assert.
use super::*;

#[derive(Debug, Default)]
struct Stats { samples: u32, not_two: u32, swaps: u32, created: u32, air_like_hull_samples: u32, max_live: usize }

/// Runs `dets(tick)` through `fuse_core` every 6 ticks; scores from `score_from` on. Hull truths are `hulls`.
fn scene(ticks: u64, score_from: u64, hulls: &[EntityId], mut dets: impl FnMut(u64) -> Vec<Detection>) -> (SidePicture, Stats) {
    let tun = tun();
    let mut pic = picture();
    let mut st = Stats::default();
    let mut owner: BTreeMap<TrackId, EntityId> = BTreeMap::new();
    let mut batch = Vec::new();
    for tick in 1..=ticks {
        batch.extend(dets(tick));
        if tick % 6 != 0 { continue; }
        let mut ch = Vec::new();
        fuse_core(&mut pic, &batch, &[], tick, &tun, &mut ch);
        batch.clear();
        st.created += ch.iter().filter(|c| matches!(c, TrackChange::Created(_))).count() as u32;
        let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
        st.max_live = st.max_live.max(live.len());
        if tick < score_from { continue; }
        st.samples += 1;
        let ok = live.len() == hulls.len() && hulls.iter().all(|h| live.iter().filter(|t| t.truth == *h).count() == 1);
        if !ok { st.not_two += 1; }
        if live.iter().any(|t| air_like(t.domain) && hulls.contains(&t.truth)) { st.air_like_hull_samples += 1; }
        for t in &live { if let Some(o) = owner.insert(t.id, t.truth) { if o != t.truth { st.swaps += 1; } } }
    }
    (pic, st)
}

#[test]
fn kb1_esm_then_radar_keeps_number_and_stance() {
    let tun = tun();
    let mut rng = Rng(5);
    let mut pic = picture();
    let from = DVec3::new(0.0, 20.0, 0.0);
    let truth = |tick: u64| DVec3::new(3_000.0, 0.0, -15_000.0) + DVec3::new(0.0, 0.0, 6.0) * (tick as f64 / 60.0);
    let mut first: Option<TrackId> = None;
    let mut bad = Vec::new();
    for tick in (6..=(5 * 3_600u64)).step_by(6) {
        let mut d = Vec::new();
        if tick % 30 == 0 { d.push(det(&mut rng, tick, sref(2), SensorKind::Esm, from, eid(9), truth(tick))); }
        if tick >= 60 * 60 && tick % 150 == 0 { d.push(det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(9), truth(tick))); }
        let mut ch = Vec::new();
        fuse_core(&mut pic, &d, &[], tick, &tun, &mut ch);
        if first.is_none() {
            if let Some(t) = pic.tracks.iter_mut().find(|t| t.is_live()) {
                // The player declares it Hostile while it is still a wedge.
                t.stance = Stance::Hostile;
                t.stance_override = Some(Stance::Hostile);
                first = Some(t.id);
            }
        }
        if first.is_none() { continue; }
        let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
        if live.len() != 1 || Some(live[0].id) != first || live[0].stance != Stance::Hostile || live[0].stance_override != Some(Stance::Hostile) || live[0].truth != eid(9) {
            bad.push((tick, live.iter().map(|t| (t.id, t.stance, t.bearing_only.is_some(), t.domain)).collect::<Vec<_>>()));
        }
    }
    let t = pic.tracks.iter().find(|t| t.is_live()).expect("live");
    eprintln!("esm->radar: first {first:?}, end {:?} bearing_only {} domain {:?} bad samples {} first bad {:?}", t.id, t.bearing_only.is_some(), t.domain, bad.len(), bad.first());
    assert!(bad.is_empty(), "{} bad samples, first {:?}", bad.len(), bad.first());
    assert!(t.bearing_only.is_none(), "the radar fixes converted the wedge");
}

#[test]
fn kb1_silent_hull_redetected_2km_away_after_5_minutes() {
    let tun = tun();
    let mut rng = Rng(7);
    let mut pic = picture();
    let from = DVec3::new(0.0, 20.0, 0.0);
    let a = DVec3::new(0.0, 0.0, -9_000.0);
    let b = a + DVec3::new(2_000.0, 0.0, 0.0);
    let mut ch = Vec::new();
    for tick in (150..=(120 * 60u64)).step_by(150) { fuse_core(&mut pic, &[det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(9), a)], &[], tick, &tun, &mut ch); }
    let old = pic.tracks.iter().find(|t| t.is_live()).map(|t| t.id).expect("tracked");
    pic.tracks.iter_mut().for_each(|t| { t.stance = Stance::Hostile; t.stance_override = Some(Stance::Hostile); });
    let back = 120 * 60 + 300 * 60;
    for tick in ((120 * 60 + 6)..back).step_by(6) { fuse_core(&mut pic, &[], &[], tick, &tun, &mut ch); }
    let mark = pic.get(old).cloned();
    ch.clear();
    for tick in (back..=(back + 60 * 60)).step_by(6) {
        let d = if tick % 150 == 0 { vec![det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(9), b)] } else { vec![] };
        fuse_core(&mut pic, &d, &[], tick, &tun, &mut ch);
    }
    let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
    eprintln!("silent: old {old:?} mark live={:?} after 5 min; at end live {:?}; mark kept {:?}; changes {:?}",
        mark.as_ref().map(|t| t.is_live()), live.iter().map(|t| (t.id, t.domain, t.stance, t.stance_override, t.truth)).collect::<Vec<_>>(),
        pic.get(old).map(|t| (t.is_live(), t.stance_override)), ch.iter().filter(|c| !matches!(c, TrackChange::Grade(..))).collect::<Vec<_>>());
    assert_eq!(live.len(), 1, "one live track of the hull");
    assert_eq!(live[0].domain, Domain::Surface);
    assert!(pic.get(old).is_some_and(|t| !t.is_live()), "the last-known mark is kept (under 10 minutes)");
}

/// Hull A is first seen at one look and missed at the next, when hull B, `sep` metres away, is first seen.
/// From the look after, both are seen at every look. `dt_ticks` is the gap between the two first looks;
/// `second_sensor` makes B's first fix another ship's radar (a datalinked consort) on its own stagger.
fn hijack(sep: f64, dt_ticks: u64, second_sensor: bool) -> (SidePicture, Stats, Vec<(TrackId, Domain, EntityId, f64)>) {
    let mut rng = Rng(3);
    let from = DVec3::new(0.0, 20.0, 0.0);
    let consort = DVec3::new(1_500.0, 20.0, 500.0);
    let ha = DVec3::new(0.0, 0.0, -8_000.0);
    let hb = ha + DVec3::new(sep, 0.0, 0.0);
    let t0 = 150;
    let t1 = t0 + dt_ticks;
    let mut snap = Vec::new();
    let tun = tun();
    let mut pic = picture();
    let mut st = Stats::default();
    let mut owner: BTreeMap<TrackId, EntityId> = BTreeMap::new();
    for tick in (6..=(60 * 60u64)).step_by(6) {
        let mut d = Vec::new();
        if tick == t0 { d.push(det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(1), ha)); }
        if tick == t1 {
            let (s, f) = if second_sensor { (sref(2), consort) } else { (sref(1), from) };
            d.push(det(&mut rng, tick, s, SensorKind::SearchRadar, f, eid(2), hb));
        }
        if tick > t1 && (tick - t0) % 150 == 0 {
            d.push(det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(1), ha));
            d.push(det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(2), hb));
        }
        let mut ch = Vec::new();
        fuse_core(&mut pic, &d, &[], tick, &tun, &mut ch);
        st.created += ch.iter().filter(|c| matches!(c, TrackChange::Created(_))).count() as u32;
        let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
        st.max_live = st.max_live.max(live.len());
        if tick == t1 + 6 { snap = live.iter().map(|t| (t.id, t.domain, t.truth, t.vel.length())).collect(); }
        if tick > t1 {
            st.samples += 1;
            if live.iter().any(|t| air_like(t.domain)) { st.air_like_hull_samples += 1; }
            if live.len() != 2 { st.not_two += 1; }
            for t in &live { if let Some(o) = owner.insert(t.id, t.truth) { if o != t.truth { st.swaps += 1; } } }
        }
    }
    (pic, st, snap)
}

#[test]
fn kb1_two_point_does_not_take_a_hull_track() {
    // b1's two-point pass moved hull A's TrackId onto hull B's first fix and made it an Air or Weapon track for
    // good, in all six geometries. Required: both hulls stay Surface with one track each, as the base code
    // (7878656) did. At 300 m B's first fix lies inside the gate of A's one-fix track and joins it by ordinary
    // association (L465), then as at base: the next look splits the two, 19 to 24 samples later.
    for (sep, dt, two) in [(600.0, 150, false), (300.0, 150, false), (1_500.0, 150, false), (600.0, 60, true), (300.0, 30, true), (2_000.0, 120, true)] {
        let (pic, st, snap) = hijack(sep, dt, two);
        let what = format!("sep {sep} dt {dt} second sensor {two}: just after B's first fix {snap:?}; {st:?}");
        assert_eq!(st.air_like_hull_samples, 0, "a hull held an air or weapon track: {what}");
        let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
        assert!(live.len() == 2 && live.iter().all(|t| t.domain == Domain::Surface) && live[0].truth != live[1].truth, "{what}");
        assert_eq!(st.created, 2, "two track numbers for two hulls: {what}");
        if sep > 300.0 {
            assert!(snap.len() == 2 && snap.iter().any(|s| s.0 == TrackId(1) && s.2 == eid(1)), "A's track was taken: {what}");
            assert_eq!((st.not_two, st.swaps), (0, 0), "{what}");
        } else {
            assert!(st.not_two <= 24 && st.swaps <= 1, "worse than the base code: {what}");
        }
    }
}

/// A slow air object (drone or helicopter) at `speed` and 150 m, seen only by a search radar (no height) 6 km
/// off, crossing. Reports the domains and the number of tracks started.
fn slow_air(speed: f64) -> (Stats, Vec<(TrackId, Domain, f64)>) {
    let mut rng = Rng(19);
    let from = DVec3::new(0.0, 20.0, 0.0);
    let at = move |tick: u64| DVec3::new(-3_000.0 + speed * (tick as f64 / 60.0), 150.0, -6_000.0);
    let (pic, st) = scene(40 * 60, 600, &[eid(7)], |tick| if tick % 150 == 0 { vec![det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(7), at(tick))] } else { vec![] });
    (st, pic.tracks.iter().filter(|t| t.is_live()).map(|t| (t.id, t.domain, t.vel.length())).collect())
}

#[test]
#[ignore = "a measurement: slow air objects seen by search radar alone become several Surface tracks (open)"]
fn kb1_slow_air_object_by_search_radar_only() {
    for speed in [40.0, 60.0, 100.0, 150.0] {
        let (st, live) = slow_air(speed);
        eprintln!("slow air {speed} m/s: live at end {live:?}; {st:?}");
    }
}

#[test]
fn kb1_own_air_unit_mark_and_own_weapon() {
    // An own drone (Air) leaving the link keeps its mark; only Weapon goes. A hostile weapon's track is unchanged.
    let tun = tun();
    let mut pic = picture();
    let mut ch = Vec::new();
    fuse_core(&mut pic, &[], &[(eid(40), Domain::Air, DVec3::new(0.0, 300.0, -2_000.0), DVec3::ZERO), (eid(41), Domain::Weapon, DVec3::new(0.0, 5.0, -1_000.0), DVec3::ZERO)], 6, &tun, &mut ch);
    fuse_core(&mut pic, &[], &[], 12, &tun, &mut ch);
    eprintln!("own units: {:?}", pic.tracks.iter().map(|t| (t.id, t.domain, t.is_live())).collect::<Vec<_>>());
    assert_eq!(pic.tracks.len(), 1);
}

/// A stationary hull seen by one search-radar return every `gap_s`, or in bursts (3 looks every 30 s) whose
/// first burst saw it only on its last look. Reports numbers used, drops and seconds with no live track.
fn sparse(gap_s: u64, burst: bool) -> (u32, u32, u64) {
    let tun = tun();
    let mut rng = Rng(23);
    let mut pic = picture();
    let from = DVec3::new(0.0, 20.0, 0.0);
    let at = DVec3::new(2_000.0, 0.0, -12_000.0);
    let (mut created, mut dropped, mut dark) = (0u32, 0u32, 0u64);
    let mut seen = false;
    for tick in (6..=(5 * 3_600u64)).step_by(6) {
        let look = if burst {
            let s = tick % 1_800;
            let in_burst = s == 0 || s == 150 || s == 300;
            in_burst && !(tick < 1_800 && s != 300)
        } else { tick % (gap_s * 60) == 0 };
        let d = if look { vec![det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(9), at)] } else { vec![] };
        let mut ch = Vec::new();
        fuse_core(&mut pic, &d, &[], tick, &tun, &mut ch);
        created += ch.iter().filter(|c| matches!(c, TrackChange::Created(_))).count() as u32;
        dropped += ch.iter().filter(|c| matches!(c, TrackChange::Dropped(_))).count() as u32;
        seen |= look;
        if seen && !pic.tracks.iter().any(|t| t.is_live()) { dark += 6; }
    }
    (created, dropped, dark / 60)
}

#[test]
fn kb1_sparse_search_radar_hull() {
    // L465: a surface track coasts 120 s. A hull that comes back as single returns every 25, 40 or 60 s, or
    // whose first Burst saw it on one look only, is one track with no drop and no gap (b1's 20 s coast for a
    // one-return track gave 12, 7 and 5 numbers and up to 159 s of 300 with no track).
    for gap in [25u64, 40, 60] { assert_eq!(sparse(gap, false), (1, 0, 0), "one return every {gap} s: (created, dropped, seconds with no track)"); }
    assert_eq!(sparse(0, true), (1, 0, 0), "burst whose first burst saw the hull once");
}

/// Crossing and abreast pairs scored by position: a track belongs to the hull its prediction is nearest; a
/// change of that hull is a swap. Also the label (`truth`) disagreeing with the position.
fn by_position(cross: bool, d: f64, seed: u64) -> (u32, u32, u32, u32) {
    let tun = tun();
    let mut rng = Rng(seed);
    let mut pic = picture();
    let from = DVec3::new(0.0, 20.0, 0.0);
    let at = move |k: u32, tick: u64| {
        let t = tick as f64 / 60.0;
        if cross { let t = t - 150.0; if k == 0 { DVec3::new(6.0 * t, 0.0, -7_000.0) } else { DVec3::new(-6.0 * t, 0.0, -7_000.0 - d) } }
        else { DVec3::new(-0.5 * d + d * k as f64, 0.0, -7_000.0) + DVec3::new(0.0, 0.0, 6.0) * t }
    };
    let mut owner: BTreeMap<TrackId, u32> = BTreeMap::new();
    let (mut updates, mut swaps, mut mislabel, mut samples) = (0u32, 0u32, 0u32, 0u32);
    let mut dets = Vec::new();
    for tick in 1..=(5 * 3_600u64) {
        if tick % 150 == 0 { for k in 0..2 { dets.push(det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(10 + k), at(k, tick))); } }
        if tick % 30 == 0 { let k = (tick / 30 % 2) as u32; dets.push(det(&mut rng, tick, sref(2), SensorKind::Visual, from, eid(10 + k), at(k, tick))); }
        if tick % 6 != 0 { continue; }
        let n = dets.len() as u32;
        let mut ch = Vec::new();
        fuse_core(&mut pic, &dets, &[], tick, &tun, &mut ch);
        dets.clear();
        if tick < 600 { continue; }
        updates += n;
        for t in pic.tracks.iter().filter(|t| t.is_live()) {
            let p = t.predict(tick);
            let dist = |k: u32| { let q = at(k, tick); libm::hypot(p.x - q.x, p.z - q.z) };
            let near = if dist(0) <= dist(1) { 0 } else { 1 };
            samples += 1;
            if t.truth != eid(10 + near) { mislabel += 1; }
            if let Some(o) = owner.insert(t.id, near) { if o != near { swaps += 1; } }
        }
    }
    (updates, swaps, mislabel, samples)
}

#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn kb1_swaps_by_position() {
    for (cross, d) in [(true, 150.0), (true, 50.0), (false, 350.0), (false, 150.0)] {
        for seed in [12u64, 13, 14] {
            let (u, s, m, n) = by_position(cross, d, seed);
            eprintln!("by position cross {cross} d {d} seed {seed}: updates {u} position swaps {s} ({:.2}%) label wrong {m} of {n} track-samples", 100.0 * s as f64 / u as f64);
        }
    }
}


#[test]
fn kb1_labels_follow_position() {
    // The hull rule in `update_track`, scored by position over the four two-hull geometries and three seeds
    // (5 minutes each): track samples whose label is not the hull the track is nearest. b1's rule (a fix much
    // coarser than the track never renames it) gave 3,229, the base rule (the latest fix renames) 2,639, the
    // sure-fix rule 2,467. Most of each is the 150 m abreast case, where the two tracks sit inside each
    // other's noise. 350 m abreast must be clean.
    let mut wrong = 0;
    for (cross, d) in [(true, 150.0), (true, 50.0), (false, 350.0), (false, 150.0)] {
        for seed in [12u64, 13, 14] {
            let (_, swaps, mislabel, _) = by_position(cross, d, seed);
            wrong += mislabel;
            if !cross && d == 350.0 { assert!(swaps <= 1 && mislabel <= 5, "350 m abreast seed {seed}: {swaps} swaps by position, {mislabel} labels wrong"); }
        }
    }
    assert!(wrong <= 2_900, "{wrong} track samples labelled with the hull they are not nearest");
}
