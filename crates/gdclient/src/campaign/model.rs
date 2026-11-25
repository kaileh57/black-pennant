//! Part of unit `gd-campaign`. Plain-Rust campaign logic behind the facade, free of Godot types so it is
//! tested headless: player slots, cove service quotes, hull summaries, Intel spends on offer, the transit
//! preview, the doctrine bulletin, act facts, the theatre log and played time.
//!
//! The theatre rules themselves stay in `sim_systems::theatre`. Where a figure comes from a rule that is
//! private to that crate (rearm and rebuild prices, store capacities), it is read by running the public rule
//! on a copy of the ledger with the Salvage limit lifted, the way `Theatre::validate` probes, so the quote
//! is always the price the rule will charge.
use content::campaign::{DifficultySliders, NodeKind, OperationKind};
use content::enums::{Act, Tier};
use content::ids::{CardId, OperationId, PartId, PlayerSlot, RegionId, TheatreNodeId};
use content::save::{IntelSpend, TheatreState};
use sim_systems::protocol::{Msg, MsgArg, Unit};
use sim_systems::theatre::{economy, logistics, opgen, progress, ChartView, TheatreData};
use std::collections::{BTreeMap, VecDeque};

/// The theatre slot of the player with this client id; a save made without an identity file (client id
/// 1, or any id not in the save) resolves to player 0.
pub(crate) fn local_slot(state: &TheatreState, client_id: u64) -> PlayerSlot {
    PlayerSlot(state.players.iter().position(|p| p.client_id == client_id).unwrap_or(0).min(u8::MAX as usize) as u8)
}

fn record(state: &TheatreState, p: PlayerSlot) -> Option<&content::save::PlayerRecord> { state.players.get(p.0 as usize) }

/// A copy of the ledger on which a service may be run to read its price.
fn probe(state: &TheatreState) -> TheatreState {
    let mut s = state.clone();
    s.resources.salvage = u32::MAX / 2;
    s
}

