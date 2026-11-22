//! Unit `sys-campaign-bot`. Headless campaign bots for balance and winnability checks.
//! DESIGN.md L879-895 (tuning gates), L315-318 (stock-officer gates), L81 (proxy length gate), L96
//! (commitment gate).
//!
//! Policies: aggressive, careful, single-method, mixed (plus all-ghost and no-ghost for the Intel gate). A bot
//! plays the theatre fully through `Theatre`'s public API and resolves sorties through a `SortieResolver`.
//! Runs nightly on free hosted runners, sharded under 6 hours per job. Gates read the report: Alert and
//! retreat (aggressive reaches Alert 5 somewhere in at least 70% of 200 Act 1 runs, careful in at most 30%),
//! supply and lane cuts, operation clock (mixed finishes the Act 1 capstone before the clock runs out in 60
//! to 85%), doctrine cards, Intel costs, repeat cap (zero no-offerable-template states in 1,000 runs),
//! economy (median income to spend 1.05 to 1.25).
//!
//! # How a bot plays (choices the spec leaves open)
//! - Fighting setup per policy ([`player_setup`]): aggressive radiates (EMCON Active); careful runs silent
//!   and attempts ghost conditions (emission control, unobserved kills); single-method holds every weapon of
//!   another method; mixed fights with stock officers as they come; all-ghost attempts every ghost condition
//!   silent; no-ghost attempts none and radiates. Every policy's Helm follows the briefing route.
//! - Starter hull: the act's starter that carries a weapon of the policy's method (single-method), then the
//!   one whose analytic Fit Check passes the briefing tags of the most of the act's operations; ties go to
//!   the act's data order. All bot players sail the same design. The winnability check instead sails, per
//!   operation, the gate's starter for it ([`op_starter`]: the fast attack craft unless the gate's table
//!   names another, refitted by the Quartermaster for the operation's tags).
//! - Chart table, in order ([`choose_action`]): the forced operation; rebuild a lost hull (a short pool
//!   pays what it holds); wait for the dock while no hull can sail; spend Intel on a queued doctrine card within 48 hours of
//!   arrival (careful and all-ghost cancel when they can and delay otherwise; mixed and no-ghost only delay;
//!   aggressive and single-method never spend, so the counters they provoke arrive); careful waits 12 hours
//!   while every offer sits in a region at Alert 3 or more and over 7 days remain on the clock; move SEXTANT
//!   to a cove that saves at least 6 hours of LANTERN's round trip to the chosen operation net of the transit;
//!   accept the best offer; with nothing on offer wait 6 hours.
//! - Offer score: capstone 1,000, story 500, ace duel 300 (careful 100), templates 0; plus the clock pushback
//!   in hours minus LANTERN's round trip; aggressive and single-method add 48 to combat templates, careful
//!   adds 48 to quiet ones and subtracts 24 per Alert level of the region, the rest subtract 8 per level;
//!   plus a seeded jitter under 12 (`rng::Stream::Theatre`, key "bot.offer" and the operation).
//! - Dock upkeep before every decision: repair in full when the pool pays for it (else a partial repair),
//!   then rearm when a tank or magazine is short. Before an operation, a hull that fails its briefing tags
//!   on the analytic screen asks the Quartermaster (no deadline, so the answer is deterministic) once per
//!   operation and takes the refit when the pool pays for it; the bot then waits for the dock and plays that
//!   operation while it stays on offer. Spend is the Salvage those services, refits and rebuilds took.
//! - An operation the resolver aborts twice (the sortie cannot be built) is skipped for the rest of the run.
//! - Report: card events are counted from the theatre's event queue (`drain_events`), never from the
//!   after-action report, which repeats them. `cards_arrived_affordable` counts arrivals while the pool held
//!   the Intel to delay the card. `no_offer_states` counts the hours the bot stepped, plus every operation's
//!   after-action state, with no template on offer. `income_spend_ratio` is the end-of-run ratio, total
//!   income over total spend (spend below 1 Salvage counts as 1); the reading at any theatre hour, hour
//!   8 x 24 included, follows from the per-operation `hour`, `income` and `spend`. `final_state_hash` is the
//!   first 8 bytes of BLAKE3 over the bincode of the theatre ledger.
//! - A run stops at the configured act's capstone or loss, a later act, campaign over, `max_operations`,
//!   `max_theatre_hours`, or [`MAX_BOT_HOURS`] and [`MAX_BOT_STEPS`] as guards against a state with no move.
use super::abstract_combat::{success, PlayerSetup, SortieResolver};
use super::{economy, logistics, opgen, progress, saves, Interrupt, NewCampaign, Theatre, TheatreCommand, TheatreData, TheatreEvent, MAX_CAMPAIGN_PLAYERS};
use crate::prelude::*;
use crate::protocol::config::Outcome;
use content::blueprint::Blueprint;
use content::campaign::{DifficultySliders, KillMethod, MissionType, OperationDef, OperationKind};
use content::part::weapons::MunitionKind;
use content::part::PartFunction;
use content::save::{IntelSpend, TheatreAction};
use sim_core::rng::{self, Key, Stream};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Policy { Aggressive, Careful, SingleMethod(KillMethod), Mixed, AllGhost, NoGhost }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BotConfig { pub policy: Policy, pub seed: u64, pub difficulty: DifficultySliders, pub ironman: bool, pub players: u8, /// Stop after this act's capstone or loss.
    pub until_act: Act, pub max_operations: u32, pub max_theatre_hours: u64 }

