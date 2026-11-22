//! Unit `net-events`. Event routing from the simulation's out queue to clients. DESIGN.md L924 (each client
//! receives only what its side may know), L920 (damage on a reliable sequenced stream), L926 (projectile spawn
//! and impact events), L929-935 (projectile events at most 25% of client bandwidth; fallback 2 batches spawns
//! per salvo), L945 (the simulation already emits exterior-only damage to the other PvP team), L1049 (PvP
//! replays and kill cam are cut elsewhere: `sim_systems::replay::record::cut_for_side`).
//!
//! The simulation stamps every `OutEvent` with its `Audience`; this module only applies
//! `Audience::admits(player, side)` per recipient (never sending `Audience::Host`), maps lanes to channels
//! ([`crate::channel::Channel::for_lane`], with `EntityRevealed` and `EntityHidden` on `Channel::Entity`), and
//! keeps emission order within each channel. It never inspects or alters event content except for the
//! salvo-batching fallback, which is off unless the bandwidth gate fails.
use crate::channel::Channel;
use crate::msg::{EventBatch, WireEvent};
use content::ids::{PlayerSlot, SideId};
use sim_systems::protocol::event::{Audience, OutEvent, SimEvent};

/// One connected player the router delivers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipient { pub player: PlayerSlot, pub side: SideId }

/// Per-recipient batches, one per server-to-client event channel; empty batches are not sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Routed { pub events: EventBatch, pub entity: EventBatch, pub projectile: EventBatch }

impl Routed {
    pub fn batch(&self, ch: Channel) -> Option<&EventBatch> { match ch { Channel::Events => Some(&self.events), Channel::Entity => Some(&self.entity), Channel::Projectile => Some(&self.projectile), _ => None } }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouterOptions {
    /// Fallback 2 of L935: one spawn event per salvo carrying per-round seeds. Requires a sim-core salvo
    /// dispersion function shared by server and client; not scheduled unless the gate fails.
    pub batch_salvos: bool,
}

/// Route one drain of events to every recipient, in recipient order. Unit: `net-events`.
///
/// `options.batch_salvos` is accepted but has no effect yet: per-salvo spawn events need a salvo wire event
/// and a sim-core dispersion function shared by server and client (L935 fallback 2), neither of which
/// exists. Spawns are already delta-coded per salvo by `schema::codec::encode_events`.
pub fn route(events: &[OutEvent], recipients: &[Recipient], options: RouterOptions) -> Vec<Routed> {
    let _ = options.batch_salvos;
    let mut out = vec![Routed::default(); recipients.len()];
    for e in events {
        // Host-only events never leave the server, whoever the recipient is.
        if matches!(e.audience, Audience::Host) { continue; }
        let ch = channel_for(e);
        let mut wire: Option<WireEvent> = None;
        for (r, routed) in recipients.iter().zip(out.iter_mut()) {
            if !e.audience.admits(r.player, r.side) { continue; }
            let w = wire.get_or_insert_with(|| to_wire(e)).clone();
            match ch {
                Channel::Entity => routed.entity.events.push(w),
                Channel::Projectile => routed.projectile.events.push(w),
                _ => routed.events.events.push(w),
            }
        }
    }
    out
}

/// Channel for one event: its lane's channel, except interest events which ride with damage. Unit: `net-events`.
pub fn channel_for(event: &OutEvent) -> Channel {
    match event.event {
        SimEvent::EntityRevealed(_) | SimEvent::EntityHidden { .. } => Channel::Entity,
        _ => Channel::for_lane(event.lane),
    }
}

/// Strip the audience. Real code.
pub fn to_wire(e: &OutEvent) -> WireEvent { WireEvent { tick: e.tick, event: e.event.clone() } }

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::EntityId;
    use sim_systems::ids::{PlayerMask, SideMask};
    use sim_systems::protocol::event::Lane;

    fn ev(tick: u64, audience: Audience, lane: Lane, event: SimEvent) -> OutEvent { OutEvent { tick, audience, lane, event } }

    #[test]
    fn routes_by_audience_lane_and_order() {
        let a = Recipient { player: PlayerSlot(0), side: SideId(0) };
        let b = Recipient { player: PlayerSlot(1), side: SideId(1) };
        let hide = |i| SimEvent::EntityHidden { entity: EntityId { index: i, generation: 1 } };
        let events = vec![
            ev(1, Audience::All, Lane::Reliable, SimEvent::SortieStarted { seed: 1, tick_hz: 60 }),
            ev(2, Audience::Host, Lane::Reliable, SimEvent::EnemyAlert { side: SideId(1), level: 2 }),
            ev(3, Audience::Sides(SideMask::only(SideId(1))), Lane::Reliable, hide(5)),
            ev(4, Audience::Players(PlayerMask(1)), Lane::Damage, hide(6)),
            ev(5, Audience::All, Lane::Projectile, hide(7)),
            ev(6, Audience::All, Lane::Log, SimEvent::SortieStarted { seed: 2, tick_hz: 60 }),
            ev(7, Audience::side(SideId(0)), Lane::Damage, SimEvent::PlayerDropped { player: PlayerSlot(1), officers_in_command: true }),
        ];
        let out = route(&events, &[a, b], RouterOptions::default());
        assert_eq!(out.len(), 2);
        let ticks = |b: &EventBatch| b.events.iter().map(|e| e.tick).collect::<Vec<_>>();
        // Interest events ride the Entity channel whatever their lane; damage too.
        assert_eq!(ticks(&out[0].events), vec![1, 6]);
        assert_eq!(ticks(&out[0].entity), vec![4, 5, 7]);
        assert!(out[0].projectile.events.is_empty());
        assert_eq!(ticks(&out[1].events), vec![1, 6]);
        assert_eq!(ticks(&out[1].entity), vec![3, 5]);
        assert_eq!(channel_for(&events[0]), Channel::Events);
        assert_eq!(channel_for(&ev(0, Audience::All, Lane::Projectile, SimEvent::SortieStarted { seed: 0, tick_hz: 60 })), Channel::Projectile);
        assert!(route(&events, &[], RouterOptions::default()).is_empty());
    }
}
