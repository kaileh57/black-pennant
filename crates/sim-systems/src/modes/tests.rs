use super::*;

fn player(slot: u8, team: u8, ready: bool) -> LobbyPlayer { LobbyPlayer { slot: PlayerSlot(slot), client_id: 100 + slot as u64, name: format!("p{slot}"), team, blueprint: None, ready, complexity: 300.0 } }

fn setup(mode: GameMode, players: Vec<LobbyPlayer>) -> LobbySetup {
    LobbySetup { mode, ruleset: Ruleset::default(), seed: 42, players, skirmish: None, weather: WeatherKind::Clear, sea_state: SeaState(3), time_of_day: TimeOfDay::Dusk, map: None, at_1_0: false }
}

/// A fixed sequence standing in for the counter-based RNG.
fn fake(purpose: u64, n: u32) -> f64 { (((purpose.wrapping_mul(2_654_435_761) ^ (n as u64).wrapping_mul(40_503)) % 1_000) as f64) / 1_000.0 }

#[test]
fn pvp_team_rules() {
    let s = setup(GameMode::PvpElimination, vec![player(0, 0, true), player(1, 1, true)]);
    assert!(shape_issues(&s).is_empty());
    let s = setup(GameMode::PvpElimination, vec![player(0, 0, true), player(1, 0, true), player(2, 1, true)]);
    assert!(shape_issues(&s).contains(&LobbyIssue::TeamsUneven));
    // 3v3 only at 1.0.
    let six: Vec<_> = (0..6).map(|i| player(i, i % 2, true)).collect();
    let s = setup(GameMode::PvpElimination, six.clone());
    let issues = shape_issues(&s);
    assert!(issues.contains(&LobbyIssue::TooManyPlayers { max: 4 }));
    assert!(issues.contains(&LobbyIssue::TeamSizeNotOffered { size: 3 }));
    let s = LobbySetup { at_1_0: true, ..setup(GameMode::PvpElimination, six) };
    assert!(shape_issues(&s).is_empty());
    // A team index from the wire outside 0 and 1.
    let s = setup(GameMode::PvpElimination, vec![player(0, 0, true), player(1, 7, true)]);
    assert!(shape_issues(&s).contains(&LobbyIssue::TeamsUneven));
    let s = setup(GameMode::PvpElimination, vec![]);
    assert!(shape_issues(&s).contains(&LobbyIssue::TeamSizeNotOffered { size: 0 }));
    let mut s = setup(GameMode::PvpElimination, vec![player(0, 0, false), player(1, 1, true)]);
    s.ruleset.luau = true;
    let issues = shape_issues(&s);
    assert!(issues.contains(&LobbyIssue::LuauInPvp));
    assert!(issues.contains(&LobbyIssue::PlayerNotReady(PlayerSlot(0))));
    assert!(shape_issues(&setup(GameMode::PvpEscort, vec![player(0, 0, true), player(1, 1, true)])).contains(&LobbyIssue::ModeNotInBuild));
    assert!(shape_issues(&setup(GameMode::Splash, vec![])).contains(&LobbyIssue::ModeNotInBuild));
    let nine: Vec<_> = (0..9).map(|i| player(i, 0, true)).collect();
    assert!(shape_issues(&setup(GameMode::Sandbox, nine)).contains(&LobbyIssue::TooManyPlayers { max: 8 }));
}

#[test]
fn lobby_blueprints_and_complexity() {
    let catalogue = Catalogue::default();
    let mut ps: Vec<_> = (0..4).map(|i| player(i, i % 2, true)).collect();
    ps[1].complexity = f64::NAN;
    ps[2].complexity = 1_000.0;
    let s = setup(GameMode::PvpElimination, ps);
    let issues = validate_lobby(&s, &[], &catalogue);
    for i in 0..4 { assert!(issues.contains(&LobbyIssue::NoBlueprint(PlayerSlot(i)))); }
    assert!(issues.iter().any(|i| matches!(i, LobbyIssue::LobbyOverComplexity { points, .. } if points.is_infinite())));
    // Campaign hulls come from the save.
    let s = setup(GameMode::Campaign, vec![player(0, 0, true)]);
    assert!(validate_lobby(&s, &[], &catalogue).is_empty());
    let (sum, bad) = lobby_complexity(&[player(0, 0, true), player(1, 0, true)]);
    assert_eq!((sum, bad), (600.0, false));
}

