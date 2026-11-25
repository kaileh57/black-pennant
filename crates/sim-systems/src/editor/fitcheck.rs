//! Unit `sys-fitcheck`. Fit Check: does this design meet the briefing? DESIGN.md L66-67, L801, L893.
//!
//! A 30 s headless simulation in the briefing's forecast sea state (a full-power acceleration run, then a
//! 180 degree turn) that scores range, speed and detection range against the known threats, and each
//! constraint tag pass or fail. Range and detection come from the analytic gauges. Time gate: p95 per
//! candidate at most 300 ms (nightly, 20 briefings plus the 90 m stress hull). Fallback: screen with the
//! analytic gauges (within 10% of simulation) and simulate only the 3 cheapest passing candidates; if still
//! over, simulate 15 s.
//!
//! # How the numbers are made (choices the spec leaves open)
//! - The run is the trial battery's manoeuvre with the helm in hand: the hull starts at the Speed gauge's top
//!   speed (so 30 s is enough for the 90 m hull too) and runs at full throttle for the first half, then holds
//!   the rudder hard to starboard. Speed is the mean over the last [`SPEED_MEAN_S`] of the straight leg: the
//!   speed the hull holds at full power in the forecast sea. The 180 degree turn time runs from rudder over;
//!   a turn not finished when the run ends is extrapolated from the yaw rate of its last 2 s, and a hull that
//!   does not turn reads 0 (as the Speed gauge writes a turning circle it cannot make).
//! - `MinSpeed` is scored on the simulated speed when the run measured one, else on the Speed gauge (the
//!   screen). Every other tag is analytic in both: draft from the Float gauge (calm-water equilibrium at full
//!   load), cross-section as the largest of the 36 aspects of the signature view, range from the Endurance
//!   gauge, passengers as passenger berths fitted, `Requires` as parts carrying the tag (or fitted in a kit
//!   that carries it).
//! - Detection: per threat class, the range at which the design first sees it and it first sees the design,
//!   by the best of search, navigation and fire-control radar, the lookout's eye and camera masts, in the
//!   briefing's weather by day. Targets present their mean radar cross-section and mean above-water area
//!   over the 36 aspects at their mast-top height. Unknown classes are left out.
//! - `approach_km` is not a tag: the briefing's `Range` tags carry the distances that are scored.
//! - A hull lost in its own run (capsized, sunk, wrecked) ends the run there; the Quartermaster never offers
//!   such a refit.
//! - The per-candidate limit (`Deadline::candidate_expired`) and the overall deadline are checked once per
//!   simulated second; a run cut short keeps what it measured and reports `seconds_simulated`.
//! - Seakeeping (DECISION: the spec's 30 s run sees the sea from one direction only, and whether a hull rolls
//!   over depends on the direction the sea comes from, so a design could pass and then be lost). [`fit_check`]
//!   also runs [`seakeeping`]: the same full-power run and turn with the forecast sea from each of
//!   [`SEA_DIRECTIONS`] directions, [`SEAKEEPING_SECONDS`] each, and reports the worst heel and every
//!   direction in which the hull went over: heeled past [`ROLLED_OVER_DEG`], past the angle at which its
//!   calm-water righting arm vanishes (the Float gauge's range of stability, when that is smaller), or was
//!   lost. A hull that comes back from beyond its range of stability survived on the timing of the waves, and
//!   another run with the same sea would roll it over, so one seed is not trusted to say otherwise; for the
//!   same reason a design whose range of stability is under [`MIN_RANGE_DEG`] can roll over in any sea of
//!   Beaufort 4 or more. A sweep a deadline cut short is incomplete and never reads as safe. It runs once per Fit Check, never per Quartermaster candidate, so
//!   the 300 ms candidate limit (L893) is untouched: the Quartermaster checks its candidates with the tag
//!   run alone, as before.
use super::Deadline;
use crate::prelude::*;
use crate::sim::SimDeps;
use content::blueprint::Blueprint;
use content::campaign::{CampaignData, ConstraintTag};
use content::part::{PartFunction, PartTag, SensorKind};
use content::weather::{SeaState, WeatherState};
use sim_core::vessel::{VesselPrefab, VesselStatus};

