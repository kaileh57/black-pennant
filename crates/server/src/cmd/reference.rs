//! Unit `server-reference`. The reference battle headless, printing a state hash. DESIGN.md L1445-1447
//! (reference scenes), L1150-1168 (same-platform reproducibility; nightly per-subsystem hash comparison
//! between macOS arm64 and Windows x86_64), L1172-1175 (gate measurement: sim pool fixed at 3 threads; p99 tick
//! over a 10-minute run with per-subsystem spans).
//!
//! Runs `editor::trials::reference_config(scene, seed)` with `HostLoad::IDLE` (the ladder stays inert, so the
//! result is deterministic) for `ticks` ticks inside the sim pool, then prints exactly one line on stdout:
//! `state_hash <16 lowercase hex digits> scene <scene> seed <seed> ticks <ticks>`, and with `--subsystems`
//! one more line per subsystem: `subsystem <name> <16 hex digits>`. Timing goes to stderr so stdout stays
//! byte-comparable across runs and platforms. Golden hashes committed by CI compare stdout.
//!
//! The stderr report (wall time of `Sim::step` only; `sim_core::spans` splits it by phase):
//! - `timing ticks N threads T mean M ms p50 A ms p95 B ms p99 C ms max D ms`
//! - `budget <row> mean M ms p99 P ms of B ms`: the tick split rows of DESIGN.md "Performance budgets"
//!   (per-tick sums of the spans that make up each row; p99 is over ticks, so the rows' p99s do not add up).
//! - `span <phase> mean M ms p99 P ms max X ms`
//! - `worst tick <index> <ms> ms: <phase> <ms>, ...` for the 5 slowest ticks with their 3 largest phases.
//!   Ticks are deterministic, so a slow tick can be replayed with `--ticks <index + 1>`.
use crate::args::ReferenceArgs;
use crate::error::{CliError, Code};
use crate::runtime::Data;
use serde::{Deserialize, Serialize};
use sim_core::hash::StateHash;
use sim_systems::editor::trials::ReferenceScene;

/// Wall-clock timing of a run (stderr only; never part of the hash line).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunTiming { pub ticks: u64, pub mean_ms: f32, pub p50_ms: f32, pub p95_ms: f32, pub p99_ms: f32, pub max_ms: f32 }

/// Per-tick phase times of a run, nanoseconds, one row per tick indexed like `sim_core::spans::Span::ALL`.
pub type SpanRows = Vec<[u64; sim_core::spans::COUNT]>;

#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceOutcome { pub hash: StateHash, pub subsystems: Vec<(&'static str, StateHash)>, pub timing: RunTiming, pub tick_ns: Vec<u64>, pub spans: SpanRows }

/// Unit: `server-reference`.
///
/// Exits 0 after printing; a run that cannot start (bad scene data, a config `Sim::new` refuses) is a
/// runtime error.
pub fn run(args: &ReferenceArgs) -> Result<Code, CliError> {
    let data = crate::runtime::load_data()?;
    let pool = crate::runtime::sim_pool(args.threads)?;
    let clock = if args.cpu_clock { Some(thread_cpu_clock()?) } else { None };
    let outcome = run_reference_with(args.scene, args.seed, args.ticks, &data, &pool, clock)?;
    let mut out = String::new();
    out.push_str(&hash_line(&outcome, args.scene, args.seed, args.ticks));
    out.push('\n');
    if args.subsystems {
        for (name, hash) in &outcome.subsystems { out.push_str(&format!("subsystem {name} {:016x}\n", hash.0)); }
    }
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(out.as_bytes())?;
        stdout.flush()?;
    }
    if args.cpu_clock { eprintln!("clock thread-cpu"); }
    eprint!("{}", timing_report(&outcome, args.threads));
    Ok(crate::error::PASS)
}

/// Build and run one reference scene. Unit: `server-reference`.
///
/// Every tick runs, even after the sortie is decided (a decided `Sim` keeps stepping), so the hash always
/// covers exactly `ticks` ticks and a gate run always measures the same amount of work.
pub fn run_reference(scene: ReferenceScene, seed: u64, ticks: u64, data: &Data, pool: &rayon::ThreadPool) -> Result<ReferenceOutcome, CliError> {
    run_reference_with(scene, seed, ticks, data, pool, None)
}

/// [`run_reference`] timed by `clock` (nanoseconds) instead of the wall clock.
pub fn run_reference_with(scene: ReferenceScene, seed: u64, ticks: u64, data: &Data, pool: &rayon::ThreadPool, clock: Option<sim_core::spans::Clock>) -> Result<ReferenceOutcome, CliError> {
    pool.install(|| {
        let config = sim_systems::editor::trials::reference_config(scene, seed, &data.campaign);
        run_config_with(config, data, ticks, clock)
    })
}

