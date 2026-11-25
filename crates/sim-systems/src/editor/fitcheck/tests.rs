use super::*;
use crate::editor::NoDeadline;

fn real_deps() -> SimDeps { crate::editor::splash::tests::real_deps() }
fn starter(name: &str) -> Blueprint { content::campaign::load::stock_blueprint(&format!("starters/{name}.bp.ron")).unwrap() }
fn pool3() -> rayon::ThreadPool { rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap() }
fn brief(tags: Vec<ConstraintTag>) -> FitBrief {
    FitBrief { tags, weather: crate::editor::splash::basin_weather(SeaState::BEAUFORT_4, 3), approach_km: 40.0, threats: vec![ClassId::new("class.patrol_boat"), ClassId::new("class.frigate"), ClassId::new("class.no_such_class"), ClassId::new("class.patrol_boat")], cap_t: 1_800.0 }
}

/// Expires the current candidate after `n` checks.
struct CandidateAfter(u32);
impl Deadline for CandidateAfter {
    fn expired(&mut self) -> bool { false }
    fn candidate_expired(&mut self) -> bool { if self.0 == 0 { true } else { self.0 -= 1; false } }
}

#[test]
fn tags_score_against_their_own_numbers() {
    let cat = content::catalogue::catalogue();
    let fac = starter("fac22");
    let corvette = starter("corvette60");
    let s = |t: ConstraintTag, bp: &Blueprint| score_tag(&t, bp, 2.0, 80.0, 1_500.0, 27.0, cat);
    let r = s(ConstraintTag::DraftLimit { max_m: 2.5 }, &fac);
    assert!(r.pass && r.value == 2.0 && r.limit == 2.5);
    assert!(!s(ConstraintTag::DraftLimit { max_m: 1.9 }, &fac).pass);
    assert!(s(ConstraintTag::RcsLimit { max_m2: 80.0 }, &fac).pass, "at the limit passes");
    assert!(!s(ConstraintTag::RcsLimit { max_m2: 79.9 }, &fac).pass);
    assert!(s(ConstraintTag::Range { min_km: 1_500.0 }, &fac).pass && !s(ConstraintTag::Range { min_km: 1_500.1 }, &fac).pass);
    assert!(s(ConstraintTag::MinSpeed { kn: 26.0 }, &fac).pass && !s(ConstraintTag::MinSpeed { kn: 28.0 }, &fac).pass);
    let crane = s(ConstraintTag::Requires(PartTag::Crane), &corvette);
    assert!(crane.pass && crane.value >= 1.0 && crane.limit == 1.0, "{crane:?}");
    assert!(!s(ConstraintTag::Requires(PartTag::Crane), &fac).pass);
    let p = s(ConstraintTag::Passengers { count: 4, allowed_losses: 1 }, &fac);
    assert!(!p.pass && p.value == 0.0 && p.limit == 4.0, "crew berths are not passenger berths");
    // Bad numbers fail and never leave a NaN behind.
    let n = score_tag(&ConstraintTag::DraftLimit { max_m: 3.0 }, &fac, f64::NAN, 0.0, 0.0, 0.0, cat);
    assert!(!n.pass && n.value == 0.0);
    let n = score_tag(&ConstraintTag::MinSpeed { kn: f64::NAN }, &fac, 0.0, 0.0, 0.0, 30.0, cat);
    assert!(!n.pass && n.limit == 0.0);
}

#[test]
fn turn_time_is_read_or_extrapolated() {
    // 6 degrees per second: 180 degrees at 30 s.
    let full: Vec<f64> = (1..=2_400).map(|i| i as f64 * 0.1).collect();
    assert!((turn_time(&full) - 30.0).abs() < 1e-9);
    let part: Vec<f64> = full[..900].to_vec(); // 90 degrees in 15 s
    assert!((turn_time(&part) - 30.0).abs() < 1e-6, "{}", turn_time(&part));
    assert_eq!(turn_time(&vec![1.0; 600]), 0.0, "a hull that does not turn");
    assert_eq!(turn_time(&[]), 0.0);
}

