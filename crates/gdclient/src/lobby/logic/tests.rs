use super::*;
use std::collections::BTreeSet;

fn names(p: PlayerSlot) -> String { format!("P{}", p.0) }
fn own_issue(i: &Issue) -> Msg { Msg::new("units.test").arg(MsgArg::Name(format!("{i:?}"))) }

#[test]
fn pvp_setups_take_a_preset_and_an_act_cap() {
    let base = initial_setup(GameMode::PvpElimination, 9);
    let s = apply_patch(&base, &SetupPatch { mode: Some(GameMode::PvpElimination), ..Default::default() }, 5).expect("the default");
    assert_eq!(ruleset_id(s.mode, &s.ruleset), DEFAULT_PVP_RULESET);
    assert_eq!(s.ruleset.tonnage_cap_t, 400.0, "400 t by default (L971)");
    assert_eq!((s.ruleset.complexity_per_vessel, s.ruleset.complexity_per_lobby), (600.0, 3_600.0));
    assert!(!s.ruleset.luau);
    let open = apply_patch(&s, &SetupPatch { ruleset: Some("open".into()), tonnage_cap_t: Some(900.0), ..Default::default() }, 5).expect("open at 900 t");
    assert_eq!(ruleset_id(open.mode, &open.ruleset), "open");
    assert_eq!(open.ruleset.tonnage_cap_t, 900.0);
    // The cap and the preset stay when only the weather changes.
    let wet = apply_patch(&open, &SetupPatch { weather: Some(WeatherKind::Rain), ..Default::default() }, 5).unwrap();
    assert_eq!((ruleset_id(wet.mode, &wet.ruleset).as_str(), wet.ruleset.tonnage_cap_t, wet.weather), ("open", 900.0, WeatherKind::Rain));
    assert_eq!(apply_patch(&s, &SetupPatch { tonnage_cap_t: Some(500.0), ..Default::default() }, 5), Err(Msg::new("net.host.bad_ruleset")));
    assert_eq!(apply_patch(&s, &SetupPatch { tonnage_cap_t: Some(f64::NAN), ..Default::default() }, 5), Err(Msg::new("net.host.bad_ruleset")));
    assert_eq!(apply_patch(&s, &SetupPatch { ruleset: Some("anything".into()), ..Default::default() }, 5), Err(Msg::new("net.host.bad_ruleset")));
    for cap in [150.0, 400.0, 900.0, 1_800.0] { assert!(pvp_cap_ok(cap)); }
}

#[test]
fn other_modes_field_their_own_rulesets() {
    let base = initial_setup(GameMode::Skirmish, 9);
    let sk = SkirmishOptions { template: MissionType::DepotStrike, act: Act::Act3, enemy_per_player: 40, difficulty: DifficultySliders::NORMAL };
    let s = apply_patch(&base, &SetupPatch { skirmish: Some(sk), seed: Some(0), sea_state: Some(SeaState(5)), ..Default::default() }, 77).unwrap();
    assert_eq!(s.ruleset, Ruleset::campaign(Act::Act3), "the act's cap binds a skirmish");
    assert_eq!(s.skirmish.as_ref().map(|k| k.enemy_per_player), Some(MAX_ENEMY_PER_PLAYER));
    assert_eq!(s.seed, 77, "seed 0 asks for a new one");
    assert_eq!(s.sea_state, SeaState(5));
    assert!(apply_patch(&base, &SetupPatch { sea_state: Some(SeaState(9)), ..Default::default() }, 1).is_err());
    let sandbox = apply_patch(&s, &SetupPatch { mode: Some(GameMode::Sandbox), ..Default::default() }, 1).unwrap();
    assert_eq!(sandbox.ruleset, sandbox_ruleset());
    assert!(sandbox.skirmish.is_none());
    // A campaign lobby stays a campaign lobby, and no other lobby becomes one.
    let coop = initial_setup(GameMode::Campaign, 1);
    assert!(apply_patch(&coop, &SetupPatch { mode: Some(GameMode::Sandbox), ..Default::default() }, 1).is_err());
    assert!(apply_patch(&sandbox, &SetupPatch { mode: Some(GameMode::Campaign), ..Default::default() }, 1).is_err());
    assert!(apply_patch(&sandbox, &SetupPatch { mode: Some(GameMode::Splash), ..Default::default() }, 1).is_err());
    assert!(apply_patch(&coop, &SetupPatch::default(), 1).is_ok());
}

