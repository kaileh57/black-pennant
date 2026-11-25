//! Unit `sys-theatre-economy`. Salvage, Intel, blueprints, stores, repairs, refits and losses.
//! DESIGN.md L811-828, L801-807 (refit cost), L894 (gate).
//!
//! | Resource | Source | Sink |
//! | Salvage | wrecks, prizes | parts, refits, repairs, rebuilds, ammunition and fuel shortfalls |
//! | Intel | 1 per ghost condition; 2 per successful photo reconnaissance; 1 per prize crew or per 10 survivors | delay a card 72 h (2); cancel a card (5); reveal a region's convoys for 4 days (1); reveal an ace (3) |
//! | Blueprints | captured or boarded enemy parts | permanent unlock of those parts, within open tiers |
//! | Fuel, ammunition | raided depots and convoys into SEXTANT's stock at no Salvage cost | loaded into tanks and magazines from stock; a shortfall is bought with Salvage |
//! Anchor: a fully fitted 150 t Act 1 hull costs about 1,000 Salvage. A wreck pays 20% of the sunk hull's
//! build cost; a prize sailed home 60%; stripped on the spot 40% plus blueprints for parts not yet unlocked.
//! Repairs cost 50% of the damaged parts' cost. A full ammunition reload is about 5% of hull build cost at the
//! shortfall rate. The Salvage income multiplier is the economy slider. Lost hull: keeps its design, loses
//! its fitted parts; rebuild costs 60% of build cost and 48 theatre hours (Ironman: permanent). SEXTANT sunk:
//! lose 30% of Salvage, fuel and ammunition (Intel and blueprints kept); the region goes to full Combine
//! control (Supply 100, cut lanes restored); SEXTANT refits at the nearest cove in another region for 72
//! hours. Crew veterancy caps at +15%; its gain rule and affected stats await an owner decision, so this unit
//! never changes veterancy.
//!
//! # Rules where the spec is silent
//! - Player slot n is `TheatreState::players[n]`. Results for unknown slots are ignored; every number read
//!   from a result is clamped (fractions to 0..=1, buckets to 0..=8, slots and cells must exist).
//! - A wreck pays only when salvageable and not taken as a prize (a captured hull pays as a prize, never
//!   twice). A prize ordered home that never reached the exit pays nothing. Income from wrecks, prizes and
//!   ghost Salvage rewards is multiplied by the economy slider; Intel is not.
//! - Intel: each met ghost condition pays its authored reward (Intel, a blueprint or Salvage, booked with the
//!   prizes); a won photo reconnaissance pays 2; each prize whose crew was captured pays 1; every 10
//!   survivors recovered pay 1.
//! - Raided stores: a tanker wreck or prize yields [`TANKER_FUEL_L_PER_T`] litres per tonne of displacement,
//!   a freighter [`FREIGHTER_AMMO_KG_PER_T`] kilograms of ammunition per tonne; a struck depot yields
//!   [`DEPOT_FUEL_L_PER_T`] litres and [`DEPOT_AMMO_KG_PER_T`] kilograms per tonne of stock it held (paid by
//!   the theatre, which knows the node). Ammunition goes into the calibres the player hulls carry, in
//!   proportion to their magazine loads; with no magazine aboard any hull it goes to calibres already in
//!   stock, else it is left behind.
//! - Blueprints: parts the player can not field yet (not in an open tier and not unlocked) are recorded in
//!   `Resources::unlocked_parts`; the dry dock offers them once their tier is open, as the spec says.
//! - Repair: a damaged part costs 50% of its price, a destroyed one (health 0) its full price; a damaged
//!   cell costs 50% of its structure and armour price, a destroyed one (bucket 8) the full price. Part-body
//!   cells are paid with their part. Repairs take no theatre time. A partial repair (`repair_partial`) fixes
//!   destroyed parts, then damaged parts, then destroyed cells, then damaged cells, while Salvage lasts.
//! - Stores: a slot missing from `PlayerRecord::ammo` is full. Rearm fills every magazine, launcher, mine
//!   rail and decoy locker and every tank. Stock is used first; the shortfall is priced by mass at 5% of build
//!   cost per full load (fuel at the same rate per full tank; decoy rounds weigh [`DECOY_ROUND_KG`]).
//! - Refit: damage and stores carry over only to parts placed identically (same part, cell and rotation) and
//!   to structure cells that still exist; parts added by a refit arrive with empty magazines. In co-op a
//!   refit commits directly when the shared pool can pay for it.
//! - A lost hull keeps its blueprint; its damage and stores are cleared. Rebuilding (60% of build cost, 48
//!   hours) returns it with empty tanks and magazines, to be filled at the tender. `rebuild` returns the
//!   Salvage spent.
//! - While SEXTANT refits after being sunk, the tender offers no services (repair, rearm, refit, rebuild).
//! - SEXTANT sunk: the region's Alert returns to its act-start level (cause `SextantSunk`); the hunter group,
//!   if any, is released. SEXTANT moves to the nearest open cove in another region, else the nearest open
//!   cove elsewhere, else the nearest closed cove in another region (which opens), else it stays.
use super::{AlertCause, Payout, TheatreData, TheatreEvent, TheatreReject};
use crate::prelude::*;
use crate::protocol::command::PrizeDecision;
use crate::protocol::config::{KillFate, Outcome, SortieResult};
use content::blueprint::{Blueprint, PartPlacement};
use content::campaign::{ClassRole, MissionType, NodeKind, OperationKind, Reward};
use content::cell::CellGrid;
use content::ids::CellIndex;
use content::part::PartFunction;
use content::save::{GroupOrder, HullDamage, PlayerRecord, TheatreState};

/// Litres of fuel raided per tonne of a sunk or captured tanker.
pub const TANKER_FUEL_L_PER_T: f64 = 6.0;
/// Kilograms of ammunition raided per tonne of a sunk or captured freighter.
pub const FREIGHTER_AMMO_KG_PER_T: f64 = 5.0;
/// Litres of fuel and kilograms of ammunition raided per tonne of stock in a struck depot.
pub const DEPOT_FUEL_L_PER_T: f64 = 20.0;
pub const DEPOT_AMMO_KG_PER_T: f64 = 10.0;
/// Mass of one decoy round for shortfall pricing.
pub const DECOY_ROUND_KG: f64 = 5.0;
/// Damage bucket of a destroyed cell (L561).
const DESTROYED_BUCKET: u8 = 8;

