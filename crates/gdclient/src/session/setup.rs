//! Part of unit `gd-session`. Offline skirmish and sandbox setups: the `setup` Dictionaries of
//! `Session.start_skirmish` and `Session.start_sandbox` read into plain data, the player's design checked, and
//! the sortie config built through `sim_systems::modes` (L959, L961). No Godot type below `read_*`.
//!
//! Decisions where the spec is silent:
//! - An offline skirmish validates the fielded design against the campaign ruleset of the chosen act
//!   (`Ruleset::campaign`: the act's per-hull tonnage cap, every control layer, complexity informational).
//!   A vs-AI sortie has no opponent to protect, so only the cap the player picked binds.
//! - An offline sandbox uses the open ruleset with the largest cap (1,800 t) and no complexity cap.
//! - Sandbox target rows: a row marked hostile sails as an AI task group that hunts the player and fires
//!   back (declared Hostile); an unmarked row is a passive target group at anchor that never fires (declared
//!   Suspect, so it is engaged only when the player designates it). Hostile groups start 9 km
//!   off the player's bow, passive groups 4 km off, each further row turned 25 degrees round from the last,
//!   ships of a row line abreast 600 m apart facing the player. Positions keep 400 m clear of islands and
//!   1 km inside the map edge. At most 8 rows of 8 ships, 24 ships in all.
use crate::convert::{get_bool, get_dict, get_i64, get_text};
use crate::enums::GdEnum;
use content::blueprint::Blueprint;
use content::campaign::{CampaignData, DifficultySliders, FleetKind, Formation, MapDef, MissionType};
use content::catalogue::Catalogue;
use content::enums::{Act, GameMode, Stance, TimeOfDay};
use content::ids::{ContentHash, PlayerSlot, SideId, TextKey};
use content::ruleset::{Ruleset, RulesetKind};
use content::validate::ValidationContext;
use content::weather::{SeaState, WeatherKind};
use glam::{DVec2, DVec3};
use godot::prelude::*;
use sim_systems::ids::{GroupId, SideMask};
use sim_systems::modes::{LobbyPlayer, LobbySetup, SkirmishOptions};
use sim_systems::protocol::config::{BlueprintRef, Faction, GroupInit, GroupOrders, SideDef, SortieConfig, VesselInit, VesselRole};
use sim_systems::protocol::Msg;
use std::collections::BTreeMap;

/// Lobby conditions shared by both setups.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Conditions { pub weather: WeatherKind, pub sea_state: SeaState, pub time_of_day: TimeOfDay, pub seed: u64 }

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SkirmishSetup { pub template: MissionType, pub act: Act, pub enemy_per_player: u8, pub difficulty: DifficultySliders, pub conditions: Conditions, pub blueprint: String }

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TargetRow { pub blueprint: String, pub hostile: bool, pub count: u8 }

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SandboxSetup { pub conditions: Conditions, pub blueprint: String, pub targets: Vec<TargetRow> }

/// Who fields the design: the local player in slot 0.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LocalPlayer { pub client_id: u64, pub name: String }

/// Most enemy hulls per player a skirmish setup accepts.
pub(crate) const MAX_ENEMY_PER_PLAYER: u8 = 8;
pub(crate) const MAX_TARGET_ROWS: usize = 8;
pub(crate) const MAX_TARGETS_PER_ROW: u8 = 8;
pub(crate) const MAX_TARGETS: usize = 24;
const HOSTILE_RANGE_M: f64 = 9_000.0;
const PASSIVE_RANGE_M: f64 = 4_000.0;
const ROW_TURN_DEG: f64 = 25.0;
const LINE_SPACING_M: f64 = 600.0;
const ISLAND_CLEARANCE_M: f64 = 400.0;
const EDGE_MARGIN_M: f64 = 1_000.0;
/// Side ids the sandbox adds for its target groups (the mode's own sides use 0 and 1).
const HOSTILE_SIDE: SideId = SideId(5);
const PASSIVE_SIDE: SideId = SideId(6);

/// A setup problem as the `MsgDict` the setup screens list.
pub(crate) fn problem(key: &str) -> Msg { Msg::new(key) }

fn read_conditions(dict: &VarDictionary) -> Result<Conditions, Msg> {
    let weather = match get_i64(dict, "weather") { None => WeatherKind::Clear, Some(v) => WeatherKind::from_gd(v).ok_or_else(|| problem("session.problem.weather"))? };
    let sea_state = match get_i64(dict, "sea_state") { None => SeaState(3), Some(v) if (0..=8).contains(&v) => SeaState(v as u8), Some(_) => return Err(problem("session.problem.sea_state")) };
    let time_of_day = match get_i64(dict, "time_of_day") { None => TimeOfDay::Dusk, Some(v) => TimeOfDay::from_gd(v).ok_or_else(|| problem("session.problem.time_of_day"))? };
    // 0 (or none) asks for a random seed; GDScript ints are signed, so the sign is dropped.
    let seed = get_i64(dict, "seed").map(|s| s.unsigned_abs()).filter(|s| *s != 0).unwrap_or_else(random_seed);
    Ok(Conditions { weather, sea_state, time_of_day, seed })
}

