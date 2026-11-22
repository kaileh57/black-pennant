use super::*;
use content::cell::CellChange;
use content::ids::{CellIndex, SideId};
use sim_systems::ids::TrackId;

fn ent(i: u32) -> EntityId { EntityId { index: i, generation: 1 } }
fn intro(e: EntityId) -> EntityIntro {
    EntityIntro { entity: e, side: SideId(1), blueprint: content::ids::ContentHash([9; 32]), exterior_only: false, class: None, name: String::new(), player: None, status: Default::default(), damage: vec![CellChange { cell: CellIndex(1), bucket: 1 }], removed: Vec::new(), parent: None, track: TrackId(4) }
}
fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }
fn snap(tick: u64) -> Arc<Snapshot> { Arc::new(Snapshot { tick, ..Default::default() }) }

#[test]
fn the_ring_keeps_twelve_seconds_and_folds_what_ages_out() {
    let mut r = ClipRecorder::default();
    r.record_events(&[ev(0, SimEvent::SortieStarted { seed: 5, tick_hz: 60 }), ev(0, SimEvent::EntityRevealed(Box::new(intro(ent(3)))))]);
    r.record_events(&[ev(100, SimEvent::Phys(PhysEvent::CellsDamaged { vessel: ent(3), changes: vec![CellChange { cell: CellIndex(1), bucket: 3 }, CellChange { cell: CellIndex(2), bucket: 2 }], cause: sim_core::damage::DamageCause::Shell, attacker: EntityId::NONE }))]);
    for t in (0..2_000).step_by(2) { r.record_frame(t as f64, &snap(t)); }
    // At 15 Hz of render ticks, over the last KEEP_TICKS.
    let n = r.frames.len() as u64;
    assert!((KEEP_TICKS / 4..=KEEP_TICKS / 4 + 1).contains(&n), "{n} frames");
    let newest = r.frames.back().map_or(0.0, |f| f.0);
    assert!(r.frames.front().is_some_and(|f| f.0 >= newest - KEEP_TICKS as f64));
    // The damage event aged out and lives on in the base.
    assert!(r.events.is_empty());
    let f = r.base.entities.get(&ent(3)).expect("folded");
    assert_eq!(f.damage.get(&1), Some(&3));
    assert_eq!(f.damage.get(&2), Some(&2));
    // A clip opens like a seek: the original start, then the reveal with its damage so far.
    let clip = r.clip(1_500, 1_900, crate::session::testkit::catalogue()).expect("the window is held");
    assert!(matches!(clip.opening.first().map(|e| &e.event), Some(SimEvent::SortieStarted { seed: 5, .. })));
    let reveal = clip.opening.iter().find_map(|e| if let SimEvent::EntityRevealed(i) = &e.event { Some(i.clone()) } else { None }).expect("revealed");
    assert_eq!(reveal.damage, vec![CellChange { cell: CellIndex(1), bucket: 3 }, CellChange { cell: CellIndex(2), bucket: 2 }]);
    assert!(clip.frames.front().is_some_and(|f| f.0 >= 1_500.0 - LEAD_TICKS));
    assert!(clip.frames.back().is_some_and(|f| f.0 <= 1_900.0));
    assert!(r.clip(10, 20, crate::session::testkit::catalogue()).is_none(), "long gone");
    assert!(r.clip(20, 10, crate::session::testkit::catalogue()).is_none());
}

#[test]
fn hidden_and_lost_entities_leave_the_opening() {
    let mut r = ClipRecorder::default();
    r.record_events(&[ev(0, SimEvent::SortieStarted { seed: 1, tick_hz: 60 })]);
    r.record_events(&[ev(1, SimEvent::EntityRevealed(Box::new(intro(ent(1))))), ev(1, SimEvent::EntityRevealed(Box::new(intro(ent(2)))))]);
    r.record_events(&[ev(5, SimEvent::EntityHidden { entity: ent(1) }), ev(6, SimEvent::Phys(PhysEvent::Split { vessel: ent(2), piece: ent(9), cells: vec![CellIndex(7)] }))]);
    r.record_frame(6.0, &snap(6));
    let live = r.live_opening();
    let reveals: Vec<EntityIntro> = live.iter().filter_map(|e| if let SimEvent::EntityRevealed(i) = &e.event { Some((**i).clone()) } else { None }).collect();
    assert_eq!(reveals.len(), 1);
    assert_eq!(reveals[0].entity, ent(2));
    assert_eq!(reveals[0].removed, vec![CellIndex(7)]);
    // A new sortie starts a new record.
    r.record_events(&[ev(0, SimEvent::SortieStarted { seed: 2, tick_hz: 60 })]);
    assert!(r.live_opening().iter().all(|e| !matches!(e.event, SimEvent::EntityRevealed(_))));
}