/// Pay out a sortie: wrecks, prizes, Intel, blueprints, raided stores; write each player's hull damage, fuel
/// and ammunition back; handle hull losses and a lost tender. Unit: `sys-theatre-economy`.
pub fn apply_result(state: &mut TheatreState, result: &SortieResult, ironman: bool, data: &TheatreData, events: &mut Vec<TheatreEvent>) -> Payout {
    let mut payout = Payout::default();
    if result.outcome == Outcome::Aborted { return payout; }
    let rules = &data.campaign.rules;
    let econ = economy_factor(state);
    let op = result.operation.as_ref().and_then(|o| data.campaign.operations.get(o));
    let success = result.outcome == Outcome::Victory || (result.outcome == Outcome::Withdrawn && result.objective_done);

    // Wrecks (a hull taken as a prize is paid as a prize only).
    let prize_records: BTreeSet<u32> = result.prizes.iter().filter_map(|p| p.hull_record).collect();
    let mut wrecks = 0.0;
    let mut fuel_l = 0.0;
    let mut ammo_kg = 0.0;
    for k in &result.kills {
        if k.fate == KillFate::Captured || k.hull_record.is_some_and(|r| prize_records.contains(&r)) || !k.salvageable { continue; }
        wrecks += fraction(rules.wreck_payout) * k.build_cost as f64;
        let (f, a) = cargo(data, k.class.as_ref(), k.tonnage_t);
        fuel_l += f;
        ammo_kg += a;
    }

    // Prizes.
    let mut prizes = 0.0;
    let mut blueprints: Vec<PartId> = Vec::new();
    let fieldable = fieldable_parts(state, data);
    for p in &result.prizes {
        let cost = p.build_cost as f64;
        match p.decision {
            PrizeDecision::SailHome => {
                if !p.reached_exit { continue; }
                prizes += fraction(rules.prize_home_payout) * cost;
            }
            PrizeDecision::StripOnSpot => {
                prizes += fraction(rules.prize_strip_payout) * cost;
                for part in &p.parts {
                    if data.catalogue.part(part).is_none() || fieldable.contains(part) || state.resources.unlocked_parts.contains(part) || blueprints.contains(part) { continue; }
                    blueprints.push(part.clone());
                }
            }
        }
        let (f, a) = cargo(data, p.class.as_ref(), p.tonnage_t);
        fuel_l += f;
        ammo_kg += a;
    }

    // Ghost conditions, photographs, prize crews and survivors.
    let mut intel: u32 = 0;
    let mut ghost_salvage = 0.0;
    if let Some(op) = op {
        for (g, met) in op.ghosts.iter().zip(result.ghosts.iter()) {
            if !*met { continue; }
            match &g.reward {
                Reward::Intel(n) => intel = intel.saturating_add(*n as u32),
                Reward::Salvage(s) => ghost_salvage += *s as f64,
                Reward::Blueprint(part) => {
                    if data.catalogue.part(part).is_some() && !state.resources.unlocked_parts.contains(part) && !blueprints.contains(part) { blueprints.push(part.clone()); }
                }
            }
        }
        if success && op.kind == OperationKind::Template(MissionType::PhotoReconnaissance) { intel = intel.saturating_add(2); }
    }
    intel = intel.saturating_add(result.prizes.iter().filter(|p| p.crew_captured > 0).count() as u32);
    intel = intel.saturating_add(result.survivors_recovered as u32 / 10);

    payout.salvage_wrecks = to_salvage(wrecks * econ);
    payout.salvage_prizes = to_salvage((prizes + ghost_salvage) * econ);
    payout.intel = intel;
    let r = &mut state.resources;
    r.salvage = r.salvage.saturating_add(payout.salvage_wrecks).saturating_add(payout.salvage_prizes);
    r.intel = r.intel.saturating_add(intel);
    for part in &blueprints {
        state.resources.unlocked_parts.insert(part.clone());
        events.push(TheatreEvent::PartUnlocked(part.clone()));
    }
    payout.blueprints = blueprints;

    // Player hulls.
    for pr in &result.players {
        let i = pr.player.0 as usize;
        let Some(rec) = state.players.get_mut(i) else { continue };
        if rec.hull_lost { continue; }
        if pr.hull_lost {
            lose_hull(rec);
            events.push(TheatreEvent::HullLost { player: pr.player });
            continue;
        }
        let grid = content::blueprint::io::build_grid(&rec.blueprint, &data.catalogue);
        rec.damage = sanitize_damage(&pr.damage, &rec.blueprint, &grid);
        rec.fuel_fraction = unit(pr.fuel_fraction);
        let caps = ammo_capacities(&rec.blueprint, data);
        rec.ammo = pr.ammo.iter().filter_map(|(slot, n)| caps.get(slot).map(|cap| (*slot, (*n).min(*cap)))).collect();
    }

    // Raided stores, after the hulls so a lost hull's calibres do not attract ammunition.
    let (f, a) = add_raided(state, data, fuel_l, ammo_kg);
    payout.fuel_l = f;
    payout.ammo_kg = a;

    if result.tender_lost { sextant_sunk(state, data, events); }
    events.push(TheatreEvent::ResourcesChanged);
    if ironman && !state.players.is_empty() && state.players.iter().all(|p| p.hull_lost) { events.push(TheatreEvent::CampaignOver); }
    payout
}

/// Salvage to repair a player's hull now (50% of damaged parts' cost; destroyed cells and parts at full
/// structure and part cost). Unit: `sys-theatre-economy`.
pub fn repair_cost(state: &TheatreState, player: PlayerSlot, data: &TheatreData) -> u32 {
    let Some(rec) = state.players.get(player.0 as usize) else { return 0 };
    if rec.hull_lost { return 0; }
    to_cost(repair_items(rec, data).iter().map(|i| i.cost).sum())
}

/// Repair the whole hull for [`repair_cost`]; refused when the pool cannot pay. Returns the Salvage spent.
pub fn repair(state: &mut TheatreState, player: PlayerSlot, data: &TheatreData, events: &mut Vec<TheatreEvent>) -> Result<u32, TheatreReject> {
    service_open(state)?;
    let rec = state.players.get(player.0 as usize).ok_or(TheatreReject::UnknownTarget)?;
    if rec.hull_lost { return Err(TheatreReject::HullNotReady); }
    let cost = repair_cost(state, player, data);
    if state.resources.salvage < cost { return Err(TheatreReject::NotEnoughSalvage); }
    state.resources.salvage -= cost;
    if let Some(rec) = state.players.get_mut(player.0 as usize) { rec.damage = HullDamage::default(); }
    events.push(TheatreEvent::ResourcesChanged);
    Ok(cost)
}

