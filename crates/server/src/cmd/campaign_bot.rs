//! Unit `server-campaign-bot`. Headless campaign bots. DESIGN.md L879-895 (policies aggressive, careful,
//! single-method and mixed play the theatre fully and resolve sorties; nightly on free hosted runners, sharded
//! under 6 hours per job; weekly 20 sampled sorties replayed with full physics, bot gates valid only while
//! agreement is at least 85%), L315-318 (stock-officer winnability).
//!
//! Runs `theatre::bot::run_campaign` for each seed of the shard (`seed + i` for run `i`, kept when
//! `i % count == index`) with the chosen resolver, and writes a [`CampaignBotSummary`] as RON. The abstract
//! resolver is blocked on an owner decision (L879; see `theatre::abstract_combat`), so `--resolver full`
//! (`FullPhysicsResolver` over `runtime::sim_deps`) is the default and `abstract` exits with a config error
//! until that unit lands. Gate evaluation is a separate script over the summaries; this command only reports.
//!
//! # Choices this unit makes where the spec is silent
//! - Every run plays on Normal difficulty, not ironman (the L879-895 gates are all "on Normal").
//! - A sortie is cut off at [`SORTIE_LIMIT_MIN`] simulated minutes: the proxy length gate (L81) fails any
//!   sortie over 45 minutes, so a longer one only costs runner time; the report's `sortie_min` still shows
//!   it reached the limit.
//! - Operations per run are unbounded by count ([`MAX_OPERATIONS`] is only a guard); the act end, the
//!   `--max-hours` theatre limit and the bot's own step guards end a run.
//! - With `--out`, the summary is rewritten after every finished run, so a shard the 6-hour job limit kills
//!   keeps the runs it finished. Progress goes to stderr, one line per run.
use crate::args::{CampaignBotArgs, ResolverKind};
use crate::error::{CliError, Code};
use serde::{Deserialize, Serialize};
use sim_systems::theatre::bot::BotReport;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CampaignBotSummary {
    pub game_version: String,
    pub policy: String,
    pub resolver: String,
    pub shard: (u32, u32),
    pub runs: Vec<(u64, BotReport)>,
}

/// Longest simulated sortie a bot plays, minutes (L81's 45-minute ceiling).
pub const SORTIE_LIMIT_MIN: u64 = 45;
/// Guard on operations per run.
pub const MAX_OPERATIONS: u32 = 10_000;

/// Seeds this shard runs, in order: `seed + i` for every run index `i` with `i % count == index`.
pub(crate) fn shard_seeds(seed: u64, runs: u32, shard: (u32, u32)) -> Vec<u64> {
    let (index, count) = (shard.0, shard.1.max(1));
    (0..runs).filter(|i| i % count == index).map(|i| seed.wrapping_add(u64::from(i))).collect()
}

pub(crate) fn resolver_name(r: ResolverKind) -> &'static str { match r { ResolverKind::Full => "full", ResolverKind::Abstract => "abstract" } }

pub(crate) fn bot_config(args: &CampaignBotArgs, seed: u64) -> sim_systems::theatre::bot::BotConfig {
    sim_systems::theatre::bot::BotConfig {
        policy: args.policy, seed, difficulty: content::campaign::DifficultySliders::NORMAL, ironman: false, players: args.players,
        until_act: args.until_act, max_operations: MAX_OPERATIONS, max_theatre_hours: args.max_hours,
    }
}

