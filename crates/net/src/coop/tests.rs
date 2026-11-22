use super::*;
use content::ids::ContentHash;
use content::save::TheatreState;

fn header(campaign: u64, seq: u64, prev: u8, played: u64) -> SaveHeader {
    SaveHeader { format: 1, game_version: "0".into(), tuning_version: 1, sequence: seq, prev_hash: ContentHash([prev; 32]), campaign_id: campaign, slot_name: "a".into(), ironman: false, played_s: played }
}

fn save(seq: u64) -> SaveGame {
    let theatre = TheatreState {
        seed: 1, act: Default::default(), hour: 0, clock_hours_left: None, retreat: false, open_theatre: false, difficulty: content::campaign::DifficultySliders::NORMAL,
        resources: Default::default(), players: Vec::new(), sextant_node: Default::default(), sextant_refit_until_hour: None, open_coves: Vec::new(), regions: Default::default(),
        lanes: Default::default(), depots: Default::default(), hulls: Vec::new(), groups: Vec::new(), convoys: Vec::new(), tallies: Default::default(), ops: Default::default(),
        offered: Vec::new(), hunter_group: None, last_admiral_plan_hour: 0, last_host_rung: 0, next_id: 0,
    };
    SaveGame { header: header(42, seq, 0, 0), theatre }
}

fn cp(tick: u64, config: u8) -> SortieCheckpoint { SortieCheckpoint { tick, keyframe_format: 1, config: vec![config; 4], keyframe: vec![tick as u8; 8] } }

fn mapped(port: u16) -> MappingReport { MappingReport { external: Some(SocketAddr::from(([203, 0, 113, 5], port))), double_nat: false } }

#[test]
fn succession_in_join_order_with_fresh_secrets() {
    let mut host = CoopHost::new(42);
    let first = host.take_succession().unwrap();
    assert!(first.entries.is_empty());
    assert_eq!(first.campaign_id, 42);
    assert!(host.take_succession().is_none(), "unchanged");
    host.member_joined(10, PlayerSlot(1));
    host.member_joined(11, PlayerSlot(2));
    host.member_joined(12, PlayerSlot(3));
    host.on_mapping(12, mapped(5000));
    host.on_mapping(11, MappingReport { external: Some(SocketAddr::from(([203, 0, 113, 6], 6000))), double_nat: true });
    host.on_mapping(10, mapped(4000));
    host.on_mapping(99, mapped(1));
    let list = host.take_succession().unwrap();
    assert_eq!(list.entries.iter().map(|e| e.client_id).collect::<Vec<_>>(), vec![10, 12]);
    assert_ne!(list.next_secret, first.next_secret);
    host.on_mapping(10, mapped(4000));
    assert!(host.take_succession().is_none(), "same report is no change");
    host.member_left(10);
    let list = host.take_succession().unwrap();
    assert_eq!(list.entries.iter().map(|e| (e.client_id, e.slot)).collect::<Vec<_>>(), vec![(12, PlayerSlot(3))]);
}

#[test]
fn checkpoint_cadence_and_bundle() {
    let mut host = CoopHost::new(42);
    assert!(host.checkpoint_due(0));
    let (tick, bytes) = host.bundle(cp(30, 1), save(3));
    assert_eq!(tick, 30);
    assert!(!host.checkpoint_due(30 + CHECKPOINT_TICKS - 1));
    assert!(host.checkpoint_due(30 + CHECKPOINT_TICKS));
    assert!(host.checkpoint_due(5), "lower tick: a new sortie");
    let back = decode_bundle(&bytes).unwrap();
    assert_eq!(back, CheckpointBundle { checkpoint: cp(30, 1), save: save(3) });
    assert!(decode_bundle(&bytes[..bytes.len() / 2]).is_none());
    host.sortie_started();
    assert!(host.checkpoint_due(1));
}

#[test]
fn member_keeps_newest_checkpoint() {
    let mut m = CoopMember::new(12);
    m.on_checkpoint(CheckpointBundle { checkpoint: cp(7200, 1), save: save(3) });
    m.on_checkpoint(CheckpointBundle { checkpoint: cp(3600, 1), save: save(3) });
    assert_eq!(m.checkpoint().unwrap().checkpoint.tick, 7200, "older tick of the same sortie ignored");
    m.on_checkpoint(CheckpointBundle { checkpoint: cp(0, 2), save: save(4) });
    assert_eq!(m.checkpoint().unwrap().checkpoint.tick, 0, "a later operation replaces it");
    m.on_checkpoint(CheckpointBundle { checkpoint: cp(9000, 1), save: save(3) });
    assert_eq!(m.checkpoint().unwrap().checkpoint.tick, 0, "earlier save sequence ignored");
}

#[test]
fn rehost_plans() {
    let mut a = CoopMember::new(10);
    let mut b = CoopMember::new(12);
    assert_eq!(a.rehost_plan(), RehostPlan::Manual { checkpoint: None });
    assert!(!a.host_lost(Duration::from_millis(9_999)) && a.host_lost(HOST_SILENCE));
    let mut host = CoopHost::new(42);
    host.member_joined(10, PlayerSlot(1));
    host.member_joined(12, PlayerSlot(2));
    host.on_mapping(10, mapped(4000));
    let list = host.take_succession().unwrap();
    a.on_succession(list.clone());
    b.on_succession(list.clone());
    // First on the list but no checkpoint: re-host by hand from the save copy.
    assert_eq!(a.rehost_plan(), RehostPlan::Manual { checkpoint: None });
    let bundle = CheckpointBundle { checkpoint: cp(60, 1), save: save(3) };
    a.on_checkpoint(bundle.clone());
    assert_eq!(a.rehost_plan(), RehostPlan::HostHere { secret: LobbySecret(list.next_secret), checkpoint: Box::new(bundle) });
    assert_eq!(b.rehost_plan(), RehostPlan::ConnectTo { addr: SocketAddr::from(([203, 0, 113, 5], 4000)), secret: LobbySecret(list.next_secret) });
    b.on_succession(SuccessionList { entries: Vec::new(), next_secret: [0; 10], campaign_id: 42 });
    assert!(matches!(b.rehost_plan(), RehostPlan::Manual { .. }));
}

#[test]
fn save_comparison() {
    let ours = header(42, 5, 1, 100);
    assert_eq!(compare_saves(&ours, &ours), SaveCompare::Same);
    let mut renamed = ours.clone();
    renamed.slot_name = "b".into();
    assert_eq!(compare_saves(&ours, &renamed), SaveCompare::Same);
    assert_eq!(compare_saves(&ours, &header(42, 6, 2, 150)), SaveCompare::TheirsNewer);
    assert_eq!(compare_saves(&ours, &header(42, 4, 0, 50)), SaveCompare::OursNewer);
    assert_eq!(compare_saves(&ours, &header(42, 5, 9, 100)), SaveCompare::Forked);
    assert_eq!(compare_saves(&ours, &header(42, 5, 1, 130)), SaveCompare::Forked, "two saves from one parent");
    assert_eq!(compare_saves(&ours, &header(43, 5, 1, 100)), SaveCompare::DifferentCampaign);
}
