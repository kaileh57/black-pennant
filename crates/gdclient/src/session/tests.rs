use super::*;

#[test]
fn only_plain_files_of_the_replay_store_are_store_paths() {
    assert_eq!(store_file("user://replays/sortie_0001.bpr"), Some("sortie_0001.bpr"));
    assert_eq!(store_file("user://replays//x.bpr"), Some("x.bpr"));
    assert_eq!(store_file("user://replays/"), None);
    assert_eq!(store_file("user://replays/../settings.cfg"), None);
    assert_eq!(store_file("user://replays/sub/x.bpr"), None);
    assert_eq!(store_file("res://test/golden.bpr"), None);
}

#[test]
fn start_facts_name_every_design_of_the_config_once() {
    let config = testkit::basin_config();
    let start = host::HostStart::New { config: Box::new(config.clone()) };
    let facts = start_facts(&start, &crate::services::Services::default()).expect("facts");
    assert_eq!(facts.mode, GameMode::Splash);
    assert_eq!(facts.side, SideId(0));
    let mut refs: Vec<String> = config.vessels.iter().map(|v| format!("{:?}", v.blueprint)).collect();
    refs.sort();
    refs.dedup();
    assert_eq!(facts.designs.len(), refs.len(), "one build per distinct design");
    assert!(start_facts(&host::HostStart::Lobby, &crate::services::Services::default()).is_none());
}

#[test]
fn designs_build_on_the_pool_into_the_store_once() {
    let pool = testkit::pool();
    let mut builds = designs::DesignBuilds::default();
    let mut store = BTreeMap::new();
    let bp = Arc::new(setup::canonical(testkit::starter()));
    let hash = designs::blueprint_hash(&bp);
    assert!(builds.wants(&hash, &store));
    builds.request(&pool, Some(hash), designs::DesignSource::Blueprint(bp.clone()), Ruleset::default());
    assert!(!builds.wants(&hash, &store), "in flight");
    // The same design asked for by hash only is found among the stock designs.
    let stock = content::campaign::load::stock_blueprint("starters/patrol15.bp.ron").expect("stock");
    let stock_hash = designs::blueprint_hash(&stock);
    builds.request(&pool, Some(stock_hash), designs::DesignSource::StockHash(stock_hash), Ruleset::default());
    let unknown = ContentHash([3; 32]);
    builds.request(&pool, Some(unknown), designs::DesignSource::StockHash(unknown), Ruleset::default());
    let end = std::time::Instant::now() + Duration::from_secs(60);
    while builds.outstanding() > 0 && std::time::Instant::now() < end { builds.poll(&mut store); std::thread::sleep(Duration::from_millis(5)); }
    assert_eq!(builds.outstanding(), 0);
    assert!(matches!(store.get(&hash), Some(crate::worker::ReceivedDesign::Full { design, .. }) if design.hash == hash));
    assert!(store.contains_key(&stock_hash));
    assert!(!store.contains_key(&unknown));
    assert!(!builds.wants(&unknown, &store), "a hash that resolves nowhere is not asked for again");
    builds.clear();
    assert!(builds.wants(&unknown, &store));
}

#[test]
fn a_second_rate_press_steps_from_the_request_in_flight() {
    use sim_systems::protocol::TimeRate::{X1, X2, X4};
    // Nothing in flight: the confirmed rate.
    assert_eq!(step_base(X1, None, 10.0), X1);
    // 2x asked 50 ms ago and the host still says 1x: the next press goes on from 2x, to 4x.
    assert_eq!(step_base(X1, Some((X2, 9.95)), 10.0), X2);
    // The host caught up, or the request is stale (lost or refused): the confirmed rate again.
    assert_eq!(step_base(X2, Some((X2, 9.95)), 10.0), X2);
    assert_eq!(step_base(X1, Some((X4, 5.0)), 10.0), X1);
}