#[test]
fn bad_forecasts_are_cleaned() {
    let mut w = crate::editor::splash::basin_weather(SeaState::BEAUFORT_4, 1);
    w.visibility_m = f64::NAN;
    w.sea_state = SeaState(40);
    w.wind_speed_mps = f64::INFINITY;
    w.cloud_cover = 7.0;
    let f = forecast(&w);
    assert_eq!(f.sea_state, SeaState(8));
    assert!(f.visibility_m == w.kind.visibility_m() && f.wind_speed_mps == 0.0 && f.cloud_cover == 1.0);
}

#[test]
fn the_screen_scores_tags_and_detection_and_the_run_agrees_on_all_but_speed() {
    pool3().install(|| {
        let bp = starter("patrol15");
        let tags = vec![ConstraintTag::DraftLimit { max_m: 10.0 }, ConstraintTag::Requires(PartTag::Crane), ConstraintTag::MinSpeed { kn: 15.0 }, ConstraintTag::Range { min_km: 100.0 }];
        let b = brief(tags.clone());
        let screen = fit_screen(&bp, &b, content::catalogue::catalogue());
        assert!(!screen.simulated && screen.seconds_simulated == 0.0);
        assert_eq!(screen.tags.iter().map(|t| t.pass).collect::<Vec<_>>(), vec![true, false, true, true]);
        assert_eq!(screen.first_failing(), Some(&ConstraintTag::Requires(PartTag::Crane)));
        assert!(!screen.passes());
        // Unknown classes are left out; repeats once.
        assert_eq!(screen.detect_km.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), vec!["class.patrol_boat", "class.frigate"]);
        for (_, ours, theirs) in &screen.detect_km { assert!(*ours > 1.0 && *theirs > 1.0 && *ours < 100.0 && *theirs < 100.0, "{:?}", screen.detect_km); }
        let fit = fit_check(&bp, &b, real_deps(), 30.0, &mut NoDeadline);
        assert!(fit.simulated && fit.seconds_simulated == 30.0, "{fit:?}");
        assert_eq!(fit.tags.len(), tags.len());
        for (a, s) in fit.tags.iter().zip(&screen.tags) {
            if !matches!(a.tag, ConstraintTag::MinSpeed { .. }) { assert_eq!(a, s); }
        }
        assert_eq!((fit.range_km, &fit.detect_km), (screen.range_km, &screen.detect_km));
        assert!((fit.top_speed_kn - screen.top_speed_kn).abs() <= 0.1 * screen.top_speed_kn, "run {} gauge {}", fit.top_speed_kn, screen.top_speed_kn);
        assert!(fit.time_to_turn_180_s > 1.0 && fit.time_to_turn_180_s < 120.0, "{fit:?}");
        // The per-candidate limit stops the run early and it keeps what it measured.
        let cut = fit_check(&bp, &b, real_deps(), 30.0, &mut CandidateAfter(3));
        assert!(cut.simulated && cut.seconds_simulated > 2.9 && cut.seconds_simulated < 4.1, "{cut:?}");
        assert!(cut.top_speed_kn > 0.0);
        // Same inputs, same answer.
        assert_eq!(fit, fit_check(&bp, &b, real_deps(), 30.0, &mut NoDeadline));
        // The seakeeping sweep ran from every direction and the starter rides out a Beaufort 4 sea.
        let sea = fit.seakeeping.as_ref().expect("the sweep ran");
        assert!(sea.complete() && sea.safe() && sea.sea_state == SeaState::BEAUFORT_4, "{sea:?}");
        assert_eq!(sea.runs.iter().map(|r| r.from_deg).collect::<Vec<_>>(), vec![0.0, 60.0, 120.0, 180.0, 240.0, 300.0]);
        assert!(sea.runs.iter().all(|r| r.peak_heel_deg > 0.5 && r.peak_heel_deg < 45.0), "{sea:?}");
        // Gone over means past a right angle or past the Float gauge's range of stability, whichever comes first.
        let range = crate::editor::gauges::float_gauge(&sim_core::vessel::build_prefab(&crate::editor::splash::canonical(&bp), content::catalogue::catalogue()), 1.0e9, 2.0, content::catalogue::catalogue()).range_of_stability_deg;
        assert!(range > MIN_RANGE_DEG && sea.limit_deg == range.min(ROLLED_OVER_DEG) && sea.range_deg == range, "{} vs range {range}", sea.limit_deg);
        // The screen and the Quartermaster's tag run carry no verdict.
        assert!(screen.seakeeping.is_none() && fit_check_tags(&bp, &b, real_deps(), 30.0, &mut NoDeadline).seakeeping.is_none());
    });
}

