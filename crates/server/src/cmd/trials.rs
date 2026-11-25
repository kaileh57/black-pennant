//! Unit `server-trials`. The trial-battery and Fit Check worker. DESIGN.md L227-229 (trial battery: turning
//! circle, acceleration, heel recovery, 1-vs-3 fight; 3 seeds on save, 20 in CI; CI fails when a gauge
//! prediction drifts more than 10% from simulation), L1300 (Mac-versus-Windows tolerances, compared by
//! `editor::trials::cross_platform_check` from two reports), L973-976 (complexity points), L801-807 (Fit Check:
//! 30 s headless in the briefing's forecast sea state).
//!
//! `trials` loads a blueprint file, runs `editor::trials::run_battery` with the chosen `BatterySpec` and
//! `NoDeadline` inside the sim pool, writes the `Scorecard` as RON, and exits 1 when any `Scorecard::drift`
//! entry exceeds 10%. `fit-check` loads a blueprint and a RON `FitBrief`, runs `editor::fitcheck::fit_check`,
//! writes the `FitResult`, and exits 1 when a tag fails or the seakeeping sweep is not safe (the hull can roll
//! over in the forecast sea, or the sweep did not finish).
//!
//! # Choices this unit makes where the spec is silent
//! - Drift is measured against the simulated value, the truth the gauge predicts: `|predicted - simulated| /
//!   simulated`. A simulated value of 0 (a hull that cannot turn) with any positive prediction fails; both 0
//!   is no entry at all (`run_battery` leaves it out). A non-finite entry fails.
//! - Exactly 10% passes ("more than 10%" fails).
//! - A scorecard with no drift entry means no manoeuvring trial ran (a design that does not validate gets an
//!   empty scorecard): the report is still written, and the command exits 1 with the reason on stderr, so a
//!   broken stock design can never pass CI by measuring nothing.
//! - The report is written before the verdict, so CI keeps the evidence of a failing run.
use crate::args::{BatteryKind, FitCheckArgs, TrialsArgs};
use crate::error::{CliError, Code};
use sim_systems::editor::trials::{BatterySpec, Scorecard};

/// Largest relative drift that passes (L229).
pub const MAX_DRIFT: f64 = 0.10;

/// Unit: `server-trials`.
pub fn run_trials(args: &TrialsArgs) -> Result<Code, CliError> {
    let data = crate::runtime::load_data()?;
    let pool = crate::runtime::sim_pool(3)?;
    let bp = crate::runtime::read_blueprint(&args.blueprint)?;
    let spec = spec_for(args.spec);
    let started = std::time::Instant::now();
    let card = pool.install(|| {
        let mut make_deps = || crate::runtime::sim_deps(&data);
        sim_systems::editor::trials::run_battery(&bp, &spec, &data.campaign, &mut make_deps, &mut sim_systems::editor::NoDeadline)
    });
    crate::runtime::write_report(&card, args.out.as_deref())?;
    eprintln!("trials {} spec {:?}: {:.1} s", args.blueprint.display(), args.spec, started.elapsed().as_secs_f64());
    Ok(verdict(&card))
}

/// Exit code of a finished battery, with the reasons on stderr.
fn verdict(card: &Scorecard) -> Code {
    if card.drift.is_empty() {
        eprintln!("trials: no gauge was measured (the design did not validate or no manoeuvring trial ran)");
        return crate::error::FAIL;
    }
    let failures = drift_failures(card);
    for f in &failures { eprintln!("drift {f}"); }
    if failures.is_empty() { crate::error::PASS } else { crate::error::FAIL }
}