/// Rounds each store slot holds when full: what a rearm loads (missing slots in a record are full).
fn capacities(state: &TheatreState, p: PlayerSlot, data: &TheatreData) -> Option<BTreeMap<content::ids::PartSlot, u32>> {
    let mut s = probe(state);
    if let Some(r) = s.players.get_mut(p.0 as usize) { r.hull_lost = false; }
    s.sextant_refit_until_hour = None;
    economy::rearm(&mut s, p, data, &mut Vec::new()).ok()?;
    s.players.get(p.0 as usize).map(|r| r.ammo.clone())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RepairQuote { pub salvage: u32, pub hours: u32, pub needed: bool }
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RearmQuote { pub salvage: u32, pub from_stock: bool, pub needed: bool }
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RebuildQuote { pub salvage: u32, pub hours: u32, pub needed: bool, pub allowed: bool }
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ServiceQuotes { pub repair: RepairQuote, pub rearm: RearmQuote, pub rebuild: RebuildQuote }

/// Prices of the cove services for one player's hull now (L811-828). Repairs take no theatre time.
pub(crate) fn service_quotes(state: &TheatreState, p: PlayerSlot, ironman: bool, data: &TheatreData) -> ServiceQuotes {
    let mut q = ServiceQuotes::default();
    let Some(rec) = record(state, p) else { return q };
    if !rec.hull_lost {
        q.repair = RepairQuote { salvage: economy::repair_cost(state, p, data), hours: 0, needed: !rec.damage.parts.is_empty() || !rec.damage.cells.is_empty() };
        let mut s = probe(state);
        s.sextant_refit_until_hour = None;
        if let Ok(cost) = economy::rearm(&mut s, p, data, &mut Vec::new()) {
            let after = &s.players[p.0 as usize];
            let short_ammo = after.ammo.iter().any(|(slot, cap)| rec.ammo.get(slot).copied().unwrap_or(*cap) < *cap);
            let short_fuel = after.fuel_fraction > rec.fuel_fraction + 1e-9;
            q.rearm = RearmQuote { salvage: cost, from_stock: cost == 0, needed: short_ammo || short_fuel };
        }
    } else {
        let mut s = probe(state);
        s.sextant_refit_until_hour = None;
        let cost = economy::rebuild(&mut s, p, false, data, &mut Vec::new()).unwrap_or(0);
        q.rebuild = RebuildQuote { salvage: cost, hours: data.campaign.rules.rebuild_hours, needed: true, allowed: !ironman };
    }
    q
}

/// The figures `Campaign.players()` shows for a hull: `damage` is the repair bill as a share of the hull's
/// build cost (0 = intact, 1 = everything to replace), `ammo` the share of full-load rounds aboard.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HullSummary { pub damage: f64, pub fuel: f64, pub ammo: f64 }

pub(crate) fn hull_summary(state: &TheatreState, p: PlayerSlot, data: &TheatreData) -> HullSummary {
    let Some(rec) = record(state, p) else { return HullSummary { damage: 0.0, fuel: 0.0, ammo: 0.0 } };
    if rec.hull_lost { return HullSummary { damage: 1.0, fuel: 0.0, ammo: 0.0 }; }
    let build = content::stats::compute_stats(&rec.blueprint, &data.catalogue).cost.max(1) as f64;
    let damage = (economy::repair_cost(state, p, data) as f64 / build).clamp(0.0, 1.0);
    let fuel = if rec.fuel_fraction.is_finite() { rec.fuel_fraction.clamp(0.0, 1.0) } else { 0.0 };
    let ammo = match capacities(state, p, data) {
        Some(caps) if caps.values().any(|c| *c > 0) => {
            let total: u64 = caps.values().map(|c| *c as u64).sum();
            let aboard: u64 = caps.iter().map(|(slot, cap)| rec.ammo.get(slot).copied().unwrap_or(*cap).min(*cap) as u64).sum();
            aboard as f64 / total as f64
        }
        _ => 1.0,
    };
    HullSummary { damage, fuel, ammo }
}

/// One Intel spend on offer and its price.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct IntelOption { pub spend: IntelSpend, pub cost: u8 }

/// Every Intel spend that has a target now (L811-822): delay or cancel each queued card, reveal the convoys
/// of each open region, locate each ace whose ship is on the chart and not yet located. Prices come from the
/// campaign rules; whether the pool can pay is the caller's comparison.
pub(crate) fn intel_options(state: &TheatreState, data: &TheatreData) -> Vec<IntelOption> {
    let rules = &data.campaign.rules;
    let mut out = Vec::new();
    for def in &data.campaign.theatre.regions {
        let Some(r) = state.regions.get(&def.id) else { continue };
        let mut queued: Vec<_> = r.queued_cards.iter().collect();
        queued.sort_by_key(|q| (q.arrives_hour, q.card.clone()));
        for q in queued {
            out.push(IntelOption { spend: IntelSpend::DelayCard { region: def.id.clone(), card: q.card.clone() }, cost: rules.intel_delay_card });
            out.push(IntelOption { spend: IntelSpend::CancelCard { region: def.id.clone(), card: q.card.clone() }, cost: rules.intel_cancel_card });
        }
    }
    for def in &data.campaign.theatre.regions {
        if region_open(state, data, &def.id) {
            out.push(IntelOption { spend: IntelSpend::RevealConvoys(def.id.clone()), cost: rules.intel_reveal_convoys });
        }
    }
    let mut aces: Vec<&content::ids::AceId> = state.hulls.iter().filter(|h| h.alive).filter_map(|h| h.ace.as_ref()).filter(|a| data.campaign.aces.contains_key(*a) && !state.ops.aces_revealed.contains(a) && !state.ops.aces_beaten.contains(a)).collect();
    aces.sort();
    aces.dedup();
    for a in aces { out.push(IntelOption { spend: IntelSpend::RevealAce(a.clone()), cost: rules.intel_reveal_ace }); }
    out
}

/// A region is open to SEXTANT once its act has come (every region in Open Theatre); unknown regions are
/// open. The same rule as the theatre's lane filter.
pub(crate) fn region_open(state: &TheatreState, data: &TheatreData, region: &RegionId) -> bool {
    state.open_theatre || data.campaign.theatre.regions.iter().find(|r| &r.id == region).is_none_or(|r| r.act <= state.act)
}

/// SEXTANT's shortest route between two nodes over lanes whose ends are both in open regions: distance in
/// km and the nodes passed, both ends included. Dijkstra with ties to the lowest node index, as the theatre.
pub(crate) fn transit_path(state: &TheatreState, data: &TheatreData, from: &TheatreNodeId, to: &TheatreNodeId) -> Option<(f64, Vec<TheatreNodeId>)> {
    let nodes = &data.campaign.theatre.nodes;
    let idx = |id: &TheatreNodeId| nodes.iter().position(|n| &n.id == id);
    let (f, t) = (idx(from)?, idx(to)?);
    if f == t { return Some((0.0, vec![from.clone()])); }
    let n = nodes.len();
    let open: Vec<bool> = nodes.iter().map(|nd| region_open(state, data, &nd.region)).collect();
    let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut seen = std::collections::BTreeSet::new();
    for l in &data.campaign.theatre.lanes {
        let (Some(a), Some(b)) = (idx(&l.a), idx(&l.b)) else { continue };
        if a == b || !seen.insert((a.min(b), a.max(b))) || !(open[a] && open[b]) { continue; }
        let (pa, pb) = (nodes[a].pos_km, nodes[b].pos_km);
        // Square root is correctly rounded on every platform, so this equals the theatre's `libm::sqrt`.
        let km = ((pa[0] - pb[0]) * (pa[0] - pb[0]) + (pa[1] - pb[1]) * (pa[1] - pb[1])).sqrt();
        let km = if km.is_finite() { km } else { 0.0 };
        adj[a].push((b, km));
        adj[b].push((a, km));
    }
    let mut d = vec![f64::INFINITY; n];
    let mut prev = vec![usize::MAX; n];
    let mut done = vec![false; n];
    d[f] = 0.0;
    loop {
        let mut best = usize::MAX;
        for i in 0..n { if !done[i] && d[i].is_finite() && (best == usize::MAX || d[i] < d[best]) { best = i; } }
        if best == usize::MAX { break; }
        done[best] = true;
        for &(to, km) in &adj[best] {
            let nd = d[best] + km;
            if nd < d[to] { d[to] = nd; prev[to] = best; }
        }
    }
    if !d[t].is_finite() { return None; }
    let mut path = vec![t];
    let mut at = t;
    while at != f { at = prev[at]; if at == usize::MAX || path.len() > n { return None; } path.push(at); }
    path.reverse();
    Some((d[t], path.into_iter().map(|i| nodes[i].id.clone()).collect()))
}

/// What `Campaign.transit_preview` shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransitPreview { pub reachable: bool, pub hours: u32, pub path: Vec<TheatreNodeId>, pub passes_cut_lane: bool, pub text: Msg }

/// The transit SEXTANT would make to `target` now, with the theatre's refusal rules for a transit (forced
/// operation pending, already under way, SEXTANT refitting, not an open cove, no open route).
pub(crate) fn transit_preview(state: &TheatreState, data: &TheatreData, target: &TheatreNodeId, under_way: bool, node_name: MsgArg) -> TransitPreview {
    let refuse = |key: &str| TransitPreview { reachable: false, hours: 0, path: Vec::new(), passes_cut_lane: false, text: Msg::new(key) };
    if *target == state.sextant_node { return TransitPreview { reachable: false, hours: 0, path: vec![target.clone()], passes_cut_lane: false, text: Msg::new("campaign.transit.here") }; }
    if opgen::offers(state, data).iter().any(|o| o.forced) { return refuse("campaign.transit.forced"); }
    if under_way { return refuse("campaign.transit.under_way"); }
    if state.sextant_refit_until_hour.is_some_and(|h| h > state.hour) { return refuse("campaign.transit.sextant_refit"); }
    if !state.open_coves.contains(target) { return refuse("campaign.transit.not_cove"); }
    let (Some(hours), Some((_, path))) = (logistics::transit_hours(state, data, &state.sextant_node, target, false), transit_path(state, data, &state.sextant_node, target)) else { return refuse("campaign.transit.no_route") };
    let hours = hours.max(1);
    let cut = path.windows(2).any(|w| {
        let key = if w[0] <= w[1] { (w[0].clone(), w[1].clone()) } else { (w[1].clone(), w[0].clone()) };
        state.lanes.get(&key).and_then(|l| l.cut_until_hour).is_some_and(|u| u > state.hour)
    });
    let text = Msg::new(if cut { "campaign.transit.hours_cut" } else { "campaign.transit.hours" }).num(hours as f64, Unit::Hours).arg(node_name);
    TransitPreview { reachable: true, hours, path, passes_cut_lane: cut, text }
}

/// Parts the campaign dry dock offers now: every part of an open tier plus blueprint unlocks, filtered by
/// act and tier as the catalogue does for the campaign (the same list a refit is validated against).
pub(crate) fn fieldable_parts(state: &TheatreState, data: &TheatreData) -> Vec<PartId> {
    let tier = progress::open_tier(state, data);
    let act = if state.open_theatre { Act::Act4 } else { state.act };
    let mut wanted: Vec<PartId> = data.catalogue.parts.values().filter(|p| p.tier <= tier).map(|p| p.id.clone()).collect();
    for p in &state.resources.unlocked_parts { if !wanted.contains(p) { wanted.push(p.clone()); } }
    data.catalogue.available_parts(content::enums::GameMode::Campaign, act, tier, &wanted).into_iter().filter_map(|i| data.catalogue.parts.get_index(i.index()).map(|(id, _)| id.clone())).collect()
}

/// The act whose rules apply: Act 4 once Open Theatre is open.
pub(crate) fn rules_act(state: &TheatreState) -> Act { if state.open_theatre { Act::Act4 } else { state.act } }

/// Parts a refit of `fielded` may carry: what the dock offers (`fieldable_parts`) plus every part the
/// detachment already owns, fitted to the hull it fields now or held in SEXTANT's parts store. A starter may
/// carry parts from a tier the campaign has not opened yet; the player keeps them and may move or refit them,
/// but buys new copies only of offered parts. One list serves the dry dock and the refit check, so the dock
/// never blocks a design the theatre would take, or the reverse.
pub(crate) fn refit_parts(state: &TheatreState, data: &TheatreData, fielded: &content::blueprint::Blueprint) -> Vec<PartId> {
    let mut allowed = fieldable_parts(state, data);
    let owned = fielded.parts.iter().map(|p| &p.part).chain(state.resources.parts_store.iter().filter(|(_, n)| **n > 0).map(|(p, _)| p));
    for p in owned { if !allowed.contains(p) { allowed.push(p.clone()); } }
    allowed
}

/// Check a design against the act's campaign ruleset and the parts a refit of `fielded` may carry
/// ([`refit_parts`]). At launch `bp` is the fielded hull itself.
pub(crate) fn validate_fielded(bp: &content::blueprint::Blueprint, fielded: &content::blueprint::Blueprint, state: &TheatreState, data: &TheatreData) -> Result<(), sim_systems::theatre::TheatreReject> {
    use sim_systems::theatre::TheatreReject;
    let ruleset = content::ruleset::Ruleset::campaign(rules_act(state));
    let allowed = refit_parts(state, data, fielded);
    let ctx = content::validate::ValidationContext { ruleset: &ruleset, available: Some(&allowed), enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(bp, &data.catalogue, &ctx);
    if report.stats.tonnage_t > ruleset.tonnage_cap_t + 1e-6 { return Err(TheatreReject::OverTonnageCap); }
    if !report.is_ok() { return Err(TheatreReject::InvalidBlueprint); }
    Ok(())
}

/// One card on the doctrine bulletin (L772).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BulletinCard { pub card: CardId, pub region: RegionId, /// 0 when active.
    pub arrives_hour: u64 }

/// Active cards first (in chart region order), then queued cards soonest first.
pub(crate) fn bulletin(chart: &ChartView) -> Vec<BulletinCard> {
    let mut active = Vec::new();
    let mut queued = Vec::new();
    for r in &chart.regions {
        for c in &r.active_cards { active.push(BulletinCard { card: c.clone(), region: r.region.clone(), arrives_hour: 0 }); }
        for (c, h) in &r.queued_cards { queued.push(BulletinCard { card: c.clone(), region: r.region.clone(), arrives_hour: (*h).max(1) }); }
    }
    queued.sort_by(|a, b| a.arrives_hour.cmp(&b.arrives_hour).then_with(|| a.card.cmp(&b.card)));
    active.extend(queued);
    active
}

/// Facts of the act in play.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActInfo {
    pub act: Act,
    pub cap_t: f64,
    pub retreat: bool,
    /// Days on the operation clock; `None` in Open Theatre.
    pub clock_days_left: Option<f64>,
    pub story_done: usize,
    pub story_total: usize,
    pub capstone_open: bool,
    /// Stock paths of the starter hulls offered so far (this act's first).
    pub starters: Vec<String>,
    pub max_tier: Tier,
    pub open_theatre: bool,
}

