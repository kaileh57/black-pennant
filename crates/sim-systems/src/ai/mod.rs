//! Unit `sys-ai`: the enemy AI's state container and per-tick orchestration. Sibling units:
//! `commander.rs` = `sys-ai-commander`, `group.rs` = `sys-ai-group`, `unit.rs` = `sys-ai-unit`,
//! `path.rs` = `sys-ai-path` (hard), `ace.rs` = `sys-ai-ace`, `air.rs` = `sys-ai-air` (Phase::Act2),
//! `sub.rs` = `sys-ai-sub` (Phase::Act3). DESIGN.md L756-770 (enemy AI levels, formation, pathing), L772
//! (doctrine cards), L774 (aces), L851-852 (accuracy and reaction time), L416 (0.5 ms share of the tick).
//!
//! Three levels exist in the spec. The Admiral plans in theatre time and lives in `crate::theatre::admiral`.
//! Inside a sortie: one commander per AI side (belief about where the player is, group tasking, in-sortie
//! alert), one state per group (formation, route), one captain per hull. A captain is a behaviour tree on top
//! of the same sensor and fire-control pipeline the player's ship uses: it reads only its side's picture and
//! writes the same `ShipOrders` a player's clicks write, at the difficulty's accuracy and reaction time.
//! No AI function reads ground truth about the other side. Decoys and emission control fool a real model.
//! Flocking and collision avoidance adjust heading and speed only, never the path.
pub mod ace;
pub mod air;
pub mod commander;
pub mod group;
pub mod path;
pub mod sub;
pub mod unit;

use crate::prelude::*;
use crate::protocol::config::{Faction, GroupInit, SortieConfig};
use crate::ship::{Proficiency, ShipTable};
use crate::tracks::{Pictures, SidePicture};
use content::campaign::{AceCue, CardEffect};
use sim_core::munitions::Steer;
use sim_core::vessel::VesselStatus;

/// Doctrine-card and ace-doctrine modifiers in force for one AI side (L772).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DoctrineMods { pub emcon: Option<Emcon>, pub engagement_range_factor: f64, pub flags: BTreeSet<String> }
impl Default for DoctrineMods { fn default() -> Self { Self { emcon: None, engagement_range_factor: 1.0, flags: BTreeSet::new() } } }

/// Fold active card effects into modifiers; later cards override earlier ones. `add_fleet`, `use_variants` and
/// `patrol_density_factor` are applied by the operation generator, not here. Real code.
pub fn doctrine_mods(cards: &[(CardId, CardEffect)]) -> DoctrineMods {
    let mut m = DoctrineMods::default();
    for (_, e) in cards {
        if e.enemy_emcon.is_some() { m.emcon = e.enemy_emcon; }
        if let Some(f) = e.engagement_range_factor { m.engagement_range_factor *= f; }
        for f in &e.behaviour_flags { m.flags.insert(f.clone()); }
    }
    m
}


/// Everything the enemy AI remembers during one sortie.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AiState {
    /// One per AI side, ascending side id.
    pub commanders: Vec<commander::CommanderState>,
    /// Ascending group id.
    pub groups: Vec<group::GroupState>,
    pub planner: path::PathPlanner,
    pub aces: Vec<ace::AceState>,
    pub air: air::AirState,
    pub mods: DoctrineMods,
    /// Private bookkeeping of the orchestration (unit `sys-ai`). Build states with [`init`] or
    /// `..Default::default()`.
    #[serde(default)]
    pub(crate) mem: AiMemory,
}

/// What the orchestration remembers between ticks besides the public fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AiMemory {
    /// Crewed hulls of commander sides seen so far, ascending vessel.
    pub(crate) hulls: Vec<HullWatch>,
    /// Ships whose proficiency and ace bookkeeping are done, ascending.
    pub(crate) adopted: Vec<EntityId>,
    /// No further preset change for a ship before this tick, ascending vessel.
    pub(crate) preset_hold: Vec<(EntityId, u64)>,
}

/// One own hull a commander watches for losses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct HullWatch { pub(crate) vessel: EntityId, pub(crate) side: SideId, pub(crate) last_pos: DVec2, pub(crate) lost: bool }