/// Unit: `server-trials`.
pub fn run_fit_check(args: &FitCheckArgs) -> Result<Code, CliError> {
    let data = crate::runtime::load_data()?;
    let pool = crate::runtime::sim_pool(3)?;
    let bp = crate::runtime::read_blueprint(&args.blueprint)?;
    let brief: sim_systems::editor::fitcheck::FitBrief = crate::runtime::read_ron(&args.brief)?;
    use sim_systems::editor::fitcheck as fc;
    // The tag run and the seakeeping sweep are timed apart: the 300 ms rule (L893) is the tag run's.
    let started = std::time::Instant::now();
    let mut result = pool.install(|| fc::fit_check_tags(&bp, &brief, crate::runtime::sim_deps(&data), args.seconds, &mut sim_systems::editor::NoDeadline));
    let tags_ms = started.elapsed().as_secs_f64() * 1000.0;
    let swept = std::time::Instant::now();
    if result.simulated { result.seakeeping = Some(pool.install(|| fc::seakeeping(&bp, &brief.weather, &mut || crate::runtime::sim_deps(&data), &mut sim_systems::editor::NoDeadline))); }
    crate::runtime::write_report(&result, args.out.as_deref())?;
    eprintln!("fit-check {}: tag run {tags_ms:.0} ms, {:.1} s simulated; seakeeping sweep {:.0} ms", args.blueprint.display(), result.seconds_simulated, swept.elapsed().as_secs_f64() * 1000.0);
    for t in result.tags.iter().filter(|t| !t.pass) { eprintln!("tag failed {:?} value {} limit {}", t.tag, t.value, t.limit); }
    // A hull that can roll over in the forecast sea fails as a tag does.
    let sea_ok = result.seakeeping.as_ref().is_none_or(|s| s.safe());
    if let Some(s) = result.seakeeping.as_ref().filter(|s| !s.safe()) {
        let w = s.worst().copied().unwrap_or_default();
        eprintln!("seakeeping in sea state {}: {} (worst heel {:.1} deg with the sea {:.0} deg off the bow)", s.sea_state.0, if s.rolls_over() { "rolls over" } else { "incomplete" }, w.peak_heel_deg, w.from_deg);
    }
    Ok(if result.passes() && sea_ok { crate::error::PASS } else { crate::error::FAIL })
}
/// Spec for a kind. Real code.
pub fn spec_for(kind: BatteryKind) -> BatterySpec { match kind { BatteryKind::OnSave => BatterySpec::ON_SAVE, BatteryKind::Ci => BatterySpec::CI, BatteryKind::Short => BatterySpec::SHORT } }

/// Relative drift of one entry against the simulated value; `None` when it cannot be measured (non-finite).
fn relative_drift(predicted: f64, simulated: f64) -> Option<f64> {
    if !(predicted.is_finite() && simulated.is_finite()) { return None; }
    let diff = (predicted - simulated).abs();
    if simulated.abs() > 0.0 { Some(diff / simulated.abs()) } else if diff == 0.0 { Some(0.0) } else { Some(f64::INFINITY) }
}

/// Drift entries over 10%, as `gauge predicted simulated` lines for the CI log. Unit: `server-trials`.
pub fn drift_failures(card: &Scorecard) -> Vec<String> {
    card.drift.iter().filter_map(|d| {
        // Exactly 10% passes; the small slack keeps 11 against 10 from failing on rounding.
        let over = relative_drift(d.predicted, d.simulated).is_none_or(|r| r > MAX_DRIFT + 1e-12);
        over.then(|| format!("{} {} {}", d.gauge, d.predicted, d.simulated))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_systems::editor::trials::Drift;

    fn card(rows: &[(f64, f64)]) -> Scorecard {
        Scorecard { drift: rows.iter().enumerate().map(|(i, &(p, s))| Drift { gauge: format!("g{i}"), predicted: p, simulated: s }).collect(), ..Default::default() }
    }

    #[test]
    fn drift_boundaries() {
        // Exactly 10% passes, just over fails, in either direction.
        assert!(drift_failures(&card(&[(11.0, 10.0), (9.0, 10.0), (20.0, 20.0)])).is_empty());
        assert_eq!(drift_failures(&card(&[(11.01, 10.0)])), vec!["g0 11.01 10".to_string()]);
        assert_eq!(drift_failures(&card(&[(8.99, 10.0)])).len(), 1);
        // A hull that cannot turn against a predicted circle fails; both zero passes.
        assert_eq!(drift_failures(&card(&[(300.0, 0.0)])).len(), 1);
        assert!(drift_failures(&card(&[(0.0, 0.0)])).is_empty());
        // A gauge predicting nothing for a hull that moves is 100% off.
        assert_eq!(drift_failures(&card(&[(0.0, 25.0)])).len(), 1);
        assert_eq!(drift_failures(&card(&[(f64::NAN, 25.0), (25.0, f64::INFINITY)])).len(), 2);
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict(&card(&[(30.0, 29.0), (400.0, 380.0)])), crate::error::PASS);
        assert_eq!(verdict(&card(&[(30.0, 20.0)])), crate::error::FAIL);
        assert_eq!(verdict(&Scorecard::default()), crate::error::FAIL, "an empty scorecard measured nothing");
    }

    #[test]
    fn specs() {
        assert_eq!(spec_for(BatteryKind::OnSave).seeds, 3);
        assert_eq!(spec_for(BatteryKind::Ci).seeds, 20);
        assert!(spec_for(BatteryKind::Short).short);
    }
}