/// A seed from the process's random hasher keys: client-side, never fed back into a running Sim.
pub(crate) fn random_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.finish().max(1)
}

/// The `start_skirmish` Dictionary. `presets` are the campaign's difficulty presets (`DifficultyDict.preset`).
pub(crate) fn read_skirmish(dict: &VarDictionary, presets: &[content::campaign::DifficultyPreset]) -> Result<SkirmishSetup, Vec<Msg>> {
    let mut problems = Vec::new();
    let template = match get_i64(dict, "template") { None => Some(MissionType::ConvoyRaid), Some(v) => MissionType::from_gd(v) };
    if template.is_none() { problems.push(problem("session.problem.template")); }
    let act = match get_i64(dict, "act") { None => Some(Act::Act1), Some(v) => Act::from_gd(v) };
    if act.is_none() { problems.push(problem("session.problem.act")); }
    let enemy_per_player = get_i64(dict, "enemy_per_player").unwrap_or(1).clamp(1, MAX_ENEMY_PER_PLAYER as i64) as u8;
    let difficulty = get_dict(dict, "difficulty").map(|d| difficulty_of(&d, presets)).unwrap_or_default();
    let conditions = read_conditions(dict).map_err(|p| problems.push(p)).ok();
    let blueprint = get_text(dict, "blueprint").unwrap_or_default().trim().to_string();
    if blueprint.is_empty() { problems.push(problem("session.problem.no_blueprint")); }
    match (template, act, conditions) {
        (Some(template), Some(act), Some(conditions)) if problems.is_empty() => Ok(SkirmishSetup { template, act, enemy_per_player, difficulty, conditions, blueprint }),
        _ => Err(problems),
    }
}

/// Sliders from a `DifficultyDict`: all six keys, else the named preset, else normal.
fn difficulty_of(dict: &VarDictionary, presets: &[content::campaign::DifficultyPreset]) -> DifficultySliders {
    if let Some(s) = crate::convert::difficulty_from_dict(dict) { return s; }
    let name = get_text(dict, "preset").unwrap_or_default();
    presets.iter().find(|p| p.id.as_str() == name).map(|p| p.sliders).unwrap_or_default()
}

/// The `start_sandbox` Dictionary.
pub(crate) fn read_sandbox(dict: &VarDictionary) -> Result<SandboxSetup, Vec<Msg>> {
    let mut problems = Vec::new();
    let conditions = read_conditions(dict).map_err(|p| problems.push(p)).ok();
    let blueprint = get_text(dict, "blueprint").unwrap_or_default().trim().to_string();
    if blueprint.is_empty() { problems.push(problem("session.problem.no_blueprint")); }
    let mut targets = Vec::new();
    for row in dicts_at(dict, "targets").into_iter().take(MAX_TARGET_ROWS) {
        let path = get_text(&row, "blueprint").unwrap_or_default().trim().to_string();
        if path.is_empty() { continue; }
        let count = get_i64(&row, "count").unwrap_or(1).clamp(1, MAX_TARGETS_PER_ROW as i64) as u8;
        targets.push(TargetRow { blueprint: path, hostile: get_bool(&row, "hostile").unwrap_or(true), count });
    }
    match conditions {
        Some(conditions) if problems.is_empty() => Ok(SandboxSetup { conditions, blueprint, targets }),
        _ => Err(problems),
    }
}

/// The Dictionaries of an `Array` value; anything else in it is skipped.
fn dicts_at(dict: &VarDictionary, key: &str) -> Vec<VarDictionary> {
    let Some(v) = dict.get(key).filter(|v| !v.is_nil()) else { return Vec::new() };
    let Ok(array) = v.try_to::<VarArray>() else { return Vec::new() };
    array.iter_shared().filter_map(|e| e.try_to::<VarDictionary>().ok()).collect()
}

/// Canonical form with its hash, as configs reference inline designs (`BlueprintRef::Inline`).
pub(crate) fn canonical(mut bp: Blueprint) -> Blueprint {
    content::blueprint::io::canonicalize_blueprint(&mut bp);
    bp.header.hash = content::blueprint::io::compute_hash(&bp);
    bp
}

/// Stock path form: forward slashes, no leading `./` or `blueprints/` (as `content` keys them).
pub(crate) fn stock_key(path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    let p = p.trim_start_matches("./");
    p.strip_prefix("blueprints/").unwrap_or(p).to_string()
}