/// The calling thread's CPU time in nanoseconds: robust to a loaded machine, where wall time also counts the
/// time the thread waited for a core.
#[cfg(unix)]
fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime only writes the timespec it is handed.
    let ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0;
    if !ok { return 0; }
    (ts.tv_sec as u64).saturating_mul(1_000_000_000).saturating_add(ts.tv_nsec as u64)
}

#[cfg(unix)]
fn thread_cpu_clock() -> Result<sim_core::spans::Clock, CliError> { Ok(thread_cpu_ns) }

#[cfg(not(unix))]
fn thread_cpu_clock() -> Result<sim_core::spans::Clock, CliError> { Err(CliError::Config("--cpu-clock is available on macOS and Linux only".into())) }

/// Run a config for `ticks` ticks with `HostLoad::IDLE` on the current thread (call inside the sim pool),
/// timing each `Sim::step` only. Queued events are drained and dropped every tick: nothing reads them here,
/// and they are not part of the state hash.
fn run_config(config: sim_systems::protocol::config::SortieConfig, data: &Data, ticks: u64) -> Result<ReferenceOutcome, CliError> { run_config_with(config, data, ticks, None) }

fn run_config_with(config: sim_systems::protocol::config::SortieConfig, data: &Data, ticks: u64, clock: Option<sim_core::spans::Clock>) -> Result<ReferenceOutcome, CliError> {
    let mut sim = sim_systems::Sim::new(config, crate::runtime::sim_deps(data))?;
    sim.set_host_load(sim_systems::lod::HostLoad::IDLE);
    let cap = usize::try_from(ticks).unwrap_or(0).min(1 << 24);
    let mut tick_ns: Vec<u64> = Vec::with_capacity(cap);
    let mut spans: SpanRows = Vec::with_capacity(cap);
    let was = sim_core::spans::enabled();
    let now: sim_core::spans::Clock = clock.unwrap_or(sim_core::spans::wall_ns);
    sim_core::spans::enable_with(now);
    let _ = sim_core::spans::take();
    for _ in 0..ticks {
        let start = now();
        sim.step();
        let ns = now().saturating_sub(start);
        spans.push(sim_core::spans::take());
        drop(sim.drain_events());
        tick_ns.push(ns);
    }
    sim_core::spans::enable(was);
    let timing = timing(&tick_ns);
    Ok(ReferenceOutcome { hash: sim.state_hash(), subsystems: sim.subsystem_hashes(), timing, tick_ns, spans })
}

/// Nearest-rank percentile of sorted nanoseconds, in ms; zero when empty.
fn rank_ms(sorted: &[u64], p: f64) -> f32 {
    let n = sorted.len();
    if n == 0 { return 0.0; }
    let idx = ((p * n as f64).ceil() as usize).clamp(1, n) - 1;
    sorted[idx] as f32 / 1.0e6
}

fn mean_ms(ns: &[u64]) -> f32 { if ns.is_empty() { 0.0 } else { (ns.iter().map(|&v| v as f64).sum::<f64>() / ns.len() as f64 / 1.0e6) as f32 } }

/// Mean, p50, p95, p99 (nearest rank) and max of the tick times; all zero for an empty run.
fn timing(tick_ns: &[u64]) -> RunTiming {
    if tick_ns.is_empty() { return RunTiming::default(); }
    let mut sorted = tick_ns.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    RunTiming { ticks: n as u64, mean_ms: mean_ms(&sorted), p50_ms: rank_ms(&sorted, 0.50), p95_ms: rank_ms(&sorted, 0.95), p99_ms: rank_ms(&sorted, 0.99), max_ms: sorted[n - 1] as f32 / 1.0e6 }
}

/// The stderr report described in the module docs.
pub fn timing_report(o: &ReferenceOutcome, threads: u8) -> String {
    use sim_core::spans::{Budget, Span};
    use std::fmt::Write;
    let t = o.timing;
    let mut out = String::new();
    let _ = writeln!(out, "timing ticks {} threads {threads} mean {:.3} ms p50 {:.3} ms p95 {:.3} ms p99 {:.3} ms max {:.3} ms", t.ticks, t.mean_ms, t.p50_ms, t.p95_ms, t.p99_ms, t.max_ms);
    if o.spans.is_empty() { return out; }
    for b in Budget::ALL {
        let mut col: Vec<u64> = o.spans.iter().map(|row| Span::ALL.iter().filter(|s| s.budget() == b).map(|s| row[*s as usize]).sum()).collect();
        let mean = mean_ms(&col);
        col.sort_unstable();
        let _ = writeln!(out, "budget {} mean {mean:.3} ms p99 {:.3} ms of {:.2} ms", b.name(), rank_ms(&col, 0.99), b.budget_ms());
    }
    for s in Span::ALL {
        let mut col: Vec<u64> = o.spans.iter().map(|row| row[s as usize]).collect();
        let mean = mean_ms(&col);
        col.sort_unstable();
        let _ = writeln!(out, "span {} mean {mean:.3} ms p99 {:.3} ms max {:.3} ms", s.name(), rank_ms(&col, 0.99), col.last().copied().unwrap_or(0) as f32 / 1.0e6);
    }
    let mut order: Vec<usize> = (0..o.tick_ns.len()).collect();
    order.sort_by(|&a, &b| o.tick_ns[b].cmp(&o.tick_ns[a]).then(a.cmp(&b)));
    for &i in order.iter().take(5) {
        let Some(row) = o.spans.get(i) else { continue };
        let mut top: Vec<(u64, Span)> = Span::ALL.iter().map(|s| (row[*s as usize], *s)).collect();
        top.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let parts: Vec<String> = top.iter().take(3).map(|(ns, s)| format!("{} {:.3}", s.name(), *ns as f64 / 1.0e6)).collect();
        let _ = writeln!(out, "worst tick {i} {:.3} ms: {}", o.tick_ns[i] as f64 / 1.0e6, parts.join(", "));
    }
    out
}