/// Repair what the pool can pay for, most important first: destroyed parts, damaged parts, destroyed
/// cells, damaged cells. Returns the Salvage spent (0 when nothing was affordable or damaged).
pub(crate) fn repair_partial(state: &mut TheatreState, player: PlayerSlot, data: &TheatreData, events: &mut Vec<TheatreEvent>) -> Result<u32, TheatreReject> {
    service_open(state)?;
    let rec = state.players.get(player.0 as usize).ok_or(TheatreReject::UnknownTarget)?;
    if rec.hull_lost { return Err(TheatreReject::HullNotReady); }
    let mut items = repair_items(rec, data);
    items.sort_by(|a, b| a.rank.cmp(&b.rank));
    let mut budget = state.resources.salvage as f64;
    let mut spent = 0.0;
    let mut fixed_parts: BTreeSet<PartSlot> = BTreeSet::new();
    let mut fixed_cells: BTreeSet<CellIndex> = BTreeSet::new();
    for it in &items {
        if it.cost > budget + 1e-9 { continue; }
        budget -= it.cost;
        spent += it.cost;
        match it.what { Fix::Part(s) => { fixed_parts.insert(s); } Fix::Cell(c) => { fixed_cells.insert(c); } }
    }
    let cost = to_cost(spent).min(state.resources.salvage);
    if fixed_parts.is_empty() && fixed_cells.is_empty() { return Ok(0); }
    state.resources.salvage -= cost;
    if let Some(rec) = state.players.get_mut(player.0 as usize) {
        rec.damage.parts.retain(|s, _| !fixed_parts.contains(s));
        rec.damage.cells.retain(|c| !fixed_cells.contains(&c.cell));
        if rec.damage.parts.is_empty() && rec.damage.cells.is_empty() { rec.damage.jury_rigged = 0; }
    }
    events.push(TheatreEvent::ResourcesChanged);
    Ok(cost)
}

/// Refuel and rearm from SEXTANT's stock; buy any shortfall with Salvage. Returns Salvage spent.
/// Unit: `sys-theatre-economy`.
pub fn rearm(state: &mut TheatreState, player: PlayerSlot, data: &TheatreData, events: &mut Vec<TheatreEvent>) -> Result<u32, TheatreReject> {
    service_open(state)?;
    let rec = state.players.get(player.0 as usize).ok_or(TheatreReject::UnknownTarget)?;
    if rec.hull_lost { return Err(TheatreReject::HullNotReady); }
    let plan = rearm_plan(rec, &state.resources, data);
    if state.resources.salvage < plan.salvage { return Err(TheatreReject::NotEnoughSalvage); }
    let r = &mut state.resources;
    r.salvage -= plan.salvage;
    r.fuel_l = (r.fuel_l - plan.fuel_from_stock).max(0.0);
    for (cal, kg) in &plan.kg_from_stock { if let Some(s) = r.ammo_kg.get_mut(cal) { *s = (*s - kg).max(0.0); } }
    for (m, n) in &plan.munitions_from_stock { if let Some(s) = r.munitions.get_mut(m) { *s = s.saturating_sub(*n); } }
    r.ammo_kg.retain(|_, v| *v > 1e-9);
    r.munitions.retain(|_, v| *v > 0);
    if let Some(rec) = state.players.get_mut(player.0 as usize) {
        let caps = ammo_capacities(&rec.blueprint, data);
        rec.ammo = caps;
        if plan.fuel_cap_l > 0.0 { rec.fuel_fraction = 1.0; }
    }
    events.push(TheatreEvent::ResourcesChanged);
    Ok(plan.salvage)
}

/// Salvage a rearm would cost now (stock first, shortfall bought).
pub(crate) fn rearm_cost(state: &TheatreState, player: PlayerSlot, data: &TheatreData) -> u32 {
    state.players.get(player.0 as usize).filter(|r| !r.hull_lost).map_or(0, |rec| rearm_plan(rec, &state.resources, data).salvage)
}

