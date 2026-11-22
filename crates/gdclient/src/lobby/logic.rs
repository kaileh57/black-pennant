//! Part of unit `gd-lobby`: the lobby's rules and wording with no Godot type, so they are tested headless.
//! DESIGN.md L957-976 (modes, rulesets, caps), L994-1003 (manual steps, VPN), L1031 (client limit),
//! L1039 (connection log).
use crate::enums::{PortMapState, SessionKind};
use crate::link::net::HostingInfo;
use content::campaign::{DifficultySliders, MissionType};
use content::enums::{Act, GameMode, TimeOfDay};
use content::ids::PlayerSlot;
use content::ruleset::{Ruleset, RulesetKind};
use content::validate::Issue;
use content::weather::{SeaState, WeatherKind};
use sim_systems::modes::{LobbyIssue, LobbySetup, SkirmishOptions};
use sim_systems::protocol::{Msg, MsgArg, Unit};
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// The PvP ruleset a lobby starts with (L965).
pub(crate) const DEFAULT_PVP_RULESET: &str = "standard";
/// Most enemy hulls per player a hosted skirmish takes (the offline setup's limit).
pub(crate) const MAX_ENEMY_PER_PLAYER: u8 = 8;
/// Lines of the connection log kept in memory for `connection_log_text`.
pub(crate) const LOG_LINES_KEPT: usize = 1_024;
/// The upload figure's range on the host panel (the `net.upload_kbps` setting's range).
pub(crate) const UPLOAD_MIN_KBPS: u32 = 256;
pub(crate) const UPLOAD_MAX_KBPS: u32 = 100_000;

/// What a host may choose the lobby to be.
pub(crate) fn hostable(mode: GameMode) -> bool {
    matches!(mode, GameMode::Campaign | GameMode::Skirmish | GameMode::Sandbox | GameMode::PvpElimination | GameMode::PvpEscort)
}
pub(crate) fn is_pvp(mode: GameMode) -> bool { matches!(mode, GameMode::PvpElimination | GameMode::PvpEscort) }

/// The session kind a lobby of this mode runs.
pub(crate) fn kind_for(mode: GameMode) -> SessionKind {
    match mode {
        GameMode::Campaign => SessionKind::Campaign,
        GameMode::Skirmish => SessionKind::Skirmish,
        GameMode::Sandbox => SessionKind::Sandbox,
        GameMode::PvpElimination | GameMode::PvpEscort => SessionKind::Pvp,
        GameMode::Splash => SessionKind::Splash,
        GameMode::Tutorial => SessionKind::Tutorial,
        GameMode::Trial => SessionKind::Scripted,
    }
}

/// The PvP ruleset presets of `content/data/rulesets.ron`, `standard` first.
pub(crate) fn presets() -> &'static [(String, Ruleset)] {
    static P: OnceLock<Vec<(String, Ruleset)>> = OnceLock::new();
    P.get_or_init(|| {
        let mut list = content::data::parse_ron::<Vec<(String, Ruleset)>>(content::data::RULESETS, "crates/content/data/rulesets.ron").unwrap_or_default();
        let mut seen = BTreeSet::new();
        list.retain(|(id, _)| seen.insert(id.clone()));
        list.sort_by_key(|(id, _)| id != DEFAULT_PVP_RULESET);
        if list.is_empty() { list.push((DEFAULT_PVP_RULESET.to_string(), Ruleset::default())); }
        list
    })
}

/// The id a ruleset is shown under: the preset of its kind for PvP rulesets, else what it is for.
pub(crate) fn ruleset_id(mode: GameMode, ruleset: &Ruleset) -> String {
    if is_pvp(mode) {
        return presets().iter().find(|(_, r)| r.kind == ruleset.kind).map_or_else(|| DEFAULT_PVP_RULESET.to_string(), |(id, _)| id.clone());
    }
    match mode {
        GameMode::Skirmish => "skirmish".into(),
        GameMode::Sandbox => "sandbox".into(),
        GameMode::Campaign => "campaign".into(),
        _ => match ruleset.kind { RulesetKind::Standard => "standard".into(), RulesetKind::Open => "open".into() },
    }
}

/// A hosted sandbox fields what an offline one does: the open ruleset at the largest cap, no complexity cap.
pub(crate) fn sandbox_ruleset() -> Ruleset {
    Ruleset { kind: RulesetKind::Open, tonnage_cap_t: Act::Act4.hull_cap_t(), complexity_per_vessel: 0.0, complexity_per_lobby: 0.0, part_limits: Default::default(), max_length_m: 90.0, luau: true }
}

/// The per-hull caps a PvP host may pick: the four act caps (L971).
pub(crate) fn pvp_cap_ok(cap_t: f64) -> bool { Act::ALL.iter().any(|a| (a.hull_cap_t() - cap_t).abs() < 1e-6) }

