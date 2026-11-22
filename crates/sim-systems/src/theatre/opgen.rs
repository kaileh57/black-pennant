//! Unit `sys-theatre-opgen` (hard: the commitment gate runs on every content change and the generator must
//! be deterministic). Operation offers, briefings and sortie generation. DESIGN.md L92-102 (enemy
//! commitment), L85-90 (approach-distance fallback), L778-782 (templates, composition, repeat cap), L832-837
//! (co-op scaling), L743 (weather at the commit hour), L890 (repeat-cap gate).
//!
//! Eleven parametric templates are filled from theatre state. An operation is composed of five parts:
//! objective, environment, task-force makeup, active doctrine and a complication. Repeat cap: each template
//! is offered at most 3 times per act and never twice in a row; story operations, ace duels, forced operations
//! and the capstone are exempt. Commitment: every operation commits at least 5x the player-side combat hull
//! count and at least 4x the player-side tonnage, where player-side tonnage = the act's hull cap x the number
//! of player hulls (not actual displacement); "committed" = hostile hulls present at the start plus scheduled
//! reinforcements; at most 4 hulls over 90 m. If the region's order of battle cannot meet a multiple the
//! generator adds a reinforcement wave from the nearest forward base; if still short, CI fails the template.
//! Co-op scaling: enemy blueprint variants (heavier armour tiers, extra escorts, better loadouts) plus one
//! extra task group per added player; no hidden hit-point multiplier. If the host reached ladder rung 2 or
//! more last operation, the added task groups are replaced by quality upgrades.
//! Draws use `rng::Stream::Generator` keyed (theatre seed, hour, operation hash, slot).
//!
//! # Offers (decisions where the spec is silent)
//! - The chart table holds the unlocked story operations, ace duels and capstone of the act, plus a slate of
//!   [`TEMPLATE_SLATE`] template operations. A template counts as offered once, when it enters the slate;
//!   it stays until played or declined (offers never lapse: `Briefing::expires_hour` is `None`). When a slot
//!   frees, the next template is the least-offered eligible one (ties by a seeded draw), excluding templates
//!   on the slate and `OpsLedger::last_offered`, which the theatre sets to the template just played or
//!   declined, so the same template is never offered twice in a row. A forced operation clears the slate
//!   and gives back the offer counts of the templates it removed.
//! - If no template is eligible and the slate is empty, the deadlock fallback of L890 applies at once: the
//!   cap rises by 1 for templates whose prerequisites the state meets; past that (a player who declines
//!   everything) the least-offered template is offered, and only then does the last-offered rule yield. The
//!   list is never left without a template while one could run.
//! - A template's prerequisites: its unlock condition, and a node of the right kind in an open region (depot
//!   strike: a depot; radar decapitation: a radar or ESM site; convoy raid, prize capture and minelaying: a
//!   sea node on a lane with traffic; agent extraction: a harbour, forward base or sea node; defector escort:
//!   sea or harbour; feint: sea, forward base or radar site; photo reconnaissance: a harbour, base, shipyard,
//!   depot or radar site; cutting out: a harbour, forward base or shipyard). Surviving a hunter group is
//!   offered only while a hunter group is at sea, at its node, and is forced once it reaches SEXTANT's cove.
//! - Placement is drawn from the candidate nodes keyed by (theatre seed, operation, times offered), so an
//!   offer keeps its node while it stands. Authored operations use their node.
//! - Open Theatre: template operations of the last act that has any; the per-act counts reset at every
//!   whole multiple of `open_theatre_repeat_window_days` theatre hours (absolute hours, since the ledger
//!   keeps no timestamps).
//!
//! # Generation (decisions where the spec is silent)
//! - Committed hulls are Combine hulls of combat roles whose declared stance is not Neutral, present at the
//!   start or arriving later. Freighters, tankers, tenders, civilians, decoys and sites do not count. Their
//!   tonnage is the displacement of the class blueprint (`content::stats`), memoised per blueprint path.
//! - Reinforcement waves: the act's task-group or hunter-killer fleet templates (seeded pick), else
//!   `task_group_hulls` hulls of the act's combat classes, arriving from [`WAVE_FIRST_S`] at
//!   [`WAVE_STEP_S`] intervals at the enemy spawn farthest from the players, ordered to guard the objective.
//!   At most [`MAX_WAVES`] waves. If the multiples are still short the numbers are reported honestly and
//!   [`commitment_ok`] fails.
//! - Campaign hulls come from persistent records: alive, not refitting, not in a convoy or the hunter group,
//!   under 40% damage; the same class first, then the same role and tier; ranked by region (the operation's
//!   first), then Combine transit hours from the operation node (the nearest forward base), then id. A unit
//!   no record can fill sails as an unrecorded rear-echelon hull (its kills count; nothing writes back).
//!   Record damage is a fraction the sortie cannot express per cell, so damaged records are avoided rather
//!   than carried in; fuel, ammunition and veterancy carry.
//! - Complication: one drawn from the operation's list (none when the list is empty). An ace complication
//!   with a beaten ace is skipped. Alert 3 adds an escort fleet, a decoy convoy on convoy raids and prize
//!   captures, and an air patrol where the act has one. Each active card adds its fleet; `use_variants` and
//!   co-op swap hulls to their heavier variants.
//! - Co-op at rung 2 or more last operation: no added task groups; every Combine hull gains +0.1 veterancy
//!   (capped at 0.15) and the heavier variant. The multiples still bind at the full player count (L94,
//!   L102), so any shortfall arrives as reinforcement waves rather than at the start.
//! - Missing markers are synthesised deterministically (player spawn west, enemy spawn east, objective
//!   markers near the centre, exits near the player side); synthesised markers are added to the sortie map.
//!   A map with an unusable size becomes a 40 km open-water square.
//! - The players spawn on the line from the objective point toward the map's player spawn marker, at the
//!   operation's approach distance times [`approach_scale`], clamped inside the map. Campaign sorties have no
//!   time limit (the objective or a withdrawal ends them). The objective point is the objective's own marker,
//!   else the map's first objective-kind marker; a cutting-out measures from the marker its prize lies at.
//! - Exit legs (22 November 2026, the L317 and L81 gate): an extraction, an escort, a cutting-out and a feint
//!   end at an exit, and none of them can finish there before its hold, rendezvous or boarding. Their exits
//!   lie at most [`EXIT_LEG_FRACTION`] of the approach distance from the objective point, on a straight leg
//!   with at least [`EXIT_LEG_DEPTH_M`] of water (the exit turns about the objective point in 15 degree steps
//!   until the leg is clear). Other objectives keep their exits at the approach distance or where the node
//!   map puts them. An extraction or a feint on a map with no exit marker gets the default one.
//! - Convoy start (same date): a group in transit, and the escort that sails with it, starts on the lane so
//!   that it passes the point nearest the objective as the players arrive there:
//!   `approach x convoy speed / player speed` short of that point, clamped to the lane. Convoy speed is the
//!   group AI's ([`CONVOY_SPEED_FRACTION`] of its slowest hull's top speed), player speed the slowest fielded
//!   hull's Helm cruise, so the start depends on the design the player brings.
//! - Emission state at the start: a player hull sails in at the operation's authored recommendation only. A
//!   recommendation the briefing infers from ghost conditions is shown and not applied.
//! - A destroy-group objective's hulls are named by the briefing (Exact at first contact, L475) and declared
//!   Hostile (L498), so Gunnery engages them without a designation.
//! - No two hulls start inside each other: a group whose formation would put a hull within [`SPAWN_CLEAR_M`]
//!   of a hull already placed (players, tender, site, earlier groups) slides sideways across its heading in
//!   [`GROUP_SHIFT_M`] steps, alternating right and left, until every member is clear. Without this two
//!   convoys transiting from the same entry marker spawned on top of each other and wrecked at tick 1.
//! - Weather: the region's weather comes in 6-hour blocks; each block keeps the previous block's kind with
//!   probability 1/2 (looking back at most 3 blocks), else draws from the region's mix. Sea state is drawn
//!   inside the kind's range per block; wind direction interpolates between blocks; the sun comes from the
//!   campaign sun table at the hour. An operation with authored sea states draws one of them and moves the
//!   weather kind to one that allows it.
use super::{Briefing, Confidence, OperationOffer, TheatreData, ThreatEntry};
use crate::prelude::*;
use crate::protocol::config::{BlueprintRef, BodyInit, Faction, GroupInit, GroupOrders, ModeRules, PlayerInit, SideDef, SortieConfig, SortieIntel, VesselInit, VesselRole};
use content::campaign::{ClassRole, Complication, DifficultySliders, FleetKind, Formation, GhostKind, MapDef, Marker, MarkerKind, MissionType, NodeKind, Objective, OperationDef, OperationKind, MAX_MAP_M};
use content::save::{HullDamage, TheatreState};
use content::weather::{SeaState, WeatherKind};
use sim_core::rng::{self, Key, Stream};
use std::sync::{Mutex, OnceLock};

/// Template operations on the chart table at a time.
pub const TEMPLATE_SLATE: usize = 3;
/// First reinforcement wave arrival and the gap between waves, in sortie seconds.
pub const WAVE_FIRST_S: u32 = 600;
pub const WAVE_STEP_S: u32 = 300;
/// Most reinforcement waves the commitment fallback adds.
pub const MAX_WAVES: usize = 6;
/// Stock blueprint path of SEXTANT, fielded when a hunter group attacks the cove.
pub const SEXTANT_BLUEPRINT: &str = "lantern/sextant.bp.ron";
const SPACING_M: f64 = 500.0;
/// DECISION: least spacing between any two hulls at spawn, and the sideways step a crowded group slides.
const SPAWN_CLEAR_M: f64 = 350.0;
const GROUP_SHIFT_M: f64 = 1_000.0;
const MARGIN_M: f64 = 1_000.0;
const DEFAULT_MAP_M: f64 = 40_000.0;
const WEATHER_BLOCK_H: u64 = 6;
const RECORD_MAX_DAMAGE: f64 = 0.4;
/// DECISION (22 November 2026): exits of an extraction, escort, cutting-out or feint lie at most this share of
/// the approach distance from the objective point, on a leg with at least this much water.
pub const EXIT_LEG_FRACTION: f64 = 0.5;
pub const EXIT_LEG_DEPTH_M: f64 = 3.0;
/// Samples of an exit leg checked for depth, and the exit's turn about the objective point per try.
const EXIT_LEG_SAMPLES: u32 = 200;
const EXIT_TURN_DEG: f64 = 15.0;
const EXIT_TURN_STEPS: u32 = 6;
/// A convoy's transit speed as a share of its slowest hull's top speed (the group AI's rule, `ai::group`),
/// and the Helm's default cruise share (`officers::helm`).
pub const CONVOY_SPEED_FRACTION: f64 = 0.75;
const HELM_CRUISE_FRACTION: f64 = 0.7;
/// Convoy speed over player speed when either cannot be measured, and the bounds of a measured ratio.
const CONVOY_LEAD_RATIO: f64 = 0.75;
const CONVOY_LEAD_RATIO_BOUNDS: (f64, f64) = (0.3, 1.2);

// Draw purposes (the `a` word of generator keys).
const P_SLOT: u64 = 1;
const P_CLASS: u64 = 2;
const P_COMPLICATION: u64 = 3;
const P_VARIANT: u64 = 4;
const P_WAVE: u64 = 5;
const P_SEA: u64 = 6;
const P_PLACE: u64 = 7;
const P_ORDER: u64 = 8;
const P_SEED: u64 = 9;
const P_HOUR: u64 = 10;
const P_TEMPLATE: u64 = 11;

/// Recompute the operations on offer: unlocked story operations, ace duels and the capstone, a forced
/// operation if one is pending (then nothing else), and templates whose prerequisites the state meets, under
/// the repeat cap. Never leaves the list empty while any template is offerable (gate at L890).
/// Unit: `sys-theatre-opgen`.
pub fn refresh_offers(state: &mut TheatreState, data: &TheatreData) {
    let c = &data.campaign;
    if state.open_theatre {
        let window = c.rules.open_theatre_repeat_window_days as u64 * 24;
        if window > 0 && state.hour > 0 && state.hour % window == 0 { state.ops.offered.clear(); }
    }
    if let Some(forced) = forced_op(state, data) {
        // Templates the forced operation pushes off the slate were never played or declined: their offer is
        // returned, so they do not use the repeat cap twice when they come back.
        let templates = template_ops(state, data);
        for id in core::mem::take(&mut state.offered) {
            if id == forced || !templates.contains(&id) { continue; }
            if let Some(n) = state.ops.offered.get_mut(&id) { *n = n.saturating_sub(1); }
        }
        state.ops.offered.retain(|_, n| *n > 0);
        state.offered = vec![forced];
        return;
    }
    // Drop what is no longer valid, keep the rest in order, no duplicates.
    let mut kept: Vec<OperationId> = Vec::new();
    for id in core::mem::take(&mut state.offered) {
        if !kept.contains(&id) && still_valid(state, data, &id) { kept.push(id); }
    }
    state.offered = kept;
    // Authored operations of the act.
    if !state.open_theatre {
        if let Some(act) = c.acts.iter().find(|a| a.act == state.act) {
            let mut authored: Vec<&OperationId> = act.story_ops.iter().chain(act.ace_duels.iter()).collect();
            authored.push(&act.capstone);
            for id in authored {
                if !state.offered.contains(id) && authored_offerable(state, data, id) { state.offered.push(id.clone()); }
            }
        }
    }
    // Template slate.
    let templates = template_ops(state, data);
    let on_slate = |s: &TheatreState| s.offered.iter().filter(|o| templates.contains(o)).count();
    let cap = c.rules.template_repeat_cap;
    while on_slate(state) < TEMPLATE_SLATE {
        let mut pick = pick_template(state, data, &templates, Some(cap), true);
        if pick.is_none() && on_slate(state) == 0 {
            pick = pick_template(state, data, &templates, Some(cap.saturating_add(1)), true)
                .or_else(|| pick_template(state, data, &templates, None, true))
                .or_else(|| pick_template(state, data, &templates, None, false));
        }
        let Some(id) = pick else { break };
        *state.ops.offered.entry(id.clone()).or_insert(0) += 1;
        state.ops.last_offered = Some(id.clone());
        state.offered.push(id);
    }
}

/// Offers with display facts for the chart table. Unit: `sys-theatre-opgen`.
pub fn offers(state: &TheatreState, data: &TheatreData) -> Vec<OperationOffer> {
    let forced = forced_op(state, data);
    state.offered.iter().filter_map(|id| {
        let op = data.campaign.operations.get(id)?;
        let (region, node) = placement(state, data, id)?;
        let template = match op.kind { OperationKind::Template(t) => Some(t), _ => None };
        Some(OperationOffer { operation: id.clone(), kind: op.kind.clone(), template, name: op.name.clone(), node: node.clone(), region, forced: op.forced || forced.as_ref() == Some(id), transit_hours: lantern_hours(state, data, &node) })
    }).collect()
}

/// A generated operation: the sortie and its briefing agree by construction.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated { pub config: SortieConfig, pub briefing: Briefing, pub committed_hulls: u16, pub committed_tonnage_t: f64, /// Persistent hull records committed, so they leave the theatre graph for the sortie's duration.
    pub hull_records: Vec<u32> }

/// Generate an offered operation for the given players at the current theatre hour: place it (authored node
/// or chosen from theatre state), draw the force from the region's hull records (variants, active cards,
/// alert-3 additions), meet the commitment multiples, pick weather and time of day for the hour, scale the
/// approach distance, and build config and briefing. Deterministic for (state, operation, players).
/// Unit: `sys-theatre-opgen`.
///
/// Players that do not exist are ignored; players whose hull is lost join as spectators. With no usable
/// player listed, every player with a hull is fielded (the briefing's canonical party).
pub fn generate(state: &TheatreState, data: &TheatreData, operation: &OperationId, players: &[PlayerSlot]) -> Option<Generated> {
    let op = data.campaign.operations.get(operation)?;
    let (region_id, node_id) = placement(state, data, operation)?;
    let node = data.campaign.theatre.nodes.iter().find(|n| n.id == node_id);
    let region = data.campaign.theatre.regions.iter().find(|r| r.id == region_id);
    let mut slots: Vec<PlayerSlot> = players.iter().copied().filter(|p| (p.0 as usize) < state.players.len()).collect();
    slots.sort();
    slots.dedup();
    if !slots.iter().any(|p| !state.players[p.0 as usize].hull_lost) {
        slots = (0..state.players.len().min(8)).map(|i| PlayerSlot(i as u8)).filter(|p| !state.players[p.0 as usize].hull_lost).collect();
        if slots.is_empty() && !state.players.is_empty() { slots.push(PlayerSlot(0)); }
    }
    let seats: Vec<Seat> = slots.iter().map(|p| {
        let rec = &state.players[p.0 as usize];
        Seat { slot: *p, client_id: rec.client_id, name: rec.name.clone(), hull: (!rec.hull_lost).then(|| SeatHull::Campaign(Box::new(rec.clone()))) }
    }).collect();
    let seed = sortie_seed(state.seed, state.hour, operation);
    let weather = op_weather(op, region_weather(state.seed, &region_id, state.hour, data), seed);
    let act = if state.open_theatre { Act::Act4 } else { state.act };
    let alert = state.regions.get(&region_id).map_or(0, |r| super::alert::alert_floor(r.alert));
    let cards = state.regions.get(&region_id).map(|r| r.active_cards.clone()).unwrap_or_default();
    let map = op.map.clone().or_else(|| node.map(|n| n.map.clone())).unwrap_or_default();
    let spec = Spec {
        op, act, seed, hour: state.hour, weather, map, region, region_id: Some(region_id), node: Some(node_id), seats, difficulty: state.difficulty.clamped(), ironman: false,
        enemy_per_player: None, alert, cards, rung: state.last_host_rung, state: Some(state),
    };
    Some(build(&spec, data))
}

/// The briefing alone (same draws as [`generate`]). Unit: `sys-theatre-opgen`.
pub fn briefing(state: &TheatreState, data: &TheatreData, operation: &OperationId) -> Option<Briefing> {
    if !state.offered.contains(operation) { return None; }
    generate(state, data, operation, &[]).map(|g| g.briefing)
}

