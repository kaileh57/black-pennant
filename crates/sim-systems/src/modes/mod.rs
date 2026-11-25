//! Unit `sys-modes`. Skirmish, sandbox and PvP: lobby setup rules and sortie config builders.
//! DESIGN.md L73, L957-978 (modes, rulesets, complexity caps), L1031 (player limits from host upstream),
//! L47 (PvP and skirmish are the only modes where sides can be even).
//!
//! | Mode | Players | Rules |
//! | Skirmish | 1 to 4 against AI | a single operation from the campaign templates; chosen tonnage cap and enemy size |
//! | Sandbox | up to 8 | free play, own designs, join any time |
//! | PvP elimination | 1v1 and 2v2 at first release; 3v3 and 4v4 by 1.0 | last team afloat; tonnage and complexity caps |
//! | PvP escort (Phase::R100) | 1v1 to 4v4 | one side runs a convoy across a 20 to 40 km map; tonnage and complexity caps |
//! Every part is available from first launch in these modes; there is no unlock track and no campaign
//! carry-over. PvP limits come from the lobby ruleset: `standard` (default) allows presets, sliders and the
//! rules table and bans forked officers and graph edits; `open` allows them. Luau never runs in a PvP lobby.
//! Per-hull tonnage cap is one of 150, 400, 900 or 1,800 t (default 400 t). Complexity caps: 600 points per
//! vessel, 3,600 per lobby. No ranked play.
//!
//! Choices this unit makes where the spec is silent (design decisions):
//! - Lobby data is network data: a non-finite complexity counts as over every cap, a team other than 0 or 1
//!   makes the teams uneven, a player whose blueprint hash matches no supplied blueprint has no blueprint.
//! - Builders never fail: a player without a usable blueprint joins as a spectator. A custom map is sanitised
//!   (finite values, size within `MAX_MAP_M`) and gets the default map's spawn markers when it lacks them.
//! - PvP elimination has a 30 minute limit ([`ELIMINATION_LIMIT_S`]) that ends in a draw. Teams start line
//!   abreast 600 m apart, 12 km (1v1) to 24 km (4v4) apart on a point-symmetric map.
//! - Sandbox puts every player on one side unless someone picks team 1, in which case the two teams can spar.
//!   It offers every timed station action and has a shore landing and a survivor marker to practise on.
//! - Skirmish weather and time of day come from the lobby; the template's map is kept (its markers carry the
//!   objective). If no template can be generated, a plain fight against classes of the chosen act is built.
//! - PvP escort: team 0 escorts an AI convoy of 2 plus team-size freighters or tankers at 14 kn from entry to
//!   exit; half the convoy's tonnage through wins it ([`ESCORT_REQUIRED_FRACTION`]).
use crate::prelude::*;
use crate::protocol::config::{BlueprintRef, Faction, GroupInit, GroupOrders, ModeRules, PlayerInit, SideDef, SortieConfig, SortieIntel, VesselInit, VesselRole};
use crate::theatre::TheatreData;
use content::blueprint::Blueprint;
use content::campaign::{CampaignData, ClassRole, FleetKind, Formation, Island, MapDef, Marker, MarkerKind, MissionType, Objective, StationAction, MAX_MAP_M};
use content::ruleset::Ruleset;
use content::settings::SimSettings;
use content::validate::{Issue, ValidationContext};
use content::weather::{SeaState, WeatherKind};
use sim_core::rng::{Key, Stream};