/// What Fit Check needs from a briefing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitBrief { pub tags: Vec<ConstraintTag>, pub weather: WeatherState, pub approach_km: f64, pub threats: Vec<ClassId>, pub cap_t: f64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TagResult { pub tag: ConstraintTag, pub pass: bool, pub value: f64, pub limit: f64 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FitResult {
    pub tags: Vec<TagResult>,
    pub top_speed_kn: f64,
    pub time_to_turn_180_s: f64,
    pub range_km: f64,
    /// First-detection range in km of each known threat class by the design's best sensor, and theirs of us.
    pub detect_km: Vec<(ClassId, f64, f64)>,
    /// False when only the analytic screen ran (fallback).
    pub simulated: bool,
    pub seconds_simulated: f64,
    /// The seakeeping sweep in the forecast sea; `None` when it did not run (the analytic screen, or the
    /// Quartermaster's candidate checks).
    #[serde(default)]
    pub seakeeping: Option<Seakeeping>,
}
impl FitResult {
    /// Every tag passes, and the seakeeping sweep, when it ran, did not find that the hull can roll over in the
    /// forecast sea: a design that can be lost on the way to the objective does not pass, whatever its tags
    /// say. A sweep cut short with no run over is not a failure here (it reads as not finished, never as safe).
    pub fn passes(&self) -> bool { self.tags.iter().all(|t| t.pass) && !self.rolls_over() }
    /// The seakeeping sweep ran and found that the hull can roll over in the forecast sea.
    pub fn rolls_over(&self) -> bool { self.seakeeping.as_ref().is_some_and(|s| s.rolls_over()) }
    pub fn first_failing(&self) -> Option<&ConstraintTag> { self.tags.iter().find(|t| !t.pass).map(|t| &t.tag) }
}

/// Default and limits of the simulated time, seconds.
pub const DEFAULT_SECONDS: f64 = 30.0;
const MIN_SECONDS: f64 = 2.0;
const MAX_SECONDS: f64 = 120.0;
/// The straight leg's speed is averaged over at most this many of its last seconds.
pub const SPEED_MEAN_S: f64 = 5.0;
/// Turn time is extrapolated from the yaw rate over this many last seconds.
const YAW_RATE_S: f64 = 2.0;
/// Yaw rate below which a hull counts as not turning, degrees per second.
const MIN_YAW_DEG_S: f64 = 0.05;

fn fin(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }

/// Full Fit Check (simulated): the tag run, then the seakeeping sweep in the forecast sea. Unit: `sys-fitcheck`.
///
/// The sweep's sims take `deps`' catalogue, campaign, design cache, budgets and ladder, with no script host
/// (trial sorties run no scripts).
pub fn fit_check(bp: &Blueprint, brief: &FitBrief, deps: SimDeps, seconds: f64, deadline: &mut dyn Deadline) -> FitResult {
    let mut sibling = sibling_maker(&deps);
    let mut result = fit_check_run(bp, brief, deps, seconds, deadline).0;
    if result.simulated { result.seakeeping = Some(seakeeping(bp, &brief.weather, &mut sibling, deadline)); }
    result
}

/// The tag run alone, without the seakeeping sweep: what the Quartermaster checks each candidate with, and
/// what a caller that runs [`seakeeping`] on its own schedule calls. Unit: `sys-fitcheck`.
pub fn fit_check_tags(bp: &Blueprint, brief: &FitBrief, deps: SimDeps, seconds: f64, deadline: &mut dyn Deadline) -> FitResult { fit_check_run(bp, brief, deps, seconds, deadline).0 }

/// A maker of fresh deps sharing `deps`' data, with the null script host.
fn sibling_maker(deps: &SimDeps) -> impl FnMut() -> SimDeps {
    let (catalogue, campaign, guest, budgets, ladder) = (deps.catalogue.clone(), deps.campaign.clone(), deps.guest_gate_passed, deps.budgets, deps.ladder);
    let designs = deps.designs.designs.clone();
    move || SimDeps {
        catalogue: catalogue.clone(), campaign: campaign.clone(), designs: crate::ship::design::DesignCache { designs: designs.clone() },
        scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: guest, budgets, ladder,
    }
}