/// Generate a template operation with no theatre behind it: skirmish, the commitment gate ("200 seeded
/// instances of every template x every act x every player count") and trials. `enemy_per_player` of `None`
/// applies the campaign commitment rule. Unit: `sys-theatre-opgen`.
pub fn generate_standalone(template: MissionType, act: Act, players: u8, seed: u64, difficulty: &DifficultySliders, enemy_per_player: Option<u8>, data: &TheatreData) -> Option<Generated> {
    let c = &data.campaign;
    let mut ops: Vec<&OperationDef> = c.operations.values().filter(|o| o.kind == OperationKind::Template(template)).collect();
    if ops.is_empty() { return None; }
    ops.sort_by(|a, b| a.id.cmp(&b.id));
    let best_act = ops.iter().filter(|o| o.act <= act).map(|o| o.act).max().unwrap_or_else(|| ops.iter().map(|o| o.act).min().unwrap_or(act));
    let same: Vec<&OperationDef> = ops.into_iter().filter(|o| o.act == best_act).collect();
    let r = Rng { seed };
    let op = same[r.below(P_TEMPLATE, 0, same.len())];
    let node = if op.map.is_some() { None } else {
        let mut cands: Vec<&content::campaign::NodeDef> = c.theatre.nodes.iter().filter(|n| fits(template, &n.kind, has_traffic(data, &n.id))).collect();
        cands.sort_by(|a, b| a.id.cmp(&b.id));
        op.node.as_ref().and_then(|id| c.theatre.nodes.iter().find(|n| &n.id == id)).or_else(|| (!cands.is_empty()).then(|| cands[r.below(P_PLACE, 0, cands.len())]))
    };
    let region = node.and_then(|n| c.theatre.regions.iter().find(|r| r.id == n.region));
    let hour = r.below(P_HOUR, 0, 24) as u64;
    let region_id = region.map(|r| r.id.clone());
    let base = match &region_id { Some(id) => region_weather(seed, id, hour, data), None => region_weather(seed, &RegionId::new(""), hour, data) };
    let weather = op_weather(op, base, seed);
    let n = players.clamp(1, 8);
    let starter = starter_path(data, act);
    let seats = (0..n).map(|i| Seat { slot: PlayerSlot(i), client_id: 0, name: String::new(), hull: Some(SeatHull::Stock(starter.clone())) }).collect();
    let map = op.map.clone().or_else(|| node.map(|n| n.map.clone())).unwrap_or_default();
    let spec = Spec {
        op, act, seed, hour, weather, map, region, region_id, node: node.map(|n| n.id.clone()), seats, difficulty: difficulty.clamped(), ironman: false,
        enemy_per_player: enemy_per_player.map(|e| e.max(1)), alert: 0, cards: Vec::new(), rung: 0, state: None,
    };
    Some(build(&spec, data))
}

/// Commitment minimums: (hulls, tonnage) = (5 x players, tonnage multiple x act cap x players) (L94).
/// Real code: spec formula.
pub fn commitment_minimums(act: Act, players: u8, rules: &content::campaign::CampaignRules) -> (u16, f64) { (rules.hull_multiple as u16 * players as u16, rules.tonnage_multiple * act.hull_cap_t() * players as f64) }

/// Check a generated operation against both multiples and the capital limit. Unit: `sys-theatre-opgen`.
///
/// The count is redone from the sortie config itself, so a config edited after generation is judged as it
/// will run.
pub fn commitment_ok(generated: &Generated, act: Act, players: u8, data: &TheatreData) -> bool {
    let (min_h, min_t) = commitment_minimums(act, players.max(1), &data.campaign.rules);
    let (hulls, tonnage, capitals) = count_committed(&generated.config, data);
    hulls >= min_h as usize && tonnage + 1e-6 >= min_t && capitals <= data.campaign.rules.max_capitals as usize
}

/// Approach-distance scale in force (1.0 by default; the length-gate fallback moves it in 25% steps within
/// 0.5..=1.5 of each template's authored value, L85-90). Data in `campaign_rules.ron` once the gate has run;
/// until then 1.0. Unit: `sys-theatre-opgen`.
pub fn approach_scale(op: &OperationDef, data: &TheatreData) -> f64 {
    match data.campaign.rules.approach_scale.get(&op.id) { Some(s) if s.is_finite() => s.clamp(0.5, 1.5), _ => 1.0 }
}

/// Seeded hourly weather of a region (L743): the same call gives the briefing forecast and the sortie's
/// weather. Unit: `sys-theatre-opgen`.
pub fn region_weather(seed: u64, region: &RegionId, hour: u64, data: &TheatreData) -> WeatherState {
    let rh = super::logistics::fnv(region.as_str());
    let mix: Vec<(WeatherKind, f64)> = data.campaign.theatre.regions.iter().find(|r| &r.id == region)
        .map(|r| r.weather.iter().copied().filter(|(_, w)| w.is_finite() && *w > 0.0).collect()).unwrap_or_default();
    let mix = if mix.is_empty() { vec![(WeatherKind::Clear, 3.0), (WeatherKind::Overcast, 2.0), (WeatherKind::Haze, 1.0), (WeatherKind::Rain, 1.0)] } else { mix };
    let block = hour / WEATHER_BLOCK_H;
    let key = |b: u64, purpose: u64| Key::new(seed, b, Stream::Weather, rh, purpose);
    let kind = block_kind(&mix, block, 0, &key);
    let (lo, hi) = kind.sea_state_range();
    let sea = SeaState(lo + rng::below(key(block, 2), (hi - lo + 1) as u32) as u8);
    // Wind direction drifts smoothly from one block's draw to the next.
    let d0 = rng::uniform(key(block, 3)) * core::f64::consts::TAU;
    let d1 = rng::uniform(key(block + 1, 3)) * core::f64::consts::TAU;
    let t = (hour % WEATHER_BLOCK_H) as f64 / WEATHER_BLOCK_H as f64;
    let mut delta = d1 - d0;
    if delta > core::f64::consts::PI { delta -= core::f64::consts::TAU; }
    if delta < -core::f64::consts::PI { delta += core::f64::consts::TAU; }
    let mut dir = d0 + delta * t;
    if dir < 0.0 { dir += core::f64::consts::TAU; }
    if dir >= core::f64::consts::TAU { dir -= core::f64::consts::TAU; }
    let gust = 0.85 + 0.3 * rng::uniform(key(block, 4));
    let mut w = weather_of(kind, sea, dir, data, hour);
    w.wind_speed_mps *= gust;
    w.cloud_cover = (w.cloud_cover + (rng::uniform(key(block, 5)) - 0.5) * 0.2).clamp(0.0, 1.0);
    w
}

// ---------------------------------------------------------------------------------------------------------
// Offers.

/// Where an operation takes place now: (region, node). Authored operations use their node; templates draw
/// from candidate nodes keyed by the times offered, so the offer keeps its node.
pub(crate) fn placement(state: &TheatreState, data: &TheatreData, operation: &OperationId) -> Option<(RegionId, TheatreNodeId)> {
    let c = &data.campaign;
    let op = c.operations.get(operation)?;
    let node_def = |id: &TheatreNodeId| c.theatre.nodes.iter().find(|n| &n.id == id);
    if let Some(id) = &op.node { return node_def(id).map(|n| (n.region.clone(), n.id.clone())); }
    let t = match op.kind {
        OperationKind::Template(t) => t,
        OperationKind::AceDuel(ref ace) => {
            if let Some(n) = state.hulls.iter().find(|h| h.alive && h.ace.as_ref() == Some(ace)).and_then(|h| node_def(&h.node)) { return Some((n.region.clone(), n.id.clone())); }
            MissionType::Feint
        }
        _ => MissionType::Feint,
    };
    if t == MissionType::HunterGroupSurvival {
        if super::alert::hunter_at_cove(state) { return node_def(&state.sextant_node).map(|n| (n.region.clone(), n.id.clone())); }
        let g = state.hunter_group.and_then(|id| state.groups.iter().find(|g| g.id == id))?;
        return node_def(&g.node).map(|n| (n.region.clone(), n.id.clone()));
    }
    let mut cands: Vec<&content::campaign::NodeDef> = c.theatre.nodes.iter()
        .filter(|n| super::logistics::region_open(state, data, &n.region) && fits(t, &n.kind, has_traffic(data, &n.id))).collect();
    if cands.is_empty() { return None; }
    cands.sort_by(|a, b| a.id.cmp(&b.id));
    let count = state.ops.offered.get(operation).copied().unwrap_or(0) as u64;
    let i = rng::below(Key::new(state.seed, count, Stream::Generator, op_hash(operation), P_PLACE), cands.len() as u32) as usize;
    let n = cands[i.min(cands.len() - 1)];
    Some((n.region.clone(), n.id.clone()))
}

fn fits(t: MissionType, kind: &NodeKind, traffic: bool) -> bool {
    use MissionType::*;
    match t {
        ConvoyRaid | PrizeCapture | Minelaying => matches!(kind, NodeKind::Sea) && traffic,
        DepotStrike => matches!(kind, NodeKind::Depot { .. }),
        RadarDecapitation => matches!(kind, NodeKind::RadarSite | NodeKind::EsmSite),
        AgentExtraction => matches!(kind, NodeKind::Harbour | NodeKind::ForwardBase | NodeKind::Sea),
        DefectorEscort => matches!(kind, NodeKind::Sea | NodeKind::Harbour),
        Feint => matches!(kind, NodeKind::Sea | NodeKind::ForwardBase | NodeKind::RadarSite),
        PhotoReconnaissance => matches!(kind, NodeKind::Harbour | NodeKind::ForwardBase | NodeKind::Shipyard { .. } | NodeKind::Depot { .. } | NodeKind::RadarSite),
        CuttingOut => matches!(kind, NodeKind::Harbour | NodeKind::ForwardBase | NodeKind::Shipyard { .. }),
        HunterGroupSurvival => false,
    }
}

fn has_traffic(data: &TheatreData, node: &TheatreNodeId) -> bool {
    data.campaign.theatre.lanes.iter().any(|l| (&l.a == node || &l.b == node) && l.traffic.is_finite() && l.traffic > 0.0)
}

/// The operation that must be played now, if any: the hunter-group survival operation while the hunter
/// group sits at SEXTANT's cove, else an authored forced operation whose unlock is met.
pub(crate) fn forced_op(state: &TheatreState, data: &TheatreData) -> Option<OperationId> {
    let c = &data.campaign;
    if super::alert::hunter_at_cove(state) {
        let mut hunts: Vec<&OperationDef> = c.operations.values().filter(|o| o.kind == OperationKind::Template(MissionType::HunterGroupSurvival)).collect();
        hunts.sort_by(|a, b| (a.act != state.act, a.act, &a.id).cmp(&(b.act != state.act, b.act, &b.id)));
        if let Some(o) = hunts.first() { return Some(o.id.clone()); }
    }
    c.operations.values().find(|o| o.forced && o.act == state.act && !state.open_theatre && !state.ops.completed.contains(&o.id) && super::progress::unlocked_in(state, &o.unlock, data) && placement(state, data, &o.id).is_some()).map(|o| o.id.clone())
}

/// Template operations in scope now: the act's, else the last act's that has any (Open Theatre: the last).
fn template_ops(state: &TheatreState, data: &TheatreData) -> Vec<OperationId> {
    let ops = &data.campaign.operations;
    let is_t = |o: &&OperationDef| matches!(o.kind, OperationKind::Template(_));
    let act = if state.open_theatre { Act::Act4 } else { state.act };
    let scope = ops.values().filter(is_t).filter(|o| o.act <= act).map(|o| o.act).max();
    let mut v: Vec<OperationId> = match scope { Some(a) => ops.values().filter(is_t).filter(|o| o.act == a).map(|o| o.id.clone()).collect(), None => Vec::new() };
    v.sort();
    v
}

fn still_valid(state: &TheatreState, data: &TheatreData, id: &OperationId) -> bool {
    let Some(op) = data.campaign.operations.get(id) else { return false };
    match &op.kind {
        OperationKind::Template(_) => template_ops(state, data).contains(id) && super::progress::unlocked_in(state, &op.unlock, data) && placement(state, data, id).is_some(),
        OperationKind::Tutorial => false,
        // Authored operations stay once offered until done or the act changes.
        _ => !state.open_theatre && op.act == state.act && !state.ops.completed.contains(id) && !op.forced && ace_free(state, op) && placement(state, data, id).is_some(),
    }
}

fn ace_free(state: &TheatreState, op: &OperationDef) -> bool {
    match &op.kind { OperationKind::AceDuel(a) => !state.ops.aces_beaten.contains(a), _ => true }
}

fn authored_offerable(state: &TheatreState, data: &TheatreData, id: &OperationId) -> bool {
    let Some(op) = data.campaign.operations.get(id) else { return false };
    if op.forced || state.ops.completed.contains(id) || !ace_free(state, op) || matches!(op.kind, OperationKind::Template(_) | OperationKind::Tutorial) { return false; }
    if op.kind == OperationKind::Capstone {
        let Some(act) = data.campaign.acts.iter().find(|a| a.act == state.act) else { return false };
        if !act.story_ops.iter().all(|s| state.ops.completed.contains(s)) { return false; }
    }
    super::progress::unlocked_in(state, &op.unlock, data) && placement(state, data, id).is_some()
}

fn pick_template(state: &TheatreState, data: &TheatreData, templates: &[OperationId], cap: Option<u8>, respect_last: bool) -> Option<OperationId> {
    let mut best: Option<(u8, u64, OperationId)> = None;
    for id in templates {
        if state.offered.contains(id) { continue; }
        if respect_last && state.ops.last_offered.as_ref() == Some(id) { continue; }
        let count = state.ops.offered.get(id).copied().unwrap_or(0);
        if cap.is_some_and(|c| count >= c) { continue; }
        let Some(op) = data.campaign.operations.get(id) else { continue };
        if !super::progress::unlocked_in(state, &op.unlock, data) || placement(state, data, id).is_none() { continue; }
        let tie = rng::bits(Key::new(state.seed, state.hour, Stream::Generator, op_hash(id), P_ORDER));
        if best.as_ref().is_none_or(|(c, t, _)| (count, tie) < (*c, *t)) { best = Some((count, tie, id.clone())); }
    }
    best.map(|b| b.2)
}

/// LANTERN's one-way passage from SEXTANT to a node in theatre hours (SEXTANT's lanes and speed; open water
/// where no lane route exists).
pub(crate) fn lantern_hours(state: &TheatreState, data: &TheatreData, node: &TheatreNodeId) -> u32 {
    if let Some(h) = super::logistics::transit_hours(state, data, &state.sextant_node, node, false) { return h; }
    let net = super::logistics::Net::new(data);
    match (net.idx(&state.sextant_node), net.idx(node)) {
        (Some(a), Some(b)) => super::logistics::hours_for(net.direct_km(a, b), super::logistics::SEXTANT_KMH).unwrap_or(0),
        _ => 0,
    }
}

fn op_hash(id: &OperationId) -> u64 { super::logistics::fnv(id.as_str()) }

fn sortie_seed(seed: u64, hour: u64, op: &OperationId) -> u64 { rng::bits(Key::new(seed, hour, Stream::Generator, op_hash(op), P_SEED)) }

// ---------------------------------------------------------------------------------------------------------
// Class facts.

/// Displacement and build cost of a stock blueprint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClassFacts { pub tonnage_t: f64, pub cost: u32 }

fn facts_cache() -> &'static Mutex<BTreeMap<String, Option<ClassFacts>>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<ClassFacts>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Facts of the stock blueprint at `path`, loaded and measured once per process. `None` when it does not
/// load.
pub(crate) fn blueprint_facts(path: &str, data: &TheatreData) -> Option<ClassFacts> {
    let mut cache = facts_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = cache.get(path) { return *f; }
    let f = content::campaign::load::stock_blueprint(path).ok().map(|bp| {
        let s = content::stats::compute_stats(&bp, &data.catalogue);
        ClassFacts { tonnage_t: if s.tonnage_t.is_finite() { s.tonnage_t.max(0.0) } else { 0.0 }, cost: s.cost }
    });
    cache.insert(path.to_string(), f);
    f
}

/// Facts of an enemy class.
pub(crate) fn class_facts(data: &TheatreData, class: &ClassId) -> Option<ClassFacts> {
    let def = data.campaign.classes.get(class)?;
    blueprint_facts(&def.blueprint, data)
}

/// Test seam: record the facts of a blueprint path without loading it.
#[cfg(test)]
pub(crate) fn set_blueprint_facts(path: &str, tonnage_t: f64, cost: u32) {
    facts_cache().lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_string(), Some(ClassFacts { tonnage_t, cost }));
}

/// Top speed and Helm cruise speed of a design, in knots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DesignSpeeds { pub top_kn: f64, pub cruise_kn: f64 }

fn speeds_cache() -> &'static Mutex<BTreeMap<String, Option<DesignSpeeds>>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<DesignSpeeds>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Speeds of a blueprint from the hydro prediction the sim's own turn model uses (`ship::design`): top speed,
/// and the cruise the Helm orders (the design's `helm.cruise` share, else the default).
fn measure_speeds(bp: &content::blueprint::Blueprint, data: &TheatreData) -> Option<DesignSpeeds> {
    let prefab = sim_core::vessel::build_prefab(bp, &data.catalogue);
    let (top, _) = sim_core::hydro::predict_speed_turn(&prefab, &data.catalogue, &data.catalogue.tuning.buoyancy);
    let top_kn = top / sim_core::time::KN;
    if !(top_kn.is_finite() && top_kn > 0.5) { return None; }
    let share = bp.officers.sliders.get(crate::officers::helm::SLIDER_CRUISE).copied().filter(|f| f.is_finite() && *f > 0.0 && *f <= 1.0).unwrap_or(HELM_CRUISE_FRACTION);
    Some(DesignSpeeds { top_kn, cruise_kn: top_kn * share })
}

/// Speeds of the stock blueprint at `path`, measured once per process.
pub(crate) fn stock_speeds(path: &str, data: &TheatreData) -> Option<DesignSpeeds> {
    if let Some(f) = speeds_cache().lock().unwrap_or_else(|e| e.into_inner()).get(path) { return *f; }
    let f = content::campaign::load::stock_blueprint(path).ok().and_then(|bp| measure_speeds(&bp, data));
    speeds_cache().lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_string(), f);
    f
}

/// Most player designs whose speeds are kept (a session's edits of one hull stay far under it).
const INLINE_SPEEDS_MAX: usize = 256;

/// Speeds of a player's own design, measured once per design per process: keyed by its content hash, which
/// covers the hull, the parts and the officer settings the cruise share comes from.
fn inline_speeds(bp: &content::blueprint::Blueprint, data: &TheatreData) -> Option<DesignSpeeds> {
    let key = format!("inline:{:?}", content::blueprint::io::compute_hash(bp));
    if let Some(f) = speeds_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) { return *f; }
    let f = measure_speeds(bp, data);
    let mut cache = speeds_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.keys().filter(|k| k.starts_with("inline:")).count() >= INLINE_SPEEDS_MAX { cache.retain(|k, _| !k.starts_with("inline:")); }
    cache.insert(key, f);
    f
}

fn combat_role(role: ClassRole) -> bool { !matches!(role, ClassRole::Freighter | ClassRole::Tanker | ClassRole::Civilian | ClassRole::Tender) }

/// (hulls, tonnage, capitals) a config commits: Combine hulls of combat roles, not declared Neutral, not sites.
fn count_committed(config: &SortieConfig, data: &TheatreData) -> (usize, f64, usize) {
    let combine: Vec<SideId> = config.sides.iter().filter(|s| s.faction == Faction::Combine).map(|s| s.id).collect();
    let (mut hulls, mut tonnage, mut capitals) = (0usize, 0.0, 0usize);
    for v in &config.vessels {
        if !combine.contains(&v.side) || v.declared == Some(Stance::Neutral) || matches!(v.role, VesselRole::Site | VesselRole::Tender | VesselRole::Target | VesselRole::PlayerHull) { continue; }
        let Some(def) = v.class.as_ref().and_then(|c| data.campaign.classes.get(c)) else { continue };
        if !combat_role(def.role) { continue; }
        hulls += 1;
        if def.capital { capitals += 1; }
        if let BlueprintRef::Stock(p) = &v.blueprint { tonnage += blueprint_facts(p, data).map_or(0.0, |f| f.tonnage_t); }
    }
    (hulls, tonnage, capitals)
}

// ---------------------------------------------------------------------------------------------------------
// Weather.

