use super::*;

fn ids(t: &EntityTable) -> Vec<(u32, u32)> { t.iter().map(|e| (e.index, e.generation)).collect() }

/// The scripted spawn and despawn sequence of L1112.
fn scripted() -> EntityTable {
    let mut t = EntityTable::new();
    let a = t.spawn(EntityKind::Vessel); // 0
    let b = t.spawn(EntityKind::Vessel); // 1
    let c = t.spawn(EntityKind::Body); // 2
    let d = t.spawn(EntityKind::Decoy); // 3
    assert!(t.despawn(b));
    assert!(t.despawn(d));
    let e = t.spawn(EntityKind::Wreck); // reuses 3 (LIFO)
    let f = t.spawn(EntityKind::Body); // reuses 1
    let g = t.spawn(EntityKind::Body); // 4
    assert!(t.despawn(a));
    assert!(!t.despawn(a));
    assert!(!t.despawn(b));
    let h = t.spawn(EntityKind::Vessel); // reuses 0
    let _ = (c, e, f, g, h);
    t
}

#[test]
fn scripted_sequence_order_and_hash() {
    let t = scripted();
    assert_eq!(ids(&t), vec![(0, 2), (1, 2), (2, 1), (3, 2), (4, 1)]);
    assert_eq!(t.iter_kind(EntityKind::Body).map(|e| e.index).collect::<Vec<_>>(), vec![1, 2, 4]);
    assert_eq!(t.iter_kind(EntityKind::Wreck).map(|e| e.index).collect::<Vec<_>>(), vec![3]);
    assert_eq!(t.len(), 5);
    let h = iteration_order_hash(&t);
    println!("{h:#x}");
    assert_eq!(h, ORDER_HASH);
    assert_eq!(iteration_order_hash(&scripted()), h);
}

const ORDER_HASH: u64 = 16_058_976_651_034_681_735;

#[test]
fn stale_ids_and_defaults() {
    let mut t = EntityTable::new();
    let a = t.spawn(EntityKind::Vessel);
    assert!(!t.is_alive(EntityId::default()));
    assert!(!t.is_alive(EntityId::NONE));
    assert!(t.despawn(a));
    let b = t.spawn(EntityKind::Wreck);
    assert_eq!(b.index, a.index);
    assert!(!t.is_alive(a));
    assert_eq!(t.kind(a), None);
    assert_eq!(t.kind(b), Some(EntityKind::Wreck));
    assert_eq!(t.id_at(0), Some(b));
    assert_eq!(t.id_at(99), None);
    assert_eq!(EntityId::from_u64(b.to_u64()), b);
}

#[test]
fn corrupt_table_does_not_panic() {
    let mut t = EntityTable::new();
    t.free = vec![7, 0, 99];
    t.generations = vec![1];
    let a = t.spawn(EntityKind::Vessel);
    assert_eq!(a.index, 0);
    let b = t.spawn(EntityKind::Vessel);
    assert_eq!(b.index, 1);
    assert_eq!(t.iter().count(), 2);
    let mut dropped = EntityTable::new();
    dropped.alive.set(5, true);
    assert_eq!(dropped.iter().count(), 0);
}

#[test]
fn generation_wrap_retires_slot() {
    let mut t = EntityTable::new();
    let a = t.spawn(EntityKind::Body);
    t.generations[0] = u32::MAX;
    let a = EntityId { generation: u32::MAX, ..a };
    assert!(t.despawn(a));
    let b = t.spawn(EntityKind::Body);
    assert_eq!(b.index, 1);
    assert_eq!(t.id_at(0), None);
}

#[test]
fn column_basics() {
    let mut t = EntityTable::new();
    let ids: Vec<_> = (0..5).map(|_| t.spawn(EntityKind::Vessel)).collect();
    let mut c: Column<u32> = Column::new();
    c.insert(ids[3], 30);
    c.insert(ids[1], 10);
    assert_eq!(c.insert(ids[1], 11), Some(10));
    assert_eq!(c.iter().collect::<Vec<_>>(), vec![(1, &11), (3, &30)]);
    for (_, v) in c.values_mut() { *v += 1; }
    assert_eq!(c.get(ids[3]), Some(&31));
    assert_eq!(c.remove(ids[1]), Some(12));
    assert_eq!(c.remove(ids[1]), None);
    assert!(!c.contains(ids[1]));
    assert_eq!(c.len(), 1);
    assert_eq!(c.insert(EntityId::NONE, 5), None);
    assert_eq!(c.insert(EntityId { index: u32::MAX - 1, generation: 1 }, 5), None);
    assert_eq!(c.get(EntityId { index: 1_000_000, generation: 1 }), None);
    assert_eq!(c.len(), 1);
}