/// [`fit_check`], and whether the hull was lost (capsized, sunk or wrecked) during its run.
pub(crate) fn fit_check_run(bp: &Blueprint, brief: &FitBrief, deps: SimDeps, seconds: f64, deadline: &mut dyn Deadline) -> (FitResult, bool) {
    let bp = super::splash::canonical(bp);
    let catalogue = deps.catalogue.clone();
    let campaign = deps.campaign.clone();
    let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &catalogue));
    let facts = DesignNumbers::of(&prefab, &catalogue);
    let mut result = screen_from(&bp, brief, &facts, &catalogue, &campaign);
    let seconds = if seconds.is_finite() { seconds.clamp(MIN_SECONDS, MAX_SECONDS) } else { DEFAULT_SECONDS };
    let design = Arc::new(crate::ship::ShipDesign::build(&bp, prefab, &catalogue, &super::splash::basin_ruleset()));
    let mut deps = deps;
    deps.designs.insert(design);
    let mut lost = false;
    if let Some(run) = simulate(&bp, brief, deps, facts.top_speed_kn, seconds, &campaign, deadline) {
        lost = run.lost;
        result.simulated = true;
        result.seconds_simulated = run.seconds;
        if let Some(kn) = run.speed_kn { result.top_speed_kn = kn; }
        result.time_to_turn_180_s = run.turn_s;
        result.tags = brief.tags.iter().map(|t| score_tag(t, &bp, facts.draft_m, facts.max_rcs_m2, facts.range_km, result.top_speed_kn, &catalogue)).collect();
    }
    (result, lost)
}

/// Analytic screen only: tags and numbers from gauges. Unit: `sys-fitcheck`.
///
/// Speed is the Speed gauge's top speed; the 180 degree turn time is half the gauge's turning circle at
/// that speed (0 when the hull cannot turn). Threat classes resolve through the build's campaign data.
pub fn fit_screen(bp: &Blueprint, brief: &FitBrief, catalogue: &Catalogue) -> FitResult { fit_screen_with(bp, brief, catalogue, content::campaign::load::campaign()) }

/// [`fit_screen`] with threat classes from the given campaign data.
pub(crate) fn fit_screen_with(bp: &Blueprint, brief: &FitBrief, catalogue: &Catalogue, campaign: &CampaignData) -> FitResult {
    let bp = super::splash::canonical(bp);
    let prefab = sim_core::vessel::build_prefab(&bp, catalogue);
    let facts = DesignNumbers::of(&prefab, catalogue);
    screen_from(&bp, brief, &facts, catalogue, campaign)
}

/// Score one tag against measured or predicted numbers. Unit: `sys-fitcheck`.
///
/// `value` is the design's number and `limit` the briefing's, in the tag's own unit (metres, square metres,
/// kilometres, berths, parts, knots). A number that is not finite fails.
pub fn score_tag(tag: &ConstraintTag, bp: &Blueprint, draft_m: f64, max_rcs_m2: f64, range_km: f64, top_speed_kn: f64, catalogue: &Catalogue) -> TagResult {
    let at_most = |v: f64, l: f64| v.is_finite() && !l.is_nan() && v <= l;
    let at_least = |v: f64, l: f64| v.is_finite() && !l.is_nan() && v >= l;
    let (pass, value, limit) = match tag {
        ConstraintTag::DraftLimit { max_m } => (at_most(draft_m, *max_m), draft_m, *max_m),
        ConstraintTag::RcsLimit { max_m2 } => (at_most(max_rcs_m2, *max_m2), max_rcs_m2, *max_m2),
        ConstraintTag::Range { min_km } => (at_least(range_km, *min_km), range_km, *min_km),
        ConstraintTag::Passengers { count, .. } => { let b = passenger_berths(bp, catalogue) as f64; (b >= *count as f64, b, *count as f64) }
        ConstraintTag::Requires(t) => { let n = parts_with_tag(bp, *t, catalogue) as f64; (n >= 1.0, n, 1.0) }
        ConstraintTag::MinSpeed { kn } => (at_least(top_speed_kn, *kn), top_speed_kn, *kn),
    };
    TagResult { tag: tag.clone(), pass, value: fin(value), limit: if limit.is_nan() { 0.0 } else { limit } }
}

/// Passenger berths fitted.
pub(crate) fn passenger_berths(bp: &Blueprint, catalogue: &Catalogue) -> u32 {
    bp.parts.iter().filter_map(|p| catalogue.part(&p.part)).map(|d| match d.function { PartFunction::PassengerBerths { count } => count as u32, _ => 0 }).sum()
}