/// The settable part of a lobby setup, as `Lobby.host` and `Lobby.set_setup` read it. `None` keeps the
/// current value.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SetupPatch {
    pub mode: Option<GameMode>,
    pub ruleset: Option<String>,
    pub tonnage_cap_t: Option<f64>,
    /// 0 asks for a new random seed.
    pub seed: Option<u64>,
    pub weather: Option<WeatherKind>,
    pub sea_state: Option<SeaState>,
    pub time_of_day: Option<TimeOfDay>,
    pub skirmish: Option<SkirmishOptions>,
}

/// A fresh lobby of `mode` before any choice: random seed, clear weather, sea state 3 at dusk (the offline
/// setups' defaults), no players (the session admits the host's own client into slot 0).
pub(crate) fn initial_setup(mode: GameMode, seed: u64) -> LobbySetup {
    LobbySetup {
        mode,
        ruleset: Ruleset::default(),
        seed,
        players: Vec::new(),
        skirmish: None,
        weather: WeatherKind::Clear,
        sea_state: SeaState(3),
        time_of_day: TimeOfDay::Dusk,
        map: None,
        at_1_0: false,
    }
}

pub(crate) fn default_skirmish() -> SkirmishOptions {
    SkirmishOptions { template: MissionType::ConvoyRaid, act: Act::Act1, enemy_per_player: 1, difficulty: DifficultySliders::NORMAL }
}

/// The setup after a change, with the ruleset each mode fields: a PvP preset with the chosen act cap (400 t
/// by default), the skirmish act's campaign ruleset, the sandbox ruleset. A campaign lobby's mode never
/// changes. `random_seed` stands in for a seed of 0.
pub(crate) fn apply_patch(base: &LobbySetup, patch: &SetupPatch, random_seed: u64) -> Result<LobbySetup, Msg> {
    let mut s = base.clone();
    let mode = patch.mode.unwrap_or(base.mode);
    if !hostable(mode) || ((base.mode == GameMode::Campaign) != (mode == GameMode::Campaign)) { return Err(Msg::new("net.host.bad_mode")); }
    s.mode = mode;
    if let Some(seed) = patch.seed { s.seed = if seed == 0 { random_seed.max(1) } else { seed }; }
    if let Some(w) = patch.weather { s.weather = w; }
    if let Some(sea) = patch.sea_state {
        if sea.0 > 8 { return Err(Msg::new("session.problem.sea_state")); }
        s.sea_state = sea;
    }
    if let Some(t) = patch.time_of_day { s.time_of_day = t; }
    match mode {
        GameMode::PvpElimination | GameMode::PvpEscort => {
            let id = patch.ruleset.as_deref().map(str::trim).filter(|id| !id.is_empty()).map(str::to_string)
                .unwrap_or_else(|| if is_pvp(base.mode) { ruleset_id(base.mode, &base.ruleset) } else { DEFAULT_PVP_RULESET.to_string() });
            let Some((_, preset)) = presets().iter().find(|(k, _)| *k == id) else { return Err(Msg::new("net.host.bad_ruleset")) };
            let cap = patch.tonnage_cap_t.unwrap_or(if is_pvp(base.mode) { base.ruleset.tonnage_cap_t } else { preset.tonnage_cap_t });
            if !cap.is_finite() || !pvp_cap_ok(cap) { return Err(Msg::new("net.host.bad_ruleset")); }
            let mut r = preset.clone();
            r.tonnage_cap_t = cap;
            // Luau never runs in a PvP lobby (L969).
            r.luau = false;
            s.ruleset = r;
            s.skirmish = None;
        }
        GameMode::Skirmish => {
            let mut sk = patch.skirmish.clone().or_else(|| base.skirmish.clone()).unwrap_or_else(default_skirmish);
            sk.enemy_per_player = sk.enemy_per_player.clamp(1, MAX_ENEMY_PER_PLAYER);
            s.ruleset = Ruleset::campaign(sk.act);
            s.skirmish = Some(sk);
        }
        GameMode::Sandbox => {
            s.ruleset = sandbox_ruleset();
            s.skirmish = None;
        }
        GameMode::Campaign => {
            s.ruleset = Ruleset::campaign(Act::Act1);
            s.skirmish = None;
        }
        _ => return Err(Msg::new("net.host.bad_mode")),
    }
    Ok(s)
}

