//! Part of unit `gd-render-hull`, used by `gd-render-parts` too. Damage is drawn at the playback tick, like
//! the poses, the tracers and the effects.
//!
//! Events reach the renderer when they arrive, which is the interpolation delay (100 to 150 ms) ahead of
//! the tick `ViewFrame` is sampled at. A hole meshed on arrival would open six or more frames before the
//! shell that made it lands and before `gd-render-vfx` shows the burst. So the events that change a hull at
//! a moment (cell damage, debris, breaches, splits, part damage, fires) wait here until playback is within
//! `lead_ticks` of their tick; the hull unit asks for a lead of [`MESH_LEAD_TICKS`], which covers its
//! meshing and upload, the part unit for none.
//!
//! Everything else (reveals, hides, despawns, a new sortie) is state, and is applied the frame it arrives.
//!
//! # Rules
//! - Nothing held is dropped for want of time: a hull that missed a hole would stay wrong for the rest of
//!   the sortie. An event more than [`HOLD_MAX_TICKS`] ahead of playback (a seek, a stalled clock) is
//!   applied at once, as are the oldest when more than [`HOLD_MAX_EVENTS`] wait.
//! - Events are applied in arrival order among those due. Over a network, cell damage, debris and splits
//!   share one ordered stream with reveals and hides (`net::Channel::Entity`) and the rest come on another
//!   (`net::Channel::Events`), so ticks ascend within each kind but not across the two; an event is taken
//!   when its own tick is due, whatever waits ahead of it.
//! - A reveal carries the vessel's cell damage so far, and it arrives after every cell event emitted
//!   before it, so that vessel's held cell damage, debris and splits up to the reveal's tick are let go.
//!   Its breaches, fires and part levels stay: a reveal carries none of them, and they travel on the other
//!   stream. A hide or a despawn lets go of everything held for the vessel up to its tick, and a new sortie
//!   of everything. (A split let go with its parent loses nothing: the piece has its own reveal.)
//! - Every event held is one the units apply idempotently, so a replay that steps back a little and
//!   delivers an event twice changes nothing.
use crate::seams::WireEvent;
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::protocol::SimEvent;

/// An event more than this many ticks ahead of playback is applied at once.
pub(crate) const HOLD_MAX_TICKS: f64 = 0.5 * sim_core::TICK_HZ as f64;
/// Most events held; past it the oldest are applied early.
pub(crate) const HOLD_MAX_EVENTS: usize = 8_192;
/// How far ahead of playback the hull unit takes a cell change: two ticks, which is the two frames a
/// remesh takes to reach the screen inside 1 km (L1945).
pub(crate) const MESH_LEAD_TICKS: f64 = 2.0;

/// The vessel an event changes at its tick, or `None` for an event that is applied on arrival.
pub(crate) fn timed_vessel(event: &SimEvent) -> Option<EntityId> {
    match event {
        SimEvent::Phys(
            PhysEvent::CellsDamaged { vessel, .. }
            | PhysEvent::Debris { vessel, .. }
            | PhysEvent::Breach { vessel, .. }
            | PhysEvent::Split { vessel, .. }
            | PhysEvent::PartDamaged { vessel, .. }
            | PhysEvent::FireStarted { vessel, .. }
            | PhysEvent::FireOut { vessel, .. },
        ) => Some(*vessel),
        _ => None,
    }
}

/// Cell damage, debris and splits: the timed events a reveal's damage list already accounts for.
fn in_reveal(event: &SimEvent) -> bool {
    matches!(event, SimEvent::Phys(PhysEvent::CellsDamaged { .. } | PhysEvent::Debris { .. } | PhysEvent::Split { .. }))
}

/// Timed events waiting for playback, in arrival order.
#[derive(Default)]
pub(crate) struct EventHold { queue: Vec<WireEvent>, scratch: Vec<WireEvent> }

impl EventHold {
    pub(crate) fn len(&self) -> usize { self.queue.len() }

    /// Let go of what is held for `entity` up to `tick`; `reveal` keeps what a reveal does not carry.
    fn forget(&mut self, entity: EntityId, tick: u64, reveal: bool) {
        self.queue.retain(|e| timed_vessel(&e.event) != Some(entity) || e.tick > tick || (reveal && !in_reveal(&e.event)));
    }