/// Parts that carry a tag, directly or through the kit they were fitted in.
pub(crate) fn parts_with_tag(bp: &Blueprint, tag: PartTag, catalogue: &Catalogue) -> u32 {
    let kit_tagged = |k: Option<KitSlot>| k.and_then(|k| bp.kits.get(k.index())).and_then(|kp| bp.embedded_kits.get(&kp.kit).or_else(|| catalogue.kit(&kp.kit))).is_some_and(|kd| kd.tags.contains(&tag));
    bp.parts.iter().filter(|p| catalogue.part(&p.part).is_some_and(|d| d.tags.contains(&tag)) || kit_tagged(p.kit)).count().min(u32::MAX as usize) as u32
}

/// The analytic numbers of one design.
struct DesignNumbers { draft_m: f64, max_rcs_m2: f64, range_km: f64, top_speed_kn: f64, turning_circle_m: f64, sight: SensorFacts }
impl DesignNumbers {
    fn of(prefab: &VesselPrefab, catalogue: &Catalogue) -> DesignNumbers {
        let float = super::gauges::float_gauge(prefab, 1.0e9, 10.0, catalogue);
        let speed = super::gauges::speed_gauge(prefab, catalogue);
        let endurance = super::gauges::endurance_gauge(prefab, catalogue);
        let areas = super::gauges::signature_view(prefab, catalogue);
        let max_rcs = areas.rcs_m2.iter().map(|v| *v as f64).filter(|v| v.is_finite()).fold(0.0, f64::max);
        DesignNumbers {
            draft_m: fin(float.draft_m), max_rcs_m2: max_rcs, range_km: fin(endurance.range_km), top_speed_kn: fin(speed.top_speed_kn),
            turning_circle_m: fin(speed.turning_circle_m), sight: SensorFacts::of(prefab, &areas, catalogue),
        }
    }
}

fn screen_from(bp: &Blueprint, brief: &FitBrief, d: &DesignNumbers, catalogue: &Catalogue, campaign: &CampaignData) -> FitResult {
    let weather = forecast(&brief.weather);
    let tags = brief.tags.iter().map(|t| score_tag(t, bp, d.draft_m, d.max_rcs_m2, d.range_km, d.top_speed_kn, catalogue)).collect();
    let speed_mps = d.top_speed_kn * sim_core::time::KN;
    let turn = if d.turning_circle_m > 0.0 && speed_mps > 0.0 { core::f64::consts::PI * 0.5 * d.turning_circle_m / speed_mps } else { 0.0 };
    let mut detect_km = Vec::new();
    let mut seen: Vec<&ClassId> = Vec::new();
    for c in &brief.threats {
        if seen.contains(&c) { continue; }
        seen.push(c);
        let Some(t) = threat_facts(c, campaign, catalogue) else { continue };
        let ours = d.sight.detects(&t, &weather, catalogue) / 1_000.0;
        let theirs = t.detects(&d.sight, &weather, catalogue) / 1_000.0;
        detect_km.push((c.clone(), fin(ours), fin(theirs)));
    }
    FitResult { tags, top_speed_kn: d.top_speed_kn, time_to_turn_180_s: fin(turn), range_km: d.range_km, detect_km, simulated: false, seconds_simulated: 0.0, seakeeping: None }
}

/// The briefing's weather with unusable values replaced.
pub(crate) fn forecast(w: &WeatherState) -> WeatherState {
    let mut w = *w;
    w.sea_state = SeaState(w.sea_state.0.min(8));
    if !(w.visibility_m.is_finite() && w.visibility_m > 0.0) { w.visibility_m = w.kind.visibility_m(); }
    if !(w.wind_speed_mps.is_finite() && w.wind_speed_mps >= 0.0) { w.wind_speed_mps = 0.0; }
    if !w.wind_dir_rad.is_finite() { w.wind_dir_rad = 0.0; }
    w.cloud_cover = if w.cloud_cover.is_finite() { w.cloud_cover.clamp(0.0, 1.0) } else { 0.0 };
    w.rain = if w.rain.is_finite() { w.rain.clamp(0.0, 1.0) } else { 0.0 };
    if !w.sun_elevation_deg.is_finite() { w.sun_elevation_deg = 40.0; }
    if !w.sun_azimuth_deg.is_finite() { w.sun_azimuth_deg = 180.0; }
    w
}

// ---------------------------------------------------------------------------------------------------------
// Detection.

