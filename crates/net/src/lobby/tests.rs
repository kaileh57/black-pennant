use super::*;
use crate::msg::BuildKind;
use content::enums::TimeOfDay;
use content::ruleset::Ruleset;
use content::weather::{SeaState, WeatherKind};

fn setup(mode: GameMode) -> LobbySetup {
    LobbySetup { mode, ruleset: Ruleset::default(), seed: 7, players: Vec::new(), skirmish: None, weather: WeatherKind::Clear, sea_state: SeaState(3), time_of_day: TimeOfDay::Dusk, map: None, at_1_0: false }
}

fn hello(id: u64, name: &str) -> Hello { Hello { protocol: crate::PROTOCOL_VERSION, game_version: "0".into(), build: BuildKind::Dev, client_id: id, name: name.into() } }

const T0: Duration = Duration::from_secs(10);

fn listen(mode: GameMode, kbps: u32) -> Lobby {
    let mut l = Lobby::new("test".into(), setup(mode), kbps, false);
    assert_eq!(l.admit(&hello(1, "host"), T0, SessionPhase::Lobby), Ok(Admission { slot: PlayerSlot(0), rejoined: false }));
    l
}

#[test]
fn host_first_then_lowest_free_slots() {
    let mut l = listen(GameMode::Sandbox, 8_000);
    assert!(l.member(PlayerSlot(0)).unwrap().host);
    assert_eq!(l.admit(&hello(2, "a"), T0, SessionPhase::Lobby).unwrap().slot, PlayerSlot(1));
    assert_eq!(l.admit(&hello(3, "b"), T0, SessionPhase::Lobby).unwrap().slot, PlayerSlot(2));
    l.remove(2, T0, false);
    assert_eq!(l.admit(&hello(4, "c"), T0, SessionPhase::Lobby).unwrap().slot, PlayerSlot(1));
    assert!(!l.member(PlayerSlot(1)).unwrap().host);
    assert_eq!(l.setup().players.iter().map(|p| p.slot.0).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(l.admit(&hello(3, "b"), T0, SessionPhase::Lobby), Err(JoinRefusal::DuplicateClient));
}

#[test]
fn upload_limit_and_saturation() {
    // 1 Mbps: floor(0.9 * 1000 / 256) = 3 clients plus the host.
    let mut l = listen(GameMode::Sandbox, 1_000);
    for id in 2..=4 { l.admit(&hello(id, "x"), T0, SessionPhase::Lobby).unwrap(); }
    assert_eq!(l.admit(&hello(5, "x"), T0, SessionPhase::Lobby), Err(JoinRefusal::LobbyFull { max_players: 4 }));
    l.remove(4, T0, false);
    l.set_saturated(true);
    assert_eq!(l.admit(&hello(5, "x"), T0, SessionPhase::Lobby), Err(JoinRefusal::HostSaturated));
    // A dedicated server seats only clients.
    let mut d = Lobby::new("d".into(), setup(GameMode::Sandbox), 1_000, true);
    assert_eq!(d.admit(&hello(9, "x"), T0, SessionPhase::Lobby).unwrap().slot, PlayerSlot(0));
    assert!(!d.members()[0].host);
    for id in 10..12 { d.admit(&hello(id, "x"), T0, SessionPhase::Lobby).unwrap(); }
    assert_eq!(d.admit(&hello(12, "x"), T0, SessionPhase::Lobby), Err(JoinRefusal::LobbyFull { max_players: 3 }));
}

#[test]
fn phase_rules_per_mode() {
    let mut c = listen(GameMode::Campaign, 1_000);
    assert_eq!(c.admit(&hello(2, "x"), T0, SessionPhase::Sortie), Err(JoinRefusal::OperationInProgress));
    assert!(c.admit(&hello(2, "x"), T0, SessionPhase::Theatre).is_ok());
    let mut p = listen(GameMode::PvpElimination, 1_000);
    assert_eq!(p.admit(&hello(2, "x"), T0, SessionPhase::Sortie), Err(JoinRefusal::MatchInProgress));
    let mut s = listen(GameMode::Sandbox, 1_000);
    assert!(s.admit(&hello(2, "x"), T0, SessionPhase::Sortie).is_ok());
}

#[test]
fn held_slot_rejoin_and_expiry() {
    let mut l = listen(GameMode::Campaign, 1_000);
    l.admit(&hello(2, "a"), T0, SessionPhase::Theatre).unwrap();
    // Co-op at first release is 2 players.
    assert_eq!(l.admit(&hello(3, "b"), T0, SessionPhase::Theatre), Err(JoinRefusal::LobbyFull { max_players: 2 }));
    l.apply(PlayerSlot(1), &LobbyRequest::SetBlueprint(ContentHash([5; 32])), SessionPhase::Lobby).unwrap();
    assert_eq!(l.remove(2, T0, true), Some(PlayerSlot(1)));
    // The held slot is not handed out, and a rejoin skips phase and saturation rules.
    l.set_saturated(true);
    let back = l.admit(&hello(2, "a"), T0 + Duration::from_secs(299), SessionPhase::Sortie).unwrap();
    assert_eq!(back, Admission { slot: PlayerSlot(1), rejoined: true });
    assert_eq!(l.setup().players.iter().find(|p| p.slot == PlayerSlot(1)).unwrap().blueprint, Some(ContentHash([5; 32])));
    // Past the hold the slot reopens.
    l.set_saturated(false);
    l.remove(2, T0, true);
    assert_eq!(l.held_slots(T0 + Duration::from_secs(100))[0].2, Duration::from_secs(200));
    assert!(l.expire_holds(T0 + Duration::from_secs(299)).is_empty());
    assert_eq!(l.expire_holds(T0 + HOLD), vec![(2, PlayerSlot(1))]);
    assert_eq!(l.admit(&hello(2, "a"), T0 + HOLD, SessionPhase::Sortie), Err(JoinRefusal::OperationInProgress));
}

#[test]
fn kick_and_ban() {
    let mut l = listen(GameMode::Sandbox, 2_000);
    l.admit(&hello(2, "a"), T0, SessionPhase::Lobby).unwrap();
    assert_eq!(l.kick(PlayerSlot(0), true), None, "host cannot be kicked");
    assert_eq!(l.kick(PlayerSlot(1), true), Some(2));
    assert!(l.member(PlayerSlot(1)).is_none());
    assert_eq!(l.admit(&hello(2, "a"), T0, SessionPhase::Lobby), Err(JoinRefusal::Banned));
    l.admit(&hello(3, "b"), T0, SessionPhase::Lobby).unwrap();
    assert_eq!(l.kick(PlayerSlot(1), false), Some(3));
    assert!(l.admit(&hello(3, "b"), T0, SessionPhase::Lobby).is_ok());
}

#[test]
fn requests() {
    let mut l = listen(GameMode::PvpElimination, 1_000);
    for id in 2..=4 { l.admit(&hello(id, "x"), T0, SessionPhase::Lobby).unwrap(); }
    let lobby = SessionPhase::Lobby;
    assert_eq!(l.apply(PlayerSlot(1), &LobbyRequest::Start, lobby), Err(RequestDenied::NotHost));
    assert_eq!(l.apply(PlayerSlot(1), &LobbyRequest::SetSetup(Box::new(setup(GameMode::Sandbox))), lobby), Err(RequestDenied::NotHost));
    assert_eq!(l.apply(PlayerSlot(9), &LobbyRequest::SetReady(true), lobby), Err(RequestDenied::NotHost));
    assert_eq!(l.apply(PlayerSlot(1), &LobbyRequest::SetReady(true), SessionPhase::Sortie), Err(RequestDenied::WrongPhase));
    // 2v2 at first release: a third player on team 1 is refused.
    l.apply(PlayerSlot(2), &LobbyRequest::SetTeam(1), lobby).unwrap();
    l.apply(PlayerSlot(3), &LobbyRequest::SetTeam(1), lobby).unwrap();
    assert_eq!(l.apply(PlayerSlot(1), &LobbyRequest::SetTeam(1), lobby), Err(RequestDenied::TeamFull));
    assert_eq!(l.apply(PlayerSlot(1), &LobbyRequest::SetTeam(2), lobby), Err(RequestDenied::TeamFull));
    assert_eq!(l.apply(PlayerSlot(1), &LobbyRequest::SetBlueprint(ContentHash::EMPTY), lobby), Err(RequestDenied::NoSuchBlueprint));
    l.apply(PlayerSlot(1), &LobbyRequest::SetReady(true), lobby).unwrap();
    l.apply(PlayerSlot(1), &LobbyRequest::SetBlueprint(ContentHash([1; 32])), lobby).unwrap();
    assert!(!l.setup().players[1].ready, "a new design clears ready");
    // The host's setup change keeps players and clears ready.
    l.apply(PlayerSlot(1), &LobbyRequest::SetReady(true), lobby).unwrap();
    let mut s = setup(GameMode::PvpElimination);
    s.seed = 99;
    l.apply(PlayerSlot(0), &LobbyRequest::SetSetup(Box::new(s)), lobby).unwrap();
    assert_eq!(l.setup().seed, 99);
    assert_eq!(l.setup().players.len(), 4);
    assert!(l.setup().players.iter().all(|p| !p.ready));
    assert_eq!(l.setup().players[2].team, 1);
    assert_eq!(l.apply(PlayerSlot(0), &LobbyRequest::Start, lobby), Ok(()));
}

#[test]
fn pvp_view_hides_opponent_designs() {
    let mut l = listen(GameMode::PvpElimination, 1_000);
    l.admit(&hello(2, "x"), T0, SessionPhase::Lobby).unwrap();
    let lobby = SessionPhase::Lobby;
    l.apply(PlayerSlot(1), &LobbyRequest::SetTeam(1), lobby).unwrap();
    l.apply(PlayerSlot(0), &LobbyRequest::SetBlueprint(ContentHash([1; 32])), lobby).unwrap();
    l.apply(PlayerSlot(1), &LobbyRequest::SetBlueprint(ContentHash([2; 32])), lobby).unwrap();
    l.set_upload_progress(PlayerSlot(1), Some(0.5));
    let cat = Catalogue::default();
    let ext = |h: &ContentHash| (h.0[0] == 1).then_some(ContentHash([11; 32]));
    let v0 = l.view_for(PlayerSlot(0), &[], &cat, &ext);
    assert_eq!(v0.members[0].blueprint, Some(ContentHash([1; 32])));
    // No exterior hash known yet: hidden, never the full hash.
    assert_eq!(v0.members[1].blueprint, None);
    assert_eq!(v0.members[1].upload, Some(0.5));
    let v1 = l.view_for(PlayerSlot(1), &[], &cat, &ext);
    assert_eq!(v1.members[0].blueprint, Some(ContentHash([11; 32])));
    assert_eq!(v1.members[1].blueprint, Some(ContentHash([2; 32])));
    // A non-member sees only exterior forms.
    let vx = l.view_for(PlayerSlot(7), &[], &cat, &ext);
    assert_eq!(vx.members[1].blueprint, None);
    assert!(!v0.can_start);
    assert!(v0.issues.contains(&LobbyIssue::PlayerNotReady(PlayerSlot(0))));
    assert_eq!(v0.max_players, 4);
}

#[test]
fn can_start_needs_everyone_ready() {
    // Campaign co-op needs no blueprints, so validation stays off the blueprint path.
    let mut l = listen(GameMode::Campaign, 1_000);
    l.admit(&hello(2, "x"), T0, SessionPhase::Lobby).unwrap();
    let cat = Catalogue::default();
    assert!(!l.can_start(&[], &cat));
    l.apply(PlayerSlot(0), &LobbyRequest::SetReady(true), SessionPhase::Lobby).unwrap();
    assert!(!l.can_start(&[], &cat));
    l.apply(PlayerSlot(1), &LobbyRequest::SetReady(true), SessionPhase::Lobby).unwrap();
    assert!(l.can_start(&[], &cat));
    assert!(l.view_for(PlayerSlot(1), &[], &cat, &|_| None).can_start);
}

#[test]
fn names_are_cleaned() {
    assert_eq!(clean_name("  a\u{7}b  "), "ab");
    assert_eq!(clean_name(&"x".repeat(40)).chars().count(), MAX_NAME_CHARS);
    assert_eq!(clean_name("\u{1F6A2}é".repeat(20).as_str()).chars().count(), MAX_NAME_CHARS);
}