/// `LobbyIssue`s as the sentences the lobby lists. The local player's own design problems are spelled out
/// with the dry dock's wording (`own_issue`) so they can be fixed; another player's are one line naming
/// them, since the details name part slots of a design that is not ours. Duplicates are dropped.
pub(crate) fn issue_msgs(issues: &[LobbyIssue], own: Option<PlayerSlot>, name_of: &dyn Fn(PlayerSlot) -> String, own_issue: &dyn Fn(&Issue) -> Msg) -> Vec<Msg> {
    let name = |p: PlayerSlot| MsgArg::Name(name_of(p));
    let mut out: Vec<Msg> = Vec::new();
    for issue in issues {
        let msg = match issue {
            LobbyIssue::TooManyPlayers { max } => Msg::new("net.lobby.issue.too_many_players").arg(MsgArg::Int(*max as i64)),
            LobbyIssue::TeamsUneven => Msg::new("net.lobby.issue.teams_uneven"),
            LobbyIssue::TeamSizeNotOffered { size } => Msg::new("net.lobby.issue.team_size_not_offered").arg(MsgArg::Int(*size as i64)),
            // Your own row reads as "you": the name in the table already says which row is yours.
            LobbyIssue::PlayerNotReady(p) if Some(*p) == own => Msg::new("net.lobby.issue.you_not_ready"),
            LobbyIssue::NoBlueprint(p) if Some(*p) == own => Msg::new("net.lobby.issue.you_no_blueprint"),
            LobbyIssue::PlayerNotReady(p) => Msg::new("net.lobby.issue.player_not_ready").arg(name(*p)),
            LobbyIssue::NoBlueprint(p) => Msg::new("net.lobby.issue.no_blueprint").arg(name(*p)),
            LobbyIssue::Blueprint { player, issue } if Some(*player) == own => own_issue(issue),
            LobbyIssue::Blueprint { player, .. } => Msg::new("net.lobby.issue.blueprint").arg(name(*player)),
            LobbyIssue::LobbyOverComplexity { points, cap } if points.is_finite() && cap.is_finite() => {
                Msg::new("net.lobby.issue.lobby_over_complexity").num(points.round(), Unit::None).num(cap.round(), Unit::None)
            }
            LobbyIssue::LobbyOverComplexity { .. } => Msg::new("net.lobby.issue.complexity_unknown"),
            LobbyIssue::ModeNotInBuild => Msg::new("net.lobby.issue.mode_not_in_build"),
            LobbyIssue::LuauInPvp => Msg::new("net.lobby.issue.luau_in_pvp"),
        };
        if !out.contains(&msg) { out.push(msg); }
    }
    out
}

/// A host's refusal of a design as a sentence.
pub(crate) fn refusal_msg(reason: net::msg::BlueprintRefusal) -> Msg {
    use net::msg::BlueprintRefusal as R;
    Msg::new(match reason {
        R::NotEntitled => "net.lobby.refused.not_entitled",
        R::Unknown => "net.lobby.refused.unknown",
        R::TooLarge => "net.lobby.refused.too_large",
        R::Invalid => "net.lobby.refused.invalid",
    })
}

/// What the host panel tells a host whose router did not open the port (L997-999): forward the port by
/// hand to this machine's LAN address and share the public address, or use a mesh VPN (offered with its
/// own code when one runs). Empty while mapping is trying or worked.
pub(crate) fn manual_steps(info: &HostingInfo) -> Vec<Msg> {
    if !matches!(info.portmap, PortMapState::Failed | PortMapState::DoubleNat | PortMapState::RenewFailed) { return Vec::new(); }
    let lan = info.lan_address.rsplit_once(':').map_or(info.lan_address.as_str(), |(ip, _)| ip).trim_start_matches('[').trim_end_matches(']').to_string();
    let mut steps = Vec::new();
    if !lan.is_empty() {
        steps.push(Msg::new("net.host.manual.forward").arg(MsgArg::Name(info.port.to_string())).arg(MsgArg::Name(lan)));
        steps.push(Msg::new("net.host.manual.public"));
    }
    if info.vpn_address.is_empty() {
        steps.push(Msg::new("net.host.manual.vpn"));
    } else {
        steps.push(Msg::new("net.host.manual.vpn_found").arg(MsgArg::Name(info.vpn_address.clone())));
    }
    steps
}

/// Clients the upload figure carries beside the host (L1031).
pub(crate) fn client_limit(mode: GameMode, at_1_0: bool, upload_kbps: u32) -> u8 {
    sim_systems::modes::player_limit(mode, at_1_0, upload_kbps).saturating_sub(1)
}

/// A typed display name as the lobby keeps it: no control characters, trimmed, at most 24 characters.
pub(crate) fn clean_name(raw: &str) -> String {
    let s: String = raw.chars().filter(|c| !c.is_control()).collect();
    let s: String = s.trim().chars().take(net::lobby::MAX_NAME_CHARS).collect();
    s.trim_end().to_string()
}

/// Connection-log lines (RON, one per line, the format the host's session writes, L1039).
pub(crate) fn log_lines(entries: Vec<(std::time::Duration, net::quality::LogKind)>) -> String {
    let mut log = net::quality::ConnectionLog::default();
    for (at, kind) in entries { log.push(at, kind); }
    log.take_unflushed_ron()
}

#[cfg(test)]
mod tests;
