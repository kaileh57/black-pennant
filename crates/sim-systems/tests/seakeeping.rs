//! Unit `sys-fitcheck`: undamaged hulls ride out the seas they meet. The six starters and every Act 1 Combine
//! class run Fit Check's seakeeping sweep (full power, straight for 60 s and then hard over, with the sea from
//! six directions, 120 s each) and none of them rolls over. DESIGN.md L537 (lost when heeled past 90 degrees
//! for 10 s), L801 (Fit Check), L1857-1868 (weather: Act 1's Gannet Banks rolls sea states 4 to 8).
//!
//! The fast test runs the Beaufort 7 sea (code 6), the heaviest the starters' tender test uses. The full sweep
//! is `#[ignore]`: every hull in codes 3 to 8, through Fit Check's manoeuvre and two independent ones (other
//! wave seeds, a straight run and an early turn). In codes 3 to 6 Fit Check must call every hull safe but the
//! pending ones; in every code, no independent run may roll over a hull Fit Check called safe, so in codes 7
//! and 8 a hull either rides the sea out or Fit Check says it can roll over. Run it with
//! `cargo test --release -p sim-systems --test seakeeping -- --ignored --nocapture`.
use content::blueprint::Blueprint;
use content::weather::SeaState;
use sim_systems::editor::fitcheck::{seakeeping_with, SeaManoeuvre, Seakeeping, ROLLED_OVER_DEG, SEA_DIRECTIONS};
use sim_systems::editor::NoDeadline;
use sim_systems::sim::SimDeps;
use std::sync::Arc;

fn deps() -> SimDeps {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    SimDeps { catalogue, campaign, designs: Default::default(), scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }
}

/// The six starters and every Act 1 Combine class, by stock path.
fn hulls() -> Vec<(String, Blueprint)> {
    let mut paths: Vec<String> = content::data::STOCK_FILES.iter().map(|(p, _)| p.to_string())
        .filter(|p| (p.starts_with("starters/") || p.starts_with("combine/act1/")) && p.ends_with(".bp.ron")).collect();
    paths.sort();
    assert!(paths.iter().filter(|p| p.starts_with("starters/")).count() == 6 && paths.len() >= 6 + 15, "{paths:?}");
    paths.into_iter().map(|p| { let bp = content::campaign::load::stock_blueprint(&p).unwrap_or_else(|e| panic!("{p}: {e}")); (p, bp) }).collect()
}

fn sweep(bp: &Blueprint, sea: SeaState, m: SeaManoeuvre) -> Seakeeping {
    let weather = sim_systems::editor::splash::basin_weather(sea, 3);
    seakeeping_with(bp, &weather, m, &mut deps, &mut NoDeadline)
}

fn line(path: &str, s: &Seakeeping) -> String {
    let runs: Vec<String> = s.runs.iter().map(|r| format!("{:.0}:{:.1}{}", r.from_deg, r.peak_heel_deg, if r.rolled_over { " OVER" } else { "" })).collect();
    format!("{path} sea {} (over at {:.0}{}): {}", s.sea_state.0, s.limit_deg, if s.short_range() { ", range too short" } else { "" }, runs.join(" "))
}

/// Fit Check's verdict (`sweeps[0]`) and the other manoeuvres' sweeps for one hull in one sea.
struct Row { path: String, sea: u8, sweeps: Vec<Seakeeping> }
impl Row {
    fn safe(&self) -> bool { let s = &self.sweeps[0]; s.safe() && s.runs.len() == SEA_DIRECTIONS && s.runs.iter().all(|r| r.peak_heel_deg < s.limit_deg.min(ROLLED_OVER_DEG)) }
    fn pending(&self) -> bool { PENDING.contains(&self.path.as_str()) }
    /// Another manoeuvre went over where Fit Check said safe.
    fn hole(&self) -> bool { self.safe() && self.sweeps[1..].iter().any(|s| s.rolls_over() || !s.complete()) }
}