/// Things the AI asks the Sim to do besides writing ship orders.
#[derive(Clone, Debug, PartialEq)]
pub enum AiOut {
    /// A Combine hull strikes its colours (owner decision pending: only `SurrenderRule::Never` ships, L537).
    Strike { vessel: EntityId },
    AceLine { ace: AceId, cue: AceCue, text: Msg, from: EntityId },
    SteerBody { body: EntityId, steer: Steer },
    /// In-sortie alert level of a side changed.
    Alert { side: SideId, level: u8 },
    CallReinforcements { side: SideId },
    /// A captain changed the ship's L0 preset in `Ship::settings`. Presets are baked into stage parameters at
    /// compile time, so the Sim recompiles the ship's graph (`ShipDesign::recompile` under the sortie's
    /// ruleset, then `graph::vm::init_state` with the previous state). Rate-limited to one per ship per
    /// 30 s. (Emission settings are written to `Ship::settings::emcon` directly and need no recompile.)
    SettingsChanged { vessel: EntityId },
    /// An aircraft releases a weapon: the Sim spawns the body at the aircraft and registers it with guidance.
    Release(air::Release),
}

// ---------------------------------------------------------------------------------------------------------
// Constants (the spec fixes none of these; listed in the unit's decisions)
// ---------------------------------------------------------------------------------------------------------

/// Belief grid cell edge, metres (100 x 100 cells on the largest map).
const BELIEF_CELL_M: f64 = 1_000.0;
/// Shortest gap between two preset changes of one ship (each costs a graph recompile), s.
const PRESET_HOLD_S: u64 = 30;
/// A hull that knew it was in a fight this recently has reported it before it was lost, s.
const REPORT_WINDOW_S: u64 = 300;
const TICKS_PER_S: u64 = sim_core::time::TICK_HZ as u64;

/// Commander phase within its 1 Hz period, staggered by side.
fn commander_phase(side: SideId) -> u64 { (side.0 as u64 * 13) % TICKS_PER_S }
/// Group phase within its 2 Hz period, staggered by group id.
fn group_phase(id: GroupId) -> u64 { (id.0 as u64 * 7) % (TICKS_PER_S / 2) }

/// Build the AI state for a sortie: commanders for AI sides, groups from `SortieConfig::groups`, clearance
/// masks per class, ace states, doctrine modifiers. Captains are created per ship in `Ship::captain` by
/// `unit::new_captain`. Unit: `sys-ai`.
///
/// - Commanders: one per Combine side (civilian, protected and target sides run captains only).
/// - Groups: one per `GroupInit` (first entry wins on a repeated id), even when no member has spawned yet:
///   members are the ships whose captain names the group and whose side matches; later arrivals are added by
///   the Sim's spawn bookkeeping.
/// - The path planner is built from the terrain only when there are groups, with the clearance mask each
///   group plans for pre-built (sandbox duels and trials then cost nothing at load).
/// - Aces: one state per distinct ace leading a group, on the member whose captain already names the ace,
///   else the group's largest hull (ties to the lowest entity); that captain is marked as the ace. An ace
///   whose group has not spawned is attached when its first hull arrives.
/// - AI hulls whose proficiency is still the stock default get the difficulty's accuracy and reaction time
///   (L851-852); proxies of player hulls keep stock officers.
pub fn init(config: &SortieConfig, world: &World, ships: &mut ShipTable, ctx: &SimCtx<'_>) -> AiState {
    let mut st = AiState { mods: doctrine_mods(&config.cards), ..Default::default() };

    let mut sides: Vec<SideId> = config.sides.iter().filter(|s| s.faction == Faction::Combine).map(|s| s.id).collect();
    sides.sort();
    sides.dedup();
    st.commanders = sides.iter().map(|&side| commander::CommanderState { side, belief: commander::new_belief(&config.map, BELIEF_CELL_M), ..Default::default() }).collect();

    let mut inits: Vec<&GroupInit> = Vec::new();
    for gi in &config.groups { if !inits.iter().any(|g| g.id == gi.id) { inits.push(gi); } }
    inits.sort_by_key(|g| g.id);
    st.groups = inits.iter().map(|gi| {
        let members: Vec<EntityId> = ships.ships.iter().filter(|s| s.side == gi.side && s.captain.as_ref().is_some_and(|c| c.group == Some(gi.id))).map(|s| s.vessel).collect();
        group::new_group(gi, members)
    }).collect();

    if !st.groups.is_empty() {
        st.planner = path::PathPlanner::new(world.terrain());
        for g in &st.groups {
            let views: Vec<group::Member> = g.members.iter().filter_map(|&m| group::member_view(m, g, ships, world, ctx.tick.0)).collect();
            if views.iter().any(|m| m.active) {
                let (beam, draft) = group::mask_numbers(&views);
                st.planner.ensure_mask(beam, draft);
            }
        }
    }

    for g in &st.groups {
        let Some(ace) = g.ace.clone() else { continue };
        if st.aces.iter().any(|a| a.ace == ace) { continue; }
        st.aces.push(ace::AceState { ace, vessel: EntityId::NONE, group: Some(g.id), ..Default::default() });
    }
    adopt(&mut st, world, ships, ctx);
    st
}

