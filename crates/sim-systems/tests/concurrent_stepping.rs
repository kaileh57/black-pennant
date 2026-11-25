//! Unit `sim-core-physics` (seen from the editor's background work): sims stepped side by side on one rayon
//! pool never hang. Rapier's parallel step hands a BVH pass to `rayon::spawn` and blocks a worker until it
//! comes back, so sims filling every worker of a pool used to deadlock; `PhysicsWorld::step` now lets only one
//! parallel Rapier step run at a time and runs the others serially (see its module notes).
//!
//! This mirrors the client's long pool (`gdclient::worker`, 3 workers, up to 3 jobs spawned with
//! `ThreadPool::spawn`): a trial battery, a Fit Check with its seakeeping sweep and a Ship's Notes effects
//! battery run at the same moment, each on a worker of one 3-worker pool. Deadlines that count simulated
//! seconds keep the runs short in debug builds.
use content::blueprint::Blueprint;
use content::weather::SeaState;
use sim_systems::editor::fitcheck::{fit_check_tags, seakeeping, FitBrief};
use sim_systems::editor::trials::{run_battery, BatterySpec};
use sim_systems::editor::Deadline;
use sim_systems::sim::SimDeps;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

fn deps() -> SimDeps {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    SimDeps { catalogue, campaign, designs: Default::default(), scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }
}

fn starter(name: &str) -> Blueprint { content::campaign::load::stock_blueprint(&format!("starters/{name}.bp.ron")).expect("stock starter") }

/// Expires after `n` checks (the runs check once per simulated second).
struct After(u32);
impl Deadline for After { fn expired(&mut self) -> bool { if self.0 == 0 { true } else { self.0 -= 1; false } } }

/// A hang fails the test instead of stalling the suite. Debug builds are slow and other tests share the
/// machine, so the limit is generous; a deadlock never finishes at all.
const LIMIT: Duration = Duration::from_secs(600);

#[test]
fn trials_fit_check_and_note_effects_step_side_by_side_on_one_pool() {
    let pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(3).thread_name(|i| format!("long-pool-{i}")).build().expect("pool"));
    // Jobs only report; asserting inside a pool job would abort the test binary on failure.
    let (tx, rx) = mpsc::channel::<(&'static str, usize)>();
    let campaign = deps().campaign;

    let (t, c) = (tx.clone(), campaign.clone());
    pool.spawn(move || {
        let card = run_battery(&starter("fac22"), &BatterySpec::SHORT, &c, &mut deps, &mut After(40));
        drop(card);
        let _ = t.send(("trials", 1));
    });
    let t = tx.clone();
    pool.spawn(move || {
        let bp = starter("corvette60");
        let weather = sim_systems::editor::splash::basin_weather(SeaState::BEAUFORT_4, 3);
        let brief = FitBrief { tags: vec![], weather: weather.clone(), approach_km: 40.0, threats: vec![], cap_t: 1_800.0 };
        let fit = fit_check_tags(&bp, &brief, deps(), 20.0, &mut After(20));
        drop(fit);
        let sea = seakeeping(&bp, &weather, &mut deps, &mut After(40));
        let _ = t.send(("fit check", sea.runs.len()));
    });
    let (t, c) = (tx.clone(), campaign);
    pool.spawn(move || {
        // Ship's Notes effects: one battery per Fix, one after another.
        for _ in 0..2 { drop(run_battery(&starter("fac22"), &BatterySpec::SHORT, &c, &mut deps, &mut After(20))); }
        let _ = t.send(("note effects", 2));
    });
    drop(tx);

    let mut done = Vec::new();
    for _ in 0..3 {
        match rx.recv_timeout(LIMIT) {
            Ok((name, runs)) => {
                assert!(runs > 0, "{name} ran nothing");
                done.push(name);
            }
            Err(e) => panic!("background jobs hung on one pool ({e:?}); finished: {done:?}"),
        }
    }
    done.sort_unstable();
    assert_eq!(done, ["fit check", "note effects", "trials"]);
}