/// The stdout line format above. Unit: `server-reference`.
pub fn hash_line(outcome: &ReferenceOutcome, scene: ReferenceScene, seed: u64, ticks: u64) -> String {
    format!("state_hash {:016x} scene {} seed {seed} ticks {ticks}", outcome.hash.0, crate::args::scene_name(scene))
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::campaign::{DifficultySliders, MissionType};
    use content::enums::Act;

    fn generated(data: &Data) -> sim_systems::protocol::config::SortieConfig {
        let td = sim_systems::theatre::TheatreData { campaign: data.campaign.clone(), catalogue: data.catalogue.clone() };
        sim_systems::theatre::opgen::generate_standalone(MissionType::HunterGroupSurvival, Act::Act1, 1, 1, &DifficultySliders::default(), None, &td).expect("an Act 1 hunter-group operation").config
    }

    #[test]
    fn hash_line_format() {
        let o = ReferenceOutcome { hash: StateHash(0xab), subsystems: vec![], timing: RunTiming::default(), tick_ns: vec![], spans: vec![] };
        assert_eq!(hash_line(&o, ReferenceScene::ReferenceBattle, 1, 3600), "state_hash 00000000000000ab scene reference seed 1 ticks 3600");
        assert_eq!(hash_line(&o, ReferenceScene::MinimumSwarm, 9, 0), "state_hash 00000000000000ab scene swarm seed 9 ticks 0");
    }

    #[test]
    fn timing_percentiles() {
        assert_eq!(timing(&[]), RunTiming::default());
        let v: Vec<u64> = (1..=100).rev().map(|i| i * 1_000_000).collect();
        let t = timing(&v);
        assert_eq!((t.ticks, t.mean_ms, t.p50_ms, t.p95_ms, t.p99_ms, t.max_ms), (100, 50.5, 50.0, 95.0, 99.0, 100.0));
        assert_eq!(timing(&[2_000_000]).p99_ms, 2.0);
    }

    #[test]
    fn report_lists_budgets_spans_and_worst_ticks() {
        let mut row = [0u64; sim_core::spans::COUNT];
        row[sim_core::spans::Span::Forces as usize] = 3_000_000;
        let o = ReferenceOutcome { hash: StateHash(1), subsystems: vec![], timing: timing(&[4_000_000, 1_000_000]), tick_ns: vec![4_000_000, 1_000_000], spans: vec![row, [0; sim_core::spans::COUNT]] };
        let r = timing_report(&o, 3);
        assert!(r.starts_with("timing ticks 2 threads 3 mean 2.500 ms"), "{r}");
        assert!(r.contains("budget buoyancy_waves mean 1.500 ms p99 3.000 ms of 1.25 ms"), "{r}");
        assert!(r.contains("span forces mean 1.500 ms"), "{r}");
        assert!(r.contains("worst tick 0 4.000 ms: forces 3.000"), "{r}");
    }

    /// The same config on a 1-thread and a 3-thread pool ends in the same state, subsystem by subsystem.
    #[test]
    fn run_is_deterministic_across_pool_sizes() {
        let data = crate::runtime::load_data().expect("embedded data");
        let one = crate::runtime::sim_pool(1).unwrap();
        let three = crate::runtime::sim_pool(3).unwrap();
        let a = one.install(|| run_config(generated(&data), &data, 240)).expect("starts");
        let b = three.install(|| run_config(generated(&data), &data, 240)).expect("starts");
        let c = three.install(|| run_config(generated(&data), &data, 0)).expect("starts");
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.subsystems, b.subsystems);
        assert!(!a.subsystems.is_empty());
        assert_ne!(a.hash, c.hash, "240 ticks change the state");
        assert_eq!(a.timing.ticks, 240);
        assert_eq!(a.spans.len(), 240);
        assert!(a.spans.iter().any(|r| r.iter().any(|&ns| ns > 0)), "spans are recorded");
        assert_eq!(c.timing, RunTiming::default());
    }
}