pub(crate) fn act_info(state: &TheatreState, data: &TheatreData) -> ActInfo {
    let def = data.campaign.acts.iter().find(|a| a.act == state.act);
    let story: &[OperationId] = def.map_or(&[], |d| d.story_ops.as_slice());
    ActInfo {
        act: state.act,
        cap_t: rules_act(state).hull_cap_t(),
        retreat: state.retreat,
        clock_days_left: state.clock_hours_left.map(|h| if h.is_finite() { (h / 24.0).max(0.0) } else { 0.0 }),
        story_done: story.iter().filter(|o| state.ops.completed.contains(o)).count(),
        story_total: story.len(),
        capstone_open: def.is_some_and(|d| state.offered.contains(&d.capstone)),
        starters: crate::content_api::model::starter_paths(&data.campaign.acts, Some(rules_act(state))),
        max_tier: progress::open_tier(state, data),
        open_theatre: state.open_theatre,
    }
}

/// The preset whose sliders these are, or `""` for a custom mix.
pub(crate) fn preset_of(s: &DifficultySliders, presets: &[content::campaign::DifficultyPreset]) -> String {
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
    presets.iter().find(|p| {
        let q = &p.sliders;
        close(q.ai_accuracy, s.ai_accuracy) && close(q.ai_reaction_s, s.ai_reaction_s) && close(q.adaptation_cadence, s.adaptation_cadence) && q.hull_count == s.hull_count && close(q.economy, s.economy) && q.clock_days == s.clock_days
    }).map(|p| p.id.as_str().to_string()).unwrap_or_default()
}