/// What one hull shows and sees, for the detection estimate.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SensorFacts {
    /// Mean radar cross-section and above-water area over the 36 aspects, and the mast top above water.
    pub rcs_m2: f64,
    pub above_m2: f64,
    pub top_m: f64,
    /// Radars as (reference range, antenna height above water).
    pub radars: Vec<(f64, f64)>,
    pub lookout_m: Option<f64>,
    pub cameras_m: Vec<f64>,
}
impl SensorFacts {
    pub(crate) fn of(prefab: &VesselPrefab, areas: &sim_core::vessel::ProjectedAreas, catalogue: &Catalogue) -> SensorFacts {
        let mean = |v: &[f32]| { let ok: Vec<f64> = v.iter().map(|x| *x as f64).filter(|x| x.is_finite() && *x >= 0.0).collect(); if ok.is_empty() { 0.0 } else { ok.iter().sum::<f64>() / ok.len() as f64 } };
        let mut f = SensorFacts { rcs_m2: mean(&areas.rcs_m2), above_m2: mean(&areas.above_m2), top_m: fin(areas.top_height_m as f64).max(0.0), ..Default::default() };
        for s in super::gauges::calc::sensor_sites(prefab, catalogue) {
            match s.kind {
                SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar => f.radars.push((s.r_ref_m, s.height_m.max(0.0))),
                SensorKind::Camera => f.cameras_m.push(s.height_m.max(0.0)),
                SensorKind::Visual => f.lookout_m = Some(f.lookout_m.map_or(s.height_m, |h: f64| h.max(s.height_m)).max(0.0)),
                _ => {}
            }
        }
        if let Some(h) = super::gauges::calc::lookout_height_m(prefab, catalogue) { f.lookout_m = Some(f.lookout_m.map_or(h, |x| x.max(h)).max(0.0)); }
        f
    }

    /// First-detection range in metres of `target` by this hull's best sensor, by day.
    pub(crate) fn detects(&self, target: &SensorFacts, weather: &WeatherState, catalogue: &Catalogue) -> f64 {
        use crate::sensors::{optical, radar};
        let t = &catalogue.tuning.sensors;
        let clutter = crate::sensors::clutter_for(weather, t);
        let mut best = 0.0f64;
        for &(r_ref, h) in &self.radars {
            let r = radar::radar_ranges(r_ref, h, target.rcs_m2, target.top_m, clutter, None, t).final_m;
            if r.is_finite() { best = best.max(r); }
        }
        let eye = |h: f64, camera: bool| optical::visual_range_m(target.above_m2, camera, false, false, weather.visibility_m, radar::horizon_m(h, target.top_m), t);
        if let Some(h) = self.lookout_m { let r = eye(h, false); if r.is_finite() { best = best.max(r); } }
        for &h in &self.cameras_m { let r = eye(h, true); if r.is_finite() { best = best.max(r); } }
        best.max(0.0)
    }
}

/// Facts of a threat class's stock design, built once per class blueprint per process.
fn threat_facts(class: &ClassId, campaign: &CampaignData, catalogue: &Catalogue) -> Option<SensorFacts> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<BTreeMap<String, SensorFacts>>> = std::sync::OnceLock::new();
    let path = campaign.classes.get(class)?.blueprint.clone();
    if path.trim().is_empty() { return None; }
    let cache = CACHE.get_or_init(Default::default);
    if let Some(f) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&path) { return Some(f.clone()); }
    let bp = content::campaign::load::stock_blueprint(&path).ok()?;
    let prefab = sim_core::vessel::build_prefab(&bp, catalogue);
    let areas = super::gauges::signature_view(&prefab, catalogue);
    let f = SensorFacts::of(&prefab, &areas, catalogue);
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(path, f.clone());
    Some(f)
}

// ---------------------------------------------------------------------------------------------------------
// The run.

struct RunOut { speed_kn: Option<f64>, turn_s: f64, seconds: f64, lost: bool }

/// Time to turn 180 degrees from the turn's heading record (one entry per tick, unwrapped degrees from the
/// start of the turn), extrapolating an unfinished turn from its last yaw rate; 0 when it does not turn.
pub(crate) fn turn_time(turned_deg: &[f64]) -> f64 {
    let hz = super::trials::TICKS_PER_S as f64;
    if let Some(i) = turned_deg.iter().position(|d| *d >= 180.0) { return (i + 1) as f64 / hz; }
    let n = turned_deg.len();
    let k = (super::trials::ticks_of(YAW_RATE_S) as usize).min(n.saturating_sub(1));
    if k == 0 { return 0.0; }
    let rate = (turned_deg[n - 1] - turned_deg[n - 1 - k]) / (k as f64 / hz);
    if !(rate.is_finite() && rate >= MIN_YAW_DEG_S) { return 0.0; }
    n as f64 / hz + (180.0 - turned_deg[n - 1]) / rate
}