/// One tick of enemy AI in this fixed order: path planner (budgeted), commanders (1 Hz, staggered by side),
/// groups (2 Hz, staggered by group id), captains (each on its own reaction-time cadence, ascending vessel),
/// (the 1 Hz and 2 Hz cadences are this contract's choice inside the 0.5 ms AI share; the spec fixes none),
/// aces, aircraft and submarines. Writes `Ship::orders` and settings of AI ships; appends requests to `out`.
/// Returns work units (decisions made, A* expansions). Unit: `sys-ai`.
///
/// Submarines decide inside their captains (`unit::decide` hands them to `sub::decide`). Ace lines are spoken
/// from the physics events by the Sim (`ace::facts` needs the tick's events and `ace::lines` the campaign's
/// ace definitions); this step keeps the ace bookkeeping (which hull is the ace). Doctrine cards apply to
/// Combine sides only; every other captain decides with no card in force.
pub fn step(state: &mut AiState, world: &World, ships: &mut ShipTable, pictures: &Pictures, ctx: &SimCtx<'_>, astar_budget: u32, out: &mut Vec<AiOut>) -> (u32, u32) {
    let now = ctx.tick.0;
    let mut decisions = 0u32;
    adopt(state, world, ships, ctx);

    // 1. Path planner.
    let expansions = state.planner.step(astar_budget, now);

    // 2. Commanders.
    for i in 0..state.commanders.len() {
        let side = state.commanders[i].side;
        if !ctx.tick.is_due(1, commander_phase(side)) { continue; }
        let losses = watch_losses(&mut state.mem, side, world, ships, ctx);
        let empty = empty_picture(side);
        let picture = pictures.side(side).unwrap_or(&empty);
        let before = (state.commanders[i].alert, state.commanders[i].reinforcements_called);
        let orders = commander::update(&mut state.commanders[i], picture, &state.groups, &losses, ctx);
        decisions = decisions.saturating_add(1);
        for (gid, o) in orders {
            if let Some(g) = state.groups.iter_mut().find(|g| g.id == gid && g.side == side) { g.orders = Some(o); }
        }
        let c = &state.commanders[i];
        out.extend(commander_events(side, before, (c.alert, c.reinforcements_called)));
    }

    // 3. Groups.
    let no_cards = DoctrineMods::default();
    for gi in 0..state.groups.len() {
        let (id, side) = (state.groups[gi].id, state.groups[gi].side);
        if !ctx.tick.is_due(2, group_phase(id)) { continue; }
        let empty = empty_picture(side);
        let picture = pictures.side(side).unwrap_or(&empty);
        let mods = if state.commanders.iter().any(|c| c.side == side) { &state.mods } else { &no_cards };
        let directives = group::update(&mut state.groups[gi], ships, world, picture, &mut state.planner, mods, ctx);
        decisions = decisions.saturating_add(1);
        for (v, d) in directives {
            if let Some(c) = ships.get_mut(v).and_then(|s| s.captain.as_mut()) { c.directive = d; }
        }
    }

    // 4. Captains, ascending vessel.
    for idx in 0..ships.ships.len() {
        let due = ships.ships[idx].captain.as_ref().is_some_and(|c| now >= c.next_decision_tick);
        if !due { continue; }
        let vessel = ships.ships[idx].vessel;
        // Wrecks and despawned hulls have nobody left to decide.
        let afloat = world.vessel(vessel).is_some_and(|v| matches!(v.status, VesselStatus::Active | VesselStatus::Disabled));
        if !afloat { continue; }
        let side = ships.ships[idx].side;
        let empty = empty_picture(side);
        let picture = pictures.side(side).unwrap_or(&empty);
        let mods = if state.commanders.iter().any(|c| c.side == side) { &state.mods } else { &no_cards };
        let Some(mut captain) = ships.ships[idx].captain.take() else { continue };
        let proficiency = ships.ships[idx].proficiency;
        // Hulls keeping station on this one: they give way to it, not it to them.
        let followers: Vec<EntityId> = ships.ships.iter().filter(|s| s.orders.formation.is_some_and(|f| f.leader == vessel)).map(|s| s.vessel).collect();
        let decided = unit::decide(&mut captain, &ships.ships[idx], world, picture, mods, proficiency, &followers, ctx);
        let ship = &mut ships.ships[idx];
        ship.captain = Some(captain);
        decisions = decisions.saturating_add(1);
        if let Some(o) = decided.orders { ship.orders = o; }
        if let Some(e) = decided.emcon { ship.settings.emcon = e; }
        if let Some(p) = decided.preset {
            if preset_allowed(&mut state.mem.preset_hold, vessel, now) {
                ship.settings.preset = p;
                out.push(AiOut::SettingsChanged { vessel });
            }
        }
        if decided.strike { out.push(AiOut::Strike { vessel }); }
    }

    // 5. Aces: bookkeeping only (see above); done in `adopt`.

    // 6. Aircraft.
    if !state.air.aircraft.is_empty() {
        let mut steer = Vec::new();
        let mut releases = Vec::new();
        let work = air::step(&mut state.air, world, pictures, ctx, &mut steer, &mut releases);
        decisions = decisions.saturating_add(work);
        out.extend(steer.into_iter().map(|(body, steer)| AiOut::SteerBody { body, steer }));
        out.extend(releases.into_iter().map(AiOut::Release));
    }

    (decisions, expansions)
}