/// DECISION: PvP elimination time limit; a draw when it runs out.
pub const ELIMINATION_LIMIT_S: u32 = 1_800;
/// DECISION: share of convoy tonnage that must reach the exit in PvP escort.
pub const ESCORT_REQUIRED_FRACTION: f64 = 0.5;
/// DECISION: convoy ordered speed in PvP escort.
pub const ESCORT_CONVOY_KN: f64 = 14.0;
/// Spacing of hulls in a team's starting line.
const LINE_SPACING_M: f64 = 600.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LobbyPlayer { pub slot: PlayerSlot, pub client_id: u64, pub name: String, /// PvP team 0 or 1; ignored elsewhere.
    pub team: u8, pub blueprint: Option<ContentHash>, pub ready: bool, /// Complexity points of the chosen design from its last trial.
    pub complexity: f64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkirmishOptions { pub template: MissionType, pub act: Act, /// Enemy hulls per player, at least 1; the 5x commitment rule does not bind skirmish.
    pub enemy_per_player: u8, pub difficulty: content::campaign::DifficultySliders }

/// Everything a lobby decides before a sortie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LobbySetup {
    pub mode: GameMode,
    pub ruleset: Ruleset,
    pub seed: u64,
    pub players: Vec<LobbyPlayer>,
    pub skirmish: Option<SkirmishOptions>,
    pub weather: WeatherKind,
    pub sea_state: SeaState,
    pub time_of_day: TimeOfDay,
    /// Map choice: `None` = the mode's default generated map for the seed.
    pub map: Option<MapDef>,
    /// First-release limits (false) or 1.0 limits (true): 4 players co-op, 3v3 and 4v4, escort.
    pub at_1_0: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LobbyIssue { TooManyPlayers { max: u8 }, TeamsUneven, TeamSizeNotOffered { size: u8 }, PlayerNotReady(PlayerSlot), NoBlueprint(PlayerSlot), Blueprint { player: PlayerSlot, issue: Issue }, LobbyOverComplexity { points: f64, cap: f64 }, ModeNotInBuild, LuauInPvp }

fn is_pvp(mode: GameMode) -> bool { matches!(mode, GameMode::PvpElimination | GameMode::PvpEscort) }

/// Team sizes a PvP mode offers.
fn team_sizes(mode: GameMode, at_1_0: bool) -> core::ops::RangeInclusive<u8> {
    match mode { GameMode::PvpElimination => if at_1_0 { 1..=4 } else { 1..=2 }, GameMode::PvpEscort => 1..=4, _ => 1..=8 }
}

/// Player-count, team and readiness rules without blueprints. Pure.
pub(crate) fn shape_issues(setup: &LobbySetup) -> Vec<LobbyIssue> {
    let mut out = Vec::new();
    let mode = setup.mode;
    match mode {
        GameMode::Splash | GameMode::Tutorial | GameMode::Trial => out.push(LobbyIssue::ModeNotInBuild),
        GameMode::PvpEscort if !setup.at_1_0 => out.push(LobbyIssue::ModeNotInBuild),
        _ => {}
    }
    let max = content::ruleset::max_players(mode, setup.at_1_0);
    if setup.players.len() > max as usize { out.push(LobbyIssue::TooManyPlayers { max }); }
    if is_pvp(mode) {
        if setup.ruleset.luau { out.push(LobbyIssue::LuauInPvp); }
        let bad_team = setup.players.iter().any(|p| p.team > 1);
        let a = setup.players.iter().filter(|p| p.team == 0).count();
        let b = setup.players.iter().filter(|p| p.team == 1).count();
        if bad_team || a != b { out.push(LobbyIssue::TeamsUneven); }
        else {
            let size = a.min(u8::MAX as usize) as u8;
            if !team_sizes(mode, setup.at_1_0).contains(&size) { out.push(LobbyIssue::TeamSizeNotOffered { size }); }
        }
    }
    let mut players: Vec<&LobbyPlayer> = setup.players.iter().collect();
    players.sort_by_key(|p| p.slot);
    for p in &players { if !p.ready { out.push(LobbyIssue::PlayerNotReady(p.slot)); } }
    out
}

/// Lobby-wide complexity: (sum, any non-finite). Non-finite values are over any cap.
pub(crate) fn lobby_complexity(players: &[LobbyPlayer]) -> (f64, bool) {
    players.iter().fold((0.0, false), |(s, bad), p| if p.complexity.is_finite() { (s + p.complexity.max(0.0), bad) } else { (s, true) })
}

/// Check a lobby against its mode and ruleset: player counts and team sizes, every blueprint validated under
/// the ruleset (stats recomputed), per-vessel and per-lobby complexity. Unit: `sys-modes`.
///
/// Campaign co-op hulls come from the save, so blueprints are not required there.
pub fn validate_lobby(setup: &LobbySetup, blueprints: &[Blueprint], catalogue: &Catalogue) -> Vec<LobbyIssue> {
    let mut out = shape_issues(setup);
    let mut ruleset = setup.ruleset.clone();
    if is_pvp(setup.mode) { ruleset.luau = false; }
    let need_blueprint = setup.mode != GameMode::Campaign;
    let hashed = hash_blueprints(blueprints, setup.players.iter().any(|p| p.blueprint.is_some()));
    let mut players: Vec<&LobbyPlayer> = setup.players.iter().collect();
    players.sort_by_key(|p| p.slot);
    for p in players {
        let bp = p.blueprint.and_then(|h| hashed.iter().find(|(x, _)| *x == h).map(|(_, b)| *b));
        let Some(bp) = bp else { if need_blueprint { out.push(LobbyIssue::NoBlueprint(p.slot)); } continue };
        let complexity = (p.complexity.is_finite() && p.complexity > 0.0).then_some(p.complexity);
        let ctx = ValidationContext { ruleset: &ruleset, available: None, enemy_class: false, complexity };
        let report = content::validate::validate_blueprint(bp, catalogue, &ctx);
        let mut has_complexity = false;
        for issue in report.issues {
            has_complexity |= matches!(issue, Issue::OverComplexity { .. });
            out.push(LobbyIssue::Blueprint { player: p.slot, issue });
        }
        let cap = ruleset.complexity_per_vessel;
        if !has_complexity && cap > 0.0 && (!p.complexity.is_finite() || p.complexity > cap) {
            out.push(LobbyIssue::Blueprint { player: p.slot, issue: Issue::OverComplexity { points: if p.complexity.is_finite() { p.complexity } else { f64::INFINITY }, cap } });
        }
    }
    let cap = ruleset.complexity_per_lobby;
    if cap > 0.0 {
        let (sum, bad) = lobby_complexity(&setup.players);
        if bad || sum > cap { out.push(LobbyIssue::LobbyOverComplexity { points: if bad { f64::INFINITY } else { sum }, cap }); }
    }
    out
}

fn hash_blueprints(blueprints: &[Blueprint], needed: bool) -> Vec<(ContentHash, &Blueprint)> {
    if !needed { return Vec::new(); }
    blueprints.iter().map(|b| (content::blueprint::io::compute_hash(b), b)).collect()
}

/// How many players a host can carry: min(floor(0.9 x upload / 256 kbps), mode maximum - 1) clients plus the
/// host (L1031). Real code: spec formula.
pub fn player_limit(mode: GameMode, at_1_0: bool, upload_kbps: u32) -> u8 {
    let max = content::ruleset::max_players(mode, at_1_0);
    let clients = ((0.9 * upload_kbps as f64) / 256.0).floor() as u32;
    (clients.min(max.saturating_sub(1) as u32) as u8) + 1
}

// ---------------------------------------------------------------------------------------------------------
// Shared builder pieces
// ---------------------------------------------------------------------------------------------------------

/// Uniform draw in [0, 1) for map and force generation, keyed (seed, purpose, index).
fn draw(seed: u64, purpose: u64, n: u32) -> f64 { sim_core::rng::uniform(Key::new(seed, 0, Stream::Generator, purpose, 0).draw(n)) }

/// Smallest act whose hull cap holds the ruleset's tonnage cap.
pub(crate) fn act_for_cap(cap_t: f64) -> Act {
    [Act::Act1, Act::Act2, Act::Act3, Act::Act4].into_iter().find(|a| cap_t <= a.hull_cap_t() + 1e-6).unwrap_or(Act::Act4)
}

/// Lobby weather to a full state: sea state clamped into the kind's range, sun elevation inside the chosen
/// time-of-day band (night -12, dusk 8, day 40 degrees). DECISION: wind speed per sea-state code follows the
/// Beaufort scale's mid values.
pub(crate) fn weather_state(kind: WeatherKind, sea: SeaState, tod: TimeOfDay, wind_dir_rad: f64) -> WeatherState {
    let (lo, hi) = kind.sea_state_range();
    let sea = SeaState(sea.0.clamp(lo, hi));
    const WIND: [f64; 9] = [0.5, 2.0, 4.5, 7.0, 9.5, 12.5, 16.0, 20.0, 26.0];
    let (cloud, rain) = match kind { WeatherKind::Clear => (0.2, 0.0), WeatherKind::Haze => (0.4, 0.0), WeatherKind::Overcast => (0.9, 0.0), WeatherKind::Rain => (1.0, 0.6), WeatherKind::Storm => (1.0, 1.0), WeatherKind::FogBank => (0.6, 0.0) };
    let (elev, azim) = match tod { TimeOfDay::Night => (-12.0, 0.0), TimeOfDay::Dusk => (8.0, 255.0), TimeOfDay::Day => (40.0, 180.0) };
    WeatherState { kind, sea_state: sea, wind_dir_rad: if wind_dir_rad.is_finite() { wind_dir_rad } else { 0.0 }, wind_speed_mps: WIND[sea.0.min(8) as usize], cloud_cover: cloud, visibility_m: kind.visibility_m(), rain, sun_elevation_deg: elev, sun_azimuth_deg: azim }
}

fn settings(setup: &LobbySetup, difficulty: content::campaign::DifficultySliders) -> SimSettings {
    let mut ruleset = setup.ruleset.clone();
    let pvp = is_pvp(setup.mode);
    if pvp { ruleset.luau = false; }
    SimSettings { difficulty: sane_difficulty(difficulty), ironman: false, guest_scripts: !pvp && ruleset.luau, ruleset, sim_threads: 3 }
}

fn sane_difficulty(d: content::campaign::DifficultySliders) -> content::campaign::DifficultySliders {
    let n = content::campaign::DifficultySliders::NORMAL;
    let f = |x: f64, y: f64| if x.is_finite() { x } else { y };
    content::campaign::DifficultySliders { ai_accuracy: f(d.ai_accuracy, n.ai_accuracy), ai_reaction_s: f(d.ai_reaction_s, n.ai_reaction_s), adaptation_cadence: f(d.adaptation_cadence, n.adaptation_cadence), economy: f(d.economy, n.economy), ..d }.clamped()
}

/// Compass heading (degrees) to unit (x, z): 0 = north = -Z, clockwise.
fn forward(heading_deg: f64) -> DVec2 { let h = heading_deg.to_radians(); DVec2::new(libm::sin(h), -libm::cos(h)) }

/// Line-abreast positions of `n` hulls centred on a marker, facing its heading.
pub(crate) fn line_abreast(centre: DVec2, heading_deg: f64, n: usize) -> Vec<DVec2> {
    let f = forward(heading_deg);
    let right = DVec2::new(-f.y, f.x);
    (0..n).map(|i| centre + right * ((i as f64 - (n as f64 - 1.0) * 0.5) * LINE_SPACING_M)).collect()
}

fn base_config(setup: &LobbySetup, rules: ModeRules, map: MapDef, difficulty: content::campaign::DifficultySliders) -> SortieConfig {
    let wind = draw(setup.seed, 0x5749_4E44, 0) * core::f64::consts::TAU;
    SortieConfig {
        protocol: crate::protocol::PROTOCOL_VERSION, seed: setup.seed, mode: setup.mode, rules, settings: settings(setup, difficulty), act: act_for_cap(setup.ruleset.tonnage_cap_t),
        weather: weather_state(setup.weather, setup.sea_state, setup.time_of_day, wind), weather_changes: Vec::new(), time_of_day: setup.time_of_day, map, layer_depth_m: 50.0,
        sides: Vec::new(), players: Vec::new(), blueprints: Vec::new(), vessels: Vec::new(), bodies: Vec::new(), groups: Vec::new(), objective: Objective::DestroyAll,
        ghosts: Vec::new(), tags: Vec::new(), actions: Vec::new(), intel: SortieIntel::default(), cards: Vec::new(), operation: None, region: None, theatre_hour: 0,
        splash: None, tutorial: None, time_limit_s: None,
    }
}

fn team_side(id: u8, name: &str, foe: Option<SideId>) -> SideDef {
    let me = SideId(id);
    SideDef { id: me, faction: Faction::Players, name: TextKey::new(name), hostile_to: foe.map_or(SideMask::NONE, SideMask::only), allied: SideMask::only(me), pvp_team: foe.map(|_| id) }
}

fn player_vessel(hash: ContentHash, side: SideId, pos: DVec2, heading_deg: f64) -> VesselInit {
    VesselInit {
        blueprint: BlueprintRef::Inline(hash), side, class: None, group: None, pos: DVec3::new(pos.x, 0.0, pos.y), heading_rad: heading_deg.to_radians(), speed_kn: 0.0, arrives_s: 0,
        damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: String::new(), declared: None, emcon: None, passengers: 0, role: VesselRole::PlayerHull,
    }
}

/// Sanitise a lobby-supplied map; `None` (or an unusable map) gives the mode's default map.
fn lobby_map(setup: &LobbySetup) -> MapDef {
    let players = setup.players.len();
    let default = default_map(setup.mode, setup.seed, players);
    let Some(m) = setup.map.as_ref() else { return default };
    let size_ok = m.size_m.iter().all(|s| s.is_finite() && (2_000.0..=MAX_MAP_M).contains(s));
    if !size_ok { return default; }
    let mut out = MapDef {
        size_m: m.size_m, depth_m: if m.depth_m.is_finite() && m.depth_m > 0.0 { m.depth_m } else { default.depth_m },
        islands: m.islands.iter().filter(|i| i.centre_m.iter().all(|c| c.is_finite()) && i.radius_m.is_finite() && i.radius_m > 0.0 && i.height_m.is_finite()).copied().collect(),
        markers: m.markers.iter().filter(|k| k.pos_m.iter().all(|c| c.is_finite()) && k.heading_deg.is_finite()).cloned().collect(), seed: m.seed,
    };
    // Spawns the builders need, taken from the default layout scaled into this map when missing.
    for d in default.markers.iter().filter(|d| matches!(d.kind, MarkerKind::PlayerSpawn | MarkerKind::EnemySpawn | MarkerKind::ConvoyEntry | MarkerKind::ConvoyExit)) {
        if !out.markers.iter().any(|k| k.name == d.name) {
            let sx = out.size_m[0] / default.size_m[0].max(1.0);
            let sz = out.size_m[1] / default.size_m[1].max(1.0);
            out.markers.push(Marker { pos_m: [d.pos_m[0] * sx, d.pos_m[1] * sz], ..d.clone() });
        }
    }
    out
}

fn spawn_marker<'a>(map: &'a MapDef, name: &str, kind: MarkerKind, nth: usize) -> Option<&'a Marker> {
    map.markers.iter().find(|m| m.name == name).or_else(|| map.markers.iter().filter(|m| m.kind == kind).nth(nth))
}