fn simulate(bp: &Blueprint, brief: &FitBrief, deps: SimDeps, start_kn: f64, seconds: f64, campaign: &CampaignData, deadline: &mut dyn Deadline) -> Option<RunOut> {
    use super::trials as tr;
    let total = tr::ticks_of(seconds).max(2);
    let straight = total / 2;
    let mut config = tr::trial_base(bp, SeaState::CALM, 1, campaign, total, start_kn);
    config.weather = forecast(&brief.weather);
    let mut sim = tr::start_manual(config, deps)?;
    let own = tr::own_vessel(&sim)?;
    let mean_ticks = tr::ticks_of(SPEED_MEAN_S) as usize;
    let mut speeds: Vec<f64> = Vec::with_capacity(straight as usize);
    let mut turned: Vec<f64> = Vec::new();
    let mut meter = tr::HeadingMeter::default();
    let mut ran = 0u64;
    let mut lost = false;
    for i in 0..total {
        if i % tr::TICKS_PER_S == 0 && i > 0 && (deadline.expired() || deadline.candidate_expired()) { break; }
        sim.set_host_load(crate::lod::HostLoad::IDLE);
        sim.step();
        drop(sim.drain_events());
        ran = i + 1;
        let Some(s) = tr::sample(&sim, own) else { lost = true; break };
        if !s.afloat { lost = true; break; }
        if ran <= straight {
            speeds.push(s.speed_mps);
            tr::set_helm(&mut sim, ran, 1.0, if ran == straight { 1.0 } else { 0.0 });
            if ran == straight { meter.update(s.heading_rad); }
        } else {
            turned.push(meter.update(s.heading_rad));
            tr::set_helm(&mut sim, ran, 1.0, 1.0);
        }
        if sim.is_over() { break; }
    }
    let speed_kn = if speeds.len() >= super::trials::TICKS_PER_S as usize {
        let k = mean_ticks.clamp(1, speeds.len() / 2 + 1).min(speeds.len());
        let v = speeds[speeds.len() - k..].iter().sum::<f64>() / k as f64;
        Some(fin(v) / sim_core::time::KN)
    } else { None };
    Some(RunOut { speed_kn, turn_s: fin(turn_time(&turned)), seconds: ran as f64 / tr::TICKS_PER_S as f64, lost })
}

// ---------------------------------------------------------------------------------------------------------
// Seakeeping.

/// Directions the forecast sea is run from: from ahead, then every 60 degrees clockwise.
pub const SEA_DIRECTIONS: usize = 6;
/// Simulated time of each direction's run, seconds: full power with the helm amidships for the first half,
/// then the helm hard to starboard, through a 180 degree turn and on.
pub const SEAKEEPING_SECONDS: f64 = 120.0;
/// A run in which the hull heels past this many degrees (or past its range of stability, when that is
/// smaller), or is lost, goes over. The capsize rule (L537) also wants 10 s past a right angle; a hull that
/// gets there has rolled over whether or not it comes back.
pub const ROLLED_OVER_DEG: f64 = 90.0;
/// A design whose range of stability is under this many degrees can roll over in any sea of Beaufort 4 or
/// more, whatever its own runs did. DECISION: one wave seed does not vouch for such a hull. The full sweep
/// (`tests/seakeeping.rs`) found the three Act 1 classes under it (34 to 40 degrees) heeling past their range
/// in a turn at codes 3 and 4 with other seeds and turn timings, while Fit Check's own runs stayed under 11
/// degrees; every hull at or above it (61 degrees and up) that Fit Check called safe stayed safe in the other
/// runs, codes 3 to 8. The value is the starters' own bar (`starter_hulls_sim.rs`: range of stability 45
/// degrees or more).
pub const MIN_RANGE_DEG: f64 = 45.0;

/// One direction of the seakeeping sweep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SeaRun {
    /// Where the sea comes from at the start of the run, degrees clockwise from the bow (0 = from ahead).
    pub from_deg: f64,
    /// Largest heel either way, degrees.
    pub peak_heel_deg: f64,
    /// Went over: heeled past [`Seakeeping::limit_deg`] or was lost (capsized, sunk).
    pub rolled_over: bool,
    /// Simulated seconds; less than [`SEAKEEPING_SECONDS`] when it rolled over or a deadline stopped it.
    pub seconds: f64,
}