/// Commit a refit to a new design: validate under the act's ruleset and unlocks, price with
/// `content::stats::refit_cost` (added parts full price unless drawn from the parts store; removed parts go
/// to the store, never sold), charge Salvage and set the hull's ready hour (2 h per module swap plus 1 h per
/// 10 t of structure; concurrent refits in co-op take the longest single refit). Returns theatre hours.
/// Unit: `sys-theatre-economy`.
pub fn refit(state: &mut TheatreState, player: PlayerSlot, blueprint: &Blueprint, data: &TheatreData, events: &mut Vec<TheatreEvent>) -> Result<u32, TheatreReject> {
    service_open(state)?;
    let hour = state.hour;
    let rec = state.players.get(player.0 as usize).ok_or(TheatreReject::UnknownTarget)?;
    if rec.hull_lost || rec.ready_at_hour.is_some_and(|h| h > hour) { return Err(TheatreReject::HullNotReady); }
    let cat = &data.catalogue;
    let act = if state.open_theatre { Act::Act4 } else { state.act };
    let ruleset = content::ruleset::Ruleset::campaign(act);
    let available = fieldable_list(state, data);
    let ctx = content::validate::ValidationContext { ruleset: &ruleset, available: Some(&available), enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(blueprint, cat, &ctx);
    if report.issues.iter().any(|i| matches!(i, content::validate::Issue::OverTonnage { .. })) || report.stats.tonnage_t > ruleset.tonnage_cap_t + 1e-6 {
        return Err(TheatreReject::OverTonnageCap);
    }
    if !report.is_ok() { return Err(TheatreReject::InvalidBlueprint); }
    let old = rec.blueprint.clone();
    let diff = content::stats::diff_blueprints(&old, blueprint, cat);
    let cost = content::stats::refit_cost(&diff, cat, &state.resources.parts_store);
    if state.resources.salvage < cost.salvage { return Err(TheatreReject::NotEnoughSalvage); }

    let r = &mut state.resources;
    r.salvage -= cost.salvage;
    for (part, n) in &cost.from_store {
        if let Some(have) = r.parts_store.get_mut(part) { *have = have.saturating_sub(*n); }
    }
    for c in &diff.changes {
        if let content::stats::PartChange::Removed(p) = c { *r.parts_store.entry(p.part.clone()).or_insert(0) = r.parts_store.get(&p.part).copied().unwrap_or(0).saturating_add(1); }
    }
    r.parts_store.retain(|_, n| *n > 0);

    let Some(rec) = state.players.get_mut(player.0 as usize) else { return Err(TheatreReject::UnknownTarget) };
    carry_over(rec, &old, blueprint, data);
    rec.blueprint = blueprint.clone();
    let hours = cost.hours;
    rec.ready_at_hour = if hours > 0 { Some(hour.saturating_add(hours as u64)) } else { None };
    events.push(TheatreEvent::RefitStarted { player, hours });
    events.push(TheatreEvent::ResourcesChanged);
    Ok(hours)
}

/// Rebuild a lost hull: 60% of build cost and 48 theatre hours; refused under Ironman.
/// Unit: `sys-theatre-economy`.
///
/// DECISION: a pool that holds less than the rebuild cost pays what it holds and the rebuild goes ahead.
/// Outside Ironman there is no campaign game over (L828), and a hull lost before the pool can pay for its
/// rebuild (a 930 S starter costs 558 S to rebuild against 400 S at the start) would otherwise leave the
/// detachment with no hull and no way to earn one.
///
/// Returns the Salvage spent.
pub fn rebuild(state: &mut TheatreState, player: PlayerSlot, ironman: bool, data: &TheatreData, events: &mut Vec<TheatreEvent>) -> Result<u32, TheatreReject> {
    let rec = state.players.get(player.0 as usize).ok_or(TheatreReject::UnknownTarget)?;
    if !rec.hull_lost { return Err(TheatreReject::UnknownTarget); }
    if ironman { return Err(TheatreReject::IronmanLocked); }
    service_open(state)?;
    let cost = rebuild_cost(state, player, data).min(state.resources.salvage);
    state.resources.salvage -= cost;
    let ready = state.hour.saturating_add(data.campaign.rules.rebuild_hours as u64);
    if let Some(rec) = state.players.get_mut(player.0 as usize) {
        rec.hull_lost = false;
        rec.damage = HullDamage::default();
        rec.fuel_fraction = 0.0;
        rec.ammo = ammo_capacities(&rec.blueprint, data).into_keys().map(|s| (s, 0)).collect();
        rec.ready_at_hour = Some(ready);
    }
    events.push(TheatreEvent::HullRebuilt { player });
    events.push(TheatreEvent::ResourcesChanged);
    Ok(cost)
}

/// Salvage a rebuild costs (0 for a hull that is not lost).
pub(crate) fn rebuild_cost(state: &TheatreState, player: PlayerSlot, data: &TheatreData) -> u32 {
    let Some(rec) = state.players.get(player.0 as usize).filter(|r| r.hull_lost) else { return 0 };
    to_cost(fraction(data.campaign.rules.rebuild_cost_fraction) * build_cost(&rec.blueprint, data) as f64)
}

/// SEXTANT sunk (L827). Unit: `sys-theatre-economy`.
pub fn sextant_sunk(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let rules = &data.campaign.rules;
    let f = fraction(rules.sextant_loss_fraction);
    let r = &mut state.resources;
    r.salvage = r.salvage.saturating_sub(libm::round(r.salvage as f64 * f) as u32);
    r.fuel_l = (finite(r.fuel_l) * (1.0 - f)).max(0.0);
    for v in r.ammo_kg.values_mut() { *v = (finite(*v) * (1.0 - f)).max(0.0); }
    for v in r.munitions.values_mut() { *v = v.saturating_sub(libm::round(*v as f64 * f) as u16); }
    r.ammo_kg.retain(|_, v| *v > 1e-9);
    r.munitions.retain(|_, v| *v > 0);

    let nodes = &data.campaign.theatre.nodes;
    let here = nodes.iter().find(|n| n.id == state.sextant_node);
    let region = here.map(|n| n.region.clone());
    if let Some(region) = &region {
        // Full Combine control: Supply 100 (logistics refills the window on the handshake), lanes restored.
        let rs = state.regions.entry(region.clone()).or_default();
        rs.supply = 100.0;
        rs.combine_control = true;
        let start_alert = if state.retreat { rules.retreat_alert.clamp(0.0, 5.0) } else { 0.0 };
        rs.alert = start_alert;
        rs.quiet_hours = 0;
        events.push(TheatreEvent::SupplyChanged { region: region.clone(), supply: 100.0 });
        events.push(TheatreEvent::AlertChanged { region: region.clone(), alert: start_alert, cause: AlertCause::SextantSunk });
        let in_region = |id: &TheatreNodeId| nodes.iter().any(|n| &n.id == id && &n.region == region);
        let restored: Vec<(TheatreNodeId, TheatreNodeId)> = state.lanes.keys().filter(|(a, b)| in_region(a) || in_region(b)).cloned().collect();
        for key in restored {
            state.lanes.remove(&key);
            events.push(TheatreEvent::LaneRestored { a: key.0, b: key.1 });
        }
    }
    // The hunter group has done its work.
    if let Some(id) = state.hunter_group.take() {
        if let Some(g) = state.groups.iter_mut().find(|g| g.id == id) { g.order = GroupOrder::Return; g.destination = None; g.arrive_hour = None; }
    }

    // Relocation.
    let pos = here.map(|n| n.pos_km).unwrap_or([0.0, 0.0]);
    let dist = |p: [f64; 2]| { let (dx, dz) = (p[0] - pos[0], p[1] - pos[1]); let d = libm::sqrt(dx * dx + dz * dz); if d.is_finite() { d } else { f64::MAX } };
    let coves: Vec<&content::campaign::NodeDef> = nodes.iter().filter(|n| matches!(n.kind, NodeKind::Cove) && n.id != state.sextant_node).collect();
    let other_region = |n: &&content::campaign::NodeDef| region.as_ref().is_none_or(|r| &n.region != r);
    let open = |n: &&content::campaign::NodeDef| state.open_coves.contains(&n.id);
    let pick = |f: &dyn Fn(&&content::campaign::NodeDef) -> bool| -> Option<TheatreNodeId> {
        coves.iter().filter(|n| f(n)).min_by(|a, b| dist(a.pos_km).total_cmp(&dist(b.pos_km)).then_with(|| a.id.cmp(&b.id))).map(|n| n.id.clone())
    };
    let to = pick(&|n| open(n) && other_region(n)).or_else(|| pick(&|n| open(n))).or_else(|| pick(&|n| other_region(n)));
    let hour = state.hour;
    let ready = hour.saturating_add(rules.sextant_refit_hours as u64);
    let to = to.unwrap_or_else(|| state.sextant_node.clone());
    if !state.open_coves.contains(&to) && nodes.iter().any(|n| n.id == to) { state.open_coves.push(to.clone()); }
    if to != state.sextant_node {
        let from = core::mem::replace(&mut state.sextant_node, to.clone());
        events.push(TheatreEvent::SextantMoved { from, to: to.clone() });
    }
    state.sextant_refit_until_hour = Some(ready);
    events.push(TheatreEvent::SextantSunk { relocated_to: to, ready_hour: ready });
    events.push(TheatreEvent::ResourcesChanged);
}

// ---------------------------------------------------------------------------------------------------------
// Shared helpers.

/// Build cost of a design in Salvage (structure, armour and parts) from `content::stats`.
pub(crate) fn build_cost(bp: &Blueprint, data: &TheatreData) -> u32 { content::stats::compute_stats(bp, &data.catalogue).cost }

/// Parts the dry dock offers now: every part of an open tier plus blueprint unlocks (filtered by tier and act
/// by the catalogue).
pub(crate) fn fieldable_list(state: &TheatreState, data: &TheatreData) -> Vec<PartId> {
    let tier = super::progress::open_tier(state, data);
    let act = if state.open_theatre { Act::Act4 } else { state.act };
    let mut wanted: Vec<PartId> = data.catalogue.parts.values().filter(|p| p.tier <= tier).map(|p| p.id.clone()).collect();
    for p in &state.resources.unlocked_parts { if !wanted.contains(p) { wanted.push(p.clone()); } }
    data.catalogue.available_parts(GameMode::Campaign, act, tier, &wanted).into_iter().filter_map(|i| data.catalogue.parts.get_index(i.index()).map(|(id, _)| id.clone())).collect()
}

fn fieldable_parts(state: &TheatreState, data: &TheatreData) -> BTreeSet<PartId> { fieldable_list(state, data).into_iter().collect() }

/// Add raided fuel and ammunition to SEXTANT's stock; returns what was actually stored.
pub(crate) fn add_raided(state: &mut TheatreState, data: &TheatreData, fuel_l: f64, ammo_kg: f64) -> (f64, f64) {
    let fuel_l = finite(fuel_l).max(0.0);
    let ammo_kg = finite(ammo_kg).max(0.0);
    state.resources.fuel_l = finite(state.resources.fuel_l) + fuel_l;
    if ammo_kg <= 0.0 { return (fuel_l, 0.0); }
    let mut weights: BTreeMap<u16, f64> = BTreeMap::new();
    for p in state.players.iter().filter(|p| !p.hull_lost) {
        for pl in &p.blueprint.parts {
            if let Some(PartFunction::Magazine { calibre_mm, ammo_kg, .. }) = data.catalogue.part(&pl.part).map(|d| &d.function) {
                if ammo_kg.is_finite() && *ammo_kg > 0.0 { *weights.entry(*calibre_mm).or_insert(0.0) += *ammo_kg; }
            }
        }
    }
    if weights.is_empty() { for c in state.resources.ammo_kg.keys() { weights.insert(*c, 1.0); } }
    let total: f64 = weights.values().sum();
    if !(total > 0.0) { return (fuel_l, 0.0); }
    for (c, w) in &weights { *state.resources.ammo_kg.entry(*c).or_insert(0.0) += ammo_kg * w / total; }
    (fuel_l, ammo_kg)
}

/// Rounds each ammunition slot of a design holds when full.
pub(crate) fn ammo_capacities(bp: &Blueprint, data: &TheatreData) -> BTreeMap<PartSlot, u32> {
    slot_stores(bp, data).into_iter().map(|s| (s.slot, s.capacity)).collect()
}

/// Full-load fuel of a design in litres.
pub(crate) fn fuel_capacity_l(bp: &Blueprint, data: &TheatreData) -> f64 {
    bp.parts.iter().filter_map(|p| match data.catalogue.part(&p.part).map(|d| &d.function) { Some(PartFunction::FuelTank { litres }) if litres.is_finite() => Some(litres.max(0.0)), _ => None }).sum()
}

// ---------------------------------------------------------------------------------------------------------

fn finite(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }
fn unit(v: f64) -> f64 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } }
fn fraction(v: f64) -> f64 { unit(v) }
fn economy_factor(state: &TheatreState) -> f64 { let e = state.difficulty.economy; if e.is_finite() { e.clamp(0.7, 1.5) } else { 1.0 } }
/// Income rounds to nearest.
fn to_salvage(v: f64) -> u32 { if v.is_finite() && v > 0.0 { libm::round(v).min(u32::MAX as f64) as u32 } else { 0 } }
/// Costs round up.
fn to_cost(v: f64) -> u32 { if v.is_finite() && v > 0.0 { libm::ceil(v - 1e-9).min(u32::MAX as f64) as u32 } else { 0 } }