/// Expires after `n` checks.
struct ExpiresAfter(u32);
impl Deadline for ExpiresAfter { fn expired(&mut self) -> bool { if self.0 == 0 { true } else { self.0 -= 1; false } } }

#[test]
fn a_seakeeping_sweep_cut_short_is_never_safe() {
    pool3().install(|| {
        let weather = crate::editor::splash::basin_weather(SeaState::BEAUFORT_7, 3);
        let s = seakeeping(&starter("patrol15"), &weather, &mut real_deps, &mut ExpiresAfter(4));
        assert!(!s.complete() && !s.safe() && !s.rolls_over(), "{s:?}");
        assert!(s.runs.len() == 1 && s.runs[0].seconds > 2.0 && s.runs[0].seconds < 6.0, "{s:?}");
        assert_eq!(Seakeeping::default().worst(), None);
        // The worst run is one that rolled over, else the largest heel.
        let run = |from_deg: f64, peak_heel_deg: f64, rolled_over: bool| SeaRun { from_deg, peak_heel_deg, rolled_over, seconds: SEAKEEPING_SECONDS };
        let mut s = Seakeeping { sea_state: SeaState(7), limit_deg: ROLLED_OVER_DEG, range_deg: 70.0, seconds: SEAKEEPING_SECONDS, runs: vec![run(0.0, 40.0, false), run(60.0, 90.0, true), run(120.0, 95.0, false)] };
        assert_eq!(s.worst().map(|r| r.from_deg), Some(60.0));
        assert!(!s.complete() && s.rolls_over() && !s.safe(), "three of six directions");
        s.runs.extend([run(180.0, 20.0, false), run(240.0, 20.0, false), run(300.0, 20.0, false)]);
        assert!(s.complete() && s.rolls_over() && !s.safe());
        s.runs[1].rolled_over = false;
        assert!(s.complete() && s.safe() && s.worst().map(|r| r.from_deg) == Some(120.0));
        // A range of stability under 45 degrees can roll over in Beaufort 4 or more, not in calmer water.
        s.range_deg = 40.0;
        assert!(s.rolls_over() && !s.safe());
        s.sea_state = SeaState(2);
        assert!(!s.rolls_over() && s.safe());
    });
}

/// A design whose seakeeping sweep finds it can roll over in the forecast sea does not pass Fit Check, even
/// when every tag passes; a sweep that did not run, or ran short without a run going over, leaves the tags'
/// verdict alone.
#[test]
fn a_design_that_can_roll_over_in_the_forecast_sea_does_not_pass() {
    let run = |from_deg: f64, peak_heel_deg: f64, rolled_over: bool| SeaRun { from_deg, peak_heel_deg, rolled_over, seconds: SEAKEEPING_SECONDS };
    let tag = TagResult { tag: ConstraintTag::MinSpeed { kn: 10.0 }, pass: true, value: 30.0, limit: 10.0 };
    let safe = Seakeeping { sea_state: SeaState(8), limit_deg: ROLLED_OVER_DEG, range_deg: 70.0, seconds: SEAKEEPING_SECONDS, runs: (0..SEA_DIRECTIONS).map(|k| run(60.0 * k as f64, 40.0, false)).collect() };
    let mut f = FitResult { tags: vec![tag.clone()], simulated: true, seakeeping: Some(safe.clone()), ..FitResult::default() };
    assert!(f.passes() && !f.rolls_over());
    f.seakeeping.as_mut().unwrap().runs[2] = run(120.0, 96.0, true);
    assert!(f.rolls_over() && !f.passes(), "every tag passes but the hull goes over with the sea on the quarter");
    f.seakeeping.as_mut().unwrap().runs[2].rolled_over = false;
    f.seakeeping.as_mut().unwrap().range_deg = 40.0;
    assert!(!f.passes(), "a short range of stability in Beaufort 4 or more can roll over");
    f.seakeeping = Some(Seakeeping { runs: vec![run(0.0, 20.0, false)], ..safe });
    assert!(f.passes(), "a sweep cut short with nothing over is not finished, not a failure");
    f.seakeeping = None;
    assert!(f.passes(), "no sweep (the analytic screen, a Quartermaster candidate): the tags decide");
    f.tags[0].pass = false;
    assert!(!f.passes());
}