pub(crate) fn skirmish_ruleset(act: Act) -> Ruleset { Ruleset::campaign(act) }
pub(crate) fn sandbox_ruleset() -> Ruleset {
    Ruleset { kind: RulesetKind::Open, tonnage_cap_t: Act::Act4.hull_cap_t(), complexity_per_vessel: 0.0, complexity_per_lobby: 0.0, part_limits: BTreeMap::new(), max_length_m: 90.0, luau: true }
}

/// Every reason the design may not be fielded under `ruleset`, as messages.
pub(crate) fn design_problems(bp: &Blueprint, catalogue: &Catalogue, ruleset: &Ruleset, enemy_class: bool) -> Vec<Msg> {
    let ctx = ValidationContext { ruleset, available: None, enemy_class, complexity: None };
    content::validate::validate_blueprint(bp, catalogue, &ctx).issues.iter().map(crate::convert::issue_msg).collect()
}

fn lobby(mode: GameMode, ruleset: Ruleset, c: &Conditions, player: &LocalPlayer, hash: ContentHash, skirmish: Option<SkirmishOptions>) -> LobbySetup {
    LobbySetup {
        mode,
        ruleset,
        seed: c.seed,
        players: vec![LobbyPlayer { slot: PlayerSlot(0), client_id: player.client_id, name: player.name.clone(), team: 0, blueprint: Some(hash), ready: true, complexity: 0.0 }],
        skirmish,
        weather: c.weather,
        sea_state: c.sea_state,
        time_of_day: c.time_of_day,
        map: None,
        at_1_0: false,
    }
}

/// The skirmish config, or why it cannot be built. `bp` is canonical.
pub(crate) fn skirmish_config(setup: &SkirmishSetup, bp: &Blueprint, player: &LocalPlayer, campaign: &CampaignData, catalogue: &Catalogue) -> Result<SortieConfig, Msg> {
    let options = SkirmishOptions { template: setup.template, act: setup.act, enemy_per_player: setup.enemy_per_player, difficulty: setup.difficulty };
    let lobby = lobby(GameMode::Skirmish, skirmish_ruleset(setup.act), &setup.conditions, player, bp.header.hash, Some(options));
    let config = sim_systems::modes::skirmish_config(&lobby, std::slice::from_ref(bp), campaign, catalogue);
    check_config(config)
}

/// A built config must seat the player with a hull and pass the Sim's own checks.
fn check_config(config: SortieConfig) -> Result<SortieConfig, Msg> {
    if !config.players.iter().any(|p| p.slot == PlayerSlot(0) && p.vessel.is_some()) { return Err(problem("session.problem.no_hull")); }
    sim_systems::sim::setup::validate_config(&config).map_err(|_| problem("session.problem.config"))?;
    Ok(config)
}

/// Where a sandbox target's design comes from, resolved by the caller.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TargetDesign {
    /// A stock class (`ClassDict.blueprint`).
    Stock { path: String, class: Option<content::ids::ClassId> },
    /// One of the player's designs, canonical.
    Inline(Box<Blueprint>),
}

/// The sandbox config with its target groups. `bp` is canonical; `targets` pairs each resolved design with
/// its row.
pub(crate) fn sandbox_config(setup: &SandboxSetup, bp: &Blueprint, targets: &[(TargetDesign, bool, u8)], player: &LocalPlayer, campaign: &CampaignData) -> Result<SortieConfig, Msg> {
    let lobby = lobby(GameMode::Sandbox, sandbox_ruleset(), &setup.conditions, player, bp.header.hash, None);
    let mut config = sim_systems::modes::sandbox_config(&lobby, std::slice::from_ref(bp), campaign);
    add_targets(&mut config, targets);
    check_config(config)
}

/// Compass heading (radians) to the unit map direction (x, z): 0 = north = -Z, clockwise.
/// Client-side maths: the config is built once here and then travels as data (checkpoints carry it), so
/// std trigonometry is fine; gdclient has no libm.
#[allow(clippy::disallowed_methods)]
fn forward(heading_rad: f64) -> DVec2 { let (s, c) = heading_rad.sin_cos(); DVec2::new(s, -c) }

/// Move `p` outward from any island it is too close to, then inside the map edge.
fn clear_of_land(mut p: DVec2, map: &MapDef) -> DVec2 {
    for _ in 0..4 {
        let mut moved = false;
        for island in &map.islands {
            let c = DVec2::from(island.centre_m);
            let need = island.radius_m.max(0.0) + ISLAND_CLEARANCE_M;
            let d = p - c;
            let dist = d.length();
            if dist.is_finite() && dist < need {
                let dir = if dist > 1e-6 { d / dist } else { DVec2::X };
                p = c + dir * need;
                moved = true;
            }
        }
        if !moved { break; }
    }
    let half = DVec2::new(map.size_m[0].abs(), map.size_m[1].abs()) * 0.5 - DVec2::splat(EDGE_MARGIN_M);
    if half.x > 0.0 && half.y > 0.0 { p = p.clamp(-half, half); }
    p
}