fn empty_picture(side: SideId) -> SidePicture { SidePicture { side, tracks: Vec::new(), next_id: 1 } }

/// Alert and reinforcement requests from a commander update.
fn commander_events(side: SideId, before: (commander::SortieAlert, bool), after: (commander::SortieAlert, bool)) -> Vec<AiOut> {
    let mut out = Vec::new();
    if before.0 != after.0 { out.push(AiOut::Alert { side, level: after.0 as u8 }); }
    if after.1 && !before.1 { out.push(AiOut::CallReinforcements { side }); }
    out
}

/// True (and starts the hold) when the ship's preset may change now.
fn preset_allowed(hold: &mut Vec<(EntityId, u64)>, vessel: EntityId, now: u64) -> bool {
    let until = now.saturating_add(PRESET_HOLD_S * TICKS_PER_S);
    match hold.binary_search_by(|h| h.0.cmp(&vessel)) {
        Ok(i) if now < hold[i].1 => false,
        Ok(i) => { hold[i].1 = until; true }
        Err(i) => { hold.insert(i, (vessel, until)); true }
    }
}

/// Take on ships the AI has not seen yet: difficulty proficiency for AI hulls, loss watch for commander
/// sides, and ace hulls for aces whose group has spawned.
fn adopt(st: &mut AiState, world: &World, ships: &mut ShipTable, ctx: &SimCtx<'_>) {
    let diff = ctx.difficulty.clamped();
    for ship in ships.ships.iter_mut() {
        let Err(i) = st.mem.adopted.binary_search(&ship.vessel) else { continue };
        let Some(captain) = ship.captain.as_ref() else { continue };
        st.mem.adopted.insert(i, ship.vessel);
        if ship.player.is_none() && captain.behaviour != unit::Behaviour::Proxy && ship.proficiency == Proficiency::default() {
            ship.proficiency = Proficiency { aim_error_mult: diff.ai_accuracy, reaction_s: diff.ai_reaction_s, veterancy: ship.proficiency.veterancy };
        }
        if st.commanders.iter().any(|c| c.side == ship.side) {
            if let Err(k) = st.mem.hulls.binary_search_by(|h| h.vessel.cmp(&ship.vessel)) {
                let last_pos = world.vessel(ship.vessel).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)).filter(|p| p.is_finite()).unwrap_or(DVec2::ZERO);
                st.mem.hulls.insert(k, HullWatch { vessel: ship.vessel, side: ship.side, last_pos, lost: false });
            }
        }
    }
    for a in 0..st.aces.len() {
        if !st.aces[a].vessel.is_none() { continue; }
        let Some(gid) = st.aces[a].group else { continue };
        let Some(g) = st.groups.iter().find(|g| g.id == gid) else { continue };
        let ace = st.aces[a].ace.clone();
        if let Some(v) = ace_hull(g, &ace, ships, world) {
            st.aces[a].vessel = v;
            if let Some(c) = ships.get_mut(v).and_then(|s| s.captain.as_mut()) { c.ace = Some(ace); }
        }
    }
}

