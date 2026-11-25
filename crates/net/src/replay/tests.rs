//! Tests of unit `net-replay`: the session's sink, PvP release, the on-disk store with retention, and the
//! capture comparison. The live wire capture is decoded in `session::server::tests`.
use super::*;
use crate::schema::QuantPose;
use content::enums::GameMode;
use content::ids::PlayerSlot;
use sim_systems::modes::{LobbyPlayer, LobbySetup};
use std::sync::{Arc, OnceLock};

type Data = (Arc<content::catalogue::Catalogue>, Arc<content::campaign::CampaignData>, content::blueprint::Blueprint);

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
        let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
        let mut bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").expect("starter");
        content::blueprint::io::canonicalize_blueprint(&mut bp);
        bp.header.hash = content::blueprint::io::compute_hash(&bp);
        (catalogue, campaign, bp)
    })
}

fn sim(mode: GameMode) -> Sim {
    let (catalogue, campaign, bp) = data().clone();
    let seat = |slot: u8, team: u8| LobbyPlayer { slot: PlayerSlot(slot), client_id: 100 + slot as u64, name: format!("p{slot}"), team, blueprint: Some(bp.header.hash), ready: true, complexity: 0.0 };
    let setup = LobbySetup { mode, ruleset: Default::default(), seed: 3, players: vec![seat(0, 0), seat(1, 1)], skirmish: None, weather: content::weather::WeatherKind::Clear, sea_state: content::weather::SeaState(2), time_of_day: content::enums::TimeOfDay::Day, map: None, at_1_0: false };
    let config = match mode {
        GameMode::PvpElimination => sim_systems::modes::elimination_config(&setup, std::slice::from_ref(&bp), &campaign),
        _ => sim_systems::modes::sandbox_config(&setup, std::slice::from_ref(&bp), &campaign),
    };
    let deps = sim_systems::SimDeps { catalogue, campaign, designs: Default::default(), scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
    Sim::new(config, deps).expect("sim")
}

fn record(mode: GameMode, ticks: u32) -> Vec<u8> {
    let mut s = sim(mode);
    let mut sink = ReplaySink::start(&s, "0.1.0-test", RecordOptions::default());
    assert_eq!(sink.is_pvp(), mode == GameMode::PvpElimination);
    let first = s.drain_events();
    sink.record(&s, &first);
    for _ in 0..ticks {
        s.step();
        let ev = s.drain_events();
        sink.record(&s, &ev);
    }
    sink.finish()
}

#[test]
fn the_sink_records_a_replay_that_plays_and_pvp_releases_one_cut_per_side() {
    let bytes = record(GameMode::Sandbox, 120);
    let player = sim_systems::replay::play::ReplayPlayer::open(&bytes).expect("plays");
    assert_eq!(player.header().mode, GameMode::Sandbox);
    assert!(player.header().duration_ticks >= 120);

    let pvp = record(GameMode::PvpElimination, 90);
    let cuts = release_pvp(&pvp, &[SideId(1), SideId(0), SideId(1)]).expect("cuts");
    assert_eq!(cuts.iter().map(|c| c.0).collect::<Vec<_>>(), vec![SideId(0), SideId(1)]);
    for (side, cut) in &cuts {
        let p = sim_systems::replay::play::ReplayPlayer::open(cut).unwrap_or_else(|e| panic!("side {side:?}: {e}"));
        assert!(!p.header().pinned);
    }
    assert!(matches!(release_pvp(b"not a replay", &[SideId(0)]), Err(NetError::Replay(_))));
}

/// A fresh empty directory under the system temp dir.
fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bp-net-replay-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn the_store_keeps_twenty_recent_plus_pinned_and_guards_its_names() {
    let dir = temp_dir("store");
    let store = ReplayStore::new(dir.clone());
    assert!(store.list().unwrap().is_empty(), "a missing directory lists empty");
    let bytes = record(GameMode::Sandbox, 30);
    let mut names = Vec::new();
    for i in 0..23 {
        let name = store.save(&bytes).unwrap();
        assert!(valid_name(&name), "{name}");
        names.push(name.clone());
        if i == 1 { store.set_pinned(&name, true).unwrap(); }
    }
    let list = store.list().unwrap();
    assert_eq!(list.len(), 21, "20 most recent plus the pinned one");
    assert_eq!(list[0].file, names[22], "newest first");
    let pinned: Vec<&ReplayEntry> = list.iter().filter(|e| e.pinned).collect();
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].file, names[1]);
    assert!(pinned[0].header.pinned);
    for gone in [&names[0], &names[2]] { assert!(!list.iter().any(|e| e.file == **gone), "{gone} was pushed out"); }
    assert!(list.iter().all(|e| e.bytes == bytes.len() as u64 && e.header.mode == GameMode::Sandbox));
    assert_eq!(store.load(&names[22]).unwrap(), bytes);
    // Unpinning lets the next save push it out.
    store.set_pinned(&names[1], false).unwrap();
    store.save(&bytes).unwrap();
    assert!(!store.list().unwrap().iter().any(|e| e.file == names[1]));
    assert_eq!(store.list().unwrap().len(), 20);
    // Names that are not the store's own never reach the file system; nor do bytes that are not replays.
    for bad in ["../index.ron", "index.ron", "000001-zzzzzzzz.bpreplay", "/etc/passwd", "000001-0123abcd.bpreplay/../x"] {
        assert_eq!(store.load(bad).unwrap_err().kind(), std::io::ErrorKind::InvalidInput, "{bad}");
        assert!(store.set_pinned(bad, true).is_err());
    }
    assert_eq!(store.set_pinned("000099-0123abcd.bpreplay", true).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    assert_eq!(store.save(b"BPRP not really").unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    // A damaged index is rebuilt from the file names; stray files are ignored.
    std::fs::write(dir.join("index.ron"), "garbage(").unwrap();
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    std::fs::write(dir.join("999999-00000000.bpreplay"), b"not a replay").unwrap();
    assert_eq!(store.list().unwrap().len(), 20);
    let name = store.save(&bytes).unwrap();
    assert!(name.starts_with("1000000-"), "numbering continues past the highest file: {name}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compare_decodes_reports_pose_and_shell_mismatches() {
    let pose = |x: i32| QuantPose { pos: [x, 2, 3], rot: 77 };
    let a = DecodedStream { poses: vec![(2, 5, pose(1)), (4, 5, pose(2))], shell_impacts: vec![(9, [0.0, 0.0, 0.0], [0.1, 0.0, 0.0])] };
    assert!(compare_decodes(&a, &a.clone(), 0.5).is_empty());
    let mut b = a.clone();
    b.poses[1].2 = pose(3);
    let lines = compare_decodes(&a, &b, 0.5);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("pose 1"));
    let mut c = a.clone();
    c.shell_impacts[0].1 = [0.0, 0.7, 0.0];
    let lines = compare_decodes(&a, &c, 0.5);
    assert!(lines.iter().any(|l| l.contains("second decode") && l.contains("shell 9")), "{lines:?}");
    let mut d = a.clone();
    d.shell_impacts.push((10, [f64::NAN; 3], [0.0; 3]));
    d.poses.pop();
    let lines = compare_decodes(&a, &d, 0.5);
    assert!(lines.iter().any(|l| l.contains("pose count")));
    assert!(lines.iter().any(|l| l.contains("shell sets differ")));
    assert!(lines.iter().any(|l| l.contains("shell 10")), "a non-finite position is a miss: {lines:?}");
    // A capture of another wire version, or one whose frames do not decode, is refused.
    let cat = &data().0;
    assert!(decode_capture(&StreamCapture { protocol: crate::WIRE_VERSION + 1, frames: vec![] }, cat).is_err());
    assert!(decode_capture(&StreamCapture { protocol: crate::WIRE_VERSION, frames: vec![(1, Channel::Unreliable.id(), vec![0xff; 7])] }, cat).is_err());
    assert_eq!(decode_capture(&StreamCapture { protocol: crate::WIRE_VERSION, frames: vec![] }, cat).unwrap(), DecodedStream::default());
}
