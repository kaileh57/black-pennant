use super::*;
use crate::campaign::{KillMethod, RangeBand};
use crate::enums::{Emcon, TimeOfDay};
use crate::error::ContentError;
use crate::ids::{MunitionId, TheatreNodeId};

fn header(sequence: u64, prev: ContentHash) -> SaveHeader {
    SaveHeader { format: SAVE_FORMAT, game_version: "0.1.0".into(), tuning_version: 3, sequence, prev_hash: prev, campaign_id: u64::MAX, slot_name: "Slot \"one\"\nnewline".into(), ironman: true, played_s: 12_345 }
}

fn theatre() -> TheatreState {
    let n = |s: &str| TheatreNodeId::new(s);
    let mut resources = Resources { salvage: 1234, intel: 7, fuel_l: 0.1 + 0.2, ..Default::default() };
    resources.ammo_kg.insert(76, 1.0 / 3.0);
    resources.munitions.insert(MunitionId::new("missile.a"), 4);
    resources.unlocked_parts.insert(PartId::new("gun.76mm.single"));
    resources.parts_store.insert(PartId::new("gun.76mm.single"), 2);
    let mut regions = BTreeMap::new();
    regions.insert(RegionId::new("region.1"), RegionState { alert: 2.75, supply: 64.5, delivered: vec![1.5, 2.0], pending_alert: vec![(40, 0.5)], queued_cards: vec![QueuedCard { card: CardId::new("card.a"), arrives_hour: 99 }], ..Default::default() });
    let mut lanes = BTreeMap::new();
    lanes.insert((n("a"), n("b")), LaneState { cut_until_hour: Some(240), mines: 3 });
    let mut tallies = BTreeMap::new();
    tallies.insert(TallyKey::Method(KillMethod::Torpedo), 3.25);
    tallies.insert(TallyKey::Range(RangeBand::Long), 1e-7);
    tallies.insert(TallyKey::Time(TimeOfDay::Night), 9.999);
    tallies.insert(TallyKey::Emission(Emcon::Silent), 0.0);
    let mut damage = HullDamage { cells: vec![CellChange { cell: crate::ids::CellIndex(u32::MAX), bucket: 8 }], jury_rigged: 2, ..Default::default() };
    damage.parts.insert(PartSlot(3), 0.125);
    let mut ammo = BTreeMap::new();
    ammo.insert(PartSlot(1), 80);
    let bp = crate::blueprint::io::new_blueprint("Kestrel", 22.0);
    let mut ops = OpsLedger { completed: vec![OperationId::new("op.a")], last_offered: Some(OperationId::new("op.b")), ..Default::default() };
    ops.offered.insert(OperationId::new("op.b"), 2);
    ops.kills_by_method.insert(KillMethod::Gunfire, 11);
    TheatreState {
        seed: u64::MAX,
        act: Act::Act2,
        hour: 1_000,
        clock_hours_left: Some(812.5),
        retreat: false,
        open_theatre: false,
        difficulty: Default::default(),
        resources,
        players: vec![PlayerRecord { client_id: 0xDEAD_BEEF_0000_0001, name: "Captain".into(), blueprint: bp, damage, fuel_fraction: 0.62, ammo, hull_lost: false, ready_at_hour: Some(1_048), veterancy: 0.05 }],
        sextant_node: n("a"),
        sextant_refit_until_hour: None,
        open_coves: vec![n("a")],
        regions,
        lanes,
        depots: [(n("b"), 37.5)].into_iter().collect(),
        hulls: vec![HullRecord { id: 1, class: ClassId::new("class.a"), variant: None, region: RegionId::new("region.1"), node: n("b"), group: Some(1), fuel: 0.9, ammunition: 1.0, damage: 0.0, veterancy: 0.0, refit_until_hour: None, alive: true, ace: Some(AceId::new("ace.1")) }],
        groups: vec![TaskGroupState { id: 1, hulls: vec![1], node: n("b"), destination: Some(n("a")), order: GroupOrder::HuntSextant, arrive_hour: Some(1_006), belief: vec![0.25, 0.75] }],
        convoys: vec![ConvoyState { id: 2, hulls: vec![], from: n("a"), to: n("b"), depart_hour: 990, arrive_hour: 1_010, cargo: 55.0, decoy: true, revealed: false }],
        tallies,
        ops,
        offered: vec![OperationId::new("op.c")],
        hunter_group: Some(1),
        last_admiral_plan_hour: 996,
        last_host_rung: 2,
        next_id: 3,
    }
}

fn save() -> SaveGame { SaveGame { header: header(4, ContentHash([9; 32])), theatre: theatre() } }