fn block_kind(mix: &[(WeatherKind, f64)], block: u64, depth: u32, key: &dyn Fn(u64, u64) -> Key) -> WeatherKind {
    if depth < 3 && block > 0 && rng::chance(key(block, 0), 0.5) { return block_kind(mix, block - 1, depth + 1, key); }
    let total: f64 = mix.iter().map(|(_, w)| *w).sum();
    let mut x = rng::uniform(key(block, 1)) * total;
    for (k, w) in mix {
        if x < *w { return *k; }
        x -= *w;
    }
    mix.last().map_or(WeatherKind::Clear, |m| m.0)
}

/// Full weather state for a kind and sea state at a theatre hour (sun from the campaign table).
fn weather_of(kind: WeatherKind, sea: SeaState, wind_dir_rad: f64, data: &TheatreData, hour: u64) -> WeatherState {
    const WIND: [f64; 9] = [0.5, 2.0, 4.5, 7.0, 9.5, 12.5, 16.0, 20.0, 26.0];
    let (lo, hi) = kind.sea_state_range();
    let sea = SeaState(sea.0.clamp(lo, hi));
    let (cloud, rain) = match kind { WeatherKind::Clear => (0.2, 0.0), WeatherKind::Haze => (0.4, 0.0), WeatherKind::Overcast => (0.9, 0.0), WeatherKind::Rain => (1.0, 0.6), WeatherKind::Storm => (1.0, 1.0), WeatherKind::FogBank => (0.6, 0.0) };
    let sun = &data.campaign.theatre.sun;
    let h = (hour % 24) as usize;
    let elev = sun.elevation_deg[h];
    let azim = sun.azimuth_deg[h];
    WeatherState {
        kind, sea_state: sea, wind_dir_rad: if wind_dir_rad.is_finite() { wind_dir_rad } else { 0.0 }, wind_speed_mps: WIND[sea.0.min(8) as usize], cloud_cover: cloud, visibility_m: kind.visibility_m(), rain,
        sun_elevation_deg: if elev.is_finite() { elev.clamp(-90.0, 90.0) } else { 0.0 }, sun_azimuth_deg: if azim.is_finite() { azim.rem_euclid(360.0) } else { 0.0 },
    }
}

/// Apply an operation's authored sea states: draw one and move the weather kind to one that allows it.
fn op_weather(op: &OperationDef, base: WeatherState, seed: u64) -> WeatherState {
    if op.sea_states.is_empty() { return base; }
    let sea = op.sea_states[rng::below(Key::new(seed, 0, Stream::Generator, P_SEA, 0), op.sea_states.len() as u32) as usize];
    let sea = SeaState(sea.0.min(8));
    let fits = |k: WeatherKind| { let (lo, hi) = k.sea_state_range(); sea.0 >= lo && sea.0 <= hi };
    let kind = if fits(base.kind) { base.kind } else {
        [WeatherKind::Overcast, WeatherKind::Clear, WeatherKind::Haze, WeatherKind::Rain, WeatherKind::Storm].into_iter().find(|k| fits(*k)).unwrap_or(base.kind)
    };
    let mut w = base;
    if kind != base.kind {
        let (cloud, rain) = match kind { WeatherKind::Clear => (0.2, 0.0), WeatherKind::Haze => (0.4, 0.0), WeatherKind::Overcast => (0.9, 0.0), WeatherKind::Rain => (1.0, 0.6), WeatherKind::Storm => (1.0, 1.0), WeatherKind::FogBank => (0.6, 0.0) };
        w.kind = kind;
        w.cloud_cover = cloud;
        w.rain = rain;
        w.visibility_m = kind.visibility_m();
    }
    const WIND: [f64; 9] = [0.5, 2.0, 4.5, 7.0, 9.5, 12.5, 16.0, 20.0, 26.0];
    w.sea_state = sea;
    w.wind_speed_mps = WIND[sea.0 as usize];
    w
}

// ---------------------------------------------------------------------------------------------------------
// The builder shared by campaign and standalone generation.

#[derive(Clone, Copy)]
struct Rng { seed: u64 }
impl Rng {
    fn key(&self, purpose: u64, i: u64) -> Key { Key::new(self.seed, 0, Stream::Generator, purpose, i) }
    fn u(&self, purpose: u64, i: u64) -> f64 { rng::uniform(self.key(purpose, i)) }
    fn below(&self, purpose: u64, i: u64, n: usize) -> usize { if n <= 1 { 0 } else { (rng::below(self.key(purpose, i), n.min(u32::MAX as usize) as u32) as usize).min(n - 1) } }
}

enum SeatHull { Campaign(Box<content::save::PlayerRecord>), Stock(Option<String>) }
struct Seat { slot: PlayerSlot, client_id: u64, name: String, hull: Option<SeatHull> }

struct Spec<'a> {
    op: &'a OperationDef,
    act: Act,
    seed: u64,
    hour: u64,
    weather: WeatherState,
    map: MapDef,
    region: Option<&'a content::campaign::RegionDef>,
    region_id: Option<RegionId>,
    node: Option<TheatreNodeId>,
    seats: Vec<Seat>,
    difficulty: DifficultySliders,
    ironman: bool,
    enemy_per_player: Option<u8>,
    alert: u8,
    cards: Vec<CardId>,
    rung: u8,
    state: Option<&'a TheatreState>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side { Combine, Civilian, Protected }

#[derive(Clone, Debug)]
struct Unit {
    class: ClassId,
    side: Side,
    stance: Stance,
    group: usize,
    tag: String,
    role: VesselRole,
    record: Option<u32>,
    ace: Option<AceId>,
    veterancy: f64,
    fuel: f64,
    ammo: f64,
    confidence: Confidence,
}

/// The fleet a doctrine card adds, in the operation's act. Cards apply across the theatre but name Act 1
/// fleets, so an Act 4 "point defence thickens" card added 350 t of Act 1 hulls against an 1,800 t player.
/// The card's fleet maps to the act's fleet with the same name ("fleet.act1.patrol.heavy" to
/// "fleet.act4.patrol.heavy"), else to the act's plainest fleet of the same kind (no ace or tutorial fleet),
/// else stays as named. Design call, 19 November 2026 (DESIGN.md is silent on card fleets in later acts).
fn card_fleet_for_act(data: &TheatreData, fleet: &FleetTemplateId, act: Act) -> FleetTemplateId {
    let fleets = &data.campaign.fleets;
    let Some(def) = fleets.get(fleet) else { return fleet.clone() };
    if def.act == act { return fleet.clone(); }
    let suffix = fleet.as_str().splitn(3, '.').nth(2).unwrap_or("");
    let named = FleetTemplateId::new(format!("fleet.{}.{suffix}", act_token(act)));
    if fleets.get(&named).is_some_and(|f| f.act == act && f.kind == def.kind) { return named; }
    fleets.iter()
        .filter(|(id, f)| f.act == act && f.kind == def.kind && !id.as_str().contains(".ace.") && !id.as_str().contains("tutorial"))
        .min_by(|(a, _), (b, _)| a.as_str().len().cmp(&b.as_str().len()).then_with(|| a.cmp(b)))
        .map_or_else(|| fleet.clone(), |(id, _)| id.clone())
}

fn act_token(act: Act) -> &'static str { match act { Act::Act1 => "act1", Act::Act2 => "act2", Act::Act3 => "act3", Act::Act4 => "act4" } }

#[derive(Clone, Debug)]
struct Group { kind: FleetKind, formation: Formation, marker: String, arrives_s: u32, tag: String, emcon: Option<Emcon>, ace: Option<AceId>, orders: Option<GroupOrders>, side: Side, theatre_group: Option<u32>, bodies: Vec<(MunitionId, u8)> }

struct Build<'a, 'd> {
    s: &'a Spec<'a>,
    data: &'d TheatreData,
    r: Rng,
    map: MapDef,
    units: Vec<Unit>,
    groups: Vec<Group>,
    draws: u64,
    variants: bool,
}

/// The objective names hulls by this tag (its targets, its prize, its escorted hull).
fn objective_names(o: &Objective, tag: &str) -> bool {
    match o {
        Objective::DestroyGroup { tag: t, .. } | Objective::Capture { tag: t } | Objective::CutOut { tag: t, .. } | Objective::Photograph { tag: t, .. } | Objective::Escort { tag: t, .. } => !t.is_empty() && t == tag,
        _ => false,
    }
}

fn build(s: &Spec, data: &TheatreData) -> Generated {
    let mut b = Build { s, data, r: Rng { seed: s.seed }, map: sanitize_map(&s.map), units: Vec::new(), groups: Vec::new(), draws: 0, variants: false };
    let objective_name = objective_marker_name(&s.op.objective);
    b.ensure_markers(objective_name.as_deref());

    let players_fielded = s.seats.iter().filter(|p| p.hull.is_some()).count().max(1) as u8;
    let coop = players_fielded > 1;
    let degraded = coop && s.rung >= 2;
    b.variants = coop || s.cards.iter().any(|c| data.campaign.cards.get(c).is_some_and(|d| d.effect.use_variants));

    // 1. Hunter group (campaign): its records are the threat.
    let hunter_op = s.op.kind == OperationKind::Template(MissionType::HunterGroupSurvival);
    if hunter_op { b.add_hunter_group(); }
    // 2. The operation's force.
    for (i, f) in s.op.force.iter().enumerate() {
        let side = if objective_tag_is_protected(&s.op.objective, &f.tag) { Side::Protected } else { Side::Combine };
        b.add_fleet(&f.fleet, &f.marker, &f.tag, f.stance, f.arrives_s, f.emcon, side, Confidence::Probable, 1000 + i as u64);
    }
    // 3. Ace duel: the ace's ship leads.
    if let OperationKind::AceDuel(ace) = &s.op.kind { b.ensure_ace(ace, 0); }
    // 4. One complication.
    b.add_complication();
    // 5. Alert 3: escorts, decoy convoys and air patrols (campaign only).
    if s.alert >= data.campaign.rules.alert_escalation.min(5) && s.state.is_some() { b.add_alert_forces(); }
    // 6. Active doctrine cards.
    for (i, c) in s.cards.iter().enumerate() {
        if let Some(f) = data.campaign.cards.get(c).and_then(|d| d.effect.add_fleet.clone()) {
            let f = card_fleet_for_act(data, &f, s.act);
            let m = b.primary_marker();
            b.add_fleet(&f, &m, "", Stance::Hostile, 0, None, Side::Combine, Confidence::Possible, 2000 + i as u64);
        }
    }
    // 7. Co-op: one task group per added player, or quality upgrades.
    if coop && !degraded && s.enemy_per_player.is_none() {
        for k in 1..players_fielded as u64 {
            let fleet = s.op.per_extra_player.clone().or_else(|| b.wave_fleet(k));
            let m = b.enemy_spawn(k as usize);
            match fleet {
                Some(f) => { b.add_fleet(&f, &m, "", Stance::Hostile, 0, None, Side::Combine, Confidence::Probable, 3000 + k); }
                None => { b.add_synth_group(&m, 0, Confidence::Probable, 3000 + k); }
            }
        }
    }
    // 8. Commitment (or the skirmish size). Records bind before every count: a record can change a hull's
    // class within its role and tier, and so its tonnage.
    let (min_h, min_t) = commitment_minimums(s.act, players_fielded, &data.campaign.rules);
    b.enforce_capitals();
    match s.enemy_per_player {
        None => {
            let mut wave = 0usize;
            while wave < MAX_WAVES {
                b.assign_records();
                let (h, t) = b.committed();
                if h >= min_h as usize && t + 1e-6 >= min_t { break; }
                let before = b.committed().0;
                b.add_wave(wave);
                b.enforce_capitals();
                wave += 1;
                if b.committed().0 == before { break; }
            }
        }
        Some(per) => b.fit_skirmish(per as usize * players_fielded as usize),
    }
    b.ensure_objective_targets();
    // The L317 winnability fallback: an Act 1 operation its stock officers could not win drops its last-added
    // hostile hulls, one per step, written to `CampaignRules::hostile_drop` (never a hull the objective names,
    // never an ace).
    let drop = data.campaign.rules.hostile_drop.get(&s.op.id).copied().unwrap_or(0);
    for _ in 0..drop {
        let named = |u: &Unit| u.role != VesselRole::Combatant || u.ace.is_some() || objective_names(&s.op.objective, &u.tag);
        let Some(k) = b.units.iter().rposition(|u| u.side == Side::Combine && !named(u)) else { break };
        b.units.remove(k);
    }
    if degraded {
        for u in b.units.iter_mut().filter(|u| u.side == Side::Combine) { u.veterancy = (u.veterancy + 0.1).min(0.15); }
    }
    // 9. Persistent hull records (campaign).
    let records = b.assign_records();
    b.finish(records)
}

impl<'a, 'd> Build<'a, 'd> {
    fn next(&mut self) -> u64 { self.draws += 1; self.draws }

    fn class_ok(&self, c: &ClassId) -> bool { self.data.campaign.classes.contains_key(c) }

    fn effective_class(&mut self, base: &ClassId) -> ClassId {
        if !self.variants { return base.clone(); }
        let data: &'d TheatreData = self.data;
        let Some(def) = data.campaign.classes.get(base) else { return base.clone() };
        let vs: Vec<&ClassId> = def.variants.iter().filter(|v| data.campaign.classes.contains_key(*v)).collect();
        if vs.is_empty() { return base.clone(); }
        let i = self.next();
        vs[self.r.below(P_VARIANT, i, vs.len())].clone()
    }

    fn new_group(&mut self, kind: FleetKind, formation: Formation, marker: &str, arrives_s: u32, tag: &str, emcon: Option<Emcon>, side: Side) -> usize {
        self.groups.push(Group { kind, formation, marker: marker.to_string(), arrives_s, tag: tag.to_string(), emcon, ace: None, orders: None, side, theatre_group: None, bodies: Vec::new() });
        self.groups.len() - 1
    }

    fn push_unit(&mut self, class: ClassId, group: usize, stance: Stance, side: Side, confidence: Confidence) {
        let role_of = self.data.campaign.classes.get(&class).map(|d| d.role);
        let side = if role_of == Some(ClassRole::Civilian) && side == Side::Combine { Side::Civilian } else { side };
        let tag = self.groups[group].tag.clone();
        let role = match self.groups[group].kind { FleetKind::Convoy | FleetKind::DecoyConvoy => VesselRole::Convoy, _ => if matches!(role_of, Some(ClassRole::Freighter | ClassRole::Tanker)) { VesselRole::Convoy } else { VesselRole::Combatant } };
        self.units.push(Unit { class, side, stance, group, tag, role, record: None, ace: None, veterancy: 0.0, fuel: 1.0, ammo: 1.0, confidence });
    }

    /// Draw a fleet template's slots into a new group at `marker`.
    #[allow(clippy::too_many_arguments)]
    fn add_fleet(&mut self, fleet: &FleetTemplateId, marker: &str, tag: &str, stance: Stance, arrives_s: u32, emcon: Option<Emcon>, side: Side, confidence: Confidence, key: u64) -> Option<usize> {
        let f = self.data.campaign.fleets.get(fleet)?.clone();
        let stance = if f.kind == FleetKind::DecoyConvoy { Stance::Neutral } else { stance };
        let tag = if tag.is_empty() { default_tag(f.kind) } else { tag.to_string() };
        let g = self.new_group(f.kind, f.formation, marker, arrives_s, &tag, emcon, side);
        self.groups[g].bodies = f.bodies.clone();
        for (si, slot) in f.slots.iter().enumerate() {
            let classes: Vec<&ClassId> = slot.classes.iter().filter(|c| self.class_ok(c)).collect();
            if classes.is_empty() { continue; }
            let (lo, hi) = (slot.min.min(slot.max) as usize, slot.min.max(slot.max) as usize);
            let n = lo + self.r.below(P_SLOT, key * 64 + si as u64, hi - lo + 1);
            for k in 0..n.min(64) {
                let c = classes[self.r.below(P_CLASS, (key * 64 + si as u64) * 64 + k as u64, classes.len())].clone();
                let c = self.effective_class(&c);
                self.push_unit(c, g, stance, side, confidence);
            }
        }
        Some(g)
    }

    /// A group of `task_group_hulls` hulls of the act's combat classes (when no fleet template fits).
    fn add_synth_group(&mut self, marker: &str, arrives_s: u32, confidence: Confidence, key: u64) -> Option<usize> {
        let pool = self.combat_pool();
        if pool.is_empty() { return None; }
        let g = self.new_group(FleetKind::TaskGroup, Formation::LineAbreast, marker, arrives_s, "task_group", None, Side::Combine);
        let n = (self.data.campaign.rules.task_group_hulls as usize).max(1);
        for k in 0..n {
            let c = pool[self.r.below(P_CLASS, key * 64 + k as u64, pool.len())].clone();
            let c = self.effective_class(&c);
            self.push_unit(c, g, Stance::Hostile, Side::Combine, confidence);
        }
        Some(g)
    }

    /// Combat classes the region fields (its order of battle), else the act's, else any earlier act's.
    fn combat_pool(&self) -> Vec<ClassId> {
        let c = &self.data.campaign;
        let ok = |id: &ClassId| c.classes.get(id).is_some_and(|d| combat_role(d.role) && !d.submersible && !d.capital && d.act <= self.s.act);
        let mut v: Vec<ClassId> = self.s.region.map(|r| r.order_of_battle.iter().map(|(c, _)| c.clone()).filter(|c| ok(c)).collect()).unwrap_or_default();
        if v.is_empty() { v = c.classes.keys().filter(|k| ok(k)).cloned().collect(); }
        v.sort();
        v.dedup();
        v
    }

    fn wave_fleet(&self, k: u64) -> Option<FleetTemplateId> {
        let c = &self.data.campaign;
        let mut v: Vec<(&FleetTemplateId, Act)> = c.fleets.iter().filter(|(_, f)| matches!(f.kind, FleetKind::TaskGroup | FleetKind::HunterKiller) && f.act <= self.s.act).map(|(id, f)| (id, f.act)).collect();
        if v.is_empty() { return None; }
        let top = v.iter().map(|x| x.1).max()?;
        v.retain(|x| x.1 == top);
        v.sort_by(|a, b| a.0.cmp(b.0));
        Some(v[self.r.below(P_WAVE, k, v.len())].0.clone())
    }

    fn add_wave(&mut self, w: usize) {
        let marker = self.enemy_spawn(usize::MAX);
        let at = WAVE_FIRST_S.saturating_add(WAVE_STEP_S.saturating_mul(w as u32));
        let g = match self.wave_fleet(100 + w as u64) {
            Some(f) => self.add_fleet(&f, &marker, "reinforcements", Stance::Hostile, at, None, Side::Combine, Confidence::Possible, 4000 + w as u64),
            None => None,
        };
        let g = g.filter(|g| self.units.iter().any(|u| u.group == *g)).or_else(|| self.add_synth_group(&marker, at, Confidence::Possible, 4100 + w as u64));
        if let Some(g) = g {
            self.groups[g].arrives_s = at;
            self.groups[g].tag = "reinforcements".into();
            for u in self.units.iter_mut().filter(|u| u.group == g) { u.tag = "reinforcements".into(); }
            let obj = self.objective_pos_name();
            self.groups[g].orders = Some(match obj { Some(m) => GroupOrders::Guard { marker: m, radius_m: 4_000.0 }, None => GroupOrders::Hunt });
        }
    }