fn service_open(state: &TheatreState) -> Result<(), TheatreReject> {
    if state.sextant_refit_until_hour.is_some_and(|h| h > state.hour) { Err(TheatreReject::NotAtCove) } else { Ok(()) }
}

/// Raided fuel and ammunition carried by a sunk or captured hull of `class`.
fn cargo(data: &TheatreData, class: Option<&ClassId>, tonnage_t: f64) -> (f64, f64) {
    let t = finite(tonnage_t).max(0.0);
    match class.and_then(|c| super::logistics::class_role(data, c)) {
        Some(ClassRole::Tanker) => (TANKER_FUEL_L_PER_T * t, 0.0),
        Some(ClassRole::Freighter) => (0.0, FREIGHTER_AMMO_KG_PER_T * t),
        _ => (0.0, 0.0),
    }
}

fn lose_hull(rec: &mut PlayerRecord) {
    rec.hull_lost = true;
    rec.damage = HullDamage::default();
    rec.fuel_fraction = 0.0;
    rec.ammo.clear();
    rec.ready_at_hour = None;
}

/// Keep only damage that refers to existing parts and cells, with values in range.
fn sanitize_damage(d: &HullDamage, bp: &Blueprint, grid: &CellGrid) -> HullDamage {
    let mut cells: BTreeMap<CellIndex, u8> = BTreeMap::new();
    for c in &d.cells {
        if c.bucket == 0 || c.cell.index() >= grid.cells.len() || grid.cells[c.cell.index()].is_empty() { continue; }
        cells.insert(c.cell, c.bucket.min(DESTROYED_BUCKET));
    }
    let parts = d.parts.iter().filter(|(s, _)| s.index() < bp.parts.len()).map(|(s, h)| (*s, unit(*h))).filter(|(_, h)| *h < 1.0).collect();
    HullDamage { cells: cells.into_iter().map(|(cell, bucket)| content::cell::CellChange { cell, bucket }).collect(), parts, jury_rigged: d.jury_rigged }
}

#[derive(Clone, Copy, Debug)]
enum Fix { Part(PartSlot), Cell(CellIndex) }
struct RepairItem { rank: u8, cost: f64, what: Fix }