/// The ace's hull in its group: the member whose captain names the ace, else the largest afloat member.
fn ace_hull(g: &group::GroupState, ace: &AceId, ships: &ShipTable, world: &World) -> Option<EntityId> {
    if let Some(&m) = g.members.iter().find(|&&m| ships.get(m).and_then(|s| s.captain.as_ref()).is_some_and(|c| c.ace.as_ref() == Some(ace))) { return Some(m); }
    let mut best: Option<(f64, EntityId)> = None;
    for &m in &g.members {
        let Some(v) = world.vessel(m) else { continue };
        if v.side != g.side || !matches!(v.status, VesselStatus::Active) || ships.get(m).and_then(|s| s.captain.as_ref()).is_none() { continue; }
        let t = if v.tonnage_t.is_finite() { v.tonnage_t } else { 0.0 };
        if best.is_none_or(|(bt, be)| t > bt || (t == bt && m < be)) { best = Some((t, m)); }
    }
    best.map(|b| b.1)
}

/// Hulls of `side` lost since the last look, as `commander::update` takes them: (vessel, last position,
/// observed). A hull is lost once it is gone, sunk or a wreck, has struck, or now sails for another side.
fn watch_losses(mem: &mut AiMemory, side: SideId, world: &World, ships: &ShipTable, ctx: &SimCtx<'_>) -> Vec<(EntityId, DVec3, bool)> {
    let now = ctx.tick.0;
    let see = visual_watch_m(ctx);
    // Refresh positions first, so witnesses are where they are now.
    let mut fresh: Vec<usize> = Vec::new();
    for (i, h) in mem.hulls.iter_mut().enumerate() {
        if h.side != side || h.lost { continue; }
        match world.vessel(h.vessel) {
            Some(v) if v.side == side && !v.struck && matches!(v.status, VesselStatus::Active | VesselStatus::Disabled) => {
                let p = DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z);
                if p.is_finite() { h.last_pos = p; }
            }
            _ => { h.lost = true; fresh.push(i); }
        }
    }
    let witnesses: Vec<DVec2> = mem.hulls.iter().filter(|h| h.side == side && !h.lost)
        .filter(|h| world.vessel(h.vessel).is_some_and(|v| v.status == VesselStatus::Active)).map(|h| h.last_pos).collect();
    fresh.into_iter().map(|i| {
        let h = mem.hulls[i];
        let reported = ships.get(h.vessel).and_then(|s| s.captain.as_ref()).is_some_and(|c| reported_fight(c, now));
        (h.vessel, DVec3::new(h.last_pos.x, 0.0, h.last_pos.y), loss_observed(reported, h.last_pos, &witnesses, see))
    }).collect()
}

/// A captain that had perceived a Hostile contact, or had been under fire recently, has radioed it before
/// going down.
fn reported_fight(c: &unit::CaptainState, now: u64) -> bool {
    c.mem.known.iter().any(|k| k.hostile) || c.mem.under_fire_tick.is_some_and(|t| now.saturating_sub(t) <= REPORT_WINDOW_S * TICKS_PER_S)
}

/// A loss is observed when the hull reported its fight or another own hull had it in sight.
fn loss_observed(reported: bool, at: DVec2, witnesses: &[DVec2], see_m: f64) -> bool {
    reported || witnesses.iter().any(|w| w.distance(at) <= see_m)
}