/// The seakeeping verdict: the design at full power in the forecast sea, straight and through a turn, with the
/// sea from each of [`SEA_DIRECTIONS`] directions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Seakeeping {
    pub sea_state: SeaState,
    /// Heel at which a run counts as gone over, degrees: [`ROLLED_OVER_DEG`] or the design's range of
    /// stability, whichever is smaller.
    pub limit_deg: f64,
    /// The design's range of stability (the Float gauge's, calm water), degrees.
    #[serde(default)]
    pub range_deg: f64,
    /// Simulated seconds each run was due.
    pub seconds: f64,
    /// Runs in direction order; fewer than [`SEA_DIRECTIONS`] when a deadline cut the sweep short.
    pub runs: Vec<SeaRun>,
}
impl Seakeeping {
    /// Every direction ran to its end or to the hull rolling over.
    pub fn complete(&self) -> bool { self.runs.len() == SEA_DIRECTIONS && self.runs.iter().all(|r| r.rolled_over || r.seconds >= self.seconds - 0.5) }
    /// The hull rolled over with the sea from at least one direction, or its range of stability is under
    /// [`MIN_RANGE_DEG`] in a sea of Beaufort 4 or more.
    pub fn rolls_over(&self) -> bool { self.runs.iter().any(|r| r.rolled_over) || self.short_range() }
    /// The range of stability is under [`MIN_RANGE_DEG`] and the sea is Beaufort 4 or more.
    pub fn short_range(&self) -> bool { self.sea_state.0 >= SeaState::BEAUFORT_4.0 && self.range_deg < MIN_RANGE_DEG }
    /// The hull rides out the forecast sea from every direction: a complete sweep with no run rolled over.
    pub fn safe(&self) -> bool { self.complete() && !self.rolls_over() }
    /// The worst run: one that rolled over, else the largest heel (the first of equals).
    pub fn worst(&self) -> Option<&SeaRun> {
        self.runs.iter().fold(None, |best: Option<&SeaRun>, r| match best {
            Some(b) if (b.rolled_over, b.peak_heel_deg) >= (r.rolled_over, r.peak_heel_deg) => Some(b),
            _ => Some(r),
        })
    }
}

/// Seakeeping sweep of a design in a forecast's sea state. Unit: `sys-fitcheck`.
///
/// Each direction is its own trial sortie (seed 1, as the tag run) with the forecast weather turned so the
/// sea comes from that direction, the hull starting at the Speed gauge's top speed. The directions run one
/// after another on the calling thread: rapier's step hands part of its work to the thread pool and blocks
/// until it comes back, so sorties stepped side by side on one pool can take every worker and deadlock.
/// `deadline.expired()` is checked once per simulated second; a cut sweep keeps what it measured and is not
/// [`Seakeeping::complete`]. A direction whose sortie will not start is left out, which also leaves the
/// sweep incomplete.
pub fn seakeeping(bp: &Blueprint, weather: &WeatherState, make_deps: &mut dyn FnMut() -> SimDeps, deadline: &mut dyn Deadline) -> Seakeeping {
    seakeeping_with(bp, weather, SeaManoeuvre::FIT_CHECK, make_deps, deadline)
}

/// The manoeuvre of each direction's run: simulated seconds, seconds before the helm goes hard to starboard
/// (the whole run for a straight one), and the sortie seed, which draws the wave set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeaManoeuvre { pub seconds: f64, pub straight_s: f64, pub seed: u64 }
impl SeaManoeuvre {
    /// Fit Check's: [`SEAKEEPING_SECONDS`], the helm over half way, seed 1 (the tag run's).
    pub const FIT_CHECK: SeaManoeuvre = SeaManoeuvre { seconds: SEAKEEPING_SECONDS, straight_s: 0.5 * SEAKEEPING_SECONDS, seed: 1 };
}