    fn add_hunter_group(&mut self) {
        let Some(state) = self.s.state else { return };
        let Some(g) = state.hunter_group.and_then(|id| state.groups.iter().find(|g| g.id == id)) else { return };
        let marker = self.enemy_spawn(0);
        let gi = self.new_group(FleetKind::HunterKiller, Formation::Wedge, &marker, 0, "hunter", None, Side::Combine);
        self.groups[gi].orders = Some(GroupOrders::Hunt);
        self.groups[gi].theatre_group = Some(g.id);
        for id in &g.hulls {
            let Some(h) = state.hulls.iter().find(|h| h.id == *id && h.alive) else { continue };
            if !self.class_ok(&h.class) { continue; }
            let class = h.variant.clone().filter(|v| self.class_ok(v)).unwrap_or_else(|| h.class.clone());
            self.push_unit(class, gi, Stance::Hostile, Side::Combine, Confidence::Confirmed);
            let u = self.units.last_mut().expect("unit just pushed");
            u.record = Some(h.id);
            u.ace = h.ace.clone();
            u.veterancy = unit_f(h.veterancy).min(0.15);
            u.fuel = unit_f(h.fuel);
            u.ammo = unit_f(h.ammunition);
            if let Some(a) = &h.ace { self.groups[gi].ace = Some(a.clone()); }
        }
    }

    /// Make sure the ace's ship sails in group `g` (or a new group) and leads it.
    fn ensure_ace(&mut self, ace: &AceId, prefer_group: usize) {
        if self.units.iter().any(|u| u.ace.as_ref() == Some(ace)) { return; }
        let data: &'d TheatreData = self.data;
        let Some(def) = data.campaign.aces.get(ace) else { return };
        if !self.class_ok(&def.ship) { return; }
        if let Some(u) = self.units.iter_mut().find(|u| u.class == def.ship && u.side == Side::Combine && u.ace.is_none()) {
            u.ace = Some(ace.clone());
            u.confidence = Confidence::Confirmed;
            let g = u.group;
            self.groups[g].ace = Some(ace.clone());
            return;
        }
        let g = if prefer_group < self.groups.len() && self.groups[prefer_group].side == Side::Combine { prefer_group } else {
            let m = self.primary_marker();
            self.new_group(FleetKind::TaskGroup, Formation::Wedge, &m, 0, "ace", None, Side::Combine)
        };
        self.push_unit(def.ship.clone(), g, Stance::Hostile, Side::Combine, Confidence::Confirmed);
        if let Some(u) = self.units.last_mut() { u.ace = Some(ace.clone()); }
        self.groups[g].ace = Some(ace.clone());
    }

    fn add_complication(&mut self) {
        let s: &'a Spec<'a> = self.s;
        let data: &'d TheatreData = self.data;
        let list = &s.op.complications;
        if list.is_empty() { return; }
        let pick = list[self.r.below(P_COMPLICATION, 0, list.len())].clone();
        let beaten = |a: &AceId| s.state.is_some_and(|st| st.ops.aces_beaten.contains(a));
        match pick {
            Complication::None => {}
            Complication::Reinforcements { fleet, at_s } => {
                let m = self.enemy_spawn(usize::MAX);
                if let Some(g) = self.add_fleet(&fleet, &m, "reinforcements", Stance::Hostile, at_s, None, Side::Combine, Confidence::Possible, 5000) {
                    let obj = self.objective_pos_name();
                    self.groups[g].orders = Some(match obj { Some(m) => GroupOrders::Guard { marker: m, radius_m: 4_000.0 }, None => GroupOrders::Hunt });
                }
            }
            Complication::WeatherTurns { .. } => {} // applied in `finish`
            Complication::AirPatrol => self.add_kind(FleetKind::AirPatrol, 5100),
            Complication::Minefield => {} // bodies added in `finish`
            Complication::DecoyConvoy => self.add_kind(FleetKind::DecoyConvoy, 5200),
            Complication::CivilianTraffic => self.add_civilians(),
            Complication::AceArrives(ace) => {
                if beaten(&ace) { return; }
                let Some(def) = data.campaign.aces.get(&ace) else { return };
                if !self.class_ok(&def.ship) || self.units.iter().any(|u| u.ace.as_ref() == Some(&ace)) { return; }
                let m = self.enemy_spawn(usize::MAX);
                let g = self.new_group(FleetKind::HunterKiller, Formation::Wedge, &m, 900, "ace", None, Side::Combine);
                self.groups[g].orders = Some(GroupOrders::Hunt);
                self.groups[g].ace = Some(ace.clone());
                let ship = def.ship.clone();
                self.push_unit(ship, g, Stance::Hostile, Side::Combine, Confidence::Possible);
                if let Some(u) = self.units.last_mut() { u.ace = Some(ace); }
            }
        }
    }

    /// Add the act's first fleet of a kind at the primary marker.
    fn add_kind(&mut self, kind: FleetKind, key: u64) {
        let data: &'d TheatreData = self.data;
        let c = &data.campaign;
        let mut v: Vec<&FleetTemplateId> = c.fleets.iter().filter(|(_, f)| f.kind == kind && f.act <= self.s.act).map(|(id, _)| id).collect();
        v.sort();
        let Some(id) = v.first().map(|x| (*x).clone()) else { return };
        let m = self.primary_marker();
        let conf = if kind == FleetKind::DecoyConvoy { Confidence::Possible } else { Confidence::Probable };
        self.add_fleet(&id, &m, "", Stance::Hostile, 0, None, Side::Combine, conf, key);
    }

    fn add_civilians(&mut self) {
        let civ: Vec<ClassId> = self.s.region.map(|r| r.civilian.clone()).unwrap_or_default().into_iter().filter(|c| self.class_ok(c)).collect();
        if civ.is_empty() { return; }
        let m = self.primary_marker();
        let g = self.new_group(FleetKind::Convoy, Formation::Column, &m, 0, "civilian", None, Side::Civilian);
        for k in 0..3u64 {
            let c = civ[self.r.below(P_CLASS, 6000 + k, civ.len())].clone();
            self.push_unit(c, g, Stance::Neutral, Side::Civilian, Confidence::Probable);
        }
    }

    fn add_alert_forces(&mut self) {
        self.add_kind(FleetKind::Escort, 7000);
        if matches!(self.s.op.kind, OperationKind::Template(MissionType::ConvoyRaid | MissionType::PrizeCapture)) { self.add_kind(FleetKind::DecoyConvoy, 7100); }
        self.add_kind(FleetKind::AirPatrol, 7200);
    }

    /// Drop capital hulls over the per-operation limit (latest first).
    fn enforce_capitals(&mut self) {
        let max = self.data.campaign.rules.max_capitals as usize;
        let cap = |c: &ClassId, d: &TheatreData| d.campaign.classes.get(c).is_some_and(|x| x.capital);
        let mut seen = 0usize;
        let mut keep = vec![true; self.units.len()];
        for (i, u) in self.units.iter().enumerate() {
            if u.side == Side::Combine && cap(&u.class, self.data) {
                seen += 1;
                if seen > max && u.ace.is_none() && u.record.is_none() { keep[i] = false; }
            }
        }
        let mut i = 0;
        self.units.retain(|_| { let k = keep[i]; i += 1; k });
    }

    fn counts(&self, u: &Unit) -> bool {
        u.side == Side::Combine && u.stance != Stance::Neutral && !matches!(u.role, VesselRole::Site | VesselRole::Tender) && self.data.campaign.classes.get(&u.class).is_some_and(|d| combat_role(d.role))
    }

    fn committed(&self) -> (usize, f64) {
        let mut h = 0usize;
        let mut t = 0.0;
        for u in &self.units {
            if !self.counts(u) { continue; }
            h += 1;
            t += class_facts(self.data, &u.class).map_or(0.0, |f| f.tonnage_t);
        }
        (h, t)
    }

    /// Skirmish: exactly `target` committed hulls, present at start.
    fn fit_skirmish(&mut self, target: usize) {
        let target = target.max(1);
        let mut guard = 0;
        while self.committed().0 < target && guard < 16 {
            let m = self.enemy_spawn(0);
            let before = self.committed().0;
            let g = self.wave_fleet(200 + guard as u64).and_then(|f| self.add_fleet(&f, &m, "task_group", Stance::Hostile, 0, None, Side::Combine, Confidence::Probable, 8000 + guard as u64));
            if g.is_none() || self.committed().0 == before { self.add_synth_group(&m, 0, Confidence::Probable, 8100 + guard as u64); }
            if self.committed().0 == before { break; }
            guard += 1;
        }
        let protected = objective_tag(&self.s.op.objective);
        let mut excess = self.committed().0.saturating_sub(target);
        let mut i = self.units.len();
        while excess > 0 && i > 0 {
            i -= 1;
            let u = &self.units[i];
            if !self.counts(u) || u.ace.is_some() { continue; }
            if protected.as_deref().is_some_and(|t| t == u.tag) && self.units.iter().filter(|x| x.tag == u.tag).count() <= 1 { continue; }
            self.units.remove(i);
            excess -= 1;
        }
        for u in self.units.iter_mut() { u.confidence = if u.confidence == Confidence::Possible { Confidence::Probable } else { u.confidence }; }
        for g in self.groups.iter_mut() { g.arrives_s = 0; }
    }

    /// Objectives that name a tag need a hull carrying it.
    fn ensure_objective_targets(&mut self) {
        let o = self.s.op.objective.clone();
        match &o {
            Objective::DestroyGroup { tag, .. } | Objective::Photograph { tag, .. } => {
                let need = if let Objective::Photograph { count, .. } = &o { (*count).max(1) as usize } else { 1 };
                let have = self.units.iter().filter(|u| u.side == Side::Combine && u.tag == *tag).count();
                if have >= need { return; }
                let mut missing = need - have;
                for u in self.units.iter_mut().filter(|u| u.side == Side::Combine && u.tag != *tag && u.stance != Stance::Neutral) {
                    if missing == 0 { break; }
                    u.tag = tag.clone();
                    missing -= 1;
                }
                if missing > 0 { self.add_target(tag, missing, Side::Combine, Stance::Hostile, VesselRole::Convoy); }
            }
            Objective::Capture { tag } | Objective::CutOut { tag, .. } => {
                for u in self.units.iter_mut().filter(|u| u.tag == *tag) { u.role = VesselRole::Prize; }
                if !self.units.iter().any(|u| u.tag == *tag && u.side == Side::Combine) { self.add_target(tag, 1, Side::Combine, Stance::Hostile, VesselRole::Prize); }
                // Cutting out takes a ship out of harbour (L778): the prize lies at anchor at its force marker (the
                // berth), not on the convoy lane its fleet kind would sail by default.
                if matches!(o, Objective::CutOut { .. }) {
                    let gs: BTreeSet<usize> = self.units.iter().filter(|u| u.tag == *tag).map(|u| u.group).collect();
                    for g in gs { if let Some(g) = self.groups.get_mut(g) { g.orders = Some(GroupOrders::Anchor); } }
                }
            }
            Objective::Escort { tag, .. } => {
                for u in self.units.iter_mut().filter(|u| u.tag == *tag) { u.side = Side::Protected; u.stance = Stance::Neutral; u.role = VesselRole::Convoy; }
                if !self.units.iter().any(|u| u.tag == *tag) { self.add_target(tag, 1, Side::Protected, Stance::Neutral, VesselRole::Convoy); }
            }
            _ => {}
        }
    }

    /// Add `n` hulls of a non-combat class (freighter, tanker, tender, else any) carrying a tag.
    fn add_target(&mut self, tag: &str, n: usize, side: Side, stance: Stance, role: VesselRole) {
        let data: &'d TheatreData = self.data;
        let c = &data.campaign;
        let pick = |roles: &[ClassRole]| -> Option<ClassId> {
            let mut v: Vec<&ClassId> = c.classes.iter().filter(|(_, d)| roles.contains(&d.role) && d.act <= self.s.act && !d.submersible).map(|(id, _)| id).collect();
            v.sort();
            v.first().map(|x| (*x).clone())
        };
        let Some(class) = pick(&[ClassRole::Freighter, ClassRole::Tanker]).or_else(|| pick(&[ClassRole::Tender, ClassRole::Civilian])).or_else(|| self.combat_pool().first().cloned()) else { return };
        let m = self.objective_pos_name().unwrap_or_else(|| self.primary_marker());
        let g = self.new_group(FleetKind::Convoy, Formation::Column, &m, 0, tag, None, side);
        for _ in 0..n.min(16) {
            self.push_unit(class.clone(), g, stance, side, Confidence::Confirmed);
            if let Some(u) = self.units.last_mut() { u.role = role; }
        }
        if role == VesselRole::Prize { self.groups[g].orders = Some(GroupOrders::Anchor); }
    }

    /// Bind units to persistent hull records (campaign only). Returns the records used.
    fn assign_records(&mut self) -> Vec<u32> {
        let Some(state) = self.s.state else { return Vec::new() };
        let net = super::logistics::Net::new(self.data);
        let _ = &net;
        let hours: Vec<Option<u32>> = self.s.node.as_ref().and_then(|n| net.idx(n)).map(|i| net.combine_hours_from(state, i)).unwrap_or_else(|| vec![None; net.len()]);
        let busy: BTreeSet<u32> = state.convoys.iter().flat_map(|c| c.hulls.iter().copied())
            .chain(state.hunter_group.and_then(|id| state.groups.iter().find(|g| g.id == id)).map(|g| g.hulls.clone()).unwrap_or_default())
            .collect();
        let mut used: BTreeSet<u32> = self.units.iter().filter_map(|u| u.record).collect();
        let region = self.s.region_id.clone();
        let data: &'d TheatreData = self.data;
        let classes = &data.campaign.classes;
        let base_of = |c: &ClassId| -> ClassId {
            // A variant maps back to the class whose variant list holds it.
            classes.iter().find(|(_, d)| d.variants.contains(c)).map(|(id, _)| id.clone()).unwrap_or_else(|| c.clone())
        };
        for ui in 0..self.units.len() {
            if self.units[ui].record.is_some() || self.units[ui].side != Side::Combine || matches!(self.units[ui].role, VesselRole::Site) { continue; }
            let want = base_of(&self.units[ui].class);
            let want_def = classes.get(&want);
            let wanted_ace = self.units[ui].ace.clone();
            let rank = |h: &content::save::HullRecord| -> (bool, u32, u32) {
                let far = region.as_ref() != Some(&h.region);
                let hrs = net.idx(&h.node).and_then(|i| hours.get(i).copied().flatten()).unwrap_or(u32::MAX);
                (far, hrs, h.id)
            };
            let usable = |h: &&content::save::HullRecord| h.alive && h.refit_until_hour.is_none() && !busy.contains(&h.id) && !used.contains(&h.id) && unit_f(h.damage) < RECORD_MAX_DAMAGE && h.ace == wanted_ace;
            let exact = state.hulls.iter().filter(usable).filter(|h| h.class == want).min_by_key(|h| rank(h));
            let similar = || state.hulls.iter().filter(usable).filter(|h| {
                let d = classes.get(&h.class);
                matches!((d, want_def), (Some(a), Some(b)) if a.role == b.role && a.tier == b.tier && a.capital == b.capital)
            }).min_by_key(|h| rank(h));
            let Some(h) = exact.or_else(similar) else { continue };
            used.insert(h.id);
            let u = &mut self.units[ui];
            if h.class != want {
                u.class = if self.variants { h.variant.clone().filter(|v| classes.contains_key(v)).unwrap_or_else(|| h.class.clone()) } else { h.class.clone() };
            } else if let Some(v) = h.variant.clone().filter(|v| classes.contains_key(v)) {
                u.class = v;
            }
            u.record = Some(h.id);
            u.veterancy = unit_f(h.veterancy).min(0.15).max(u.veterancy);
            u.fuel = unit_f(h.fuel);
            u.ammo = unit_f(h.ammunition);
            let g = u.group;
            if self.groups[g].theatre_group.is_none() { self.groups[g].theatre_group = h.group; }
        }
        let mut v: Vec<u32> = self.units.iter().filter_map(|u| u.record).collect();
        v.sort();
        v.dedup();
        v
    }

    // ---- markers ----

    fn marker(&self, name: &str) -> Option<&Marker> { self.map.markers.iter().find(|m| m.name == name) }

    fn half(&self) -> (f64, f64) { (self.map.size_m[0] * 0.5 - MARGIN_M, self.map.size_m[1] * 0.5 - MARGIN_M) }

    fn add_marker(&mut self, kind: MarkerKind, name: &str, pos: [f64; 2], heading: f64) {
        if self.marker(name).is_some() { return; }
        let (hx, hz) = self.half();
        let p = [pos[0].clamp(-hx, hx), pos[1].clamp(-hz, hz)];
        self.map.markers.push(Marker { kind, name: name.to_string(), pos_m: p, heading_deg: heading });
    }

    fn ensure_markers(&mut self, objective: Option<&str>) {
        let (hx, hz) = self.half();
        if !self.map.markers.iter().any(|m| m.kind == MarkerKind::PlayerSpawn) { self.add_marker(MarkerKind::PlayerSpawn, "spawn", [-0.8 * hx, 0.0], 90.0); }
        if !self.map.markers.iter().any(|m| m.kind == MarkerKind::EnemySpawn) { self.add_marker(MarkerKind::EnemySpawn, "enemy", [0.8 * hx, 0.0], 270.0); }
        if let Some(name) = objective {
            let kind = match &self.s.op.objective { Objective::DestroySite { .. } => MarkerKind::DepotSite, Objective::Extract { .. } => MarkerKind::ShoreLanding, _ => MarkerKind::Objective };
            self.add_marker(kind, name, [0.25 * hx, 0.0], 0.0);
        }
        for (i, f) in self.s.op.force.iter().enumerate() {
            let a = i as f64 * 1.3;
            let p = [0.25 * hx + 0.3 * hx * libm::cos(a), 0.3 * hz * libm::sin(a)];
            self.add_marker(MarkerKind::EnemySpawn, &f.marker, p, 270.0);
        }
        for exit in objective_exits(&self.s.op.objective) { self.add_marker(MarkerKind::Exit, &exit, [-0.9 * hx, -0.5 * hz], 270.0); }
        // An extraction and a feint name no exit and end at any: make sure one exists.
        if matches!(self.s.op.objective, Objective::Extract { .. } | Objective::Feint { .. }) && !self.map.markers.iter().any(|m| m.kind == MarkerKind::Exit) {
            self.add_marker(MarkerKind::Exit, "exit", [-0.9 * hx, -0.5 * hz], 270.0);
        }
        if !self.map.markers.iter().any(|m| m.kind == MarkerKind::ConvoyEntry) { self.add_marker(MarkerKind::ConvoyEntry, "convoy_entry", [0.2 * hx, -0.9 * hz], 180.0); }
        if !self.map.markers.iter().any(|m| m.kind == MarkerKind::ConvoyExit) { self.add_marker(MarkerKind::ConvoyExit, "convoy_exit", [0.2 * hx, 0.9 * hz], 180.0); }
    }

    fn player_spawn_marker(&self) -> &Marker {
        self.map.markers.iter().find(|m| m.kind == MarkerKind::PlayerSpawn).or_else(|| self.map.markers.first()).expect("markers ensured")
    }