/// One operation as the bot played it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BotOperation { pub operation: OperationId, pub hour: u64, pub outcome: Outcome, pub sortie_min: f64, pub income: u32, pub spend: u32, pub alert_after: f64, pub ghosts_met: u8, pub cards_queued: u8 }

/// What a run produced, for the gate scripts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BotReport {
    pub operations: Vec<BotOperation>,
    pub peak_alert: f64,
    pub reached_alert_5: bool,
    pub capstone_done: Vec<Act>,
    pub acts_lost: Vec<Act>,
    pub retreat_completed: bool,
    pub cards_arrived: u16,
    pub cards_delayed: u16,
    pub cards_cancelled: u16,
    pub total_income: u32,
    pub total_spend: u32,
    /// Income to spend ratio at theatre hour 8 x 24 and at the end.
    pub income_spend_ratio: f64,
    pub hulls_lost: u16,
    /// Hours at which no template was offerable (repeat-cap deadlock gate).
    pub no_offer_states: u32,
    pub theatre_hours: u64,
    pub final_state_hash: u64,
    /// Cards that arrived while the pool held the Intel to delay them (doctrine-card gate, L892).
    #[serde(default)]
    pub cards_arrived_affordable: u16,
}

/// Longest run in theatre hours whatever the config says (two years), and most loop steps.
pub const MAX_BOT_HOURS: u64 = 24 * 365 * 2;
pub const MAX_BOT_STEPS: u32 = 200_000;
/// Client ids of bot players.
const BOT_CLIENT: u64 = 0xB07_0000;
/// Spend Intel on a card this close to arrival.
const CARD_SPEND_WINDOW_H: u64 = 48;

/// The setup a policy fights with. Unit: `sys-campaign-bot`.
pub fn player_setup(policy: Policy) -> PlayerSetup {
    let (method, emcon, ghosts) = match policy {
        Policy::Aggressive => (None, Some(Emcon::Active), false),
        Policy::Careful => (None, Some(Emcon::Silent), true),
        Policy::SingleMethod(m) => (Some(m), None, false),
        Policy::Mixed => (None, None, false),
        Policy::AllGhost => (None, Some(Emcon::Silent), true),
        Policy::NoGhost => (None, Some(Emcon::Active), false),
    };
    PlayerSetup { reference_setup: None, method, emcon, ghosts, follow_briefing_route: true }
}

/// Choose the bot's next chart-table action from the offers and state (transit, refit, wait, accept, spend
/// Intel), by policy. Deterministic; draws use `rng::Stream::Theatre` with a bot-specific key.
/// Unit: `sys-campaign-bot`.
pub fn choose_action(theatre: &Theatre, policy: Policy, seed: u64) -> content::save::TheatreAction { decide(theatre, policy, seed, &Memory::default()) }

/// Play a campaign to the configured end and report. Unit: `sys-campaign-bot`.
pub fn run_campaign(config: &BotConfig, data: TheatreData, resolver: &mut dyn SortieResolver) -> BotReport {
    let setup = player_setup(config.policy);
    let n = (config.players.max(1) as usize).min(MAX_CAMPAIGN_PLAYERS);
    let starter = campaign_starter(config.policy, Act::Act1, &data);
    let new = NewCampaign {
        seed: config.seed, difficulty: config.difficulty, ironman: config.ironman, slot_name: "bot".into(),
        players: (0..n).map(|i| (BOT_CLIENT + i as u64, format!("bot {}", i + 1), starter.clone())).collect(), start_act: Act::Act1,
    };
    let mut t = Theatre::new_campaign(new, data);
    let mut run = Run { rep: BotReport::default(), mem: Memory::default(), retreat_acts: BTreeSet::new(), dock_spend: 0 };
    run.absorb(&mut t);
    let hour_cap = config.max_theatre_hours.min(MAX_BOT_HOURS);
    let mut stuck = 0u32;
    for _ in 0..MAX_BOT_STEPS {
        if run.finished(&t, config, hour_cap) { break; }
        let hour = t.state.hour;
        let ops = run.rep.operations.len();
        run.dock(&mut t);
        let action = decide(&t, config.policy, config.seed, &run.mem);
        match action {
            TheatreAction::AcceptOperation(op) => run.play(&mut t, &op, &setup, resolver),
            TheatreAction::Transit(node) => {
                if t.commit(&TheatreAction::Transit(node)).is_ok() { run.pass_time(&mut t, hour_cap); } else { run.mem.no_transit_hour = Some(hour); }
            }
            TheatreAction::SpendIntel(spend) => {
                if let IntelSpend::DelayCard { region, card } | IntelSpend::CancelCard { region, card } = &spend { run.mem.spent.insert((region.clone(), card.clone())); }
                let _ = t.commit(&TheatreAction::SpendIntel(spend));
            }
            TheatreAction::RebuildHull { player } => {
                let before = t.state.resources.salvage;
                if t.commit(&TheatreAction::RebuildHull { player }).is_err() { run.mem.no_rebuild_hour = Some(hour); }
                run.dock_spend = run.dock_spend.saturating_add(before.saturating_sub(t.state.resources.salvage));
            }
            other => { if t.commit(&other).is_ok() { run.pass_time(&mut t, hour_cap); } }
        }
        run.absorb(&mut t);
        // A step that neither moved time nor played an operation nor changed a resource is a no-move state.
        if t.state.hour == hour && run.rep.operations.len() == ops { stuck += 1; } else { stuck = 0; }
        if stuck > 64 { break; }
    }
    run.absorb(&mut t);
    let mut rep = run.rep;
    rep.total_spend = rep.total_spend.saturating_add(run.dock_spend);
    rep.theatre_hours = t.state.hour;
    rep.income_spend_ratio = rep.total_income as f64 / (rep.total_spend.max(1) as f64);
    rep.final_state_hash = state_hash(&t.state);
    rep
}

