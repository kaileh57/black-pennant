use super::*;

fn h(n: u8) -> ContentHash { ContentHash([n; 32]) }

fn drain(out: &mut Outgoing, inc: &mut Incoming, budget: usize, calls: usize) -> (Vec<Completed>, usize) {
    let mut done = Vec::new();
    let mut sent = 0;
    for _ in 0..calls {
        for m in out.take(budget) {
            if let BulkMsg::Chunk { bytes, .. } = &m { sent += bytes.len(); }
            if let Some(c) = inc.on_msg(m).unwrap() { done.push(c); }
        }
    }
    (done, sent)
}

#[test]
fn round_trip_in_order_and_progress() {
    let mut out = Outgoing::new();
    let a: Vec<u8> = (0..40_000u32).map(|i| (i * 7) as u8).collect();
    let b = vec![9u8; 100];
    let sa = out.enqueue(TransferKind::Checkpoint(60), a.clone());
    let sb = out.enqueue(TransferKind::Blueprint(h(3)), b.clone());
    assert_eq!(sa.chunks, 3);
    assert_eq!(sb.id, sa.id + 1);
    assert_eq!(out.progress(sa.id), Some(0.0));
    let mut inc = Incoming::new();
    let first = out.take(CHUNK_BYTES + 200);
    assert!(matches!(first[0], BulkMsg::Start(_)));
    assert_eq!(first.len(), 2, "start plus one whole chunk");
    for m in first { assert_eq!(inc.on_msg(m), Ok(None)); }
    assert!((out.progress(sa.id).unwrap() - 1.0 / 3.0).abs() < 1e-6);
    assert!((inc.progress(TransferKind::Checkpoint(60)).unwrap() - 1.0 / 3.0).abs() < 1e-6);
    let (done, _) = drain(&mut out, &mut inc, 1 << 20, 1);
    assert_eq!(done, vec![Completed { kind: TransferKind::Checkpoint(60), bytes: a }, Completed { kind: TransferKind::Blueprint(h(3)), bytes: b }]);
    assert!(out.is_idle());
    assert_eq!(out.progress(sa.id), None);
}

#[test]
fn small_live_budget_carries_over_at_its_average_rate() {
    // 128 kbps at 30 Hz, 25% live: about 133 bytes per interval, far below a chunk.
    let budget = bulk_budget(128_000 / 8 / 30, true);
    assert_eq!(budget, 133);
    let mut out = Outgoing::new();
    out.enqueue(TransferKind::Replay, vec![1; CHUNK_BYTES * 2]);
    let mut inc = Incoming::new();
    let calls = 400;
    let (done, sent) = drain(&mut out, &mut inc, budget, calls);
    assert_eq!(done.len(), 1);
    assert_eq!(sent, CHUNK_BYTES * 2, "both chunks out within {calls} calls");
    // Never above the budget over the run.
    assert!(sent <= budget * calls);
    // Idle resets the credit: no burst after a pause.
    for _ in 0..1000 { out.take(budget); }
    out.enqueue(TransferKind::Replay, vec![2; CHUNK_BYTES]);
    let burst: usize = out.take(budget).iter().map(|m| if let BulkMsg::Chunk { bytes, .. } = m { bytes.len() } else { 0 }).sum();
    assert_eq!(burst, 0);
}

#[test]
fn empty_transfer_completes_on_start() {
    let mut out = Outgoing::new();
    let s = out.enqueue(TransferKind::SaveCopy(4), Vec::new());
    assert_eq!(s.chunks, 0);
    let mut inc = Incoming::new();
    let msgs = out.take(1000);
    assert_eq!(msgs.len(), 1);
    assert_eq!(inc.on_msg(msgs[0].clone()), Ok(Some(Completed { kind: TransferKind::SaveCopy(4), bytes: Vec::new() })));
}