    /// The `k`-th enemy spawn marker name (`usize::MAX` = the one farthest from the players).
    fn enemy_spawn(&self, k: usize) -> String {
        let spawns: Vec<&Marker> = self.map.markers.iter().filter(|m| m.kind == MarkerKind::EnemySpawn).collect();
        if spawns.is_empty() { return "enemy".into(); }
        if k == usize::MAX {
            let p = self.player_spawn_marker().pos_m;
            return spawns.iter().max_by(|a, b| d2(a.pos_m, p).total_cmp(&d2(b.pos_m, p)).then_with(|| b.name.cmp(&a.name))).map(|m| m.name.clone()).unwrap_or_default();
        }
        spawns[k % spawns.len()].name.clone()
    }

    fn primary_marker(&self) -> String {
        self.s.op.force.first().map(|f| f.marker.clone()).or_else(|| self.objective_pos_name()).unwrap_or_else(|| self.enemy_spawn(0))
    }

    fn objective_pos_name(&self) -> Option<String> {
        objective_marker_name(&self.s.op.objective).filter(|n| self.marker(n).is_some())
            .or_else(|| self.map.markers.iter().find(|m| matches!(m.kind, MarkerKind::Objective | MarkerKind::DepotSite | MarkerKind::RadarSite | MarkerKind::Berth)).map(|m| m.name.clone()))
    }

    /// The marker a cutting-out's prize lies at (its force entry's marker: the berth), which may be kilometres
    /// from the map's first objective-kind marker.
    fn prize_marker_name(&self) -> Option<String> {
        let Objective::CutOut { tag, .. } = &self.s.op.objective else { return None };
        self.s.op.force.iter().find(|f| !tag.is_empty() && f.tag == *tag).map(|f| f.marker.clone()).filter(|n| self.marker(n).is_some())
    }

    /// The marker the group a destroy-group objective names starts at (its force entry's marker: the ace's
    /// patrol station in a duel), when the map has it. See [`destroy_group_marker`].
    fn target_group_marker_name(&self) -> Option<String> {
        destroy_group_marker(self.s.op, self.data).map(str::to_string).filter(|n| self.marker(n).is_some())
    }

    /// The marker the briefing route ends at: a cutting-out's berth, a destroy-group's target group, else the
    /// objective's own marker or the map's first objective-kind marker.
    fn objective_marker(&self) -> Option<String> {
        self.prize_marker_name().or_else(|| self.target_group_marker_name()).or_else(|| self.objective_pos_name())
    }

    fn objective_point(&self) -> [f64; 2] {
        self.objective_marker().and_then(|n| self.marker(&n).map(|m| m.pos_m))
            .or_else(|| self.s.op.force.first().and_then(|f| self.marker(&f.marker)).map(|m| m.pos_m))
            .unwrap_or([0.0, 0.0])
    }

    /// Convoy speed over player speed for the convoy start: the group AI's transit speed of these hulls over
    /// the Helm cruise of the slowest fielded player hull.
    fn convoy_speed_ratio(&self, members: &[usize], player_cruise_kn: f64) -> f64 {
        let data = self.data;
        let convoy = members.iter().filter_map(|i| self.units.get(*i)).filter_map(|u| data.campaign.classes.get(&u.class)).filter_map(|d| stock_speeds(&d.blueprint, data)).map(|f| f.top_kn).fold(f64::INFINITY, f64::min) * CONVOY_SPEED_FRACTION;
        let ratio = convoy / player_cruise_kn;
        if ratio.is_finite() && ratio > 0.0 { ratio.clamp(CONVOY_LEAD_RATIO_BOUNDS.0, CONVOY_LEAD_RATIO_BOUNDS.1) } else { CONVOY_LEAD_RATIO }
    }

    /// Helm cruise of the slowest fielded player hull, in knots (infinite when none can be measured).
    fn player_cruise_kn(&self) -> f64 {
        let data = self.data;
        self.s.seats.iter().filter_map(|p| p.hull.as_ref()).filter_map(|h| match h {
            SeatHull::Campaign(rec) => inline_speeds(&rec.blueprint, data),
            SeatHull::Stock(path) => path.as_deref().and_then(|p| stock_speeds(p, data)),
        }).map(|f| f.cruise_kn).fold(f64::INFINITY, f64::min)
    }

    // ---- final assembly ----

    fn finish(mut self, records: Vec<u32>) -> Generated {
        let s = self.s;
        let data = self.data;
        let c = &data.campaign;
        let obj = self.objective_point();
        // Player spawn on the approach line.
        let sp = self.player_spawn_marker().clone();
        let (dx, dz) = (sp.pos_m[0] - obj[0], sp.pos_m[1] - obj[1]);
        let len = libm::sqrt(dx * dx + dz * dz);
        let dir = if len > 1.0 { [dx / len, dz / len] } else { [-1.0, 0.0] };
        let approach = if s.op.approach_m.is_finite() && s.op.approach_m > 0.0 { s.op.approach_m } else { 10_000.0 } * approach_scale(s.op, data);
        let (hx, hz) = self.half();
        let mut t = approach;
        // Shorten along the line until inside the map.
        if dir[0].abs() > 1e-9 { t = t.min(((if dir[0] > 0.0 { hx } else { -hx }) - obj[0]) / dir[0]); }
        if dir[1].abs() > 1e-9 { t = t.min(((if dir[1] > 0.0 { hz } else { -hz }) - obj[1]) / dir[1]); }
        let t = t.max(0.0);
        let spawn = [obj[0] + dir[0] * t, obj[1] + dir[1] * t];
        let distance_km = t / 1000.0;
        let player_heading = heading_deg([obj[0] - spawn[0], obj[1] - spawn[1]]);
        // Exits lie no farther from the objective point than the approach distance: an exit marker beyond it is
        // pulled in along its line to the objective (and off any island), so the way out scales with the way in
        // and with the L85-90 approach fallback instead of staying at the node map's edge.
        // DECISION (r3): the pull never applies to a Survive objective (reaching its exit ends the sortie, so a
        // pulled exit cut hunter-group survival's 900 s to minutes), and a pulled exit is kept only when it
        // still lies at least the approach distance from the player spawn; otherwise the exit stays where the
        // theatre data puts it. The shipped generator and the winnability harness build the same map.
        // DECISION (22 November 2026): an extraction, an escort, a cutting-out and a feint cannot finish at an
        // exit before the hold, the rendezvous or the boarding, so for them the spawn guard does not apply and
        // the exit comes in to `EXIT_LEG_FRACTION` of the approach on a leg with water under it (the guard
        // left their exits 25 to 29 km from the objective, which no Act 1 starter sails inside 45 minutes).
        let exit_leg = matches!(s.op.objective, Objective::Extract { .. } | Objective::Escort { .. } | Objective::CutOut { .. } | Objective::Feint { .. });
        if exit_leg {
            let reach = EXIT_LEG_FRACTION * approach;
            let (hx, hz) = self.half();
            let terrain = sim_core::terrain::Terrain::new(self.map.clone());
            let mut moved: Vec<(usize, [f64; 2])> = Vec::new();
            for (k, m) in self.map.markers.iter().enumerate() {
                if m.kind != MarkerKind::Exit { continue; }
                let (ex, ez) = (m.pos_m[0] - obj[0], m.pos_m[1] - obj[1]);
                let d = libm::sqrt(ex * ex + ez * ez);
                if !(d.is_finite() && reach.is_finite() && reach > 0.0 && d > reach) { continue; }
                moved.push((k, exit_on_clear_leg(obj, [ex / d, ez / d], reach, &self.map, &terrain, hx, hz)));
            }
            for (k, p) in moved { self.map.markers[k].pos_m = p; }
        } else if !matches!(s.op.objective, Objective::Survive { .. }) {
            let reach = approach.max(t);
            let (hx, hz) = self.half();
            let mut moved: Vec<(usize, [f64; 2])> = Vec::new();
            for (k, m) in self.map.markers.iter().enumerate() {
                if m.kind != MarkerKind::Exit { continue; }
                let (ex, ez) = (m.pos_m[0] - obj[0], m.pos_m[1] - obj[1]);
                let d = libm::sqrt(ex * ex + ez * ez);
                if !(d.is_finite() && reach.is_finite() && reach > 0.0 && d > reach) { continue; }
                let p = [obj[0] + ex / d * reach, obj[1] + ez / d * reach];
                let p = avoid_islands(clamp_in(p, hx, hz), &self.map, hx, hz);
                if d2(p, spawn) < approach * approach { continue; }
                moved.push((k, p));
            }
            for (k, p) in moved { self.map.markers[k].pos_m = p; }
        }

        let mut config = SortieConfig {
            protocol: crate::protocol::PROTOCOL_VERSION, seed: s.seed, mode: GameMode::Campaign, rules: ModeRules::Objective,
            settings: content::settings::SimSettings { difficulty: s.difficulty, ironman: s.ironman, ruleset: content::ruleset::Ruleset::campaign(s.act), guest_scripts: false, sim_threads: 3 },
            act: s.act, weather: s.weather, weather_changes: Vec::new(), time_of_day: TimeOfDay::from_sun_elevation_deg(s.weather.sun_elevation_deg), map: MapDef::default(),
            layer_depth_m: s.region.map_or(50.0, |r| if r.layer_depth_m.is_finite() { r.layer_depth_m.clamp(30.0, 80.0) } else { 50.0 }),
            sides: Vec::new(), players: Vec::new(), blueprints: Vec::new(), vessels: Vec::new(), bodies: Vec::new(), groups: Vec::new(),
            objective: s.op.objective.clone(), ghosts: s.op.ghosts.clone(), tags: s.op.tags.clone(), actions: s.op.actions.clone(), intel: SortieIntel::default(),
            cards: Vec::new(), operation: Some(s.op.id.clone()), region: s.region_id.clone(), theatre_hour: s.hour, splash: None, tutorial: None,
            time_limit_s: None,
        };

        // Sides.
        let used = |side: Side| self.units.iter().any(|u| u.side == side);
        let (players_side, combine, civilian, protected) = (SideId(0), SideId(1), SideId(2), SideId(3));
        config.sides.push(SideDef { id: players_side, faction: Faction::Players, name: TextKey::new("objective.side.players"), hostile_to: SideMask::only(combine), allied: SideMask::only(players_side), pvp_team: None });
        let combine_foes = if used(Side::Protected) { SideMask::only(players_side).with(protected) } else { SideMask::only(players_side) };
        config.sides.push(SideDef { id: combine, faction: Faction::Combine, name: TextKey::new("objective.side.combine"), hostile_to: combine_foes, allied: SideMask::only(combine), pvp_team: None });
        if used(Side::Civilian) { config.sides.push(SideDef { id: civilian, faction: Faction::Civilian, name: TextKey::new("objective.side.civilian"), hostile_to: SideMask::NONE, allied: SideMask::only(civilian), pvp_team: None }); }
        if used(Side::Protected) { config.sides.push(SideDef { id: protected, faction: Faction::Protected, name: TextKey::new("objective.side.protected"), hostile_to: SideMask::NONE, allied: SideMask::only(protected), pvp_team: None }); }
        let side_id = |side: Side| match side { Side::Combine => combine, Side::Civilian => civilian, Side::Protected => protected };

        // Players.
        let fielded: Vec<&Seat> = s.seats.iter().filter(|p| p.hull.is_some()).collect();
        let line = line_abreast(spawn, player_heading, fielded.len());
        let mut seats_sorted: Vec<&Seat> = s.seats.iter().collect();
        seats_sorted.sort_by_key(|p| p.slot);
        let mut fi = 0usize;
        for seat in seats_sorted {
            let vessel = seat.hull.as_ref().map(|hull| {
                let pos = line[fi.min(line.len().saturating_sub(1))];
                fi += 1;
                let mut v = VesselInit {
                    blueprint: BlueprintRef::Stock(String::new()), side: players_side, class: None, group: None, pos: DVec3::new(pos[0], 0.0, pos[1]), heading_rad: player_heading.to_radians(), speed_kn: 0.0, arrives_s: 0,
                    damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: String::new(), declared: None, emcon: None, passengers: 0, role: VesselRole::PlayerHull,
                };
                match hull {
                    SeatHull::Campaign(rec) => {
                        let hash = content::blueprint::io::compute_hash(&rec.blueprint);
                        if !config.blueprints.iter().any(|b| content::blueprint::io::compute_hash(b) == hash) { config.blueprints.push(rec.blueprint.clone()); }
                        v.blueprint = BlueprintRef::Inline(hash);
                        v.damage = (rec.damage != HullDamage::default()).then(|| rec.damage.clone());
                        v.fuel_fraction = unit_f(rec.fuel_fraction);
                        v.ammo = rec.ammo.clone();
                        v.veterancy = unit_f(rec.veterancy).min(0.15);
                    }
                    SeatHull::Stock(path) => v.blueprint = BlueprintRef::Stock(path.clone().unwrap_or_default()),
                }
                config.vessels.push(v);
                (config.vessels.len() - 1) as u16
            });
            config.players.push(PlayerInit { slot: seat.slot, client_id: seat.client_id, name: seat.name.clone(), side: players_side, vessel, human: true });
        }

        // SEXTANT at its cove when the hunter group attacks it.
        let hunted = s.op.kind == OperationKind::Template(MissionType::HunterGroupSurvival) && s.state.is_some_and(super::alert::hunter_at_cove);
        if hunted {
            let tender_path = if content::data::STOCK_FILES.iter().any(|(p, _)| *p == SEXTANT_BLUEPRINT) { Some(SEXTANT_BLUEPRINT.to_string()) } else {
                let mut v: Vec<(&ClassId, &content::campaign::EnemyClassDef)> = c.classes.iter().filter(|(_, d)| d.role == ClassRole::Tender).collect();
                v.sort_by(|a, b| a.0.cmp(b.0));
                v.first().map(|(_, d)| d.blueprint.clone())
            };
            if let Some(path) = tender_path {
                let at = self.map.markers.iter().find(|m| m.kind == MarkerKind::Berth).map(|m| m.pos_m).unwrap_or([spawn[0] - dir[0] * 1500.0, spawn[1] - dir[1] * 1500.0]);
                let at = clamp_in(at, hx, hz);
                config.vessels.push(VesselInit {
                    blueprint: BlueprintRef::Stock(path), side: players_side, class: None, group: None, pos: DVec3::new(at[0], 0.0, at[1]), heading_rad: player_heading.to_radians(), speed_kn: 0.0, arrives_s: 0,
                    damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: "sextant".into(), declared: Some(Stance::Friend), emcon: None, passengers: 0, role: VesselRole::Tender,
                });
            }
        }

        // Site for site objectives.
        if let Objective::DestroySite { marker } = &s.op.objective {
            let mut v: Vec<(&ClassId, &content::campaign::EnemyClassDef)> = c.classes.iter().filter(|(_, d)| d.act <= s.act && !d.submersible).collect();
            v.sort_by(|a, b| (site_rank(a.1.role), a.0).cmp(&(site_rank(b.1.role), b.0)));
            if let Some((id, d)) = v.first() {
                let at = self.marker(marker).map(|m| m.pos_m).unwrap_or(obj);
                config.vessels.push(VesselInit {
                    blueprint: BlueprintRef::Stock(d.blueprint.clone()), side: combine, class: Some((*id).clone()), group: None, pos: DVec3::new(at[0], 0.0, at[1]), heading_rad: 0.0, speed_kn: 0.0, arrives_s: 0,
                    damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: marker.clone(), declared: Some(Stance::Hostile), emcon: Some(Emcon::Active), passengers: 0, role: VesselRole::Site,
                });
            }
        }

        // Groups and vessels.
        let enemy_emcon = s.cards.iter().filter_map(|id| c.cards.get(id)).find_map(|d| d.effect.enemy_emcon);
        let mut group_ids: Vec<Option<GroupId>> = vec![None; self.groups.len()];
        let player_pos = spawn;
        // Measured at most once per generation, and only when a convoy sails.
        let player_cruise: std::cell::OnceCell<f64> = std::cell::OnceCell::new();
        for (gi, g) in self.groups.iter().enumerate() {
            let members: Vec<usize> = (0..self.units.len()).filter(|i| self.units[*i].group == gi).collect();
            if members.is_empty() && g.bodies.is_empty() { continue; }
            let id = GroupId(config.groups.len() as u16);
            group_ids[gi] = Some(id);
            let anchor = self.marker(&g.marker).map(|m| (m.pos_m, m.heading_deg)).unwrap_or((obj, 0.0));
            let heading = if g.kind == FleetKind::Convoy || g.kind == FleetKind::DecoyConvoy {
                let a = self.map.markers.iter().find(|m| m.kind == MarkerKind::ConvoyEntry).map(|m| m.pos_m).unwrap_or(anchor.0);
                let b = self.map.markers.iter().find(|m| m.kind == MarkerKind::ConvoyExit).map(|m| m.pos_m).unwrap_or(obj);
                heading_deg([b[0] - a[0], b[1] - a[1]])
            } else if anchor.1 != 0.0 { anchor.1 } else { heading_deg([player_pos[0] - anchor.0[0], player_pos[1] - anchor.0[1]]) };
            let orders = g.orders.clone().unwrap_or_else(|| default_orders(g, &self.map, &s.op.objective, &self.groups));
            let speed = match &orders { GroupOrders::Transit { .. } | GroupOrders::Escort { .. } => 10.0, GroupOrders::Patrol { .. } => 12.0, GroupOrders::Hunt | GroupOrders::Flee { .. } => 16.0, GroupOrders::Guard { .. } | GroupOrders::Search { .. } => 6.0, _ => 0.0 };
            // A convoy starts on its lane where it will pass the point nearest the objective as the players get
            // there; its escort starts with it (see the module docs).
            let lane_start = |o: &GroupOrders, members: &[usize]| -> Option<[f64; 2]> {
                let GroupOrders::Transit { from, to } = o else { return None };
                let a = self.marker(from)?.pos_m;
                let Some(b) = self.marker(to).map(|m| m.pos_m) else { return Some(a) };
                let len = libm::sqrt(d2(a, b));
                if !(len.is_finite() && len > 1.0) { return Some(a); }
                let u = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
                let nearest = ((obj[0] - a[0]) * u[0] + (obj[1] - a[1]) * u[1]).clamp(0.0, len);
                let cruise = *player_cruise.get_or_init(|| self.player_cruise_kn());
                let at = (nearest - approach * self.convoy_speed_ratio(members, cruise)).clamp(0.0, len);
                Some([a[0] + u[0] * at, a[1] + u[1] * at])
            };
            let start = match &orders {
                GroupOrders::Transit { .. } => lane_start(&orders, &members).unwrap_or(anchor.0),
                GroupOrders::Escort { tag } => self.groups.iter().enumerate().find(|(_, x)| x.tag == *tag && matches!(x.kind, FleetKind::Convoy | FleetKind::DecoyConvoy) && x.side == g.side)
                    .and_then(|(ci, x)| {
                        let convoy: Vec<usize> = (0..self.units.len()).filter(|i| self.units[*i].group == ci).collect();
                        lane_start(&x.orders.clone().unwrap_or_else(|| default_orders(x, &self.map, &s.op.objective, &self.groups)), &convoy)
                    }).unwrap_or(anchor.0),
                _ => anchor.0,
            };
            let offsets = formation_offsets(g.formation, members.len(), heading);
            let at = |shift: f64| -> Vec<[f64; 2]> {
                let f = forward(heading);
                let o = [-f[1] * shift, f[0] * shift];
                offsets.iter().map(|d| avoid_islands(clamp_in([start[0] + o[0] + d[0], start[1] + o[1] + d[1]], hx, hz), &self.map, hx, hz)).collect()
            };
            let placed: Vec<[f64; 2]> = config.vessels.iter().map(|v| [v.pos.x, v.pos.z]).collect();
            let crowded = |ps: &[[f64; 2]]| ps.iter().any(|p| placed.iter().any(|q| { let (dx, dz) = (p[0] - q[0], p[1] - q[1]); dx * dx + dz * dz < SPAWN_CLEAR_M * SPAWN_CLEAR_M }));
            let mut spots = at(0.0);
            for n in 1..=12u32 {
                if !crowded(&spots) { break; }
                let side = if n % 2 == 1 { 1.0 } else { -1.0 };
                spots = at(side * f64::from(n.div_ceil(2)) * GROUP_SHIFT_M);
            }
            for (k, ui) in members.iter().enumerate() {
                let u = &self.units[*ui];
                let Some(def) = c.classes.get(&u.class) else { continue };
                let p = spots[k];
                // The hulls a destroy-group objective names are the briefing's targets: declared Hostile (L498).
                let target = matches!(&s.op.objective, Objective::DestroyGroup { tag, .. } if !tag.is_empty() && *tag == u.tag);
                let declared = match (u.side, u.stance) {
                    (Side::Combine, Stance::Neutral) | (Side::Civilian, _) | (Side::Protected, _) => Some(Stance::Neutral),
                    _ if s.op.declared_hostile.contains(&u.class) || target => Some(Stance::Hostile),
                    _ => None,
                };
                config.vessels.push(VesselInit {
                    blueprint: BlueprintRef::Stock(def.blueprint.clone()), side: side_id(u.side), class: Some(u.class.clone()), group: Some(id), pos: DVec3::new(p[0], 0.0, p[1]), heading_rad: heading.to_radians(), speed_kn: speed, arrives_s: g.arrives_s,
                    damage: None, fuel_fraction: u.fuel, ammo_fraction: u.ammo, ammo: BTreeMap::new(), hull_record: u.record, veterancy: u.veterancy, tag: u.tag.clone(), declared,
                    emcon: if u.side == Side::Combine { g.emcon.or(enemy_emcon) } else { None }, passengers: 0, role: u.role,
                });
            }
            let ace = g.ace.clone().or_else(|| members.iter().find_map(|i| self.units[*i].ace.clone()));
            config.groups.push(GroupInit { id, side: side_id(g.side), kind: g.kind, formation: g.formation, orders, ace, theatre_group: g.theatre_group, tag: g.tag.clone() });
            for (m, n) in &g.bodies {
                if data.catalogue.munition(m).is_none() { continue; }
                for k in 0..(*n).min(16) {
                    let off = formation_offsets(Formation::LineAbreast, *n as usize, heading)[k as usize];
                    let p = clamp_in([anchor.0[0] + off[0], anchor.0[1] + off[1]], hx, hz);
                    config.bodies.push(BodyInit { munition: m.clone(), side: side_id(g.side), group: Some(id), pos: DVec3::new(p[0], 300.0, p[1]), vel: DVec3::ZERO, arrives_s: g.arrives_s });
                }
            }
        }

        // Complication effects that are not hulls.
        if let Some(cx) = s.op.complications.get(self.r.below(P_COMPLICATION, 0, s.op.complications.len().max(1))).filter(|_| !s.op.complications.is_empty()) {
            match cx {
                Complication::WeatherTurns { to, at_s } => {
                    let (lo, hi) = to.sea_state_range();
                    let sea = SeaState(s.weather.sea_state.0.clamp(lo, hi));
                    let w = weather_of(*to, sea, s.weather.wind_dir_rad, data, s.hour);
                    config.weather_changes.push((*at_s, w));
                }
                Complication::Minefield => {
                    let mut mines: Vec<&MunitionId> = data.catalogue.munitions.iter().filter(|(_, d)| d.kind == content::part::weapons::MunitionKind::Mine).map(|(id, _)| id).collect();
                    mines.sort();
                    if let Some(m) = mines.first() {
                        // A line of 12 mines across the approach, 60% of the way in.
                        let mid = [obj[0] + dir[0] * t * 0.4, obj[1] + dir[1] * t * 0.4];
                        let perp = [-dir[1], dir[0]];
                        for k in 0..12 {
                            let o = (k as f64 - 5.5) * 60.0;
                            let p = clamp_in([mid[0] + perp[0] * o, mid[1] + perp[1] * o], hx, hz);
                            config.bodies.push(BodyInit { munition: (*m).clone(), side: combine, group: None, pos: DVec3::new(p[0], -3.0, p[1]), vel: DVec3::ZERO, arrives_s: 0 });
                        }
                    }
                }
                _ => {}
            }
        }

        // Cards in force: the region's, plus the doctrine of any ace leading a group.
        let mut cards: Vec<CardId> = s.cards.clone();
        for g in &config.groups {
            if let Some(a) = g.ace.as_ref().and_then(|a| c.aces.get(a)) { for d in &a.doctrine { if !cards.contains(d) { cards.push(d.clone()); } } }
        }
        config.cards = cards.iter().filter_map(|id| c.cards.get(id).map(|d| (id.clone(), d.effect.clone()))).collect();

        // Intel.
        let mut oob: Vec<ClassId> = s.region.map(|r| r.order_of_battle.iter().map(|(c, _)| c.clone()).collect()).unwrap_or_default();
        let mut civ: Vec<ClassId> = s.region.map(|r| r.civilian.clone()).unwrap_or_default();
        for v in &config.vessels {
            let Some(cl) = &v.class else { continue };
            if config.sides.iter().any(|sd| sd.id == v.side && sd.faction == Faction::Civilian) { civ.push(cl.clone()); } else { oob.push(cl.clone()); }
        }
        oob.sort();
        oob.dedup();
        civ.sort();
        civ.dedup();
        // Named hulls (Exact at first association, L475): prizes, sites, ace flagships and the hulls a
        // destroy-group objective is about.
        let destroy_tag = match &s.op.objective { Objective::DestroyGroup { tag, .. } if !tag.is_empty() => Some(tag.as_str()), _ => None };
        let named: Vec<u16> = config.vessels.iter().enumerate().filter(|(_, v)| matches!(v.role, VesselRole::Prize | VesselRole::Site) || (v.side == combine && v.declared == Some(Stance::Hostile) && destroy_tag == Some(v.tag.as_str())) || (v.group.is_some_and(|g| config.groups.iter().any(|x| x.id == g && x.ace.is_some())) && v.class.as_ref().is_some_and(|cl| config.groups.iter().filter_map(|x| x.ace.as_ref()).any(|a| c.aces.get(a).is_some_and(|d| &d.ship == cl))))).map(|(i, _)| i as u16).collect();
        let esm_sites: Vec<[f64; 2]> = self.map.markers.iter().filter(|m| m.kind == MarkerKind::RadarSite).map(|m| m.pos_m).collect();
        let recommended = s.op.recommended_emcon.or_else(|| s.op.ghosts.iter().any(|g| matches!(g.kind, GhostKind::NoRadarEmissions | GhostKind::NeverDetected)).then_some(Emcon::Silent));
        // DECISION: player hulls sail in at the operation's authored emission state, so an operation built
        // around silence does not fail its ghost conditions in its first seconds; the player radiates by choice
        // (the emission control key), and a design's own setting applies when nothing is authored.
        // DECISION (22 November 2026, L507 and L317): a recommendation inferred from ghost conditions is shown
        // in the briefing and not applied. Applied, it left the stock Sensors officer silent in 14 of the 18
        // Act 1 operations, six of which need gunfire to finish, and in Silent the guns hold for want of a
        // track graded Fine (L484).
        if let Some(e) = s.op.recommended_emcon {
            for v in config.vessels.iter_mut().filter(|v| v.role == VesselRole::PlayerHull) { v.emcon = Some(e); }
        }
        config.intel = SortieIntel { order_of_battle: oob, civilian: civ, declared_hostile: s.op.declared_hostile.clone(), named, esm_sites, recommended_emcon: recommended };
        config.map = self.map.clone();

        // Briefing.
        let (committed_hulls, committed_tonnage) = self.committed();
        let mut route = vec![spawn, obj];
        if let Some(exit) = objective_exits(&s.op.objective).first().and_then(|e| self.marker(e)) { route.push(exit.pos_m); }
        let revealed = |a: &AceId| s.state.is_some_and(|st| st.ops.aces_revealed.contains(a));
        let threats = threat_picture(&self.units, &self.groups, s, data, &revealed);
        let forced = s.op.forced || hunted;
        // The briefing's chart shows the markers this operation uses (start, exits, its objective, the places
        // its forces are named at, the shore or survivors its station actions need), not the whole
        // vocabulary every sortie map carries. The sortie itself keeps every marker.
        let mut briefing_map = config.map.clone();
        {
            let mut named: Vec<String> = objective_exits(&s.op.objective);
            let goal = self.objective_marker();
            if let Some(n) = goal.clone() { named.push(n); }
            named.extend(s.op.force.iter().map(|f| f.marker.clone()));
            let convoy = matches!(s.op.objective, Objective::DestroyGroup { .. } | Objective::Escort { .. } | Objective::Photograph { .. });
            let wants = |k: MarkerKind| match k {
                MarkerKind::PlayerSpawn | MarkerKind::Exit => true,
                MarkerKind::ShoreLanding => s.op.actions.contains(&content::campaign::StationAction::InsertOrExtract),
                MarkerKind::Survivors => s.op.actions.contains(&content::campaign::StationAction::RecoverSurvivors),
                MarkerKind::ConvoyEntry | MarkerKind::ConvoyExit => convoy,
                _ => false,
            };
            briefing_map.markers.retain(|m| wants(m.kind) || (m.kind != MarkerKind::EnemySpawn && named.contains(&m.name)) || goal.as_ref() == Some(&m.name));
            // A destroy-group's target group starts at a force marker the chart does not draw (an enemy
            // spawn): on the briefing's copy it is drawn as the objective the route ends at.
            if let Some(n) = self.target_group_marker_name() {
                for m in briefing_map.markers.iter_mut().filter(|m| m.name == n && m.kind == MarkerKind::EnemySpawn) { m.kind = MarkerKind::Objective; }
            }
        }
        let briefing = Briefing {
            operation: s.op.id.clone(), kind: s.op.kind.clone(), name: s.op.name.clone(), text: s.op.briefing.clone(), region: s.region_id.clone().unwrap_or_default(), node: s.node.clone().unwrap_or_default(),
            weather: config.weather, time_of_day: config.time_of_day, distance_km, threats, tags: s.op.tags.clone(), ghosts: s.op.ghosts.clone(), actions: s.op.actions.clone(), recommended_emcon: recommended,
            map: briefing_map, route, committed_hulls: committed_hulls.min(u16::MAX as usize) as u16, committed_tonnage_t: committed_tonnage, pushback_days: super::progress::pushback_days(s.op, data),
            forced, expires_hour: None, seed: s.seed,
        };
        Generated { config, briefing, committed_hulls: committed_hulls.min(u16::MAX as usize) as u16, committed_tonnage_t: committed_tonnage, hull_records: records }
    }
}