/// The stock-officer winnability check for one operation (L317): `seeds` runs with stock officers, Helm
/// following the briefing route, on Normal; returns (wins, median sortie minutes, longest sortie minutes).
/// Unit: `sys-campaign-bot`.
///
/// Each seed starts a fresh campaign at `act` (campaign seed from the seed index and the operation) with the
/// operation's starter ([`op_starter`]) and builds the operation with the generator as the chart table would. An operation that
/// does not generate, or a sortie that cannot be built (aborted), is a loss and has no length. The median of
/// an even count is the mean of the two middle lengths. The caller's `setup` is used as given: the L317
/// gate passes `follow_briefing_route: true` (a player hull spawns under a stop order and, without Helm
/// orders, never leaves its spawn).
pub fn winnability(operation: &OperationId, act: Act, seeds: u32, setup: &PlayerSetup, data: &TheatreData, resolver: &mut dyn SortieResolver) -> (u32, f64, f64) {
    let Some(op) = data.campaign.operations.get(operation) else { return (0, 0.0, 0.0) };
    let starter = op_starter(op, act, data);
    let mut wins = 0u32;
    let mut minutes: Vec<f64> = Vec::new();
    for s in 0..seeds {
        let Some(config) = winnability_config(operation, act, s, &starter, data) else { continue };
        let r = resolver.resolve(&config, setup, data);
        if r.outcome == Outcome::Aborted { continue; }
        if success(&r) { wins += 1; }
        let m = resolver.last_duration_min();
        minutes.push(if m.is_finite() { m.max(0.0) } else { 0.0 });
    }
    minutes.sort_by(f64::total_cmp);
    let median = match minutes.len() {
        0 => 0.0,
        n if n % 2 == 1 => minutes[n / 2],
        n => 0.5 * (minutes[n / 2 - 1] + minutes[n / 2]),
    };
    (wins, median, minutes.last().copied().unwrap_or(0.0))
}

/// The sortie of winnability run `index`: a fresh campaign at `act` with the starter, the operation built by
/// the generator as the chart table would. An operation the fresh chart cannot place (the hunter-group
/// survival, which needs the hunter group at the cove) is built by the template generator for its act and
/// seed, with the player's hull swapped for the starter.
pub(crate) fn winnability_config(operation: &OperationId, act: Act, index: u32, starter: &Blueprint, data: &TheatreData) -> Option<crate::protocol::config::SortieConfig> {
    let seed = winnability_seed(index, operation);
    let new = NewCampaign { seed, difficulty: DifficultySliders::NORMAL, ironman: false, slot_name: "winnability".into(), players: vec![(BOT_CLIENT, "bot".into(), starter.clone())], start_act: act };
    let t = Theatre::new_campaign(new, data.clone());
    if let Some(g) = opgen::generate(&t.state, data, operation, &[]) { return Some(g.config); }
    let OperationKind::Template(m) = data.campaign.operations.get(operation)?.kind else { return None };
    let mut c = opgen::generate_standalone(m, act, 1, seed, &DifficultySliders::NORMAL, None, data)?.config;
    if c.operation.as_ref() != Some(operation) { return None; }
    let canon = crate::editor::splash::canonical(starter);
    let h = canon.header.hash;
    for p in c.players.clone() {
        if let Some(v) = p.vessel.and_then(|v| c.vessels.get_mut(v as usize)) { v.blueprint = crate::protocol::config::BlueprintRef::Inline(h); }
    }
    if !c.blueprints.iter().any(|b| b.header.hash == h) { c.blueprints.push(canon); }
    Some(c)
}

/// Campaign seed of winnability run `index` of an operation.
pub(crate) fn winnability_seed(index: u32, operation: &OperationId) -> u64 {
    rng::bits(Key::new(index as u64, 0, Stream::Theatre, logistics::fnv("bot.winnability"), logistics::fnv(operation.as_str())))
}

// ---------------------------------------------------------------------------------------------------------
// Decisions
// ---------------------------------------------------------------------------------------------------------