#[test]
fn incoming_refuses_bad_input() {
    let mut inc = Incoming::new();
    let hash = content::blueprint::io::blake3_hash(&[1, 2, 3]);
    let big = TransferStart { id: 1, kind: TransferKind::Blueprint(h(1)), total_bytes: content::blueprint::MAX_COMPRESSED_BYTES as u32 + 1, chunks: 513, hash };
    assert_eq!(inc.on_msg(BulkMsg::Start(big)), Err(IncomingError::TooLarge));
    let lie = TransferStart { id: 2, kind: TransferKind::Replay, total_bytes: 3, chunks: 5, hash };
    assert_eq!(inc.on_msg(BulkMsg::Start(lie)), Err(IncomingError::BadChunk));
    assert_eq!(inc.on_msg(BulkMsg::Chunk { id: 77, index: 0, bytes: vec![1] }), Err(IncomingError::Unknown));
    let ok = TransferStart { id: 3, kind: TransferKind::Replay, total_bytes: 3, chunks: 1, hash };
    inc.on_msg(BulkMsg::Start(ok)).unwrap();
    assert_eq!(inc.on_msg(BulkMsg::Chunk { id: 3, index: 0, bytes: vec![1, 2] }), Err(IncomingError::BadChunk));
    assert!(inc.partial.is_empty(), "a failed transfer is dropped");
    inc.on_msg(BulkMsg::Start(ok)).unwrap();
    assert_eq!(inc.on_msg(BulkMsg::Chunk { id: 3, index: 0, bytes: vec![1, 2, 4] }), Err(IncomingError::HashMismatch));
    inc.on_msg(BulkMsg::Start(ok)).unwrap();
    assert_eq!(inc.on_msg(BulkMsg::Cancel { id: 3 }), Ok(None));
    assert!(inc.progress(TransferKind::Replay).is_none());
    for id in 10..10 + MAX_PARTIAL_TRANSFERS as u32 { inc.on_msg(BulkMsg::Start(TransferStart { id, ..ok })).unwrap(); }
    assert_eq!(inc.on_msg(BulkMsg::Start(TransferStart { id: 99, ..ok })), Err(IncomingError::TooLarge));
}

#[test]
fn accept_upload_refuses_oversize_and_garbage() {
    let cat = content::catalogue::Catalogue::default();
    let rules = Ruleset::default();
    let big = PackedBlueprint { hash: h(1), compressed: vec![0; content::blueprint::MAX_COMPRESSED_BYTES + 1] };
    assert_eq!(accept_upload(&big, &cat, &rules), Err(BlueprintRefusal::TooLarge));
    let junk = PackedBlueprint { hash: h(1), compressed: vec![1, 2, 3, 4] };
    assert_eq!(accept_upload(&junk, &cat, &rules), Err(BlueprintRefusal::Invalid));
    assert_eq!(unpack_exterior(&junk), Err(BlueprintRefusal::Invalid));
}

#[test]
fn exterior_pack_round_trip() {
    let cat = content::catalogue::catalogue();
    let bp = content::blueprint::io::new_blueprint("Test", 40.0);
    let (ext, packed) = pack_exterior(&bp, cat);
    assert_eq!(packed.hash, content::blueprint::io::exterior_hash(&ext));
    assert_eq!(unpack_exterior(&packed), Ok(ext));
    let mut bad = packed.clone();
    bad.hash = h(9);
    assert_eq!(unpack_exterior(&bad), Err(BlueprintRefusal::Invalid));
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bp-net-cache-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn disk_cache_lru_and_reopen() {
    let dir = temp_dir("lru");
    let mut c = DiskCache::open(dir.clone(), 250).unwrap();
    c.put(ContentHash::EMPTY, vec![0; 10]);
    assert_eq!(c.total_bytes(), 0);
    c.put(h(1), vec![1; 100]);
    c.put(h(2), vec![2; 100]);
    assert_eq!(c.get(&h(1)), Some(vec![1; 100]), "1 is now most recent");
    c.put(h(3), vec![3; 100]);
    assert!(!c.contains(&h(2)), "least recently used goes first");
    assert!(c.contains(&h(1)) && c.contains(&h(3)));
    c.put(h(4), vec![4; 300]);
    assert!(!c.contains(&h(4)), "larger than the whole budget");
    std::fs::write(dir.join("not-a-hash"), b"x").unwrap();
    // Reopen: order comes from modified times (set explicitly, no sleeping).
    let t = std::time::SystemTime::now();
    let set = |n: u8, age: u64| std::fs::File::options().write(true).open(dir.join(h(n).to_hex())).unwrap().set_modified(t - std::time::Duration::from_secs(age)).unwrap();
    set(1, 100);
    set(3, 50);
    let mut c = DiskCache::open(dir.clone(), 150).unwrap();
    assert!(!c.contains(&h(1)) && c.contains(&h(3)));
    assert_eq!(c.total_bytes(), 100);
    // A file deleted behind the cache's back reads as a miss.
    std::fs::remove_file(dir.join(h(3).to_hex())).unwrap();
    assert_eq!(c.get(&h(3)), None);
    assert!(!c.contains(&h(3)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entitlements_per_client() {
    let mut e = Entitlements::default();
    e.grant(1, h(1));
    assert!(e.allowed(1, &h(1)) && !e.allowed(2, &h(1)) && !e.allowed(1, &h(2)));
    e.revoke_client(1);
    assert!(!e.allowed(1, &h(1)));
}