/// Shallow samples (less than [`EXIT_LEG_DEPTH_M`] of water) on the straight leg from `a` to `b`.
pub(crate) fn shallow_samples(terrain: &sim_core::terrain::Terrain, a: [f64; 2], b: [f64; 2]) -> u32 {
    (0..=EXIT_LEG_SAMPLES).filter(|k| {
        let f = f64::from(*k) / f64::from(EXIT_LEG_SAMPLES);
        terrain.height_at(a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f) > -EXIT_LEG_DEPTH_M
    }).count() as u32
}

/// An exit `reach` from the objective point along `dir`, on a leg with water under it: the first of the
/// bearings 0, +15, -15, ... +90, -90 degrees off `dir` whose exit (clamped inside the map and pushed off any
/// island) is no farther than `reach` and has no shallow sample on its leg; when none is clear, the one with
/// the fewest shallow samples (ties to the smaller turn).
fn exit_on_clear_leg(obj: [f64; 2], dir: [f64; 2], reach: f64, map: &MapDef, terrain: &sim_core::terrain::Terrain, hx: f64, hz: f64) -> [f64; 2] {
    let mut best: Option<((bool, u32), [f64; 2])> = None;
    for n in 0..=2 * EXIT_TURN_STEPS {
        let turn = (if n % 2 == 1 { 1.0 } else { -1.0 }) * f64::from(n.div_ceil(2)) * EXIT_TURN_DEG.to_radians();
        let (sn, cs) = (libm::sin(turn), libm::cos(turn));
        let d = [dir[0] * cs - dir[1] * sn, dir[0] * sn + dir[1] * cs];
        let p = avoid_islands(clamp_in([obj[0] + d[0] * reach, obj[1] + d[1] * reach], hx, hz), map, hx, hz);
        let score = (d2(p, obj) > (reach + 1.0) * (reach + 1.0), shallow_samples(terrain, obj, p));
        if score == (false, 0) { return p; }
        if best.as_ref().is_none_or(|b| score < b.0) { best = Some((score, p)); }
    }
    best.map_or([obj[0] + dir[0] * reach, obj[1] + dir[1] * reach], |b| b.1)
}

fn site_rank(role: ClassRole) -> u8 { match role { ClassRole::Tender => 0, ClassRole::Freighter => 1, ClassRole::Tanker => 2, _ => 3 } }

fn threat_picture(units: &[Unit], groups: &[Group], s: &Spec, data: &TheatreData, revealed: &dyn Fn(&AceId) -> bool) -> Vec<ThreatEntry> {
    let mut map: BTreeMap<(core::cmp::Reverse<Confidence>, ClassId, Option<AceId>), u16> = BTreeMap::new();
    for u in units.iter().filter(|u| u.side == Side::Combine) {
        let late = groups.get(u.group).is_some_and(|g| g.arrives_s > 0);
        let mut conf = if late { Confidence::Possible } else { u.confidence };
        if s.op.declared_hostile.contains(&u.class) { conf = Confidence::Confirmed; }
        if let Some(a) = &u.ace { if revealed(a) || matches!(&s.op.kind, OperationKind::AceDuel(x) if x == a) { conf = Confidence::Confirmed; } }
        *map.entry((core::cmp::Reverse(conf), u.class.clone(), u.ace.clone())).or_insert(0) += 1;
    }
    map.into_iter().map(|((conf, class, ace), n)| {
        let conf = conf.0;
        let n = n.min(255) as u8;
        let (lo, hi) = match conf { Confidence::Confirmed => (n, n), Confidence::Probable => (n.saturating_sub(1).max(1), n.saturating_add(1)), Confidence::Possible => (0, n) };
        ThreatEntry { role: data.campaign.classes.get(&class).map(|d| d.role), class: Some(class), count_min: lo, count_max: hi, confidence: conf, ace }
    }).collect()
}

fn default_tag(kind: FleetKind) -> String {
    match kind {
        FleetKind::Convoy => "convoy", FleetKind::DecoyConvoy => "decoy", FleetKind::Escort => "escort", FleetKind::Garrison => "garrison", FleetKind::Patrol => "patrol",
        FleetKind::HunterKiller => "hunter", FleetKind::AirPatrol => "air", FleetKind::SubmarinePatrol => "submarine", FleetKind::TaskGroup => "task_group",
    }.to_string()
}

fn default_orders(g: &Group, map: &MapDef, objective: &Objective, groups: &[Group]) -> GroupOrders {
    let entry = map.markers.iter().find(|m| m.kind == MarkerKind::ConvoyEntry).map(|m| m.name.clone());
    let exit = map.markers.iter().find(|m| m.kind == MarkerKind::ConvoyExit).map(|m| m.name.clone());
    if g.side == Side::Protected {
        // DECISION (r3, L797 "Neutral until the briefing's rendezvous"): the escorted hull (the defector) holds
        // at its start; the Sim orders it to Flee for the objective's exit once a player hull comes within
        // `crate::sim::RENDEZVOUS_M` of it (`Sim::release_escorted`). It never leaves on its own.
        return GroupOrders::Anchor;
    }
    match g.kind {
        FleetKind::Convoy | FleetKind::DecoyConvoy => match (entry, exit) { (Some(a), Some(b)) => GroupOrders::Transit { from: a, to: b }, _ => GroupOrders::Guard { marker: g.marker.clone(), radius_m: 2_000.0 } },
        FleetKind::Escort => match groups.iter().find(|x| x.kind == FleetKind::Convoy && x.side == Side::Combine) { Some(cv) => GroupOrders::Escort { tag: cv.tag.clone() }, None => GroupOrders::Guard { marker: g.marker.clone(), radius_m: 3_000.0 } },
        FleetKind::Patrol | FleetKind::SubmarinePatrol => {
            let mut ms = vec![g.marker.clone()];
            if let Some(o) = objective_marker_name(objective) { if o != g.marker { ms.push(o); } }
            GroupOrders::Patrol { markers: ms }
        }
        FleetKind::Garrison => GroupOrders::Guard { marker: g.marker.clone(), radius_m: 3_000.0 },
        FleetKind::AirPatrol => GroupOrders::AirPatrol { marker: objective_marker_name(objective).unwrap_or_else(|| g.marker.clone()), radius_m: 10_000.0 },
        FleetKind::HunterKiller => GroupOrders::Hunt,
        FleetKind::TaskGroup => if matches!(objective, Objective::Survive { .. } | Objective::Feint { .. }) { GroupOrders::Hunt } else { GroupOrders::Guard { marker: g.marker.clone(), radius_m: 5_000.0 } },
    }
}

/// The force marker of the group a destroy-group objective names (the first force entry carrying its tag):
/// the briefing route ends there, not at the map's first objective-kind marker, which in an ace duel lay 11 km
/// or more from the ace's 3 km patrol ring (22 November 2026). A convoy is the exception: its marker is the
/// lane entry it leaves at the start, and the generator puts it on its lane to pass the objective point as the
/// players arrive, so a convoy's objective keeps that point. The pilot (`abstract_combat`) reads the same rule.
pub(crate) fn destroy_group_marker<'o>(op: &'o OperationDef, data: &TheatreData) -> Option<&'o str> {
    let Objective::DestroyGroup { tag, .. } = &op.objective else { return None };
    if tag.is_empty() { return None; }
    let f = op.force.iter().find(|f| f.tag == *tag)?;
    let kind = data.campaign.fleets.get(&f.fleet).map(|t| t.kind)?;
    (!matches!(kind, FleetKind::Convoy | FleetKind::DecoyConvoy)).then_some(f.marker.as_str())
}

fn objective_marker_name(o: &Objective) -> Option<String> {
    match o {
        Objective::DestroySite { marker } | Objective::Extract { marker, .. } | Objective::LayMines { marker, .. } => Some(marker.clone()),
        _ => None,
    }
}

fn objective_exits(o: &Objective) -> Vec<String> {
    match o { Objective::Escort { exit, .. } | Objective::Survive { exit, .. } | Objective::CutOut { exit, .. } if !exit.is_empty() => vec![exit.clone()], _ => Vec::new() }
}

fn objective_tag(o: &Objective) -> Option<String> {
    match o { Objective::DestroyGroup { tag, .. } | Objective::Capture { tag } | Objective::Escort { tag, .. } | Objective::Photograph { tag, .. } | Objective::CutOut { tag, .. } => Some(tag.clone()), _ => None }
}