/// What a run remembers between decisions (not part of the theatre state).
#[derive(Clone, Debug, Default)]
struct Memory {
    /// Cards already delayed or cancelled once.
    spent: BTreeSet<(RegionId, CardId)>,
    /// Operations whose sortie aborted, with the count.
    aborted: BTreeMap<OperationId, u8>,
    /// A transit or rebuild was refused at this hour: do not choose it again until time moves.
    no_transit_hour: Option<u64>,
    no_rebuild_hour: Option<u64>,
    /// (operation, player) pairs the Quartermaster was asked about.
    refit_tried: BTreeSet<(OperationId, u8)>,
    /// The operation a refit was made for: played next while it stays on offer.
    intended: Option<OperationId>,
}
impl Memory {
    fn avoided(&self, op: &OperationId) -> bool { self.aborted.get(op).is_some_and(|n| *n >= 2) }
}

fn decide(t: &Theatre, policy: Policy, seed: u64, mem: &Memory) -> TheatreAction {
    let s = &t.state;
    let d = &t.data;
    let hour = s.hour;
    let fieldable = s.players.iter().any(|p| !p.hull_lost && !p.ready_at_hour.is_some_and(|h| h > hour));
    // 1. The forced operation.
    if let Some(f) = opgen::forced_op(s, d) { if fieldable && s.offered.contains(&f) && !mem.avoided(&f) { return TheatreAction::AcceptOperation(f); } }
    // 2. Rebuild a lost hull.
    if mem.no_rebuild_hour != Some(hour) && !t.header.ironman {
        for (i, p) in s.players.iter().enumerate() {
            if p.hull_lost && p.ready_at_hour.is_none() { return TheatreAction::RebuildHull { player: i as u8 }; }
        }
    }
    // 3. Nothing can sail: wait for the dock (or for time, when no hull is coming back).
    if !fieldable {
        let docking = s.players.iter().any(|p| !p.hull_lost && p.ready_at_hour.is_some_and(|h| h > hour));
        return if docking { TheatreAction::Refit } else { TheatreAction::Wait { hours: 24 } };
    }
    // 4. Intel on doctrine cards.
    if let Some(spend) = intel_spend(t, policy, mem) { return TheatreAction::SpendIntel(spend); }
    // 5. Offers.
    let offers: Vec<&OperationDef> = s.offered.iter().filter(|o| !mem.avoided(o)).filter_map(|o| d.campaign.operations.get(o)).collect();
    if offers.is_empty() { return TheatreAction::Wait { hours: 6 }; }
    let alert_of = |op: &OperationDef| opgen::placement(s, d, &op.id).and_then(|(r, _)| s.regions.get(&r).map(|x| super::alert::alert_floor(x.alert))).unwrap_or(0);
    if policy == Policy::Careful && s.clock_hours_left.is_some_and(|h| h > 7.0 * 24.0) && offers.iter().all(|o| alert_of(o) >= 3 && !matches!(o.kind, OperationKind::Capstone)) {
        return TheatreAction::Wait { hours: 12 };
    }
    let mut best: Option<(f64, &OperationDef)> = None;
    for op in &offers {
        let sc = offer_score(t, op, policy, seed, alert_of(op));
        if best.is_none_or(|(b, o)| sc > b || (sc == b && op.id < o.id)) { best = Some((sc, op)); }
    }
    let Some((_, mut op)) = best else { return TheatreAction::Wait { hours: 6 } };
    if let Some(i) = mem.intended.as_ref().and_then(|i| offers.iter().find(|o| &o.id == i)) { op = i; }
    // 6. Move SEXTANT closer first when that saves LANTERN's time.
    if mem.no_transit_hour != Some(hour) && opgen::forced_op(s, d).is_none() {
        if let Some(c) = better_cove(t, &op.id) { return TheatreAction::Transit(c); }
    }
    TheatreAction::AcceptOperation(op.id.clone())
}

fn combat_template(m: MissionType) -> bool {
    matches!(m, MissionType::ConvoyRaid | MissionType::DepotStrike | MissionType::RadarDecapitation | MissionType::PrizeCapture | MissionType::CuttingOut | MissionType::HunterGroupSurvival)
}

fn offer_score(t: &Theatre, op: &OperationDef, policy: Policy, seed: u64, alert: u8) -> f64 {
    let s = &t.state;
    let d = &t.data;
    let kind = match &op.kind {
        OperationKind::Capstone => 1_000.0,
        OperationKind::Story => 500.0,
        OperationKind::AceDuel(_) => if policy == Policy::Careful { 100.0 } else { 300.0 },
        _ => 0.0,
    };
    let passage = opgen::placement(s, d, &op.id).map_or(0.0, |(_, node)| opgen::lantern_hours(s, d, &node) as f64);
    let clock = progress::pushback_days(op, d) * 24.0 - 2.0 * passage;
    let template = match op.kind { OperationKind::Template(m) => Some(m), _ => None };
    let bias = match policy {
        Policy::Aggressive | Policy::SingleMethod(_) => if template.is_some_and(combat_template) { 48.0 } else { 0.0 },
        Policy::Careful => (if template.is_some_and(|m| !combat_template(m)) { 48.0 } else { 0.0 }) - 24.0 * alert as f64,
        _ => -8.0 * alert as f64,
    };
    let jitter = 12.0 * rng::uniform(Key::new(seed, s.hour, Stream::Theatre, logistics::fnv("bot.offer"), logistics::fnv(op.id.as_str())));
    kind + clock + bias + jitter
}