/// Add the sandbox's target groups (see the module decisions).
pub(crate) fn add_targets(c: &mut SortieConfig, targets: &[(TargetDesign, bool, u8)]) {
    if targets.is_empty() { return; }
    let Some(own) = c.players.iter().find(|p| p.slot == PlayerSlot(0)).and_then(|p| p.vessel).and_then(|i| c.vessels.get(i as usize)).cloned() else { return };
    let player_side = own.side;
    let origin = DVec2::new(own.pos.x, own.pos.z);
    let heading = if own.heading_rad.is_finite() { own.heading_rad } else { 0.0 };
    let foes = SideMask::only(player_side);
    let any_hostile = targets.iter().any(|t| t.1);
    let any_passive = targets.iter().any(|t| !t.1);
    if any_hostile && !c.sides.iter().any(|s| s.id == HOSTILE_SIDE) {
        c.sides.push(SideDef { id: HOSTILE_SIDE, faction: Faction::Combine, name: TextKey::new("session.side.hostile_targets"), hostile_to: foes, allied: SideMask::only(HOSTILE_SIDE), pvp_team: None });
    }
    if any_passive && !c.sides.iter().any(|s| s.id == PASSIVE_SIDE) {
        c.sides.push(SideDef { id: PASSIVE_SIDE, faction: Faction::Target, name: TextKey::new("session.side.targets"), hostile_to: SideMask::NONE, allied: SideMask::only(PASSIVE_SIDE), pvp_team: None });
    }
    // The player's side sees the hostile targets as enemies for its AI rules of engagement too.
    if any_hostile { if let Some(s) = c.sides.iter_mut().find(|s| s.id == player_side) { s.hostile_to = s.hostile_to.with(HOSTILE_SIDE); } }
    let first_group = c.groups.iter().map(|g| g.id.0 + 1).max().unwrap_or(0);
    let mut placed = 0usize;
    for (row, (design, hostile, count)) in targets.iter().enumerate() {
        if placed >= MAX_TARGETS { break; }
        let n = (*count as usize).clamp(1, MAX_TARGETS_PER_ROW as usize).min(MAX_TARGETS - placed);
        let turn = ROW_TURN_DEG.to_radians() * ((row as f64 + 1.0) / 2.0).floor() * if row % 2 == 0 { 1.0 } else { -1.0 };
        let bearing = heading + turn;
        let range = if *hostile { HOSTILE_RANGE_M } else { PASSIVE_RANGE_M };
        let centre = origin + forward(bearing) * range;
        let facing = (bearing + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU);
        let right = forward(facing + std::f64::consts::FRAC_PI_2);
        let group = GroupId(first_group + row as u16);
        let side = if *hostile { HOSTILE_SIDE } else { PASSIVE_SIDE };
        let (blueprint, class) = match design {
            TargetDesign::Stock { path, class } => (BlueprintRef::Stock(path.clone()), class.clone()),
            TargetDesign::Inline(bp) => {
                if !c.blueprints.iter().any(|b| b.header.hash == bp.header.hash) { c.blueprints.push((**bp).clone()); }
                (BlueprintRef::Inline(bp.header.hash), None)
            }
        };
        if let Some(cl) = &class {
            if !c.intel.order_of_battle.contains(cl) { c.intel.order_of_battle.push(cl.clone()); }
            if *hostile && !c.intel.declared_hostile.contains(cl) { c.intel.declared_hostile.push(cl.clone()); }
        }
        let tag = if *hostile { "hostile_targets" } else { "targets" };
        for i in 0..n {
            let offset = (i as f64 - (n as f64 - 1.0) * 0.5) * LINE_SPACING_M;
            let pos = clear_of_land(centre + right * offset, &c.map);
            c.vessels.push(VesselInit {
                blueprint: blueprint.clone(), side, class: class.clone(), group: Some(group), pos: DVec3::new(pos.x, 0.0, pos.y), heading_rad: facing,
                speed_kn: if *hostile { 12.0 } else { 0.0 }, arrives_s: 0, damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None,
                veterancy: 0.0, tag: tag.into(), declared: Some(if *hostile { Stance::Hostile } else { Stance::Suspect }), emcon: None, passengers: 0, role: VesselRole::Target,
            });
        }
        placed += n;
        c.groups.push(GroupInit {
            id: group, side, kind: if *hostile { FleetKind::TaskGroup } else { FleetKind::Patrol }, formation: Formation::LineAbreast,
            orders: if *hostile { GroupOrders::Hunt } else { GroupOrders::Anchor }, ace: None, theatre_group: None, tag: tag.into(),
        });
    }
}

#[cfg(test)]
mod tests;