fn objective_tag_is_protected(o: &Objective, tag: &str) -> bool { matches!(o, Objective::Escort { tag: t, .. } if t == tag && !tag.is_empty()) }

fn starter_path(data: &TheatreData, act: Act) -> Option<String> {
    let mut acts: Vec<&content::campaign::ActDef> = data.campaign.acts.iter().filter(|a| a.act <= act).collect();
    acts.sort_by(|a, b| b.act.cmp(&a.act));
    acts.iter().find_map(|a| a.starters.first().cloned()).or_else(|| data.campaign.acts.iter().find_map(|a| a.starters.first().cloned()))
}

fn sanitize_map(m: &MapDef) -> MapDef {
    let ok = m.size_m.iter().all(|s| s.is_finite() && *s >= 4_000.0);
    if !ok { return MapDef { size_m: [DEFAULT_MAP_M, DEFAULT_MAP_M], depth_m: 80.0, islands: Vec::new(), markers: Vec::new(), seed: m.seed }; }
    MapDef {
        size_m: [m.size_m[0].min(MAX_MAP_M), m.size_m[1].min(MAX_MAP_M)],
        depth_m: if m.depth_m.is_finite() && m.depth_m > 0.0 { m.depth_m } else { 80.0 },
        islands: m.islands.iter().filter(|i| i.centre_m.iter().all(|c| c.is_finite()) && i.radius_m.is_finite() && i.radius_m > 0.0 && i.height_m.is_finite()).copied().collect(),
        markers: m.markers.iter().filter(|k| k.pos_m.iter().all(|c| c.is_finite()) && k.heading_deg.is_finite()).cloned().collect(),
        seed: m.seed,
    }
}

fn unit_f(v: f64) -> f64 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } }
fn d2(a: [f64; 2], b: [f64; 2]) -> f64 { let (x, z) = (a[0] - b[0], a[1] - b[1]); x * x + z * z }
fn clamp_in(p: [f64; 2], hx: f64, hz: f64) -> [f64; 2] { [p[0].clamp(-hx.max(0.0), hx.max(0.0)), p[1].clamp(-hz.max(0.0), hz.max(0.0))] }

/// Compass heading in degrees of a world (x east, z south) direction: 0 = north = -Z, clockwise.
fn heading_deg(v: [f64; 2]) -> f64 {
    if v[0].abs() < 1e-9 && v[1].abs() < 1e-9 { return 90.0; }
    let h = libm::atan2(v[0], -v[1]).to_degrees();
    if h < 0.0 { h + 360.0 } else { h }
}

fn forward(heading: f64) -> [f64; 2] { let h = heading.to_radians(); [libm::sin(h), -libm::cos(h)] }

fn line_abreast(centre: [f64; 2], heading: f64, n: usize) -> Vec<[f64; 2]> {
    let f = forward(heading);
    let right = [-f[1], f[0]];
    (0..n.max(1)).map(|i| { let o = (i as f64 - (n as f64 - 1.0) * 0.5) * 600.0; [centre[0] + right[0] * o, centre[1] + right[1] * o] }).collect()
}

/// Offsets of `n` hulls in a formation facing `heading`.
fn formation_offsets(formation: Formation, n: usize, heading: f64) -> Vec<[f64; 2]> {
    let f = forward(heading);
    let r = [-f[1], f[0]];
    let at = |ahead: f64, right: f64| [f[0] * ahead + r[0] * right, f[1] * ahead + r[1] * right];
    (0..n).map(|i| {
        let k = i as f64;
        match formation {
            Formation::Column => at(-k * SPACING_M, 0.0),
            Formation::LineAbreast => at(0.0, (k - (n as f64 - 1.0) * 0.5) * SPACING_M),
            Formation::Wedge => { if i == 0 { at(0.0, 0.0) } else { let row = ((i + 1) / 2) as f64; let side = if i % 2 == 1 { 1.0 } else { -1.0 }; at(-row * SPACING_M, side * row * SPACING_M) } }
            Formation::Screen => { let a = (k - (n as f64 - 1.0) * 0.5) * 0.5; at(libm::cos(a) * 2.0 * SPACING_M, libm::sin(a) * 2.0 * SPACING_M) }
            Formation::Box => { let w = libm::ceil(libm::sqrt(n as f64)).max(1.0) as usize; at(-((i / w) as f64) * SPACING_M, ((i % w) as f64 - (w as f64 - 1.0) * 0.5) * SPACING_M) }
        }
    }).collect()
}