/// [`seakeeping`] with another manoeuvre: what a check independent of Fit Check's own runs uses.
/// Unit: `sys-fitcheck`.
pub fn seakeeping_with(bp: &Blueprint, weather: &WeatherState, m: SeaManoeuvre, make_deps: &mut dyn FnMut() -> SimDeps, deadline: &mut dyn Deadline) -> Seakeeping {
    use super::trials as tr;
    let bp = super::splash::canonical(bp);
    let w = forecast(weather);
    let first = make_deps();
    let (catalogue, campaign) = (first.catalogue.clone(), first.campaign.clone());
    let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &catalogue));
    let start_kn = fin(super::gauges::speed_gauge(&prefab, &catalogue).top_speed_kn).max(0.0);
    let range = super::gauges::float_gauge(&prefab, 1.0e9, 2.0, &catalogue).range_of_stability_deg;
    let limit_deg = if range.is_finite() && range > 0.0 { range.min(ROLLED_OVER_DEG) } else { ROLLED_OVER_DEG };
    let design = Arc::new(crate::ship::ShipDesign::build(&bp, prefab, &catalogue, &super::splash::basin_ruleset()));
    let total = tr::ticks_of(m.seconds).max(2);
    let straight = tr::ticks_of(m.straight_s).min(total);
    let mut first = Some(first);
    let range_deg = if range.is_finite() { range.max(0.0) } else { 0.0 };
    let mut out = Seakeeping { sea_state: w.sea_state, limit_deg, range_deg, seconds: total as f64 / tr::TICKS_PER_S as f64, runs: Vec::with_capacity(SEA_DIRECTIONS) };
    for i in 0..SEA_DIRECTIONS {
        if deadline.expired() { break; }
        let mut deps = first.take().unwrap_or_else(&mut *make_deps);
        deps.designs.insert(design.clone());
        let from_deg = 360.0 * i as f64 / SEA_DIRECTIONS as f64;
        let Some(mut leg) = SeaLeg::start(&bp, &w, from_deg, deps, start_kn, total, m.seed, limit_deg, &campaign) else { continue };
        let mut tick = 0;
        while tick < total && !leg.done {
            if tick > 0 && deadline.expired() { break; }
            let n = tr::TICKS_PER_S.min(total - tick);
            leg.step(tick, n, total, straight);
            tick += n;
        }
        let cut = !leg.done;
        out.runs.push(leg.run);
        if cut { break; }
    }
    out
}

/// One direction of the sweep while it runs.
struct SeaLeg { sim: crate::Sim, own: EntityId, run: SeaRun, done: bool, limit_deg: f64 }
impl SeaLeg {
    #[allow(clippy::too_many_arguments)]
    fn start(bp: &Blueprint, w: &WeatherState, from_deg: f64, deps: SimDeps, start_kn: f64, total: u64, seed: u64, limit_deg: f64, campaign: &CampaignData) -> Option<SeaLeg> {
        use super::trials as tr;
        let mut config = tr::trial_base(bp, SeaState::CALM, seed, campaign, total, start_kn);
        let heading = config.vessels.first()?.heading_rad;
        config.weather = *w;
        // The sea runs with the wind, which blows toward its direction: a sea from `from_deg` off the bow comes
        // from that bearing and runs toward the opposite one.
        config.weather.wind_dir_rad = (heading + from_deg.to_radians() + core::f64::consts::PI).rem_euclid(core::f64::consts::TAU);
        let sim = tr::start_manual(config, deps)?;
        let own = tr::own_vessel(&sim)?;
        Some(SeaLeg { sim, own, run: SeaRun { from_deg, ..Default::default() }, done: false, limit_deg })
    }

    /// Ticks `from + 1 ..= from + n` of the run: full power, the helm amidships for the first `straight` ticks
    /// and hard to starboard after.
    fn step(&mut self, from: u64, n: u64, total: u64, straight: u64) {
        use super::trials as tr;
        for i in from..from + n {
            if self.done { return; }
            self.sim.set_host_load(crate::lod::HostLoad::IDLE);
            self.sim.step();
            drop(self.sim.drain_events());
            self.run.seconds = (i + 1) as f64 / tr::TICKS_PER_S as f64;
            // Heel from the hull's up axis (past a right angle when it is over), not the splash's beam tilt.
            let Some((heel, afloat)) = self.sim.world().vessel(self.own).map(|v| (v.heel_trim().0.to_degrees(), matches!(v.status, VesselStatus::Active | VesselStatus::Disabled))) else {
                (self.run.rolled_over, self.done) = (true, true);
                return;
            };
            self.run.peak_heel_deg = self.run.peak_heel_deg.max(fin(heel).abs());
            if !afloat || self.run.peak_heel_deg >= self.limit_deg { (self.run.rolled_over, self.done) = (true, true); return; }
            tr::set_helm(&mut self.sim, i + 1, 1.0, if i + 1 >= straight { 1.0 } else { 0.0 });
            if self.sim.is_over() || i + 1 >= total { self.done = true; }
        }
    }
}

#[cfg(test)]
mod tests;