/// SEXTANT lies at an open cove (and so offers the dry dock and the tender), unless it is under way.
pub(crate) fn at_cove(state: &TheatreState, data: &TheatreData, under_way: bool) -> bool {
    !under_way && (state.open_coves.contains(&state.sextant_node) || data.campaign.theatre.nodes.iter().any(|n| n.id == state.sextant_node && n.kind == NodeKind::Cove))
}

/// The operation kind of an offer for `Bp.OPERATION_*`.
pub(crate) fn operation_kind_gd(k: &OperationKind) -> i64 {
    match k { OperationKind::Template(_) => 0, OperationKind::Story => 1, OperationKind::AceDuel(_) => 2, OperationKind::Capstone => 3, OperationKind::Tutorial => 4 }
}

// ---- Theatre log ----

/// Most theatre events kept for the log.
pub(crate) const LOG_CAP: usize = 400;

/// The theatre log: recent events with the hour they happened at, oldest first.
#[derive(Default)]
pub(crate) struct TheatreLog { pub entries: VecDeque<(u64, sim_systems::theatre::TheatreEvent)> }
impl TheatreLog {
    pub fn push(&mut self, hour: u64, e: sim_systems::theatre::TheatreEvent) {
        if self.entries.len() >= LOG_CAP { self.entries.pop_front(); }
        self.entries.push_back((hour, e));
    }
    /// The newest `limit` entries, oldest first.
    pub fn tail(&self, limit: usize) -> impl Iterator<Item = &(u64, sim_systems::theatre::TheatreEvent)> {
        self.entries.iter().skip(self.entries.len().saturating_sub(limit))
    }
    pub fn clear(&mut self) { self.entries.clear(); }
}