#[test]
fn issues_name_players_and_spell_out_only_our_own_design() {
    let me = PlayerSlot(1);
    let issues = vec![
        LobbyIssue::PlayerNotReady(PlayerSlot(0)),
        LobbyIssue::NoBlueprint(PlayerSlot(2)),
        LobbyIssue::Blueprint { player: me, issue: Issue::OverTonnage { tonnage_t: 450.0, cap_t: 400.0 } },
        LobbyIssue::Blueprint { player: PlayerSlot(2), issue: Issue::GridOutOfBounds },
        LobbyIssue::Blueprint { player: PlayerSlot(2), issue: Issue::LayerNotAllowed },
        LobbyIssue::LobbyOverComplexity { points: 3_700.4, cap: 3_600.0 },
        LobbyIssue::LobbyOverComplexity { points: f64::INFINITY, cap: 3_600.0 },
        LobbyIssue::TooManyPlayers { max: 4 },
        LobbyIssue::TeamSizeNotOffered { size: 3 },
        LobbyIssue::TeamsUneven, LobbyIssue::ModeNotInBuild, LobbyIssue::LuauInPvp,
    ];
    let out = issue_msgs(&issues, Some(me), &names, &own_issue);
    let keys: Vec<&str> = out.iter().map(|m| m.key.0.as_str()).collect();
    assert_eq!(keys, vec![
        "net.lobby.issue.player_not_ready", "net.lobby.issue.no_blueprint", "units.test", "net.lobby.issue.blueprint",
        "net.lobby.issue.lobby_over_complexity", "net.lobby.issue.complexity_unknown", "net.lobby.issue.too_many_players",
        "net.lobby.issue.team_size_not_offered", "net.lobby.issue.teams_uneven", "net.lobby.issue.mode_not_in_build", "net.lobby.issue.luau_in_pvp",
    ], "another player's two design problems are one line");
    assert_eq!(out[0].args, vec![MsgArg::Name("P0".into())]);
    assert_eq!(out[3].args, vec![MsgArg::Name("P2".into())]);
    assert_eq!(out[4].args, vec![MsgArg::Num { value: 3_700.0, unit: Unit::None }, MsgArg::Num { value: 3_600.0, unit: Unit::None }]);
    // A spectator (no slot yet) sees every design problem as a named line.
    let out = issue_msgs(&issues[2..3], None, &names, &own_issue);
    assert_eq!(out[0].key.0, "net.lobby.issue.blueprint");
    // Our own row reads as "you", without a name.
    let out = issue_msgs(&[LobbyIssue::PlayerNotReady(me), LobbyIssue::NoBlueprint(me)], Some(me), &names, &own_issue);
    let keys: Vec<&str> = out.iter().map(|m| m.key.0.as_str()).collect();
    assert_eq!(keys, vec!["net.lobby.issue.you_not_ready", "net.lobby.issue.you_no_blueprint"]);
    assert!(out.iter().all(|m| m.args.is_empty()));
}

#[test]
fn manual_steps_appear_only_when_mapping_did_not_work() {
    let mut info = HostingInfo { port: 47610, lan_address: "192.168.1.20:47610".into(), portmap: PortMapState::Trying, ..Default::default() };
    assert!(manual_steps(&info).is_empty());
    info.portmap = PortMapState::Mapped;
    assert!(manual_steps(&info).is_empty());
    info.portmap = PortMapState::Failed;
    let steps = manual_steps(&info);
    assert_eq!(steps.iter().map(|m| m.key.0.as_str()).collect::<Vec<_>>(), vec!["net.host.manual.forward", "net.host.manual.public", "net.host.manual.vpn"]);
    assert_eq!(steps[0].args, vec![MsgArg::Name("47610".into()), MsgArg::Name("192.168.1.20".into())], "a port is written as typed, not grouped like a count");
    info.portmap = PortMapState::DoubleNat;
    info.vpn_address = "100.101.102.103:47610".into();
    info.lan_address = "[fd00::5]:47610".into();
    let steps = manual_steps(&info);
    assert_eq!(steps[0].args[1], MsgArg::Name("fd00::5".into()));
    assert_eq!(steps.last().map(|m| m.key.0.as_str()), Some("net.host.manual.vpn_found"));
}

#[test]
fn client_limit_follows_the_upload_figure() {
    // 1 Mbps carries 3 clients (4 players); an 8-player sandbox needs about 2 Mbps (L1031).
    assert_eq!(client_limit(GameMode::Sandbox, false, 1_000), 3);
    assert_eq!(client_limit(GameMode::Sandbox, false, 2_000), 7);
    assert_eq!(client_limit(GameMode::PvpElimination, false, 100_000), 3);
    assert_eq!(client_limit(GameMode::Campaign, false, 1_000), 1);
}

#[test]
fn names_are_cleaned_like_the_host_cleans_them() {
    assert_eq!(clean_name("  Captain\u{7}  Ahab  "), "Captain  Ahab");
    assert_eq!(clean_name(&"x".repeat(40)).chars().count(), 24);
    assert_eq!(clean_name("\n\t"), "");
}

#[test]
fn every_lobby_key_has_an_english_row() {
    let csv = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n/net.csv")).expect("net.csv");
    let rows: BTreeSet<&str> = csv.lines().skip(1).filter_map(|l| l.split(',').next()).collect();
    let src = include_str!("../../lobby.rs").to_string() + include_str!("../logic.rs");
    let mut found = BTreeSet::new();
    for part in src.split("\"net.").skip(1) {
        let key: String = part.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.').collect();
        let key = key.trim_end_matches('.');
        // `net.port` and `net.upload_kbps` are setting keys, worded by the settings screen.
        if key.is_empty() || key.ends_with('*') || key == "port" || key == "upload_kbps" { continue; }
        found.insert(format!("net.{key}"));
    }
    assert!(found.len() > 20);
    for key in &found { assert!(rows.contains(key.as_str()), "net.csv lacks {key}"); }
    // No row is orphaned: every row is a key this unit or `link::net` produces.
    let link: BTreeSet<String> = crate::link::net::text_keys().into_iter().map(String::from).collect();
    for row in rows { assert!(found.contains(row) || link.contains(row), "{row} is never produced"); }
    assert!(log_lines(vec![(std::time::Duration::from_secs(2), net::quality::LogKind::Left { client_id: 7 })]).contains("Left"));
}