/// A queued card worth spending on now, by policy.
fn intel_spend(t: &Theatre, policy: Policy, mem: &Memory) -> Option<IntelSpend> {
    let (may_cancel, may_delay) = match policy {
        Policy::Careful | Policy::AllGhost => (true, true),
        Policy::Mixed | Policy::NoGhost => (false, true),
        Policy::Aggressive | Policy::SingleMethod(_) => (false, false),
    };
    if !may_delay && !may_cancel { return None; }
    let s = &t.state;
    let rules = &t.data.campaign.rules;
    let intel = s.resources.intel;
    let mut queued: Vec<(u64, &RegionId, &CardId)> = s.regions.iter().flat_map(|(r, st)| st.queued_cards.iter().map(move |q| (q.arrives_hour, r, &q.card))).collect();
    queued.sort();
    for (arrives, region, card) in queued {
        if arrives.saturating_sub(s.hour) > CARD_SPEND_WINDOW_H || mem.spent.contains(&(region.clone(), card.clone())) { continue; }
        if may_cancel && intel >= rules.intel_cancel_card as u32 { return Some(IntelSpend::CancelCard { region: region.clone(), card: card.clone() }); }
        if may_delay && intel >= rules.intel_delay_card as u32 { return Some(IntelSpend::DelayCard { region: region.clone(), card: card.clone() }); }
    }
    None
}

/// An open cove from which LANTERN's round trip to the operation, plus SEXTANT's transit there, is at least
/// 6 hours shorter than from SEXTANT's cove now.
fn better_cove(t: &Theatre, op: &OperationId) -> Option<TheatreNodeId> {
    let s = &t.state;
    let d = &t.data;
    if s.sextant_refit_until_hour.is_some_and(|h| h > s.hour) { return None; }
    let (_, node) = opgen::placement(s, d, op)?;
    let now = 2 * opgen::lantern_hours(s, d, &node) as u64;
    let mut best: Option<(u64, TheatreNodeId)> = None;
    for c in &s.open_coves {
        if *c == s.sextant_node { continue; }
        let Some(transit) = logistics::transit_hours(s, d, &s.sextant_node, c, false) else { continue };
        let mut probe = s.clone();
        probe.sextant_node = c.clone();
        let cost = transit as u64 + 2 * opgen::lantern_hours(&probe, d, &node) as u64;
        if cost + 6 <= now && best.as_ref().is_none_or(|(b, n)| cost < *b || (cost == *b && c < n)) { best = Some((cost, c.clone())); }
    }
    best.map(|b| b.1)
}

// ---------------------------------------------------------------------------------------------------------
// Starters
// ---------------------------------------------------------------------------------------------------------

fn act_starters(act: Act, data: &TheatreData) -> Vec<(String, Blueprint)> {
    let mut acts: Vec<&content::campaign::ActDef> = data.campaign.acts.iter().filter(|a| a.act <= act && !a.starters.is_empty()).collect();
    acts.sort_by_key(|a| core::cmp::Reverse(a.act));
    let paths: Vec<String> = acts.first().map(|a| a.starters.clone()).or_else(|| data.campaign.acts.iter().find(|a| !a.starters.is_empty()).map(|a| a.starters.clone())).unwrap_or_default();
    paths.into_iter().filter_map(|p| content::campaign::load::stock_blueprint(&p).ok().map(|b| (p, b))).collect()
}

fn fallback_hull() -> Blueprint { content::blueprint::io::new_blueprint("bot", 15.0) }

/// The design passes the operation's briefing tags on the analytic screen.
fn fits(bp: &Blueprint, op: &OperationDef, act: Act, data: &TheatreData) -> bool {
    if op.tags.is_empty() { return true; }
    let brief = crate::editor::fitcheck::FitBrief { tags: op.tags.clone(), weather: WeatherState::default(), approach_km: (op.approach_m / 1000.0).max(0.0), threats: Vec::new(), cap_t: act.hull_cap_t() };
    crate::editor::fitcheck::fit_screen_with(bp, &brief, &data.catalogue, &data.campaign).passes()
}

/// The starter the gate sails when its table names none: the fast attack craft, the first starter a player
/// meets (L151).
pub(crate) const GATE_STARTER_DEFAULT: &str = "starters/fac22.bp.ron";

/// The gate's starter per operation, where it is not the default. READING of L317 "its starter design"
/// (coordinator's ruling, 22 November 2026): one starter per operation, chosen from the act's starters and
/// refitted the way the Quartermaster would for the operation's tags; another than the default only where a
/// measurement shows it is the better fit. The numbers behind each entry are in the comment beside it.
pub(crate) const GATE_STARTERS: &[(&str, &str)] = &[];