/// Seat lobby players on team sides with hulls on their spawn lines. `team_of` picks the side index per
/// player; returns nothing, filling `config` in place.
fn seat_players(config: &mut SortieConfig, setup: &LobbySetup, blueprints: &[Blueprint], team_of: &dyn Fn(&LobbyPlayer) -> u8, spawns: &[(DVec2, f64)]) {
    let hashed = hash_blueprints(blueprints, setup.players.iter().any(|p| p.blueprint.is_some()));
    let mut players: Vec<&LobbyPlayer> = setup.players.iter().collect();
    players.sort_by_key(|p| p.slot);
    players.dedup_by_key(|p| p.slot);
    for team in 0..spawns.len().max(1) as u8 {
        let members: Vec<&LobbyPlayer> = players.iter().copied().filter(|p| team_of(p) == team).collect();
        let fielded: Vec<(&LobbyPlayer, ContentHash, &Blueprint)> = members.iter().filter_map(|p| { let h = p.blueprint?; hashed.iter().find(|(x, _)| *x == h).map(|(_, b)| (*p, h, *b)) }).collect();
        let (centre, heading) = spawns.get(team as usize).copied().unwrap_or((DVec2::ZERO, 0.0));
        let line = line_abreast(centre, heading, fielded.len());
        let side = SideId(team);
        for p in &members {
            let seat = fielded.iter().position(|(f, _, _)| f.slot == p.slot);
            let vessel = seat.map(|i| {
                let (_, hash, bp) = fielded[i];
                if !config.blueprints.iter().any(|b| b == bp) { config.blueprints.push(bp.clone()); }
                config.vessels.push(player_vessel(hash, side, line[i], heading));
                (config.vessels.len() - 1) as u16
            });
            config.players.push(PlayerInit { slot: p.slot, client_id: p.client_id, name: p.name.clone(), side, vessel, human: true });
        }
    }
    config.players.sort_by_key(|p| p.slot);
}