#[test]
fn save_round_trips_exactly() {
    let s = save();
    let bytes = encode_save(&s);
    assert_eq!(decode_save(&bytes).unwrap(), s);
    assert_eq!(encode_save(&s), bytes, "encoding is deterministic");
    // The hash is over the canonical text, so it is stable and content sensitive.
    let h = save_hash(&s);
    assert_eq!(h, save_hash(&decode_save(&bytes).unwrap()));
    let mut t = s.clone();
    t.theatre.hour += 1;
    assert_ne!(save_hash(&t), h);
}

#[test]
fn suspend_round_trips_binary_keyframes() {
    let keyframe: Vec<u8> = (0..70_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let s = SuspendSave { header: header(5, ContentHash::EMPTY), theatre: theatre(), sortie: SortieCheckpoint { tick: 3_600, keyframe_format: 2, config: vec![0, 255, 1], keyframe } };
    let bytes = encode_suspend(&s);
    assert_eq!(decode_suspend(&bytes).unwrap(), s);
    let empty = SuspendSave { sortie: SortieCheckpoint { tick: 0, keyframe_format: 1, config: vec![], keyframe: vec![] }, ..s };
    assert_eq!(decode_suspend(&encode_suspend(&empty)).unwrap(), empty);
}

#[test]
fn newer_format_is_refused_with_its_release() {
    let mut s = save();
    s.header.format = SAVE_FORMAT + 1;
    match decode_save(&encode_save(&s)) {
        Err(ContentError::FormatTooNew { found, supported, .. }) => assert_eq!((found, supported), (SAVE_FORMAT + 1, SAVE_FORMAT)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn damaged_bytes_are_errors_not_panics() {
    let bytes = encode_save(&save());
    assert!(decode_save(&[]).is_err());
    assert!(decode_save(b"not a save").is_err());
    assert!(decode_save(&bytes[..bytes.len() / 2]).is_err());
    let mut flipped = bytes.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x5a;
    assert!(decode_save(&flipped).is_err());
    // A suspend file is not a plain save's shape and vice versa; neither panics.
    assert!(decode_suspend(&bytes).is_err());
    // Valid frame, hostile text.
    let deep = format!("(header: (format: 1), x: {}{})", "[".repeat(200), "]".repeat(200));
    assert!(matches!(decode_save(&crate::share::compress(deep.as_bytes())), Err(ContentError::Limit(_))));
    assert!(decode_save(&crate::share::compress(&[0xff, 0xfe, 0x00])).is_err());
}

#[test]
fn reconcile_rules() {
    let a = save();
    let ha = save_hash(&a);
    let next = header(a.header.sequence + 1, ha);
    let hn = ContentHash([1; 32]);
    assert_eq!(reconcile(&a.header, ha, &a.header, ha), Reconcile::Same);
    assert_eq!(reconcile(&a.header, ha, &next, hn), Reconcile::TakeTheirs);
    assert_eq!(reconcile(&next, hn, &a.header, ha), Reconcile::TakeMine);
    // Same sequence, different content: two hosts saved over each other.
    assert_eq!(reconcile(&a.header, ha, &a.header, hn), Reconcile::Forked);
    // The next save does not chain from mine.
    let stray = header(a.header.sequence + 1, ContentHash([2; 32]));
    assert_eq!(reconcile(&a.header, ha, &stray, hn), Reconcile::Forked);
    assert_eq!(reconcile(&stray, hn, &a.header, ha), Reconcile::Forked);
    // A member who missed several saves takes the newest copy.
    let later = header(a.header.sequence + 3, ContentHash([3; 32]));
    assert_eq!(reconcile(&a.header, ha, &later, hn), Reconcile::TakeTheirs);
    // Another campaign is never merged.
    let mut other = next.clone();
    other.campaign_id = 1;
    assert_eq!(reconcile(&a.header, ha, &other, hn), Reconcile::Forked);
    // Equal hashes at different sequences cannot happen honestly.
    assert_eq!(reconcile(&a.header, ha, &next, ha), Reconcile::Forked);
}

#[test]
fn non_finite_numbers_survive_a_round_trip() {
    let mut s = save();
    s.theatre.players[0].fuel_fraction = f64::NAN;
    s.theatre.clock_hours_left = Some(f64::INFINITY);
    s.theatre.resources.fuel_l = f64::NEG_INFINITY;
    let back = decode_save(&encode_save(&s)).unwrap();
    assert!(back.theatre.players[0].fuel_fraction.is_nan());
    assert_eq!(back.theatre.clock_hours_left, Some(f64::INFINITY));
    assert_eq!(back.theatre.resources.fuel_l, f64::NEG_INFINITY);
}