/// The operation's starter design (L317 "its starter design"): the gate table's starter for it
/// ([`GATE_STARTERS`], else [`GATE_STARTER_DEFAULT`]) when it passes the operation's briefing tags, bare or
/// refitted by the Quartermaster the way the dock would, from the parts open when the operation can first be
/// offered ([`op_open_parts`]); when it cannot (the fast attack craft is over the cross-section cap of the
/// stealth briefings, and no refit shrinks a hull), the first other starter of the act, in data order, that
/// can, by the same two steps; else the first starter. An act whose starters do not include the table's
/// choice (Acts 2 to 4) goes by data order alone.
pub(crate) fn op_starter(op: &OperationDef, act: Act, data: &TheatreData) -> Blueprint {
    let list = act_starters(act, data);
    let preferred = GATE_STARTERS.iter().find(|(id, _)| *id == op.id.as_str()).map_or(GATE_STARTER_DEFAULT, |x| x.1);
    let mut order: Vec<&Blueprint> = list.iter().filter(|(p, _)| p == preferred).map(|(_, b)| b).collect();
    order.extend(list.iter().filter(|(p, _)| p != preferred).map(|(_, b)| b));
    order.into_iter().find_map(|b| fitted(b, op, act, data)).unwrap_or_else(|| list.first().map(|(_, b)| b.clone()).unwrap_or_else(fallback_hull))
}