/// Fit Check wall time per starter (L893: 300 ms p95 per candidate).
#[test]
#[ignore]
fn fit_timing_probe() {
    pool3().install(|| {
        for name in ["patrol15", "fac22", "insertion26", "corvette40", "corvette60", "destroyer90"] {
            let bp = starter(name);
            let _ = fit_check_tags(&bp, &brief(vec![]), real_deps(), 30.0, &mut NoDeadline);
            let mut ms = Vec::new();
            for _ in 0..5 {
                let t0 = std::time::Instant::now();
                let f = fit_check_tags(&bp, &brief(vec![]), real_deps(), 30.0, &mut NoDeadline);
                ms.push(t0.elapsed().as_secs_f64() * 1e3);
                assert!(f.simulated);
            }
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            // The whole Fit Check the editor and the briefing run: the tag run, then the seakeeping sweep. The
            // 300 ms rule (L893) is the tag run's, per Quartermaster candidate; the sweep runs once per Fit
            // Check under the editor's own ceiling, so it is timed beside it rather than against it.
            let mut full = Vec::new();
            for _ in 0..3 {
                let t0 = std::time::Instant::now();
                let f = fit_check(&bp, &brief(vec![]), real_deps(), 30.0, &mut NoDeadline);
                full.push(t0.elapsed().as_secs_f64() * 1e3);
                assert!(f.simulated && f.seakeeping.as_ref().is_some_and(|s| s.complete()), "{f:?}");
            }
            full.sort_by(|a, b| a.partial_cmp(b).unwrap());
            eprintln!("{name}: tag run {:.0} ms median, {:.0} ms max; full fit check with the seakeeping sweep {:.0} ms median, {:.0} ms max", ms[2], ms[4], full[1], full[2]);
        }
    });
}

#[test]
fn a_hull_lost_in_its_run_is_flagged_and_never_offered_as_a_refit() {
    pool3().install(|| {
        // The tutorial's sinking hull founders about 10 s into the run.
        let bp = content::campaign::load::stock_blueprint("tutorial/fac22_tutorial.bp.ron").unwrap();
        let (f, lost) = fit_check_run(&bp, &brief(vec![]), real_deps(), 30.0, &mut NoDeadline);
        assert!(lost && f.simulated && f.seconds_simulated < 30.0, "{f:?}");
        let (_, ok) = fit_check_run(&starter("patrol15"), &brief(vec![]), real_deps(), 4.0, &mut NoDeadline);
        assert!(!ok);
        let tag = ConstraintTag::Requires(PartTag::Crane);
        // Whatever the Quartermaster offers for it stays afloat through its own check.
        match crate::editor::quartermaster::quartermaster(&bp, &brief(vec![tag.clone()]), &BTreeMap::new(), None, &crate::editor::splash::basin_ruleset(), &mut real_deps, &mut NoDeadline) {
            crate::editor::quartermaster::QmResult::Refit { blueprint, .. } => assert!(!fit_check_run(&blueprint, &brief(vec![tag]), real_deps(), 30.0, &mut NoDeadline).1),
            crate::editor::quartermaster::QmResult::NoLegalRefit { blocking } => assert_eq!(blocking, tag),
            other => panic!("{other:?}"),
        }
    });
}

/// Seakeeping sweep wall time per starter in a Beaufort 7 sea: `cargo test --release -p sim-systems --lib
/// seakeeping_timing_probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn seakeeping_timing_probe() {
    pool3().install(|| {
        for name in ["patrol15", "fac22", "insertion26", "corvette40", "corvette60", "destroyer90"] {
            let bp = starter(name);
            let weather = crate::editor::splash::basin_weather(SeaState::BEAUFORT_7, 3);
            let t0 = std::time::Instant::now();
            let s = seakeeping(&bp, &weather, &mut real_deps, &mut NoDeadline);
            eprintln!("{name}: seakeeping {:.0} ms, {:?}", t0.elapsed().as_secs_f64() * 1e3, s.runs.iter().map(|r| (r.from_deg, (r.peak_heel_deg * 10.0).round() / 10.0, r.rolled_over)).collect::<Vec<_>>());
        }
    });
}