/// Range at which a lookout sees a hull go down: the visual reference range (night factor applied) capped
/// by the weather's visibility.
fn visual_watch_m(ctx: &SimCtx<'_>) -> f64 {
    let s = &ctx.tuning.sensors;
    let night = if ctx.time_of_day == TimeOfDay::Night { s.night_visual_factor } else { 1.0 };
    let vis = if ctx.weather.visibility_m.is_finite() && ctx.weather.visibility_m > 0.0 { ctx.weather.visibility_m } else { s.visual_ref_range_m };
    let r = (s.visual_ref_range_m * night).min(vis);
    if r.is_finite() { r.max(0.0) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commander::SortieAlert;

    #[test]
    fn cadences_are_staggered_and_periodic() {
        let s: Vec<u64> = (0..4u8).map(|i| commander_phase(SideId(i))).collect();
        assert!(s.iter().enumerate().all(|(i, a)| s[..i].iter().all(|b| a != b)));
        let g: Vec<u64> = (0..6u16).map(|i| group_phase(GroupId(i))).collect();
        assert!(g.iter().enumerate().all(|(i, a)| g[..i].iter().all(|b| a != b)));
        // Exactly once per period.
        let due_c = (0..60u64).filter(|t| Tick(*t).is_due(1, commander_phase(SideId(2)))).count();
        let due_g = (0..60u64).filter(|t| Tick(*t).is_due(2, group_phase(GroupId(5)))).count();
        assert_eq!((due_c, due_g), (1, 2));
    }

    #[test]
    fn commander_changes_become_requests() {
        let side = SideId(1);
        assert!(commander_events(side, (SortieAlert::Unaware, false), (SortieAlert::Unaware, false)).is_empty());
        assert_eq!(commander_events(side, (SortieAlert::Suspicious, false), (SortieAlert::Alerted, true)), vec![AiOut::Alert { side, level: 2 }, AiOut::CallReinforcements { side }]);
        assert_eq!(commander_events(side, (SortieAlert::Engaged, true), (SortieAlert::Alerted, true)), vec![AiOut::Alert { side, level: 2 }]);
    }

    #[test]
    fn preset_changes_are_rate_limited() {
        let mut hold = Vec::new();
        let (a, b) = (EntityId::from_u64(3), EntityId::from_u64(1));
        assert!(preset_allowed(&mut hold, a, 100));
        assert!(!preset_allowed(&mut hold, a, 100 + 29 * 60));
        assert!(preset_allowed(&mut hold, b, 200));
        assert!(preset_allowed(&mut hold, a, 100 + 30 * 60));
        assert_eq!(hold.iter().map(|h| h.0).collect::<Vec<_>>(), vec![b, a]);
    }

    #[test]
    fn loss_observation_rules() {
        let at = DVec2::new(1_000.0, 0.0);
        assert!(loss_observed(true, at, &[], 5_000.0));
        assert!(!loss_observed(false, at, &[DVec2::new(10_000.0, 0.0)], 5_000.0));
        assert!(loss_observed(false, at, &[DVec2::new(5_000.0, 0.0)], 5_000.0));
        let mut c = unit::new_captain(unit::Behaviour::Combatant, None, None, content::campaign::SurrenderRule::Never);
        assert!(!reported_fight(&c, 10_000));
        c.mem.under_fire_tick = Some(10_000 - 60 * 60);
        assert!(reported_fight(&c, 10_000));
        c.mem.under_fire_tick = Some(0);
        assert!(!reported_fight(&c, 10_000 + REPORT_WINDOW_S * TICKS_PER_S));
        c.mem.known.push(unit::Known { id: TrackId(4), hostile: true, inbound: false });
        assert!(reported_fight(&c, 10_000 + REPORT_WINDOW_S * TICKS_PER_S));
    }

    #[test]
    fn doctrine_cards_fold_in_order() {
        let e1 = CardEffect { enemy_emcon: Some(Emcon::Silent), engagement_range_factor: Some(1.2), behaviour_flags: vec!["close_in".into()], ..Default::default() };
        let e2 = CardEffect { enemy_emcon: Some(Emcon::Active), engagement_range_factor: Some(0.5), behaviour_flags: vec!["aggressive".into()], ..Default::default() };
        let m = doctrine_mods(&[(CardId::new("a"), e1), (CardId::new("b"), e2)]);
        assert_eq!(m.emcon, Some(Emcon::Active));
        assert!((m.engagement_range_factor - 0.6).abs() < 1e-12);
        assert!(m.flags.contains("close_in") && m.flags.contains("aggressive"));
    }

    #[test]
    fn visual_watch_follows_weather_and_night() {
        let env = commander::tests::Env::new();
        let day = env.ctx(60);
        let r = visual_watch_m(&day);
        assert!(r > 0.0 && r <= env.tuning.sensors.visual_ref_range_m);
        let mut night = day;
        night.time_of_day = TimeOfDay::Night;
        assert!(visual_watch_m(&night) <= r);
        let fog = WeatherState { visibility_m: 400.0, ..WeatherState::default() };
        let mut foggy = day;
        foggy.weather = &fog;
        assert!(visual_watch_m(&foggy) <= 400.0);
    }
}