/// A design made to pass the operation's briefing tags: itself when it does, else the Quartermaster's refit
/// of it from the parts open when the operation can first be offered ([`op_open_parts`]), when that passes.
pub(crate) fn fitted(bp: &Blueprint, op: &OperationDef, act: Act, data: &TheatreData) -> Option<Blueprint> {
    if fits(bp, op, act, data) { return Some(bp.clone()); }
    let available = op_open_parts(op, act, data);
    let brief = crate::editor::fitcheck::FitBrief { tags: op.tags.clone(), weather: WeatherState::default(), approach_km: (op.approach_m / 1000.0).max(0.0), threats: Vec::new(), cap_t: act.hull_cap_t() };
    let ruleset = content::ruleset::Ruleset::campaign(act);
    let (cat, camp) = (data.catalogue.clone(), data.campaign.clone());
    let mut deps = move || crate::sim::SimDeps { catalogue: cat.clone(), campaign: camp.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
    match crate::editor::quartermaster::quartermaster(bp, &brief, &BTreeMap::new(), Some(&available), &ruleset, &mut deps, &mut crate::editor::NoDeadline) {
        crate::editor::quartermaster::QmResult::Refit { blueprint, .. } if fits(&blueprint, op, act, data) => Some(*blueprint),
        _ => None,
    }
}

/// The tier of parts open when `op` can first be offered: the start tiers of the acts up to `act`, raised by
/// the tier unlocks of the operations its unlock condition requires (`All` takes the highest of its parts,
/// `Any` the lowest, since either branch may open the operation).
pub(crate) fn op_open_tier(op: &OperationDef, act: Act, data: &TheatreData) -> content::enums::Tier {
    use content::campaign::Unlock;
    use content::enums::Tier;
    fn needs(u: &Unlock, data: &TheatreData) -> Tier {
        match u {
            Unlock::Completed(ops) => ops.iter().flat_map(|o| data.campaign.acts.iter().flat_map(move |a| a.tier_unlocks.iter().filter(move |(x, _)| x == o).map(|(_, t)| *t))).max().unwrap_or(Tier::T1),
            Unlock::All(v) => v.iter().map(|x| needs(x, data)).max().unwrap_or(Tier::T1),
            Unlock::Any(v) => v.iter().map(|x| needs(x, data)).min().unwrap_or(Tier::T1),
            _ => Tier::T1,
        }
    }
    let start = data.campaign.acts.iter().filter(|a| a.act <= act).map(|a| a.start_tier).max().unwrap_or(Tier::T1);
    start.max(needs(&op.unlock, data))
}

/// Campaign parts open at [`op_open_tier`] (the economy's fieldable rule without a theatre state).
pub(crate) fn op_open_parts(op: &OperationDef, act: Act, data: &TheatreData) -> Vec<PartId> {
    let tier = op_open_tier(op, act, data);
    let wanted: Vec<PartId> = data.catalogue.parts.values().filter(|p| p.tier <= tier).map(|p| p.id.clone()).collect();
    data.catalogue.available_parts(GameMode::Campaign, act, tier, &wanted).into_iter().filter_map(|i| data.catalogue.parts.get_index(i.index()).map(|(id, _)| id.clone())).collect()
}

/// The design carries a weapon that kills by `m`.
fn carries(bp: &Blueprint, m: KillMethod, cat: &Catalogue) -> bool {
    bp.parts.iter().filter_map(|p| cat.part(&p.part)).any(|def| match &def.function {
        PartFunction::Gun(_) => m == KillMethod::Gunfire,
        PartFunction::Launcher(l) => matches!((cat.munition(&l.munition).map(|x| x.kind), m), (Some(MunitionKind::AntiShipMissile), KillMethod::Missile) | (Some(MunitionKind::Torpedo), KillMethod::Torpedo) | (Some(MunitionKind::DepthCharge), KillMethod::DepthCharge)),
        PartFunction::MineRail { .. } => m == KillMethod::Mine,
        _ => false,
    })
}

fn campaign_starter(policy: Policy, act: Act, data: &TheatreData) -> Blueprint {
    let list = act_starters(act, data);
    let ops: Vec<&OperationDef> = data.campaign.operations.values().filter(|o| o.act == act && o.kind != OperationKind::Tutorial).collect();
    let method = match policy { Policy::SingleMethod(m) => Some(m), _ => None };
    let mut best: Option<(bool, usize, usize)> = None;
    for (i, (_, bp)) in list.iter().enumerate() {
        let armed = method.is_none_or(|m| carries(bp, m, &data.catalogue));
        let fitting = ops.iter().filter(|o| fits(bp, o, act, data)).count();
        if best.is_none_or(|(a, f, _)| (armed, fitting) > (a, f)) { best = Some((armed, fitting, i)); }
    }
    best.map(|b| list[b.2].1.clone()).unwrap_or_else(fallback_hull)
}

// ---------------------------------------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------------------------------------

struct Run {
    rep: BotReport,
    mem: Memory,
    /// Acts started in the retreat configuration.
    retreat_acts: BTreeSet<Act>,
    /// Salvage spent at the dock since the last operation.
    dock_spend: u32,
}

impl Run {
    fn finished(&self, t: &Theatre, config: &BotConfig, hour_cap: u64) -> bool {
        let r = &self.rep;
        r.capstone_done.contains(&config.until_act) || r.acts_lost.contains(&config.until_act) || t.state.act > config.until_act || t.state.open_theatre
            || saves::campaign_over(t) || r.operations.len() as u64 >= config.max_operations as u64 || t.state.hour >= hour_cap
    }

    fn all(t: &Theatre) -> PlayerMask { (0..t.state.players.len()).fold(PlayerMask(0), |m, i| m.with(PlayerSlot(i as u8))) }

    /// Repairs and rearming between operations; the Salvage they take is spend.
    fn dock(&mut self, t: &mut Theatre) {
        if t.active().is_some() { return; }
        let all = Run::all(t);
        let before = t.state.resources.salvage;
        for i in 0..t.state.players.len() {
            let slot = PlayerSlot(i as u8);
            let hour = t.state.hour;
            let rec = &t.state.players[i];
            if rec.hull_lost || rec.ready_at_hour.is_some_and(|h| h > hour) { continue; }
            let cost = economy::repair_cost(&t.state, slot, &t.data);
            if cost > 0 && t.state.resources.salvage > 0 {
                let full = cost <= t.state.resources.salvage;
                let _ = t.command(slot, true, all, TheatreCommand::Repair { full });
            }
            let rec = &t.state.players[i];
            let caps = economy::ammo_capacities(&rec.blueprint, &t.data);
            let short = rec.fuel_fraction < 0.999 || caps.iter().any(|(s, c)| rec.ammo.get(s).is_some_and(|n| n < c));
            if short { let _ = t.command(slot, true, all, TheatreCommand::Rearm); }
        }
        self.dock_spend = self.dock_spend.saturating_add(before.saturating_sub(t.state.resources.salvage));
    }

    /// Run a committed time action hour by hour until a decision is due.
    fn pass_time(&mut self, t: &mut Theatre, hour_cap: u64) {
        loop {
            let before = t.state.hour;
            let stop = t.advance(1);
            self.absorb(t);
            if t.state.hour != before { self.hour_checks(t); }
            if t.state.hour == before || t.state.hour >= hour_cap || stop != Interrupt::Paused { break; }
        }
    }

    fn hour_checks(&mut self, t: &Theatre) {
        let s = &t.state;
        let template = s.offered.iter().any(|o| t.data.campaign.operations.get(o).is_some_and(|d| matches!(d.kind, OperationKind::Template(_))));
        if !template { self.rep.no_offer_states = self.rep.no_offer_states.saturating_add(1); }
    }

    /// Fold the theatre's queued events into the report.
    fn absorb(&mut self, t: &mut Theatre) {
        let delay_cost = t.data.campaign.rules.intel_delay_card as u32;
        let intel = t.state.resources.intel;
        for e in t.drain_events() {
            match e {
                TheatreEvent::CardArrived { .. } => {
                    self.rep.cards_arrived = self.rep.cards_arrived.saturating_add(1);
                    if intel >= delay_cost { self.rep.cards_arrived_affordable = self.rep.cards_arrived_affordable.saturating_add(1); }
                }
                TheatreEvent::CardDelayed { .. } => self.rep.cards_delayed = self.rep.cards_delayed.saturating_add(1),
                TheatreEvent::CardCancelled { .. } => self.rep.cards_cancelled = self.rep.cards_cancelled.saturating_add(1),
                TheatreEvent::ActStarted { act, retreat } => { if retreat { self.retreat_acts.insert(act); } else { self.retreat_acts.remove(&act); } }
                TheatreEvent::ActWon { act } => {
                    if !self.rep.capstone_done.contains(&act) { self.rep.capstone_done.push(act); }
                    if self.retreat_acts.contains(&act) { self.rep.retreat_completed = true; }
                }
                TheatreEvent::ActLost { act } if !self.rep.acts_lost.contains(&act) => self.rep.acts_lost.push(act),
                _ => {}
            }
        }
        for r in t.state.regions.values() {
            if r.alert.is_finite() && r.alert > self.rep.peak_alert { self.rep.peak_alert = r.alert; }
        }
        if self.rep.peak_alert >= 5.0 { self.rep.reached_alert_5 = true; }
    }

    /// Accept, sortie, resolve and apply one operation.
    fn play(&mut self, t: &mut Theatre, op: &OperationId, setup: &PlayerSetup, resolver: &mut dyn SortieResolver) {
        if self.refit_for(t, op) { return; }
        self.mem.intended = None;
        if t.commit(&TheatreAction::AcceptOperation(op.clone())).is_err() { self.note_abort(op); return; }
        let hour = t.state.hour;
        let config = match t.start_sortie(op, &[]) { Ok(c) => c, Err(_) => { self.note_abort(op); return; } };
        let region = t.active().map(|a| a.region.clone()).or_else(|| config.region.clone()).unwrap_or_default();
        let result = resolver.resolve(&config, setup, t.data());
        let minutes = resolver.last_duration_min();
        if result.outcome == Outcome::Aborted {
            t.apply_result(&result);
            self.note_abort(op);
            return;
        }
        let aa = t.apply_result(&result);
        self.absorb(t);
        self.hour_checks(t);
        let income = aa.payout.salvage_wrecks.saturating_add(aa.payout.salvage_prizes);
        self.rep.total_income = self.rep.total_income.saturating_add(income);
        self.rep.total_spend = self.rep.total_spend.saturating_add(self.dock_spend);
        self.rep.hulls_lost = self.rep.hulls_lost.saturating_add(aa.hulls_lost.len().min(u16::MAX as usize) as u16);
        self.rep.operations.push(BotOperation {
            operation: op.clone(), hour, outcome: result.outcome, sortie_min: if minutes.is_finite() { minutes.max(0.0) } else { 0.0 }, income, spend: self.dock_spend,
            alert_after: t.state.regions.get(&region).map_or(0.0, |r| r.alert), ghosts_met: aa.ghosts.iter().filter(|g| g.1).count().min(u8::MAX as usize) as u8,
            cards_queued: aa.queued_cards.len().min(u8::MAX as usize) as u8,
        });
        self.dock_spend = 0;
    }

    /// Refit, through the Quartermaster, every hull that fails the operation's briefing tags, once per hull
    /// and operation and only when the pool pays for it. True when a refit started: the operation is then
    /// played once the dock is done.
    fn refit_for(&mut self, t: &mut Theatre, op: &OperationId) -> bool {
        let Some(def) = t.data.campaign.operations.get(op).cloned() else { return false };
        let act = if t.state.open_theatre { Act::Act4 } else { t.state.act };
        let Some(brief) = t.briefing(op) else { return false };
        let fit = crate::editor::fitcheck::FitBrief {
            tags: brief.tags.clone(), weather: brief.weather, approach_km: if brief.distance_km.is_finite() { brief.distance_km.max(0.0) } else { 0.0 },
            threats: brief.threats.iter().filter_map(|x| x.class.clone()).collect(), cap_t: act.hull_cap_t(),
        };
        let all = Run::all(t);
        let mut started = false;
        for i in 0..t.state.players.len() {
            let hour = t.state.hour;
            let rec = &t.state.players[i];
            if rec.hull_lost || rec.ready_at_hour.is_some_and(|h| h > hour) || fits(&rec.blueprint, &def, act, &t.data) { continue; }
            if !self.mem.refit_tried.insert((op.clone(), i as u8)) { continue; }
            let available = economy::fieldable_list(&t.state, &t.data);
            let ruleset = content::ruleset::Ruleset::campaign(act);
            let (cat, camp) = (t.data.catalogue.clone(), t.data.campaign.clone());
            let mut deps = move || crate::sim::SimDeps { catalogue: cat.clone(), campaign: camp.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
            let qm = crate::editor::quartermaster::quartermaster(&rec.blueprint, &fit, &t.state.resources.parts_store, Some(&available), &ruleset, &mut deps, &mut crate::editor::NoDeadline);
            if let crate::editor::quartermaster::QmResult::Refit { blueprint, cost, .. } = qm {
                if cost.salvage > t.state.resources.salvage { continue; }
                let before = t.state.resources.salvage;
                if t.command(PlayerSlot(i as u8), true, all, TheatreCommand::Refit { blueprint }).is_ok() { started = true; }
                self.dock_spend = self.dock_spend.saturating_add(before.saturating_sub(t.state.resources.salvage));
            }
        }
        if started { self.mem.intended = Some(op.clone()); }
        started
    }

    fn note_abort(&mut self, op: &OperationId) {
        let n = self.mem.aborted.entry(op.clone()).or_insert(0);
        *n = n.saturating_add(1);
    }
}

fn state_hash(state: &content::save::TheatreState) -> u64 {
    let bytes = bincode::serialize(state).unwrap_or_default();
    let h = content::blueprint::io::blake3_hash(&bytes);
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.0[..8]);
    u64::from_le_bytes(b)
}

#[cfg(test)]
mod tests;