// ---- Played time (L79: pauses over 2 minutes do not count) ----

/// Seconds without player input after which wall time stops counting as played.
pub(crate) const IDLE_LIMIT_S: f64 = 120.0;

/// Wall-clock seconds played, excluding every stretch of more than [`IDLE_LIMIT_S`] without a campaign
/// decision, theatre animation or sortie running. An idle stretch is held back until it ends: up to the
/// limit it counts, beyond it none of it does. Whole seconds are handed to the theatre.
#[derive(Default)]
pub(crate) struct PlayedClock { idle_s: f64, counted_s: f64 }
impl PlayedClock {
    /// Something happened: the idle stretch ends.
    pub fn activity(&mut self) {
        if self.idle_s <= IDLE_LIMIT_S { self.counted_s += self.idle_s; }
        self.idle_s = 0.0;
    }
    /// Advance by one frame (at most 1 s counts per frame); `busy` while time animates or a sortie runs.
    /// Returns whole seconds to add.
    pub fn tick(&mut self, delta_s: f64, busy: bool) -> u64 {
        let d = if delta_s.is_finite() { delta_s.clamp(0.0, 1.0) } else { 0.0 };
        if busy { self.activity(); self.counted_s += d; } else { self.idle_s += d; }
        let whole = self.counted_s.floor();
        self.counted_s -= whole;
        whole as u64
    }
}