#[test]
fn limits_caps_and_weather() {
    assert_eq!(player_limit(GameMode::Sandbox, false, 1_000), 4);
    assert_eq!(player_limit(GameMode::Sandbox, false, 2_000), 8);
    assert_eq!(act_for_cap(150.0), Act::Act1);
    assert_eq!(act_for_cap(400.0), Act::Act2);
    assert_eq!(act_for_cap(1_800.0), Act::Act4);
    assert_eq!(act_for_cap(5_000.0), Act::Act4);
    for tod in [TimeOfDay::Night, TimeOfDay::Dusk, TimeOfDay::Day] {
        let w = weather_state(WeatherKind::Storm, SeaState(2), tod, f64::NAN);
        assert_eq!(TimeOfDay::from_sun_elevation_deg(w.sun_elevation_deg), tod);
        assert_eq!(w.sea_state, SeaState(7));
        assert_eq!(w.visibility_m, 1_500.0);
        assert_eq!(w.wind_dir_rad, 0.0);
    }
    let w = weather_state(WeatherKind::FogBank, SeaState(8), TimeOfDay::Dusk, 1.0);
    assert_eq!(w.sea_state, SeaState(3));
}

#[test]
fn line_abreast_is_centred_and_spaced() {
    // Heading east: the line runs north to south.
    let l = line_abreast(DVec2::new(100.0, 0.0), 90.0, 3);
    assert!((l[1] - DVec2::new(100.0, 0.0)).length() < 1e-9);
    assert!((l[0].distance(l[1]) - 600.0).abs() < 1e-9);
    assert!((l[0].x - 100.0).abs() < 1e-9);
    assert!(line_abreast(DVec2::ZERO, 0.0, 0).is_empty());
}

#[test]
fn elimination_map_is_point_symmetric_and_clear_of_spawns() {
    for players in [2, 4, 8] {
        let m = layout(GameMode::PvpElimination, 9, players, &fake);
        let a = m.markers.iter().find(|k| k.name == "spawn_a").unwrap();
        let b = m.markers.iter().find(|k| k.name == "spawn_b").unwrap();
        assert_eq!(a.pos_m[0], -b.pos_m[0]);
        let sep = DVec2::from(a.pos_m).distance(DVec2::from(b.pos_m));
        assert!((12_000.0..=24_000.0).contains(&sep), "{sep}");
        assert_eq!(m.islands.len() % 2, 0);
        for i in &m.islands {
            assert!(m.islands.iter().any(|j| j.centre_m[0] == -i.centre_m[0] && j.centre_m[1] == -i.centre_m[1] && j.radius_m == i.radius_m));
            for k in [a, b] { assert!(DVec2::from(i.centre_m).distance(DVec2::from(k.pos_m)) >= i.radius_m + 3_000.0); }
            assert!(i.centre_m[0].abs() + i.radius_m < m.size_m[0] * 0.5);
        }
    }
}

#[test]
fn other_maps() {
    let m = layout(GameMode::PvpEscort, 1, 4, &fake);
    assert!((20_000.0..=40_000.0).contains(&m.size_m[0]));
    let entry = m.markers.iter().find(|k| k.kind == MarkerKind::ConvoyEntry).unwrap();
    let exit = m.markers.iter().find(|k| k.kind == MarkerKind::ConvoyExit).unwrap();
    assert!(exit.pos_m[0] - entry.pos_m[0] >= 16_000.0);
    for i in &m.islands { assert!(i.centre_m[1].abs() >= i.radius_m + 2_500.0); }
    let s = layout(GameMode::Sandbox, 1, 8, &fake);
    assert!(s.markers.iter().any(|k| k.kind == MarkerKind::Survivors));
    assert_eq!(s.markers.iter().filter(|k| k.kind == MarkerKind::PlayerSpawn).count(), 2);
    let k = layout(GameMode::Skirmish, 1, 1, &fake);
    assert!(k.markers.iter().any(|m| m.kind == MarkerKind::EnemySpawn));
    let basin = layout(GameMode::Splash, 1, 1, &fake);
    assert!(basin.islands.is_empty());
    // Same draws, same map.
    assert_eq!(layout(GameMode::Sandbox, 5, 3, &fake), layout(GameMode::Sandbox, 5, 3, &fake));
}