// ---------------------------------------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------------------------------------

/// PvP elimination: two sides hostile to each other, spawn markers on opposite edges, last team afloat.
/// Unit: `sys-modes`.
pub fn elimination_config(setup: &LobbySetup, blueprints: &[Blueprint], campaign: &CampaignData) -> SortieConfig {
    let _ = campaign;
    let map = lobby_map(setup);
    let a = spawn_marker(&map, "spawn_a", MarkerKind::PlayerSpawn, 0).map_or((DVec2::new(-6_000.0, 0.0), 90.0), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let b = spawn_marker(&map, "spawn_b", MarkerKind::PlayerSpawn, 1).map_or((DVec2::new(6_000.0, 0.0), 270.0), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let mut c = base_config(setup, ModeRules::Elimination, map, Default::default());
    c.sides = vec![team_side(0, "objective.side.team_a", Some(SideId(1))), team_side(1, "objective.side.team_b", Some(SideId(0)))];
    seat_players(&mut c, setup, blueprints, &|p| p.team.min(1), &[a, b]);
    c.time_limit_s = Some(ELIMINATION_LIMIT_S);
    c
}

/// PvP escort (Phase::R100): one side's AI convoy crosses a 20 to 40 km map. Unit: `sys-modes`.
pub fn escort_config(setup: &LobbySetup, blueprints: &[Blueprint], campaign: &CampaignData) -> SortieConfig {
    let map = lobby_map(setup);
    let entry = spawn_marker(&map, "convoy_entry", MarkerKind::ConvoyEntry, 0).cloned();
    let exit = spawn_marker(&map, "convoy_exit", MarkerKind::ConvoyExit, 0).cloned();
    let a = spawn_marker(&map, "spawn_a", MarkerKind::PlayerSpawn, 0).map_or((DVec2::ZERO, 90.0), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let b = spawn_marker(&map, "spawn_b", MarkerKind::PlayerSpawn, 1).map_or((DVec2::ZERO, 180.0), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let entry_name = entry.as_ref().map_or_else(|| "convoy_entry".to_string(), |m| m.name.clone());
    let exit_name = exit.as_ref().map_or_else(|| "convoy_exit".to_string(), |m| m.name.clone());
    let rules = ModeRules::Escort { convoy_side: SideId(0), entry: entry_name.clone(), exit: exit_name.clone(), required_fraction: ESCORT_REQUIRED_FRACTION };
    let mut c = base_config(setup, rules, map, Default::default());
    c.sides = vec![team_side(0, "objective.side.team_a", Some(SideId(1))), team_side(1, "objective.side.team_b", Some(SideId(0)))];
    seat_players(&mut c, setup, blueprints, &|p| p.team.min(1), &[a, b]);
    // The convoy: freighters and tankers of the cap's act or earlier, drawn by seed.
    let act = c.act;
    let pool: Vec<(&ClassId, &content::campaign::EnemyClassDef)> = campaign.classes.iter().filter(|(_, d)| matches!(d.role, ClassRole::Freighter | ClassRole::Tanker) && d.act <= act && !d.submersible).collect();
    let team_size = setup.players.iter().filter(|p| p.team == 0).count().max(1);
    let count = if pool.is_empty() { 0 } else { 2 + team_size };
    let (start, heading) = entry.as_ref().map_or((a.0, a.1), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let goal = exit.as_ref().map_or(start + forward(heading) * 20_000.0, |m| DVec2::from(m.pos_m));
    let dir = (goal - start).try_normalize().unwrap_or(forward(heading));
    let heading_rad = libm::atan2(dir.x, -dir.y);
    for i in 0..count {
        let pick = ((draw(setup.seed, 0x434F_4E56, i as u32) * pool.len() as f64).floor() as usize).min(pool.len() - 1);
        let (id, def) = pool[pick];
        let pos = start - dir * (i as f64 * 400.0);
        c.vessels.push(VesselInit {
            blueprint: BlueprintRef::Stock(def.blueprint.clone()), side: SideId(0), class: Some(id.clone()), group: Some(GroupId(0)), pos: DVec3::new(pos.x, 0.0, pos.y), heading_rad, speed_kn: ESCORT_CONVOY_KN,
            arrives_s: 0, damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: "convoy".into(), declared: None, emcon: None, passengers: 0, role: VesselRole::Convoy,
        });
    }
    if count > 0 {
        c.groups.push(GroupInit { id: GroupId(0), side: SideId(0), kind: FleetKind::Convoy, formation: Formation::Column, orders: GroupOrders::Transit { from: entry_name, to: exit_name }, ace: None, theatre_group: None, tag: "convoy".into() });
    }
    // Time for the convoy to make its run with half again in hand.
    let run = start.distance(goal) + count as f64 * 400.0;
    c.time_limit_s = Some(((run / (ESCORT_CONVOY_KN * sim_core::time::KN)) * 1.5 + 300.0).min(7_200.0) as u32);
    c
}

/// Skirmish: one operation built from a campaign template with the chosen cap and enemy size, through
/// `theatre::opgen::generate_standalone`. Unit: `sys-modes`.
pub fn skirmish_config(setup: &LobbySetup, blueprints: &[Blueprint], campaign: &CampaignData, catalogue: &Catalogue) -> SortieConfig {
    let opts = setup.skirmish.clone().unwrap_or(SkirmishOptions { template: MissionType::ConvoyRaid, act: act_for_cap(setup.ruleset.tonnage_cap_t), enemy_per_player: 1, difficulty: Default::default() });
    let difficulty = sane_difficulty(opts.difficulty);
    let hashed = hash_blueprints(blueprints, setup.players.iter().any(|p| p.blueprint.is_some()));
    let mut lobby: Vec<&LobbyPlayer> = setup.players.iter().collect();
    lobby.sort_by_key(|p| p.slot);
    lobby.dedup_by_key(|p| p.slot);
    let fielded: Vec<(&LobbyPlayer, ContentHash, &Blueprint)> = lobby.iter().filter_map(|p| { let h = p.blueprint?; hashed.iter().find(|(x, _)| *x == h).map(|(_, b)| (*p, h, *b)) }).collect();
    let n = fielded.len().clamp(1, 4) as u8;
    let data = TheatreData { campaign: Arc::new(campaign.clone()), catalogue: Arc::new(catalogue.clone()) };
    let per = opts.enemy_per_player.max(1);
    let templates = core::iter::once(opts.template).chain(MissionType::ALL.into_iter().filter(|t| *t != opts.template));
    let generated = templates.into_iter().find_map(|t| crate::theatre::opgen::generate_standalone(t, opts.act, n, setup.seed, &difficulty, Some(per), &data));
    let mut c = match generated { Some(g) => g.config, None => fallback_skirmish(setup, campaign, opts.act, per as usize * n as usize, difficulty) };
    c.mode = GameMode::Skirmish;
    c.seed = setup.seed;
    c.rules = ModeRules::Objective;
    c.settings = settings(setup, difficulty);
    c.time_of_day = setup.time_of_day;
    let wind = c.weather.wind_dir_rad;
    c.weather = weather_state(setup.weather, setup.sea_state, setup.time_of_day, wind);
    c.weather_changes.clear();
    seat_skirmish_players(&mut c, setup, &lobby, &fielded);
    c
}

/// Put lobby players into a generated skirmish: reuse its player hulls in order, add more on the same line,
/// drop unused ones (remapping vessel indices), everyone else spectates on the player side.
fn seat_skirmish_players(c: &mut SortieConfig, setup: &LobbySetup, lobby: &[&LobbyPlayer], fielded: &[(&LobbyPlayer, ContentHash, &Blueprint)]) {
    let player_side = c.vessels.iter().find(|v| v.role == VesselRole::PlayerHull).map(|v| v.side)
        .or_else(|| c.sides.iter().find(|s| s.faction == Faction::Players).map(|s| s.id)).unwrap_or(SideId(0));
    if !c.sides.iter().any(|s| s.id == player_side) {
        let foes = c.sides.iter().fold(SideMask::NONE, |m, s| m.with(s.id));
        c.sides.push(SideDef { id: player_side, faction: Faction::Players, name: TextKey::new("objective.side.players"), hostile_to: foes, allied: SideMask::only(player_side), pvp_team: None });
    }
    let mut hulls: Vec<usize> = c.vessels.iter().enumerate().filter(|(_, v)| v.role == VesselRole::PlayerHull).map(|(i, _)| i).collect();
    // Remove surplus player hulls from the back, keeping every stored index valid.
    while hulls.len() > fielded.len() {
        let Some(gone) = hulls.pop() else { break };
        c.vessels.remove(gone);
        let g = gone as u16;
        c.intel.named.retain(|i| *i != g);
        for i in c.intel.named.iter_mut() { if *i > g { *i -= 1; } }
        for h in hulls.iter_mut() { if *h > gone { *h -= 1; } }
    }
    let spawn = spawn_marker(&c.map, "spawn_a", MarkerKind::PlayerSpawn, 0).map(|m| (DVec2::from(m.pos_m), m.heading_deg));
    c.players.clear();
    for (i, (p, hash, bp)) in fielded.iter().enumerate() {
        if !c.blueprints.iter().any(|b| b == *bp) { c.blueprints.push((*bp).clone()); }
        let idx = if let Some(&v) = hulls.get(i) {
            c.vessels[v].blueprint = BlueprintRef::Inline(*hash);
            c.vessels[v].side = player_side;
            v
        } else {
            // Extend the line from the last player hull, or start one at the spawn marker.
            let (pos, heading) = match hulls.last().map(|&v| &c.vessels[v]) {
                Some(last) => (DVec2::new(last.pos.x, last.pos.z) + DVec2::new(libm::cos(last.heading_rad), libm::sin(last.heading_rad)) * LINE_SPACING_M, last.heading_rad.to_degrees()),
                None => spawn.map_or((DVec2::ZERO, 90.0), |(p, h)| (p + DVec2::new(libm::cos(h.to_radians()), libm::sin(h.to_radians())) * (i as f64 * LINE_SPACING_M), h)),
            };
            c.vessels.push(player_vessel(*hash, player_side, pos, heading));
            let v = c.vessels.len() - 1;
            hulls.push(v);
            v
        };
        c.players.push(PlayerInit { slot: p.slot, client_id: p.client_id, name: p.name.clone(), side: player_side, vessel: Some(idx as u16), human: true });
    }
    for p in lobby.iter().filter(|p| !fielded.iter().any(|(f, _, _)| f.slot == p.slot)) {
        c.players.push(PlayerInit { slot: p.slot, client_id: p.client_id, name: p.name.clone(), side: player_side, vessel: None, human: true });
    }
    c.players.sort_by_key(|p| p.slot);
    let _ = setup;
}

/// A plain fight when no template can be generated: hulls of the act's combat classes at the enemy spawn,
/// hunting; sink them all.
fn fallback_skirmish(setup: &LobbySetup, campaign: &CampaignData, act: Act, enemies: usize, difficulty: content::campaign::DifficultySliders) -> SortieConfig {
    let map = default_map(GameMode::Skirmish, setup.seed, setup.players.len());
    let enemy = spawn_marker(&map, "enemy", MarkerKind::EnemySpawn, 0).map_or((DVec2::new(9_000.0, 0.0), 270.0), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let mut c = base_config(setup, ModeRules::Objective, map, difficulty);
    c.act = act;
    c.sides = vec![
        SideDef { id: SideId(0), faction: Faction::Players, name: TextKey::new("objective.side.players"), hostile_to: SideMask::only(SideId(1)), allied: SideMask::only(SideId(0)), pvp_team: None },
        SideDef { id: SideId(1), faction: Faction::Combine, name: TextKey::new("objective.side.combine"), hostile_to: SideMask::only(SideId(0)), allied: SideMask::only(SideId(1)), pvp_team: None },
    ];
    let pool: Vec<(&ClassId, &content::campaign::EnemyClassDef)> = campaign.classes.iter()
        .filter(|(_, d)| d.act <= act && !d.capital && !d.submersible && matches!(d.role, ClassRole::PatrolBoat | ClassRole::FastAttack | ClassRole::Corvette | ClassRole::Frigate | ClassRole::Destroyer | ClassRole::Picket)).collect();
    let line = line_abreast(enemy.0, enemy.1, if pool.is_empty() { 0 } else { enemies.max(1) });
    for (i, pos) in line.iter().enumerate() {
        let pick = ((draw(setup.seed, 0x534B_524D, i as u32) * pool.len() as f64).floor() as usize).min(pool.len() - 1);
        let (id, def) = pool[pick];
        c.vessels.push(VesselInit {
            blueprint: BlueprintRef::Stock(def.blueprint.clone()), side: SideId(1), class: Some(id.clone()), group: Some(GroupId(0)), pos: DVec3::new(pos.x, 0.0, pos.y), heading_rad: enemy.1.to_radians(), speed_kn: 10.0,
            arrives_s: 0, damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: "task_group".into(), declared: Some(Stance::Hostile), emcon: None, passengers: 0, role: VesselRole::Combatant,
        });
        if !c.intel.order_of_battle.contains(id) { c.intel.order_of_battle.push(id.clone()); }
    }
    if !line.is_empty() { c.groups.push(GroupInit { id: GroupId(0), side: SideId(1), kind: FleetKind::TaskGroup, formation: Formation::LineAbreast, orders: GroupOrders::Hunt, ace: None, theatre_group: None, tag: "task_group".into() }); }
    // With no enemy classes in the data the sortie is a timed patrol.
    if line.is_empty() { c.time_limit_s = Some(ELIMINATION_LIMIT_S); }
    // A player hull placeholder is added by `seat_skirmish_players` at the spawn marker.
    c
}

/// Sandbox: free play on an open map, no end condition, players join any time. Unit: `sys-modes`.
pub fn sandbox_config(setup: &LobbySetup, blueprints: &[Blueprint], campaign: &CampaignData) -> SortieConfig {
    let _ = campaign;
    let map = lobby_map(setup);
    let a = spawn_marker(&map, "spawn_a", MarkerKind::PlayerSpawn, 0).map_or((DVec2::new(-6_000.0, 0.0), 90.0), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let b = spawn_marker(&map, "spawn_b", MarkerKind::PlayerSpawn, 1).map_or((DVec2::new(6_000.0, 0.0), 270.0), |m| (DVec2::from(m.pos_m), m.heading_deg));
    let mut c = base_config(setup, ModeRules::Sandbox, map, Default::default());
    let two_teams = setup.players.iter().any(|p| p.team >= 1);
    if two_teams {
        c.sides = vec![team_side(0, "objective.side.team_a", Some(SideId(1))), team_side(1, "objective.side.team_b", Some(SideId(0)))];
        seat_players(&mut c, setup, blueprints, &|p| p.team.min(1), &[a, b]);
    } else {
        c.sides = vec![team_side(0, "objective.side.players", None)];
        seat_players(&mut c, setup, blueprints, &|_| 0, &[a]);
    }
    c.actions = vec![StationAction::BoardAndCapture, StationAction::InsertOrExtract, StationAction::RecoverSurvivors, StationAction::LayMines, StationAction::Photograph];
    c
}

// ---------------------------------------------------------------------------------------------------------
// Default maps
// ---------------------------------------------------------------------------------------------------------

/// A seeded default map for a mode: open water with a few islands sized to the mode (escort 20 to 40 km).
/// Unit: `sys-modes`.
pub fn default_map(mode: GameMode, seed: u64, players: usize) -> MapDef { layout(mode, seed, players, &|purpose, n| draw(seed, purpose, n)) }

fn marker(kind: MarkerKind, name: &str, x: f64, z: f64, heading_deg: f64) -> Marker { Marker { kind, name: name.into(), pos_m: [x, z], heading_deg } }

/// The map layout with an injected uniform draw `(purpose, n) -> [0, 1)`. Pure.
pub(crate) fn layout(mode: GameMode, seed: u64, players: usize, u: &dyn Fn(u64, u32) -> f64) -> MapDef {
    let players = players.clamp(1, 8);
    let per_team = players.div_ceil(2).max(1);
    let mut markers = Vec::new();
    let (size, symmetric, islands) = match mode {
        GameMode::PvpElimination => {
            let side = 16_000.0 + 4_000.0 * (per_team as f64 - 1.0);
            let d = side * 0.5 - 2_000.0;
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_a", -d, 0.0, 90.0));
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_b", d, 0.0, 270.0));
            ([side, side], true, 1 + per_team)
        }
        GameMode::PvpEscort => {
            let len = (20_000.0 + (u(0x4C45_4E, 0) * 21.0).floor() * 1_000.0).clamp(20_000.0, 40_000.0);
            let width = 20_000.0;
            let x = len * 0.5 - 2_000.0;
            markers.push(marker(MarkerKind::ConvoyEntry, "convoy_entry", -x, 0.0, 90.0));
            markers.push(marker(MarkerKind::ConvoyExit, "convoy_exit", x, 0.0, 90.0));
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_a", -x + 1_500.0, 1_500.0, 90.0));
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_b", 0.0, -width * 0.5 + 2_500.0, 180.0));
            ([len, width], false, 5)
        }
        GameMode::Sandbox => {
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_a", -6_000.0, 0.0, 90.0));
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_b", 6_000.0, 0.0, 270.0));
            markers.push(marker(MarkerKind::Survivors, "survivors", 0.0, 4_000.0, 0.0));
            markers.push(marker(MarkerKind::Exit, "exit", 0.0, -14_000.0, 0.0));
            ([30_000.0, 30_000.0], false, 6)
        }
        GameMode::Skirmish => {
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_a", -9_000.0, 0.0, 90.0));
            markers.push(marker(MarkerKind::EnemySpawn, "enemy", 9_000.0, 0.0, 270.0));
            markers.push(marker(MarkerKind::Exit, "exit", -11_000.0, 0.0, 270.0));
            ([24_000.0, 24_000.0], false, 4)
        }
        GameMode::Campaign | GameMode::Splash | GameMode::Tutorial | GameMode::Trial => {
            markers.push(marker(MarkerKind::PlayerSpawn, "spawn_a", 0.0, 0.0, 0.0));
            ([8_000.0, 8_000.0], false, 0)
        }
    };
    let keep_clear: Vec<DVec2> = markers.iter().map(|m| DVec2::from(m.pos_m)).collect();
    let mut placed: Vec<Island> = Vec::new();
    let ok = |c: DVec2, r: f64, placed: &[Island]| {
        let half = DVec2::new(size[0], size[1]) * 0.5;
        c.x.abs() + r < half.x - 1_000.0 && c.y.abs() + r < half.y - 1_000.0
            && keep_clear.iter().all(|k| k.distance(c) >= r + 3_000.0)
            && placed.iter().all(|i| DVec2::from(i.centre_m).distance(c) >= r + i.radius_m + 800.0)
            && (mode != GameMode::PvpEscort || c.y.abs() >= r + 2_500.0)
    };
    let mut n: u32 = 0;
    for k in 0..islands as u32 {
        for _attempt in 0..8 {
            let r = 250.0 + 650.0 * u(0x4953_4C, n);
            let h = 15.0 + 55.0 * u(0x4953_4C, n + 1);
            let (sx, sz) = (u(0x4953_4C, n + 2), u(0x4953_4C, n + 3));
            n += 4;
            let half = DVec2::new(size[0], size[1]) * 0.5;
            let c = if symmetric { DVec2::new(-half.x * 0.7 * sx, (sz * 2.0 - 1.0) * half.y * 0.8) } else { DVec2::new((sx * 2.0 - 1.0) * half.x * 0.8, (sz * 2.0 - 1.0) * half.y * 0.8) };
            let mirror = -c;
            if !ok(c, r, &placed) || (symmetric && (!ok(mirror, r, &placed) || c.distance(mirror) < 2.0 * r + 800.0)) { continue; }
            let isle_seed = (seed as u32).wrapping_mul(31).wrapping_add(k);
            placed.push(Island { centre_m: c.into(), radius_m: r, height_m: h, seed: isle_seed });
            if symmetric { placed.push(Island { centre_m: mirror.into(), radius_m: r, height_m: h, seed: isle_seed }); }
            break;
        }
    }
    // Sandbox: a landing on the first island to practise inserts.
    if mode == GameMode::Sandbox {
        if let Some(i) = placed.first() {
            let c = DVec2::from(i.centre_m);
            let out = if c.length() > 1.0 { -c / c.length() } else { DVec2::X };
            let p = c + out * (i.radius_m + 60.0);
            markers.push(marker(MarkerKind::ShoreLanding, "landing", p.x, p.y, 0.0));
        }
    }
    MapDef { size_m: size, depth_m: 200.0, islands: placed, markers, seed: (seed ^ (seed >> 32)) as u32 }
}

#[cfg(test)]
mod tests;