fn repair_items(rec: &PlayerRecord, data: &TheatreData) -> Vec<RepairItem> {
    let rules = &data.campaign.rules;
    let half = fraction(rules.repair_cost_fraction);
    let cat = &data.catalogue;
    let mut out = Vec::new();
    for (slot, health) in &rec.damage.parts {
        let Some(def) = rec.blueprint.parts.get(slot.index()).and_then(|p| cat.part(&p.part)) else { continue };
        let h = unit(*health);
        if h >= 1.0 { continue; }
        let destroyed = h <= 0.0;
        out.push(RepairItem { rank: if destroyed { 0 } else { 1 }, cost: def.cost as f64 * if destroyed { 1.0 } else { half }, what: Fix::Part(*slot) });
    }
    if !rec.damage.cells.is_empty() {
        let grid = content::blueprint::io::build_grid(&rec.blueprint, cat);
        for c in &rec.damage.cells {
            if c.bucket == 0 { continue; }
            let Some(cell) = grid.cells.get(c.cell.index()) else { continue };
            let Some(mat) = cat.materials.iter().find(|m| m.material == cell.material) else { continue };
            let armour = match cell.armour as usize { 0 => 0.0, t => mat.armour_cost.get(t - 1).copied().unwrap_or(0.0) };
            let full = finite(mat.cell_cost) + finite(armour);
            let destroyed = c.bucket >= DESTROYED_BUCKET;
            out.push(RepairItem { rank: if destroyed { 2 } else { 3 }, cost: full.max(0.0) * if destroyed { 1.0 } else { half }, what: Fix::Cell(c.cell) });
        }
    }
    out
}

/// One ammunition store of a design.
struct SlotStore { slot: PartSlot, capacity: u32, kg_per_round: f64, kind: StoreKind }
enum StoreKind { Shells(u16), Munition(MunitionId), Decoys }

fn slot_stores(bp: &Blueprint, data: &TheatreData) -> Vec<SlotStore> {
    let cat = &data.catalogue;
    let mut out = Vec::new();
    for (i, p) in bp.parts.iter().enumerate() {
        let Some(def) = cat.part(&p.part) else { continue };
        let slot = PartSlot(i.min(u16::MAX as usize) as u16);
        let munition_kg = |m: &MunitionId| cat.munition(m).map_or(0.0, |d| finite(d.mass_kg).max(0.0));
        let s = match &def.function {
            PartFunction::Magazine { calibre_mm, rounds, ammo_kg } if *rounds > 0 => SlotStore { slot, capacity: *rounds, kg_per_round: finite(*ammo_kg).max(0.0) / *rounds as f64, kind: StoreKind::Shells(*calibre_mm) },
            PartFunction::Launcher(l) if l.cells > 0 => SlotStore { slot, capacity: l.cells as u32, kg_per_round: munition_kg(&l.munition), kind: StoreKind::Munition(l.munition.clone()) },
            PartFunction::MineRail { mine, capacity } if *capacity > 0 => SlotStore { slot, capacity: *capacity as u32, kg_per_round: munition_kg(mine), kind: StoreKind::Munition(mine.clone()) },
            PartFunction::Decoy(d) if d.rounds > 0 && d.kind != content::part::DecoyKind::TowedDecoy => SlotStore { slot, capacity: d.rounds as u32, kg_per_round: DECOY_ROUND_KG, kind: StoreKind::Decoys },
            _ => continue,
        };
        out.push(s);
    }
    out
}

struct RearmPlan { salvage: u32, fuel_cap_l: f64, fuel_from_stock: f64, kg_from_stock: BTreeMap<u16, f64>, munitions_from_stock: BTreeMap<MunitionId, u16> }

fn rearm_plan(rec: &PlayerRecord, stock: &content::save::Resources, data: &TheatreData) -> RearmPlan {
    let mut kg_stock: BTreeMap<u16, f64> = stock.ammo_kg.iter().map(|(c, v)| (*c, finite(*v).max(0.0))).collect();
    let mut mun_stock = stock.munitions.clone();
    let mut kg_from: BTreeMap<u16, f64> = BTreeMap::new();
    let mut mun_from: BTreeMap<MunitionId, u16> = BTreeMap::new();
    let (mut full_mass, mut short_mass) = (0.0, 0.0);
    for s in slot_stores(&rec.blueprint, data) {
        let have = rec.ammo.get(&s.slot).copied().unwrap_or(s.capacity).min(s.capacity);
        let missing = (s.capacity - have) as f64;
        full_mass += s.capacity as f64 * s.kg_per_round;
        if missing <= 0.0 { continue; }
        match &s.kind {
            StoreKind::Shells(cal) => {
                let need = missing * s.kg_per_round;
                let avail = kg_stock.get(cal).copied().unwrap_or(0.0);
                let take = need.min(avail);
                if take > 0.0 { *kg_stock.entry(*cal).or_insert(0.0) -= take; *kg_from.entry(*cal).or_insert(0.0) += take; }
                short_mass += need - take;
            }
            StoreKind::Munition(m) => {
                let avail = mun_stock.get(m).copied().unwrap_or(0) as f64;
                let take = missing.min(avail);
                if take > 0.0 {
                    let t = take as u16;
                    if let Some(v) = mun_stock.get_mut(m) { *v = v.saturating_sub(t); }
                    *mun_from.entry(m.clone()).or_insert(0) += t;
                }
                short_mass += (missing - take) * s.kg_per_round;
            }
            StoreKind::Decoys => short_mass += missing * s.kg_per_round,
        }
    }
    let fuel_cap = fuel_capacity_l(&rec.blueprint, data);
    let fuel_need = fuel_cap * (1.0 - unit(rec.fuel_fraction));
    let fuel_take = fuel_need.min(finite(stock.fuel_l).max(0.0));
    let fuel_short = (fuel_need - fuel_take).max(0.0);
    let ammo_share = if full_mass > 0.0 { short_mass / full_mass } else { 0.0 };
    let fuel_share = if fuel_cap > 0.0 { fuel_short / fuel_cap } else { 0.0 };
    let salvage = if ammo_share > 1e-12 || fuel_share > 1e-12 {
        let rate = fraction(data.campaign.rules.reload_cost_fraction) * build_cost(&rec.blueprint, data) as f64;
        to_cost(rate * (ammo_share + fuel_share))
    } else { 0 };
    RearmPlan { salvage, fuel_cap_l: fuel_cap, fuel_from_stock: fuel_take, kg_from_stock: kg_from, munitions_from_stock: mun_from }
}