/// Push a position out of any island it falls on (radius plus 300 m of clear water).
fn avoid_islands(p: [f64; 2], map: &MapDef, hx: f64, hz: f64) -> [f64; 2] {
    let mut p = p;
    for isl in &map.islands {
        let (dx, dz) = (p[0] - isl.centre_m[0], p[1] - isl.centre_m[1]);
        let d = libm::sqrt(dx * dx + dz * dz);
        let clear = isl.radius_m + 300.0;
        if d < clear {
            let dir = if d > 1e-6 { [dx / d, dz / d] } else { [1.0, 0.0] };
            p = clamp_in([isl.centre_m[0] + dir[0] * clear, isl.centre_m[1] + dir[1] * clear], hx, hz);
        }
    }
    p
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::logistics::testkit::*;
    use super::*;
    use content::campaign::*;

    /// Real data: every doctrine card's fleet becomes a fleet of the operation's own act, of the same kind.
    #[test]
    fn card_fleets_follow_the_act() {
        let catalogue = std::sync::Arc::new(content::catalogue::Catalogue::load_embedded().unwrap());
        let campaign = std::sync::Arc::new(content::campaign::load::load_embedded(&catalogue).unwrap());
        let data = TheatreData { campaign: campaign.clone(), catalogue };
        let id = |s: &str| FleetTemplateId::new(s);
        assert_eq!(card_fleet_for_act(&data, &id("fleet.act1.patrol.heavy"), Act::Act4), id("fleet.act4.patrol.heavy"));
        assert_eq!(card_fleet_for_act(&data, &id("fleet.act1.pickets"), Act::Act2), id("fleet.act2.patrol"));
        assert_eq!(card_fleet_for_act(&data, &id("fleet.act1.escort"), Act::Act3), id("fleet.act3.escort.convoy"));
        assert_eq!(card_fleet_for_act(&data, &id("fleet.act1.escort"), Act::Act1), id("fleet.act1.escort"));
        for card in campaign.cards.values() {
            let Some(f) = &card.effect.add_fleet else { continue };
            let kind = campaign.fleets.get(f).map(|d| d.kind);
            for act in Act::ALL {
                let m = card_fleet_for_act(&data, f, act);
                let d = campaign.fleets.get(&m).unwrap_or_else(|| panic!("{}: {} in {act:?} is no fleet", card.id.as_str(), m.as_str()));
                assert_eq!((d.act, Some(d.kind)), (act, kind), "{}: {} in {act:?}", card.id.as_str(), m.as_str());
            }
        }
    }

    /// Testkit data with blueprint paths per class, measured facts, fleets and a template force.
    pub(crate) fn gen_data() -> TheatreData {
        let mut d = data();
        let mut c = (*d.campaign).clone();
        for (id, t, cost) in [("pb", 60.0, 300u32), ("frig", 400.0, 2000), ("frt", 500.0, 600), ("ace_ship", 900.0, 5000)] {
            let path = format!("test/{id}.bp.ron");
            set_blueprint_facts(&path, t, cost);
            c.classes.get_mut(&ClassId::new(id)).unwrap().blueprint = path;
        }
        c.fleets.insert(FleetTemplateId::new("fleet.patrol"), FleetTemplate { id: FleetTemplateId::new("fleet.patrol"), kind: FleetKind::Patrol, act: Act::Act1, slots: vec![FleetSlot { classes: vec![ClassId::new("pb")], min: 2, max: 3 }], formation: Formation::LineAbreast, bodies: vec![] });
        c.fleets.insert(FleetTemplateId::new("fleet.tg"), FleetTemplate { id: FleetTemplateId::new("fleet.tg"), kind: FleetKind::TaskGroup, act: Act::Act1, slots: vec![FleetSlot { classes: vec![ClassId::new("pb"), ClassId::new("frig")], min: 5, max: 5 }], formation: Formation::Wedge, bodies: vec![] });
        c.fleets.insert(FleetTemplateId::new("fleet.convoy"), FleetTemplate { id: FleetTemplateId::new("fleet.convoy"), kind: FleetKind::Convoy, act: Act::Act1, slots: vec![FleetSlot { classes: vec![ClassId::new("frt")], min: 3, max: 3 }], formation: Formation::Column, bodies: vec![] });
        let raid = c.operations.get_mut(&OperationId::new("op.raid")).unwrap();
        raid.objective = Objective::DestroyGroup { tag: "convoy".into(), fraction: 0.6 };
        raid.force = vec![
            ForceEntry { fleet: FleetTemplateId::new("fleet.convoy"), marker: "lane".into(), tag: "convoy".into(), stance: Stance::Hostile, arrives_s: 0, emcon: None },
            ForceEntry { fleet: FleetTemplateId::new("fleet.patrol"), marker: "lane".into(), tag: "escort".into(), stance: Stance::Hostile, arrives_s: 0, emcon: None },
        ];
        let depot = c.operations.get_mut(&OperationId::new("op.depot")).unwrap();
        depot.objective = Objective::DestroySite { marker: "depot".into() };
        depot.force = vec![ForceEntry { fleet: FleetTemplateId::new("fleet.patrol"), marker: "depot".into(), tag: "garrison".into(), stance: Stance::Hostile, arrives_s: 0, emcon: None }];
        d.campaign = Arc::new(c);
        d
    }

    pub(crate) fn started_gen() -> (TheatreState, TheatreData) {
        let d = gen_data();
        let mut s = state();
        let mut ev = Vec::new();
        super::super::progress::start_act(&mut s, Act::Act1, false, &d, &mut ev);
        s.players.push(player());
        (s, d)
    }

    pub(crate) fn player() -> content::save::PlayerRecord {
        content::save::PlayerRecord { client_id: 1, name: "a".into(), blueprint: content::blueprint::io::new_blueprint("t", 15.0), damage: Default::default(), fuel_fraction: 1.0, ammo: BTreeMap::new(), hull_lost: false, ready_at_hour: None, veterancy: 0.0 }
    }

    #[test]
    fn commitment_gate_on_every_template_act_and_player_count() {
        let d = gen_data();
        let diff = DifficultySliders::NORMAL;
        for t in [MissionType::ConvoyRaid, MissionType::DepotStrike, MissionType::Feint] {
            for players in 1..=4u8 {
                for seed in 0..40u64 {
                    let g = generate_standalone(t, Act::Act1, players, seed, &diff, None, &d).expect("template exists");
                    assert!(commitment_ok(&g, Act::Act1, players, &d), "{t:?} x{players} seed {seed}: {} hulls {} t", g.committed_hulls, g.committed_tonnage_t);
                    assert_eq!(g.briefing.seed, g.config.seed);
                    assert_eq!(g.config.players.len(), players as usize);
                    assert!(g.config.vessels.iter().all(|v| v.pos.x.abs() <= g.config.map.size_m[0] * 0.5 && v.pos.z.abs() <= g.config.map.size_m[1] * 0.5));
                }
            }
        }
        // A template with no operation in the data cannot be generated.
        assert!(generate_standalone(MissionType::CuttingOut, Act::Act1, 1, 1, &diff, None, &d).is_none());
    }

    #[test]
    fn standalone_is_deterministic_and_skirmish_sized() {
        let d = gen_data();
        let diff = DifficultySliders::NORMAL;
        let a = generate_standalone(MissionType::ConvoyRaid, Act::Act1, 2, 77, &diff, None, &d).unwrap();
        let b = generate_standalone(MissionType::ConvoyRaid, Act::Act1, 2, 77, &diff, None, &d).unwrap();
        assert_eq!(a, b);
        let s = generate_standalone(MissionType::ConvoyRaid, Act::Act1, 2, 77, &diff, Some(3), &d).unwrap();
        assert_eq!(count_committed(&s.config, &d).0, 6);
        assert!(s.config.vessels.iter().all(|v| v.arrives_s == 0));
        // The objective's convoy is present.
        assert!(s.config.vessels.iter().any(|v| v.tag == "convoy"));
    }

    #[test]
    fn campaign_generation_uses_records_and_meets_commitment() {
        let (s, d) = started_gen();
        let mut s = s;
        refresh_offers(&mut s, &d);
        let op = OperationId::new("op.depot");
        assert!(s.offered.contains(&op), "offered {:?}", s.offered);
        let g = generate(&s, &d, &op, &[PlayerSlot(0)]).unwrap();
        assert!(commitment_ok(&g, Act::Act1, 1, &d));
        assert!(!g.hull_records.is_empty());
        // Every record used is alive and unique.
        let mut seen = BTreeSet::new();
        for r in &g.hull_records { assert!(s.hulls.iter().any(|h| h.id == *r && h.alive)); assert!(seen.insert(*r)); }
        // The depot site is there and the briefing agrees with the config.
        assert!(g.config.vessels.iter().any(|v| v.role == VesselRole::Site && v.tag == "depot"));
        assert_eq!(g.briefing.node, id("D"));
        assert_eq!(g.briefing.weather, g.config.weather);
        assert_eq!(briefing(&s, &d, &op), Some(g.briefing.clone()));
        assert_eq!(g.config.players[0].vessel, Some(0));
        // Same state, same sortie.
        assert_eq!(generate(&s, &d, &op, &[PlayerSlot(0)]), Some(g));
    }

    #[test]
    fn coop_generation_meets_the_multiples() {
        let (mut s, d) = started_gen();
        s.players.push(player());
        refresh_offers(&mut s, &d);
        let op = OperationId::new("op.raid");
        for rung in [0u8, 2] {
            s.last_host_rung = rung;
            let g = generate(&s, &d, &op, &[PlayerSlot(0), PlayerSlot(1)]).unwrap();
            assert!(commitment_ok(&g, Act::Act1, 2, &d), "rung {rung}: {} hulls {} t", g.committed_hulls, g.committed_tonnage_t);
            assert_eq!(g.config.players.iter().filter(|p| p.vessel.is_some()).count(), 2);
            if rung >= 2 { assert!(g.config.vessels.iter().filter(|v| v.class.is_some()).all(|v| v.veterancy >= 0.1 || v.role == VesselRole::Site)); }
        }
    }

    #[test]
    fn repeat_cap_never_deadlocks_and_never_repeats_in_a_row() {
        let (mut s, d) = started_gen();
        let mut history: Vec<OperationId> = Vec::new();
        for step in 0..200u64 {
            s.hour = step;
            refresh_offers(&mut s, &d);
            let templates: Vec<OperationId> = s.offered.iter().filter(|o| matches!(d.campaign.operations[*o].kind, OperationKind::Template(_))).cloned().collect();
            assert!(!templates.is_empty(), "no template on offer at step {step}");
            // Hourly refresh with nothing played changes nothing.
            let before = (s.offered.clone(), s.ops.offered.clone());
            refresh_offers(&mut s, &d);
            assert_eq!(before, (s.offered.clone(), s.ops.offered.clone()));
            // Play the first template on offer.
            let played = templates[0].clone();
            if let Some(last) = history.last() { assert_ne!(last, &played, "twice in a row at step {step}"); }
            history.push(played.clone());
            s.offered.retain(|o| o != &played);
            s.ops.last_offered = Some(played);
        }
    }

    #[test]
    fn region_weather_is_coherent_and_legal() {
        let d = gen_data();
        let mut changes = 0;
        let mut prev = region_weather(5, &rid("r1"), 0, &d);
        for h in 0..24 * 10 {
            let w = region_weather(5, &rid("r1"), h, &d);
            assert_eq!(w, region_weather(5, &rid("r1"), h, &d));
            let (lo, hi) = w.kind.sea_state_range();
            assert!(w.sea_state.0 >= lo && w.sea_state.0 <= hi);
            assert_eq!(w.sun_elevation_deg, 30.0);
            assert!(w.wind_dir_rad >= 0.0 && w.wind_dir_rad < core::f64::consts::TAU);
            if w.kind != prev.kind { changes += 1; }
            if h % WEATHER_BLOCK_H != 0 { assert_eq!(w.kind, prev.kind); }
            prev = w;
        }
        assert!(changes < 40, "weather flickers: {changes} changes in 10 days");
    }

    #[test]
    fn forced_hunter_operation_is_alone() {
        let (mut s, d) = started_gen();
        refresh_offers(&mut s, &d);
        // Put a hunter group at SEXTANT's cove.
        let hulls: Vec<u32> = s.hulls.iter().filter(|h| h.class == ClassId::new("frig")).take(3).map(|h| h.id).collect();
        s.groups.push(content::save::TaskGroupState { id: 999, hulls: hulls.clone(), node: s.sextant_node.clone(), destination: None, order: content::save::GroupOrder::HuntSextant, arrive_hour: None, belief: vec![] });
        s.hunter_group = Some(999);
        refresh_offers(&mut s, &d);
        assert_eq!(s.offered, vec![OperationId::new("op.hunt")]);
        let o = offers(&s, &d);
        assert!(o[0].forced);
        let g = generate(&s, &d, &OperationId::new("op.hunt"), &[PlayerSlot(0)]).unwrap();
        assert!(g.briefing.forced);
        for h in &hulls { assert!(g.hull_records.contains(h)); }
    }

    fn real_data() -> TheatreData {
        let catalogue = std::sync::Arc::new(content::catalogue::Catalogue::load_embedded().unwrap());
        let campaign = std::sync::Arc::new(content::campaign::load::load_embedded(&catalogue).unwrap());
        TheatreData { campaign, catalogue }
    }

    /// Every node map an Act 1 operation can be generated on: its authored map or node, the nodes its template
    /// fits, and every node for hunter-group survival and ace duels, which follow a group.
    fn act1_maps<'d>(op: &OperationDef, data: &'d TheatreData) -> Vec<(Option<&'d content::campaign::NodeDef>, MapDef)> {
        let c = &data.campaign;
        if let Some(m) = &op.map { return vec![(None, m.clone())]; }
        if let Some(id) = &op.node { return c.theatre.nodes.iter().filter(|n| &n.id == id).map(|n| (Some(n), n.map.clone())).collect(); }
        c.theatre.nodes.iter().filter(|n| match op.kind {
            OperationKind::Template(MissionType::HunterGroupSurvival) | OperationKind::AceDuel(_) => true,
            OperationKind::Template(t) => fits(t, &n.kind, has_traffic(data, &n.id)),
            _ => fits(MissionType::Feint, &n.kind, has_traffic(data, &n.id)),
        }).map(|n| (Some(n), n.map.clone())).collect()
    }

    fn act1_ops(data: &TheatreData) -> Vec<&OperationDef> {
        let mut ops: Vec<&OperationDef> = data.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial).collect();
        ops.sort_by(|a, b| a.id.cmp(&b.id));
        ops
    }

    fn act1_spec<'a>(op: &'a OperationDef, node: Option<&'a content::campaign::NodeDef>, map: MapDef, seed: u64, starter: &str, data: &'a TheatreData) -> Spec<'a> {
        let region = node.and_then(|n| data.campaign.theatre.regions.iter().find(|r| r.id == n.region));
        let seats = vec![Seat { slot: PlayerSlot(0), client_id: 0, name: String::new(), hull: Some(SeatHull::Stock(Some(starter.to_string()))) }];
        Spec {
            op, act: Act::Act1, seed, hour: 12, weather: WeatherState::default(), map, region, region_id: region.map(|r| r.id.clone()), node: node.map(|n| n.id.clone()),
            seats, difficulty: DifficultySliders::NORMAL, ironman: false, enemy_per_player: None, alert: 0, cards: Vec::new(), rung: 0, state: None,
        }
    }

    const PATROL: &str = "starters/patrol15.bp.ron";
    const FAC: &str = "starters/fac22.bp.ron";

    fn approach_of(op: &OperationDef, data: &TheatreData) -> f64 { (if op.approach_m.is_finite() && op.approach_m > 0.0 { op.approach_m } else { 10_000.0 }) * approach_scale(op, data) }

    /// Real data, every Act 1 operation on every node map it can be generated on.
    /// - An extraction, an escort, a cutting-out and a feint: every exit lies within half the approach distance
    ///   of the objective point, on a straight leg with at least 3 m of water at every sample, and the map has
    ///   an exit (DECISION of 22 November 2026 in `finish`).
    /// - Every other objective: each exit is either where the theatre data puts it or at least the approach
    ///   distance from the player spawn, and hunter-group survival's exit never moves (DECISION r3).
    #[test]
    fn act1_exits_follow_the_objective_and_the_spawn_guard() {
        let data = real_data();
        let mut checked = 0u32;
        let mut legs = 0u32;
        let mut pulled = 0u32;
        let mut hunter_maps = 0u32;
        for op in act1_ops(&data) {
            let maps = act1_maps(op, &data);
            assert!(!maps.is_empty(), "{} has no map to generate on", op.id.as_str());
            let survive = matches!(op.objective, Objective::Survive { .. });
            let exit_leg = matches!(op.objective, Objective::Extract { .. } | Objective::Escort { .. } | Objective::CutOut { .. } | Objective::Feint { .. });
            for (node, map) in maps {
                let spec = act1_spec(op, node, map, 3, PATROL, &data);
                // The exits as the theatre data puts them (the node map's own, else the generator's default).
                let mut before = Build { s: &spec, data: &data, r: Rng { seed: spec.seed }, map: sanitize_map(&spec.map), units: Vec::new(), groups: Vec::new(), draws: 0, variants: false };
                before.ensure_markers(objective_marker_name(&op.objective).as_deref());
                let g = build(&spec, &data);
                let v = g.config.players[0].vessel.expect("a fielded hull") as usize;
                let spawn = [g.config.vessels[v].pos.x, g.config.vessels[v].pos.z];
                let approach = approach_of(op, &data);
                let where_ = node.map_or("authored map", |n| n.id.as_str());
                let exits: Vec<&Marker> = g.config.map.markers.iter().filter(|m| m.kind == MarkerKind::Exit).collect();
                if exit_leg {
                    assert!(!exits.is_empty(), "{} on {where_}: no exit to leave by", op.id.as_str());
                    let obj = g.briefing.route[1];
                    let terrain = sim_core::terrain::Terrain::new(g.config.map.clone());
                    let reach = EXIT_LEG_FRACTION * approach;
                    for m in exits {
                        let d = libm::sqrt(d2(m.pos_m, obj));
                        assert!(d <= reach + 1.0, "{} on {where_}: exit {} is {d:.0} m from the objective point, over half the approach {approach:.0} m", op.id.as_str(), m.name);
                        let shallow = shallow_samples(&terrain, obj, m.pos_m);
                        assert_eq!(shallow, 0, "{} on {where_}: the leg from {obj:?} to exit {} at {:?} has {shallow} samples under 3 m of water", op.id.as_str(), m.name, m.pos_m);
                        if before.map.markers.iter().find(|b| b.name == m.name).map(|b| b.pos_m) != Some(m.pos_m) { pulled += 1; }
                        legs += 1;
                    }
                    // The same map whatever the seed and the hull: the exit depends on the map and the objective.
                    let other = build(&act1_spec(op, node, spec.map.clone(), 77, FAC, &data), &data);
                    let exit_of = |g: &Generated| g.config.map.markers.iter().filter(|m| m.kind == MarkerKind::Exit).map(|m| m.pos_m).collect::<Vec<_>>();
                    assert_eq!(exit_of(&g), exit_of(&other), "{} on {where_}", op.id.as_str());
                    continue;
                }
                for m in exits {
                    let src = before.map.markers.iter().find(|b| b.name == m.name).map(|b| b.pos_m).expect("every exit comes from the data or the defaults");
                    let unmoved = src == m.pos_m;
                    if survive { assert!(unmoved, "{} on {where_}: Survive exit {} moved from {src:?} to {:?}", op.id.as_str(), m.name, m.pos_m); }
                    let far = libm::sqrt(d2(m.pos_m, spawn)) >= approach - 1e-6;
                    assert!(unmoved || far, "{} on {where_}: exit {} pulled to {:?}, {:.0} m from the spawn, under the approach {approach:.0} m", op.id.as_str(), m.name, m.pos_m, libm::sqrt(d2(m.pos_m, spawn)));
                    checked += 1;
                }
                if survive && node.is_some_and(|n| n.id.as_str() == "cove.wren") {
                    hunter_maps += 1;
                    let exit = g.config.map.markers.iter().find(|m| m.name == "exit").expect("cove.wren has an exit");
                    assert_eq!(exit.pos_m, [-18_000.0, -14_000.0], "hunter_survival's exit on cove.wren stays where the theatre puts it");
                }
            }
        }
        assert!(checked > 0);
        assert!(legs > 0 && pulled > 0, "{legs} exit legs checked, {pulled} pulled in");
        assert!(hunter_maps > 0, "hunter_survival was checked on cove.wren");
    }

    /// Real data: a convoy in transit starts on its lane `approach x convoy speed / player speed` short of the
    /// lane point nearest the objective, and its escort starts with it. 200 seeds per map (the convoy's classes
    /// are drawn), with the fast attack craft, whose 19 kn cruise against a 14.6 kn convoy puts the convoy
    /// raid's start about 10.8 km up the lane.
    #[test]
    fn act1_convoys_start_where_they_meet_the_players() {
        let data = real_data();
        let fac = stock_speeds(FAC, &data).expect("the fast attack craft has a speed");
        assert!((18.0..=20.0).contains(&fac.cruise_kn), "fast attack craft cruise {:.1} kn", fac.cruise_kn);
        let (mut checked, mut moved, mut escorts) = (0u32, 0u32, 0u32);
        for id in ["op.act1.convoy_raid", "op.act1.prize_capture", "op.act1.story.kessel_toll"] {
            let op = &data.campaign.operations[&OperationId::new(id)];
            for (node, map) in act1_maps(op, &data) {
                let where_ = node.map_or("authored map", |n| n.id.as_str());
                for seed in 0..200u64 {
                    let g = build(&act1_spec(op, node, map.clone(), seed, FAC, &data), &data);
                    let c = &g.config;
                    let find = |k: MarkerKind| c.map.markers.iter().find(|m| m.kind == k).map(|m| m.pos_m).expect("lane markers");
                    let (a, b) = (find(MarkerKind::ConvoyEntry), find(MarkerKind::ConvoyExit));
                    let len = libm::sqrt(d2(a, b));
                    let u = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
                    let along = |p: [f64; 2]| (p[0] - a[0]) * u[0] + (p[1] - a[1]) * u[1];
                    let obj = g.briefing.route[1];
                    let nearest = along(obj).clamp(0.0, len);
                    let approach = approach_of(op, &data);
                    let leader = |gid: GroupId| c.vessels.iter().find(|v| v.group == Some(gid)).map(|v| [v.pos.x, v.pos.z]).expect("a group has a hull");
                    let mut convoy_at: BTreeMap<String, f64> = BTreeMap::new();
                    for gr in c.groups.iter().filter(|x| matches!(x.orders, GroupOrders::Transit { .. })) {
                        let slowest = c.vessels.iter().filter(|v| v.group == Some(gr.id)).filter_map(|v| v.class.as_ref()).filter_map(|cl| stock_speeds(&data.campaign.classes[cl].blueprint, &data)).map(|f| f.top_kn).fold(f64::INFINITY, f64::min);
                        let lead = approach * (slowest * CONVOY_SPEED_FRACTION / fac.cruise_kn).clamp(CONVOY_LEAD_RATIO_BOUNDS.0, CONVOY_LEAD_RATIO_BOUNDS.1);
                        let want = (nearest - lead).clamp(0.0, len);
                        let at = along(leader(gr.id));
                        assert!((at - want).abs() <= 400.0, "{id} on {where_} seed {seed}: group {:?} starts {at:.0} m up the lane, expected {want:.0} m (lead {lead:.0} m, nearest point {nearest:.0} m)", gr.tag);
                        if gr.kind == FleetKind::Convoy && id == "op.act1.convoy_raid" && gr.tag == "convoy" {
                            assert!((10_000.0..=11_600.0).contains(&lead), "{id} on {where_} seed {seed}: lead {lead:.0} m is not near the 10.8 km of a 14.6 kn convoy and a 19 kn raider");
                        }
                        if want > 1_000.0 { moved += 1; }
                        convoy_at.insert(gr.tag.clone(), at);
                        checked += 1;
                    }
                    for gr in &c.groups {
                        let GroupOrders::Escort { tag } = &gr.orders else { continue };
                        let Some(want) = convoy_at.get(tag) else { continue };
                        let at = along(leader(gr.id));
                        assert!((at - want).abs() <= 2_500.0, "{id} on {where_} seed {seed}: escort {:?} starts {at:.0} m up the lane, its convoy {want:.0} m", gr.tag);
                        escorts += 1;
                    }
                }
            }
        }
        assert!(checked > 0 && moved > 0 && escorts > 0, "{checked} convoys checked, {moved} started up the lane, {escorts} escorts");
    }

    /// Real data: the player hull sails in at the operation's authored emission state and at no inferred one,
    /// while the briefing still recommends Silent wherever a ghost condition forbids emissions or detection.
    #[test]
    fn act1_spawn_emission_state_is_the_authored_one() {
        let data = real_data();
        let (mut authored, mut inferred) = (Vec::new(), 0u32);
        for op in act1_ops(&data) {
            let (node, map) = act1_maps(op, &data).into_iter().next().expect("a map");
            let g = build(&act1_spec(op, node, map, 3, FAC, &data), &data);
            let hull = &g.config.vessels[g.config.players[0].vessel.expect("a fielded hull") as usize];
            assert_eq!(hull.emcon, op.recommended_emcon, "{}: the hull sails in at the authored setting only", op.id.as_str());
            let forbids = op.ghosts.iter().any(|x| matches!(x.kind, GhostKind::NoRadarEmissions | GhostKind::NeverDetected));
            let shown = op.recommended_emcon.or(forbids.then_some(Emcon::Silent));
            assert_eq!((g.briefing.recommended_emcon, g.config.intel.recommended_emcon), (shown, shown), "{}: the briefing's recommendation", op.id.as_str());
            if op.recommended_emcon.is_some() { authored.push(op.id.as_str().to_string()); } else if forbids { inferred += 1; }
        }
        // The operations a stock hull finishes without gunfire keep their authored silence; the rest sail in
        // at the design's own setting.
        assert_eq!(authored, ["op.act1.agent_extraction", "op.act1.minelaying", "op.act1.photo_reconnaissance", "op.act1.story.quiet_hand"]);
        assert!(inferred >= 6, "{inferred} operations recommend Silent without applying it");
    }

    /// Real data: the hulls a destroy-group objective names are briefing-named (Exact at first contact, L475)
    /// and declared Hostile (L498); no other objective names its convoy.
    #[test]
    fn act1_destroy_group_hulls_are_named_and_declared_hostile() {
        let data = real_data();
        let mut seen = 0u32;
        for op in act1_ops(&data) {
            let Objective::DestroyGroup { tag, .. } = &op.objective else { continue };
            let (node, map) = act1_maps(op, &data).into_iter().next().expect("a map");
            for seed in 0..20u64 {
                let g = build(&act1_spec(op, node, map.clone(), seed, FAC, &data), &data);
                let combine = g.config.sides.iter().find(|s| s.faction == Faction::Combine).expect("the Combine").id;
                let targets: Vec<usize> = g.config.vessels.iter().enumerate().filter(|(_, v)| v.side == combine && v.tag == *tag).map(|(i, _)| i).collect();
                assert!(!targets.is_empty(), "{} seed {seed}: no hull carries {tag:?}", op.id.as_str());
                for i in targets {
                    assert_eq!(g.config.vessels[i].declared, Some(Stance::Hostile), "{} seed {seed}: vessel {i}", op.id.as_str());
                    assert!(g.config.intel.named.contains(&(i as u16)), "{} seed {seed}: vessel {i} is not named", op.id.as_str());
                    seen += 1;
                }
                // Escorts and decoys are not the objective's: they are not named for it.
                for (i, v) in g.config.vessels.iter().enumerate().filter(|(_, v)| v.role == VesselRole::Combatant && v.tag != *tag && v.side == combine) {
                    let ace = v.group.is_some_and(|gid| g.config.groups.iter().any(|x| x.id == gid && x.ace.is_some()));
                    assert!(ace || !g.config.intel.named.contains(&(i as u16)), "{} seed {seed}: vessel {i} ({:?}) is named", op.id.as_str(), v.tag);
                }
            }
        }
        assert!(seen > 0);
    }

    /// Real data, 200 seeds on every map of each ace duel: the briefing route ends at the ace group's marker,
    /// the ace starts within its patrol ring of the route's end, the spawn lies on the approach to it, the
    /// briefing chart draws the end as the objective, and the gate's pilot steers for the same point. Before
    /// 22 November 2026 the route ended at the map's first Objective marker, 11 km or more from the ace.
    #[test]
    fn act1_ace_duel_route_ends_at_the_ace() {
        let data = real_data();
        let mut seen = 0u32;
        for op in act1_ops(&data) {
            if !matches!(op.kind, OperationKind::AceDuel(_)) { continue; }
            let Objective::DestroyGroup { tag, .. } = &op.objective else { panic!("{}: a duel destroys the ace", op.id.as_str()) };
            let marker = destroy_group_marker(op, &data).expect("a duel's target is not a convoy");
            for (node, map) in act1_maps(op, &data) {
                let where_ = node.map_or("authored map", |n| n.id.as_str());
                for seed in 0..200u64 {
                    let g = build(&act1_spec(op, node, map.clone(), seed, FAC, &data), &data);
                    let at = g.config.map.markers.iter().find(|m| m.name == marker).expect("the ace's marker").pos_m;
                    assert_eq!(g.briefing.route[1], at, "{} on {where_} seed {seed}: the route ends at the ace's marker", op.id.as_str());
                    let combine = g.config.sides.iter().find(|s| s.faction == Faction::Combine).expect("the Combine").id;
                    let ace = g.config.vessels.iter().filter(|v| v.side == combine && v.tag == *tag).map(|v| libm::sqrt(d2([v.pos.x, v.pos.z], at))).fold(f64::INFINITY, f64::min);
                    assert!(ace <= 3_000.0, "{} on {where_} seed {seed}: the ace starts {ace:.0} m from the route's end", op.id.as_str());
                    assert!((g.briefing.distance_km * 1000.0 - libm::sqrt(d2(g.briefing.route[0], at))).abs() < 1.0, "{} on {where_} seed {seed}: the approach is measured to the ace", op.id.as_str());
                    let drawn = g.briefing.map.markers.iter().find(|m| m.pos_m == at).map(|m| m.kind);
                    assert_eq!(drawn, Some(MarkerKind::Objective), "{} on {where_} seed {seed}: the chart draws the route's end", op.id.as_str());
                    let setup = crate::theatre::abstract_combat::PlayerSetup { reference_setup: None, method: None, emcon: None, ghosts: false, follow_briefing_route: true };
                    let pilot = crate::theatre::abstract_combat::Pilot::new(&g.config, &setup, &data);
                    assert_eq!(pilot.objective_pos(), glam::DVec2::from(at), "{} on {where_} seed {seed}: the pilot steers for the briefing's end", op.id.as_str());
                    seen += 1;
                }
            }
        }
        assert!(seen >= 400, "{seen} duel sorties checked");
    }

    /// Real data: a destroy-group whose targets are a convoy keeps its objective point (the convoy is put on
    /// its lane to pass it), and the pilot agrees with the briefing route.
    #[test]
    fn act1_convoy_destroy_group_keeps_its_point() {
        let data = real_data();
        let mut seen = 0u32;
        for op in act1_ops(&data) {
            if !matches!(op.objective, Objective::DestroyGroup { .. }) || matches!(op.kind, OperationKind::AceDuel(_)) { continue; }
            assert_eq!(destroy_group_marker(op, &data), None, "{}: its targets are a convoy", op.id.as_str());
            for (node, map) in act1_maps(op, &data) {
                for seed in 0..10u64 {
                    let g = build(&act1_spec(op, node, map.clone(), seed, FAC, &data), &data);
                    let first = g.config.map.markers.iter().find(|m| matches!(m.kind, MarkerKind::Objective | MarkerKind::DepotSite | MarkerKind::RadarSite | MarkerKind::Berth)).map(|m| m.pos_m);
                    if let Some(p) = first { assert_eq!(g.briefing.route[1], p, "{} seed {seed}", op.id.as_str()); }
                    let setup = crate::theatre::abstract_combat::PlayerSetup { reference_setup: None, method: None, emcon: None, ghosts: false, follow_briefing_route: true };
                    let pilot = crate::theatre::abstract_combat::Pilot::new(&g.config, &setup, &data);
                    assert_eq!(pilot.objective_pos(), glam::DVec2::from(g.briefing.route[1]), "{} seed {seed}", op.id.as_str());
                    seen += 1;
                }
            }
        }
        assert!(seen > 0);
    }

    /// Real data: a cutting-out measures from the marker its prize lies at (the berth), so the briefing route
    /// ends there and the spawn lies on the line through it; every other objective keeps its point.
    #[test]
    fn act1_cutting_out_measures_from_the_prize() {
        let data = real_data();
        let mut seen = 0u32;
        for op in act1_ops(&data) {
            let Objective::CutOut { tag, .. } = &op.objective else { continue };
            let marker = &op.force.iter().find(|f| f.tag == *tag).expect("the prize's force entry").marker;
            for (node, map) in act1_maps(op, &data) {
                let where_ = node.map_or("authored map", |n| n.id.as_str());
                let g = build(&act1_spec(op, node, map, 3, FAC, &data), &data);
                let berth = g.config.map.markers.iter().find(|m| m.name == *marker).expect("the prize's marker").pos_m;
                assert_eq!(g.briefing.route[1], berth, "{} on {where_}: the route ends at the prize", op.id.as_str());
                let prize = g.config.vessels.iter().find(|v| v.role == VesselRole::Prize).expect("a prize");
                assert!(libm::sqrt(d2([prize.pos.x, prize.pos.z], berth)) <= 1_500.0, "{} on {where_}: the prize lies at its marker", op.id.as_str());
                assert!((g.briefing.distance_km * 1000.0 - libm::sqrt(d2(g.briefing.route[0], berth))).abs() < 1.0, "{} on {where_}: the approach is measured to the prize", op.id.as_str());
                seen += 1;
            }
        }
        assert!(seen > 0);
    }
}