/// Every hull in every listed sea, each through Fit Check's manoeuvre and then `others`. Sweeps run four at a
/// time, each on its own thread with a one-worker pool: rapier blocks a pool worker while its step's spawned
/// work runs, so sorties must not share a pool's workers (see `fitcheck::seakeeping`). One worker or three,
/// the result is the same.
fn run_all(seas: &[u8], others: &[SeaManoeuvre]) -> Vec<Row> {
    let hulls = hulls();
    let jobs: Vec<(&str, &Blueprint, u8)> = hulls.iter().flat_map(|(p, bp)| seas.iter().map(move |s| (p.as_str(), bp, *s))).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done: std::sync::Mutex<Vec<Option<Row>>> = std::sync::Mutex::new((0..jobs.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().expect("pool");
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((p, bp, sea)) = jobs.get(i) else { break };
                    let sweeps = pool.install(|| std::iter::once(SeaManoeuvre::FIT_CHECK).chain(others.iter().copied()).map(|m| sweep(bp, SeaState(*sea), m)).collect());
                    done.lock().expect("results")[i] = Some(Row { path: p.to_string(), sea: *sea, sweeps });
                }
            });
        }
    });
    let rows: Vec<Row> = done.into_inner().expect("results").into_iter().map(|r| r.expect("every job ran")).collect();
    for r in &rows { for s in &r.sweeps { eprintln!("{}", line(&r.path, s)); } }
    rows
}

/// Sweeps run at once.
const THREADS: usize = 4;

/// Known misses waiting on their build sheets, by stock path. None: the launch, the missile corvette and the
/// frigate rolled over in a hard turn with ranges of stability of 34 to 40 degrees until their sheets
/// (`content/tests/combine/classes.rs`) lowered their centres of mass, and now keep a range past 60 degrees.
/// The test fails on an entry that no longer misses.
const PENDING: [&str; 0] = [];

/// Fit Check's verdicts that break the bar: a hull not [`PENDING`] that can roll over, and a pending hull
/// that no longer can.
fn unexpected(rows: &[Row]) -> Vec<String> {
    let mut out: Vec<String> = rows.iter().filter(|r| !r.safe() && !r.pending()).map(|r| line(&r.path, &r.sweeps[0])).collect();
    for p in PENDING { if rows.iter().all(|r| r.path != p || r.safe()) { out.push(format!("{p} no longer rolls over: take it off PENDING")); } }
    out
}

#[test]
fn starters_and_act1_classes_ride_out_a_beaufort_7_sea_from_every_direction() {
    let rows = run_all(&[SeaState::BEAUFORT_7.0], &[]);
    let bad = unexpected(&rows);
    assert!(bad.is_empty(), "undamaged hulls can roll over at full power: {bad:#?}");
    // No starter is ever pending.
    assert!(rows.iter().all(|r| !r.path.starts_with("starters/") || r.safe()));
}

/// Manoeuvres independent of Fit Check's own runs: another wave seed with the helm amidships throughout, and a
/// third with the helm over after 30 s.
const OTHERS: [SeaManoeuvre; 2] = [
    SeaManoeuvre { seconds: 120.0, straight_s: 120.0, seed: 2 },
    SeaManoeuvre { seconds: 120.0, straight_s: 30.0, seed: 3 },
];

#[test]
#[ignore = "the full sweep: every hull in sea states 3 to 8 through Fit Check's manoeuvre and two others"]
fn starters_and_act1_classes_ride_out_sea_states_3_to_6_and_fit_check_warns_of_the_rest() {
    let rows = run_all(&[3, 4, 5, 6, 7, 8], &OTHERS);
    let (light, heavy): (Vec<&Row>, Vec<&Row>) = rows.iter().partition(|r| r.sea <= 6);
    // Sea states 3 to 6: Fit Check's verdict is safe for every hull but the pending ones.
    let light_rows: Vec<Row> = light.iter().map(|r| Row { path: r.path.clone(), sea: r.sea, sweeps: r.sweeps.clone() }).collect();
    let bad = unexpected(&light_rows);
    // Every sea state: no manoeuvre rolls a hull over that Fit Check called safe.
    let holes: Vec<String> = rows.iter().filter(|r| r.hole()).map(|r| format!("{} sea {}: Fit Check safe, but {}", r.path, r.sea, r.sweeps[1..].iter().map(|s| line("", s)).collect::<Vec<_>>().join(" | "))).collect();
    let warned: Vec<String> = heavy.iter().filter(|r| !r.safe()).map(|r| format!("{} sea {}", r.path, r.sea)).collect();
    eprintln!("Fit Check says can roll over in sea states 7 and 8: {warned:#?}");
    assert!(bad.is_empty() && holes.is_empty(), "verdicts {bad:#?}\nholes {holes:#?}");
}