/// Move damage and stores from the old design to the new one where the part or cell still exists.
fn carry_over(rec: &mut PlayerRecord, old: &Blueprint, new: &Blueprint, data: &TheatreData) {
    let same = |a: &PartPlacement, b: &PartPlacement| a.part == b.part && a.pos == b.pos && a.rot == b.rot;
    let mut used = vec![false; new.parts.len()];
    let mut map: BTreeMap<PartSlot, PartSlot> = BTreeMap::new();
    for (i, p) in old.parts.iter().enumerate() {
        if let Some(j) = (0..new.parts.len()).find(|&j| !used[j] && same(p, &new.parts[j])) {
            used[j] = true;
            map.insert(PartSlot(i as u16), PartSlot(j as u16));
        }
    }
    rec.damage.parts = rec.damage.parts.iter().filter_map(|(s, h)| map.get(s).map(|n| (*n, *h))).collect();
    if !rec.damage.cells.is_empty() {
        let cat = &data.catalogue;
        let (og, ng) = (content::blueprint::io::build_grid(old, cat), content::blueprint::io::build_grid(new, cat));
        rec.damage.cells = rec.damage.cells.iter().filter_map(|c| {
            if c.cell.index() >= og.cells.len() { return None; }
            let pos = og.pos(c.cell);
            let ni = ng.index(pos)?;
            if ng.cells[ni.index()].is_empty() { return None; }
            Some(content::cell::CellChange { cell: ni, bucket: c.bucket })
        }).collect();
    }
    let caps = ammo_capacities(new, data);
    let mut ammo: BTreeMap<PartSlot, u32> = BTreeMap::new();
    for (slot, cap) in &caps {
        let from_old = map.iter().find(|(_, n)| *n == slot).map(|(o, _)| *o);
        let n = match from_old { Some(o) => rec.ammo.get(&o).copied().unwrap_or(*cap), None => 0 };
        ammo.insert(*slot, n.min(*cap));
    }
    rec.ammo = ammo;
}

#[cfg(test)]
mod tests {
    use super::super::logistics::testkit::*;
    use super::*;
    use crate::protocol::config::{KillRecord, PlayerResult, PrizeRecord};
    use content::campaign::RangeBand;
    use content::save::{HullDamage, PlayerRecord};

    fn catalogue() -> Catalogue {
        let mut c = Catalogue::default();
        let parts: Vec<content::part::PartDef> = content::data::parse_ron(r#"[
            (id: "mag.76", name: "m", description: "m", category: Gun, tier: T1, size: (1, 1, 1), mass_kg: 500.0, cost: 40,
             function: Magazine(calibre_mm: 76, rounds: 100, ammo_kg: 1000.0)),
            (id: "tank.s", name: "t", description: "t", category: Fuel, tier: T1, size: (1, 1, 1), mass_kg: 100.0, cost: 10,
             function: FuelTank(litres: 2000.0)),
            (id: "gun.t3", name: "g", description: "g", category: Hull, tier: T3, size: (1, 1, 1), mass_kg: 100.0, cost: 200,
             function: Structure),
        ]"#, "test").expect("test parts");
        for p in parts { c.parts.insert(p.id.clone(), p); }
        c.materials = content::data::parse_ron(content::data::MATERIALS, "materials").expect("materials");
        c
    }

    fn with_player() -> (TheatreState, TheatreData) {
        let (mut s, mut d) = started();
        d.catalogue = Arc::new(catalogue());
        let mut bp = content::blueprint::io::new_blueprint("t", 15.0);
        bp.parts = vec![
            PartPlacement { part: PartId::new("mag.76"), pos: content::cell::CellPos::new(0, 0, 0), rot: Default::default(), kit: None, label: None, load: None },
            PartPlacement { part: PartId::new("tank.s"), pos: content::cell::CellPos::new(1, 0, 0), rot: Default::default(), kit: None, label: None, load: None },
        ];
        s.players.push(PlayerRecord { client_id: 1, name: "a".into(), blueprint: bp, damage: HullDamage::default(), fuel_fraction: 1.0, ammo: BTreeMap::new(), hull_lost: false, ready_at_hour: None, veterancy: 0.0 });
        (s, d)
    }

    fn kill(cost: u32, fate: KillFate, salvageable: bool, record: Option<u32>) -> KillRecord {
        KillRecord { tick: 0, hull_record: record, class: Some(ClassId::new("frt")), side: SideId(1), tonnage_t: 400.0, build_cost: cost, fate, method: content::campaign::KillMethod::Gunfire, range: RangeBand::Medium, time: TimeOfDay::Dusk, emcon: Emcon::Burst, observed: true, by_player: Some(PlayerSlot(0)), salvageable }
    }

    fn result() -> SortieResult { super::super::alert::tests::result(vec![], 0, 0) }

    #[test]
    fn payouts_follow_the_spec_rates() {
        let (mut s, d) = with_player();
        let mut ev = Vec::new();
        let mut r = result();
        r.kills = vec![kill(1000, KillFate::Lost, true, Some(1)), kill(1000, KillFate::Lost, false, Some(2)), kill(500, KillFate::Captured, true, Some(3)), kill(500, KillFate::Disabled, true, Some(4))];
        r.prizes = vec![
            PrizeRecord { hull_record: Some(4), class: None, decision: PrizeDecision::SailHome, tonnage_t: 100.0, build_cost: 500, parts: vec![], reached_exit: true, crew_captured: 12 },
            PrizeRecord { hull_record: Some(3), class: None, decision: PrizeDecision::StripOnSpot, tonnage_t: 100.0, build_cost: 500, parts: vec![PartId::new("gun.t3"), PartId::new("mag.76"), PartId::new("nope")], reached_exit: false, crew_captured: 0 },
        ];
        r.survivors_recovered = 25;
        let p = apply_result(&mut s, &r, false, &d, &mut ev);
        // One salvageable wreck (record 4 is a prize, record 3 is captured): 20% of 1000.
        assert_eq!(p.salvage_wrecks, 200);
        // Home 60% of 500 + strip 40% of 500.
        assert_eq!(p.salvage_prizes, 500);
        // Only the tier-3 part is a new blueprint (tier 1 is open; unknown parts are ignored).
        assert_eq!(p.blueprints, vec![PartId::new("gun.t3")]);
        // One prize crew + 25 survivors / 10.
        assert_eq!(p.intel, 3);
        // The freighter wreck's cargo: 5 kg per tonne into the 76 mm magazine calibre.
        assert!((p.ammo_kg - 2000.0).abs() < 1e-9);
        assert!((s.resources.ammo_kg[&76] - 2000.0).abs() < 1e-9);
        assert_eq!(s.resources.salvage, 700);
        // The economy slider multiplies Salvage income only.
        let (mut s2, d2) = with_player();
        s2.difficulty.economy = 1.5;
        let p2 = apply_result(&mut s2, &r, false, &d2, &mut ev);
        assert_eq!((p2.salvage_wrecks, p2.salvage_prizes, p2.intel), (300, 750, 3));
    }