    /// Take this frame's events and hand `apply` every event that is due, in order: state events as they
    /// come, timed events once `render_tick` is within `lead_ticks` of their tick.
    pub(crate) fn step(&mut self, events: &[WireEvent], render_tick: f64, lead_ticks: f64, apply: &mut dyn FnMut(&WireEvent)) {
        for ev in events {
            if timed_vessel(&ev.event).is_some() {
                self.queue.push(ev.clone());
                continue;
            }
            match &ev.event {
                SimEvent::SortieStarted { .. } => self.queue.clear(),
                SimEvent::EntityRevealed(intro) => self.forget(intro.entity, ev.tick, true),
                SimEvent::EntityHidden { entity } => self.forget(*entity, ev.tick, false),
                SimEvent::Phys(PhysEvent::VesselDespawned { vessel }) => self.forget(*vessel, ev.tick, false),
                SimEvent::Phys(PhysEvent::WreckDespawned { wreck }) => self.forget(*wreck, ev.tick, false),
                _ => {}
            }
            apply(ev);
        }
        if self.queue.is_empty() { return; }
        let over = self.queue.len().saturating_sub(HOLD_MAX_EVENTS);
        std::mem::swap(&mut self.queue, &mut self.scratch);
        for (k, ev) in self.scratch.drain(..).enumerate() {
            let ahead = ev.tick as f64 - render_tick;
            // Written so that a NaN playback tick holds nothing.
            if k >= over && ahead > lead_ticks && ahead <= HOLD_MAX_TICKS { self.queue.push(ev); } else { apply(&ev); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::cell::CellChange;
    use content::enums::DamageLevel;
    use content::ids::{CellIndex, ContentHash, PartSlot};
    use sim_core::damage::DamageCause;
    use sim_systems::protocol::EntityIntro;

    fn id(n: u64) -> EntityId { EntityId::from_u64(n) }
    fn hit(tick: u64, vessel: u64, cell: u32) -> WireEvent {
        WireEvent { tick, event: SimEvent::Phys(PhysEvent::CellsDamaged { vessel: id(vessel), changes: vec![CellChange { cell: CellIndex(cell), bucket: 8 }], cause: DamageCause::Shell, attacker: EntityId::NONE }) }
    }
    fn part(tick: u64, vessel: u64) -> WireEvent {
        WireEvent { tick, event: SimEvent::Phys(PhysEvent::PartDamaged { vessel: id(vessel), slot: PartSlot(0), level: DamageLevel::Heavy }) }
    }
    fn reveal(tick: u64, entity: u64) -> WireEvent {
        WireEvent { tick, event: SimEvent::EntityRevealed(Box::new(EntityIntro { entity: id(entity), side: Default::default(), blueprint: ContentHash([1; 32]), exterior_only: false, class: None, name: String::new(), player: None, status: Default::default(), damage: Vec::new(), removed: Vec::new(), parent: None, track: Default::default() })) }
    }
    fn breach(tick: u64, vessel: u64) -> WireEvent {
        WireEvent { tick, event: SimEvent::Phys(PhysEvent::Breach { vessel: id(vessel), compartment: content::ids::CompartmentId(0), below_waterline: true }) }
    }
    fn hidden(tick: u64, entity: u64) -> WireEvent { WireEvent { tick, event: SimEvent::EntityHidden { entity: id(entity) } } }
    /// The ticks of the events applied by one step.
    fn step(hold: &mut EventHold, events: &[WireEvent], render_tick: f64, lead: f64) -> Vec<u64> {
        let mut seen = Vec::new();
        hold.step(events, render_tick, lead, &mut |e| seen.push(e.tick));
        seen
    }

    #[test]
    fn timed_events_wait_for_playback_and_keep_their_order() {
        let mut hold = EventHold::default();
        // Playback at tick 100; the hits are 6 and 8 ticks ahead, the reveal is state.
        assert_eq!(step(&mut hold, &[hit(106, 1, 0), reveal(107, 2), hit(108, 1, 1)], 100.0, 0.0), vec![107]);
        assert_eq!(hold.len(), 2);
        assert_eq!(step(&mut hold, &[], 105.9, 0.0), Vec::<u64>::new());
        assert_eq!(step(&mut hold, &[], 106.0, 0.0), vec![106]);
        // A lead applies an event that many ticks early.
        assert_eq!(step(&mut hold, &[], 106.5, 2.0), vec![108]);
        // An event at or behind playback is applied the frame it arrives, after the state events of that
        // frame and in arrival order with what was held.
        assert_eq!(step(&mut hold, &[hit(120, 1, 2)], 110.0, 0.0), Vec::<u64>::new());
        assert_eq!(step(&mut hold, &[hit(121, 1, 3), reveal(121, 3)], 125.0, 0.0), vec![121, 120, 121]);
        assert_eq!(hold.len(), 0);
    }

    #[test]
    fn nothing_is_held_past_its_limits() {
        let mut hold = EventHold::default();
        // More than half a second ahead (a seek): applied at once.
        assert_eq!(step(&mut hold, &[hit(1_000, 1, 0)], 100.0, 0.0), vec![1_000]);
        // A playback tick that is not a number holds nothing.
        assert_eq!(step(&mut hold, &[hit(110, 1, 0)], f64::NAN, 0.0), vec![110]);
        // A stepped-back clock keeps what it holds until playback reaches it again.
        assert_eq!(step(&mut hold, &[hit(110, 1, 0)], 105.0, 0.0), Vec::<u64>::new());
        assert_eq!(step(&mut hold, &[], 95.0, 0.0), Vec::<u64>::new());
        assert_eq!(step(&mut hold, &[], 110.0, 0.0), vec![110]);
        // Past the cap the oldest go early, and none is lost.
        let flood: Vec<WireEvent> = (0..HOLD_MAX_EVENTS as u32 + 10).map(|k| hit(110, 1, k)).collect();
        assert_eq!(step(&mut hold, &flood, 100.0, 0.0).len(), 10);
        assert_eq!(hold.len(), HOLD_MAX_EVENTS);
        assert_eq!(step(&mut hold, &[], 110.0, 0.0).len(), HOLD_MAX_EVENTS);
    }

    #[test]
    fn a_reveal_supersedes_held_cell_damage_and_a_hide_everything() {
        let mut hold = EventHold::default();
        step(&mut hold, &[hit(110, 1, 0), part(110, 1), breach(109, 1), hit(110, 2, 0), part(111, 2)], 100.0, 0.0);
        // Vessel 1 is revealed again: its cell damage is in the reveal, its part level and breach are not.
        assert_eq!(step(&mut hold, &[reveal(110, 1)], 101.0, 0.0), vec![110]);
        assert_eq!(hold.len(), 4);
        // Vessel 2 is lost from sight: nothing held for it is drawn.
        assert_eq!(step(&mut hold, &[hidden(111, 2)], 102.0, 0.0), vec![111]);
        assert_eq!(hold.len(), 2);
        assert_eq!(step(&mut hold, &[], 110.0, 0.0), vec![110, 109]);
        // A hit ticked after the reveal that overtakes it is kept: the reveal does not hold it.
        step(&mut hold, &[hit(125, 1, 4)], 120.0, 0.0);
        assert_eq!(step(&mut hold, &[reveal(124, 1), hidden(124, 3)], 120.0, 0.0), vec![124, 124]);
        assert_eq!(step(&mut hold, &[], 125.0, 0.0), vec![125]);
        // A new sortie starts clean.
        step(&mut hold, &[hit(130, 1, 0)], 126.0, 0.0);
        step(&mut hold, &[WireEvent { tick: 0, event: SimEvent::SortieStarted { seed: 0, tick_hz: 60 } }], 126.0, 0.0);
        assert_eq!(hold.len(), 0);
    }

    #[test]
    fn the_two_streams_do_not_wait_for_each_other() {
        let mut hold = EventHold::default();
        // A hit for tick 108 arrives before a breach for tick 103 (another stream, resent late).
        step(&mut hold, &[hit(108, 1, 0), breach(103, 1)], 100.0, 0.0);
        assert_eq!(step(&mut hold, &[], 103.0, 0.0), vec![103]);
        assert_eq!(step(&mut hold, &[], 108.0, 0.0), vec![108]);
    }
}