/// Unit: `server-campaign-bot`.
pub fn run(args: &CampaignBotArgs) -> Result<Code, CliError> {
    if args.resolver == ResolverKind::Abstract {
        return Err(CliError::Config("the abstract resolver awaits the owner decision on its inputs, outcome rule and Salvage and Alert outputs (DESIGN.md L879); use --resolver full".into()));
    }
    let data = crate::runtime::load_data()?;
    let pool = crate::runtime::sim_pool(3)?;
    let seeds = shard_seeds(args.seed, args.runs, args.shard);
    let mut summary = CampaignBotSummary {
        game_version: env!("CARGO_PKG_VERSION").to_string(),
        policy: crate::args::policy_name(args.policy),
        resolver: resolver_name(args.resolver).to_string(),
        shard: args.shard,
        runs: Vec::with_capacity(seeds.len()),
    };
    let theatre = sim_systems::theatre::TheatreData { campaign: data.campaign.clone(), catalogue: data.catalogue.clone() };
    for (k, &seed) in seeds.iter().enumerate() {
        let started = std::time::Instant::now();
        let deps_data = data.clone();
        let config = bot_config(args, seed);
        let theatre = theatre.clone();
        // The resolver (and its boxed deps factory) lives on the pool thread that plays the run.
        let report = pool.install(move || {
            let mut resolver = sim_systems::theatre::abstract_combat::FullPhysicsResolver {
                make_deps: Box::new(move || crate::runtime::sim_deps(&deps_data)),
                max_ticks: SORTIE_LIMIT_MIN * 60 * sim_core::time::TICK_HZ as u64,
                last_min: 0.0,
            };
            sim_systems::theatre::bot::run_campaign(&config, theatre, &mut resolver)
        });
        eprintln!(
            "campaign-bot run {}/{} seed {seed}: {} operations, {} theatre hours, peak alert {:.2}, capstones {:?}, {:.1} s",
            k + 1, seeds.len(), report.operations.len(), report.theatre_hours, report.peak_alert, report.capstone_done, started.elapsed().as_secs_f64()
        );
        summary.runs.push((seed, report));
        if args.out.is_some() { crate::runtime::write_report(&summary, args.out.as_deref())?; }
    }
    if args.out.is_none() { crate::runtime::write_report(&summary, None)?; }
    Ok(crate::error::PASS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_systems::theatre::bot::Policy;

    #[test]
    fn shards_partition_the_runs() {
        assert_eq!(shard_seeds(10, 5, (0, 1)), vec![10, 11, 12, 13, 14]);
        assert_eq!(shard_seeds(10, 5, (0, 2)), vec![10, 12, 14]);
        assert_eq!(shard_seeds(10, 5, (1, 2)), vec![11, 13]);
        assert!(shard_seeds(10, 2, (2, 3)).is_empty());
        let mut all: Vec<u64> = (0..3).flat_map(|i| shard_seeds(1, 200, (i, 3))).collect();
        all.sort();
        assert_eq!(all, (1..=200).collect::<Vec<_>>());
        // Seeds wrap instead of overflowing.
        assert_eq!(shard_seeds(u64::MAX, 2, (0, 1)), vec![u64::MAX, 0]);
    }

    #[test]
    fn abstract_is_refused_and_summaries_round_trip() {
        let args = CampaignBotArgs { policy: Policy::Careful, seed: 1, runs: 1, until_act: content::enums::Act::Act1, players: 2, resolver: ResolverKind::Abstract, shard: (0, 1), max_hours: 100, out: None };
        assert!(matches!(run(&args), Err(CliError::Config(_))));
        let c = bot_config(&args, 7);
        assert_eq!((c.seed, c.players, c.max_theatre_hours, c.ironman), (7, 2, 100, false));
        let s = CampaignBotSummary { game_version: "0".into(), policy: "careful".into(), resolver: "full".into(), shard: (1, 3), runs: vec![(4, BotReport { peak_alert: 2.5, theatre_hours: 99, ..Default::default() })] };
        let dir = std::env::temp_dir().join(format!("bp-campaign-bot-{}", std::process::id()));
        let path = dir.join("s.ron");
        crate::runtime::write_report(&s, Some(&path)).unwrap();
        assert_eq!(crate::runtime::read_ron::<CampaignBotSummary>(&path).unwrap(), s);
        let _ = std::fs::remove_dir_all(dir);
    }
}