    #[test]
    fn ghost_rewards_and_photo_intel() {
        let (mut s, mut d) = with_player();
        let mut c = (*d.campaign).clone();
        let op = c.operations.get_mut(&OperationId::new("op.raid")).unwrap();
        op.kind = OperationKind::Template(MissionType::PhotoReconnaissance);
        op.ghosts = vec![
            content::campaign::GhostCondition { kind: content::campaign::GhostKind::NoLosses, reward: Reward::Intel(1) },
            content::campaign::GhostCondition { kind: content::campaign::GhostKind::NoKills, reward: Reward::Salvage(100) },
            content::campaign::GhostCondition { kind: content::campaign::GhostKind::NeverDetected, reward: Reward::Intel(1) },
        ];
        d.campaign = Arc::new(c);
        let mut r = result();
        r.operation = Some(OperationId::new("op.raid"));
        r.ghosts = vec![true, true, false];
        let mut ev = Vec::new();
        let p = apply_result(&mut s, &r, false, &d, &mut ev);
        assert_eq!(p.intel, 1 + 2);
        assert_eq!(p.salvage_prizes, 100);
    }

    #[test]
    fn hull_loss_and_write_back() {
        let (mut s, d) = with_player();
        let mut ev = Vec::new();
        let mut r = result();
        let mut damage = HullDamage::default();
        damage.parts.insert(PartSlot(0), 0.5);
        damage.parts.insert(PartSlot(9), 0.5);
        damage.cells.push(content::cell::CellChange { cell: CellIndex(u32::MAX), bucket: 3 });
        let mut ammo = BTreeMap::new();
        ammo.insert(PartSlot(0), 9999);
        ammo.insert(PartSlot(7), 5);
        r.players = vec![PlayerResult { player: PlayerSlot(0), hull_lost: false, damage, fuel_fraction: f64::NAN, ammo, expended: BTreeMap::new(), ammo_kg_expended: BTreeMap::new(), fuel_l_used: 0.0, passengers_lost: 0, dropped: false }];
        apply_result(&mut s, &r, true, &d, &mut ev);
        let p = &s.players[0];
        // Out-of-range slots and cells are dropped; values clamp.
        assert_eq!(p.damage.parts.len(), 1);
        assert!(p.damage.cells.is_empty());
        assert_eq!(p.fuel_fraction, 0.0);
        assert_eq!(p.ammo.get(&PartSlot(0)), Some(&100));
        assert_eq!(p.ammo.len(), 1);
        // Repair: 50% of the damaged magazine's 40.
        assert_eq!(repair_cost(&s, PlayerSlot(0), &d), 20);
        s.resources.salvage = 19;
        assert_eq!(repair(&mut s, PlayerSlot(0), &d, &mut ev), Err(TheatreReject::NotEnoughSalvage));
        s.resources.salvage = 25;
        assert_eq!(repair(&mut s, PlayerSlot(0), &d, &mut ev), Ok(20));
        assert_eq!(s.resources.salvage, 5);
        assert!(s.players[0].damage.parts.is_empty());

        r.players[0].hull_lost = true;
        apply_result(&mut s, &r, true, &d, &mut ev);
        assert!(s.players[0].hull_lost);
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::HullLost { .. })));
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::CampaignOver)));
        assert_eq!(rebuild(&mut s, PlayerSlot(0), true, &d, &mut ev), Err(TheatreReject::IronmanLocked));
        assert_eq!(repair(&mut s, PlayerSlot(0), &d, &mut ev), Err(TheatreReject::HullNotReady));
        assert_eq!(repair(&mut s, PlayerSlot(3), &d, &mut ev), Err(TheatreReject::UnknownTarget));
        // Outside Ironman a short pool pays what it holds and the hull is rebuilt anyway.
        let full = rebuild_cost(&s, PlayerSlot(0), &d);
        assert!(full > 1);
        s.resources.salvage = full - 1;
        assert_eq!(rebuild(&mut s, PlayerSlot(0), false, &d, &mut ev), Ok(full - 1));
        assert_eq!(s.resources.salvage, 0);
        assert!(!s.players[0].hull_lost && s.players[0].ready_at_hour.is_some());
    }

    #[test]
    fn partial_repair_takes_parts_first() {
        let (mut s, d) = with_player();
        let mut ev = Vec::new();
        s.players[0].damage.parts.insert(PartSlot(0), 0.0);
        s.players[0].damage.parts.insert(PartSlot(1), 0.4);
        // Destroyed magazine 40, damaged tank 5.
        assert_eq!(repair_cost(&s, PlayerSlot(0), &d), 45);
        s.resources.salvage = 42;
        assert_eq!(repair_partial(&mut s, PlayerSlot(0), &d, &mut ev), Ok(40));
        assert_eq!(s.players[0].damage.parts.keys().copied().collect::<Vec<_>>(), vec![PartSlot(1)]);
        assert_eq!(s.resources.salvage, 2);
    }

    #[test]
    fn rearm_from_stock_costs_nothing() {
        let (mut s, d) = with_player();
        let mut ev = Vec::new();
        s.players[0].ammo.insert(PartSlot(0), 40);
        s.players[0].fuel_fraction = 0.5;
        s.resources.ammo_kg.insert(76, 1000.0);
        s.resources.fuel_l = 5000.0;
        assert_eq!(rearm_cost(&s, PlayerSlot(0), &d), 0);
        assert_eq!(rearm(&mut s, PlayerSlot(0), &d, &mut ev), Ok(0));
        assert_eq!(s.players[0].ammo[&PartSlot(0)], 100);
        assert_eq!(s.players[0].fuel_fraction, 1.0);
        assert!((s.resources.ammo_kg[&76] - 400.0).abs() < 1e-9);
        assert!((s.resources.fuel_l - 4000.0).abs() < 1e-9);
    }

    #[test]
    fn sextant_sunk_relocates_and_restores() {
        let (mut s, d) = with_player();
        let mut ev = Vec::new();
        s.resources.salvage = 1000;
        s.resources.fuel_l = 100.0;
        s.resources.intel = 4;
        s.open_coves = vec![id("C1"), id("C2")];
        s.sextant_node = id("C1");
        cut_lane_for_test(&mut s, &d);
        sextant_sunk(&mut s, &d, &mut ev);
        assert_eq!(s.resources.salvage, 700);
        assert!((s.resources.fuel_l - 70.0).abs() < 1e-9);
        assert_eq!(s.resources.intel, 4);
        assert_eq!(s.sextant_node, id("C2"));
        assert_eq!(s.sextant_refit_until_hour, Some(72));
        assert!(s.regions[&rid("r1")].combine_control);
        assert!(s.lanes.is_empty());
        assert_eq!(repair(&mut s, PlayerSlot(0), &d, &mut ev), Err(TheatreReject::NotAtCove));
    }

    fn cut_lane_for_test(s: &mut TheatreState, d: &TheatreData) {
        let mut ev = Vec::new();
        super::super::logistics::cut_lane(s, &id("S1"), &id("D"), d, &mut ev);
        assert!(!s.lanes.is_empty());
    }
}
