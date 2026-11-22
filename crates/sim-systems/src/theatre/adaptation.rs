//! Unit `sys-theatre-adaptation`. Doctrine adaptation: kill tallies and counter cards.
//! DESIGN.md L772, L888-889 (gates).
//!
//! About 30 authored doctrine cards. Each kill adds 1 x the adaptation cadence slider to four tallies: kill
//! method, engagement-range band, time-of-day band and emission state at the kill. Tallies decay
//! exponentially with a half-life of 120 theatre hours. When a tally reaches 10 the matching counter card
//! goes on the doctrine bulletin with its arrival 72 theatre hours ahead. During that window the player may
//! spend Intel at the chart table to delay it 72 hours (2 Intel) or cancel it (5 Intel). At most 2 cards are
//! active per region.
//!
//! # Rules where the spec is silent
//! - Tallies are theatre-wide (one set, `TheatreState::tallies`); cards queue in the sortie's region.
//! - The per-region cap counts active plus queued cards, so a queued card always has a slot on arrival.
//! - Queuing a card answers its tally: the tally resets to 0, so a cancelled card is not re-queued by the
//!   same old kills; the player has to keep winning that way for the Combine to adapt again. A tally at the
//!   threshold with no free slot (or no unused matching card) keeps its value and queues once room exists.
//! - The matching card is the first card in data order that counters the tally and is neither active nor
//!   queued in that region. When several tallies cross in one sortie, cards queue in data order.
//! - A card may be delayed repeatedly while queued, each time for the full price. Revealing an ace needs a
//!   live hull record carrying that ace; otherwise the spend is refused before Intel is charged.
//! - `record_kills` also adds each kill to `OpsLedger::kills_by_method` (the after-action win-method tally).
//! - Active cards and the bulletin clear at act start (`progress::start_act`); tallies carry over and decay.
use super::{TheatreData, TheatreEvent, TheatreReject};
use crate::prelude::*;
use crate::protocol::config::KillRecord;
use content::campaign::TallyKey;
use content::save::{IntelSpend, QueuedCard, TheatreState};

/// Tallies below this are dropped from the map.
const TALLY_EPSILON: f64 = 1e-3;

/// Add a sortie's kills to the four tallies and queue any counter card whose tally reached the threshold
/// (in the sortie's region; ties and the per-region cap resolved by card order in data).
/// Unit: `sys-theatre-adaptation`.
pub fn record_kills(state: &mut TheatreState, region: &RegionId, kills: &[KillRecord], data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let rules = &data.campaign.rules;
    let step = state.difficulty.adaptation_cadence;
    let step = if step.is_finite() { step.clamp(0.0, 10.0) } else { 1.0 };
    let mut touched: BTreeSet<TallyKey> = BTreeSet::new();
    for k in kills {
        *state.ops.kills_by_method.entry(k.method).or_insert(0) += 1;
        for key in [TallyKey::Method(k.method), TallyKey::Range(k.range), TallyKey::Time(k.time), TallyKey::Emission(k.emcon)] {
            *state.tallies.entry(key).or_insert(0.0) += step;
            touched.insert(key);
        }
    }
    if data.campaign.theatre.regions.iter().any(|r| &r.id == region) {
        let r = state.regions.entry(region.clone()).or_default();
        let cap = rules.max_cards_per_region as usize;
        let threshold = if rules.tally_threshold.is_finite() && rules.tally_threshold > 0.0 { rules.tally_threshold } else { 10.0 };
        for card in data.campaign.cards.values() {
            if r.active_cards.len() + r.queued_cards.len() >= cap { break; }
            let tally = state.tallies.get(&card.counters).copied().unwrap_or(0.0);
            if tally + 1e-9 < threshold { continue; }
            if r.active_cards.contains(&card.id) || r.queued_cards.iter().any(|q| q.card == card.id) { continue; }
            let arrives = state.hour.saturating_add(rules.card_lead_h as u64);
            r.queued_cards.push(QueuedCard { card: card.id.clone(), arrives_hour: arrives });
            state.tallies.insert(card.counters, 0.0);
            touched.insert(card.counters);
            events.push(TheatreEvent::CardQueued { region: region.clone(), card: card.id.clone(), arrives_hour: arrives });
        }
    }
    for key in touched {
        let value = state.tallies.get(&key).copied().unwrap_or(0.0);
        events.push(TheatreEvent::TallyChanged { key, value });
    }
}

/// One hour: decay tallies (`libm::exp2(-1 / half_life)` per hour) and activate cards whose arrival hour
/// came. Unit: `sys-theatre-adaptation`.
pub fn step_hour(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let rules = &data.campaign.rules;
    let half = if rules.tally_half_life_h.is_finite() && rules.tally_half_life_h > 0.0 { rules.tally_half_life_h } else { 120.0 };
    let k = libm::exp2(-1.0 / half);
    for v in state.tallies.values_mut() { *v = if v.is_finite() { *v * k } else { 0.0 }; }
    state.tallies.retain(|_, v| *v >= TALLY_EPSILON);

    let hour = state.hour;
    let cap = rules.max_cards_per_region as usize;
    for (region, r) in state.regions.iter_mut() {
        let mut i = 0;
        while i < r.queued_cards.len() {
            if r.queued_cards[i].arrives_hour <= hour && r.active_cards.len() < cap.max(1) {
                let q = r.queued_cards.remove(i);
                if !r.active_cards.contains(&q.card) { r.active_cards.push(q.card.clone()); }
                events.push(TheatreEvent::CardArrived { region: region.clone(), card: q.card });
            } else {
                i += 1;
            }
        }
    }
}

/// Spend Intel: delay or cancel a queued card, reveal a region's convoys for 4 theatre days (1 Intel), or
/// reveal an ace's location (3 Intel). Unit: `sys-theatre-adaptation`.
pub fn spend_intel(state: &mut TheatreState, spend: &IntelSpend, data: &TheatreData, events: &mut Vec<TheatreEvent>) -> Result<(), TheatreReject> {
    let rules = &data.campaign.rules;
    let hour = state.hour;
    let queued_index = |state: &TheatreState, region: &RegionId, card: &CardId| -> Result<usize, TheatreReject> {
        state.regions.get(region).and_then(|r| r.queued_cards.iter().position(|q| &q.card == card)).ok_or(TheatreReject::UnknownTarget)
    };
    let afford = |state: &TheatreState, cost: u8| -> Result<u32, TheatreReject> {
        if state.resources.intel < cost as u32 { Err(TheatreReject::NotEnoughIntel) } else { Ok(cost as u32) }
    };
    match spend {
        IntelSpend::DelayCard { region, card } => {
            let i = queued_index(state, region, card)?;
            let cost = afford(state, rules.intel_delay_card)?;
            state.resources.intel -= cost;
            let Some(r) = state.regions.get_mut(region) else { return Err(TheatreReject::UnknownTarget) };
            let q = &mut r.queued_cards[i];
            q.arrives_hour = q.arrives_hour.saturating_add(rules.card_delay_h as u64);
            events.push(TheatreEvent::CardDelayed { region: region.clone(), card: card.clone(), arrives_hour: q.arrives_hour });
        }
        IntelSpend::CancelCard { region, card } => {
            let i = queued_index(state, region, card)?;
            let cost = afford(state, rules.intel_cancel_card)?;
            state.resources.intel -= cost;
            let Some(r) = state.regions.get_mut(region) else { return Err(TheatreReject::UnknownTarget) };
            r.queued_cards.remove(i);
            events.push(TheatreEvent::CardCancelled { region: region.clone(), card: card.clone() });
        }
        IntelSpend::RevealConvoys(region) => {
            if !data.campaign.theatre.regions.iter().any(|r| &r.id == region) { return Err(TheatreReject::UnknownTarget); }
            let cost = afford(state, rules.intel_reveal_convoys)?;
            state.resources.intel -= cost;
            let until = hour.saturating_add(rules.reveal_convoys_days as u64 * 24);
            let r = state.regions.entry(region.clone()).or_default();
            r.convoys_revealed_until_hour = Some(r.convoys_revealed_until_hour.map_or(until, |u| u.max(until)));
            let in_region = |n: &TheatreNodeId| data.campaign.theatre.nodes.iter().any(|d| &d.id == n && &d.region == region);
            for c in state.convoys.iter_mut() {
                if !c.revealed && (in_region(&c.from) || in_region(&c.to)) {
                    c.revealed = true;
                    events.push(TheatreEvent::ConvoyRevealed { convoy: c.id });
                }
            }
        }
        IntelSpend::RevealAce(ace) => {
            if !data.campaign.aces.contains_key(ace) { return Err(TheatreReject::UnknownTarget); }
            let node = state.hulls.iter().find(|h| h.alive && h.ace.as_ref() == Some(ace)).map(|h| h.node.clone()).ok_or(TheatreReject::UnknownTarget)?;
            let cost = afford(state, rules.intel_reveal_ace)?;
            state.resources.intel -= cost;
            if !state.ops.aces_revealed.contains(ace) { state.ops.aces_revealed.push(ace.clone()); }
            events.push(TheatreEvent::AceRevealed { ace: ace.clone(), node });
        }
    }
    events.push(TheatreEvent::ResourcesChanged);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::alert::tests::kill;
    use super::super::logistics::testkit::*;
    use super::*;
    use content::campaign::KillMethod;

    fn missile_kills(n: usize) -> Vec<KillRecord> {
        (0..n).map(|_| { let mut k = kill(true); k.method = KillMethod::Missile; k }).collect()
    }

    #[test]
    fn tallies_queue_cards_at_threshold() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        record_kills(&mut s, &rid("r1"), &missile_kills(9), &d, &mut ev);
        assert!((s.tallies[&TallyKey::Method(KillMethod::Missile)] - 9.0).abs() < 1e-12);
        assert!(s.regions[&rid("r1")].queued_cards.is_empty());
        assert_eq!(s.ops.kills_by_method[&KillMethod::Missile], 9);
        record_kills(&mut s, &rid("r1"), &missile_kills(1), &d, &mut ev);
        // Missile, Close, Night and Silent all reached 10; the cap of 2 takes the first two cards in data
        // order: card.pd (missile) and card.pickets (night).
        let q: Vec<&str> = s.regions[&rid("r1")].queued_cards.iter().map(|q| q.card.as_str()).collect();
        assert_eq!(q, vec!["card.pd", "card.pickets"]);
        assert_eq!(s.regions[&rid("r1")].queued_cards[0].arrives_hour, 72);
        assert_eq!(s.tallies[&TallyKey::Method(KillMethod::Missile)], 0.0);
        // Range Close kept its value: no slot was free.
        assert!((s.tallies[&TallyKey::Range(content::campaign::RangeBand::Close)] - 10.0).abs() < 1e-12);
        for h in 1..=72 { s.hour = h; step_hour(&mut s, &d, &mut ev); }
        assert_eq!(s.regions[&rid("r1")].active_cards.len(), 2);
        assert!(s.regions[&rid("r1")].queued_cards.is_empty());
    }

    #[test]
    fn half_life_is_120_hours() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        s.tallies.insert(TallyKey::Method(KillMethod::Torpedo), 8.0);
        for _ in 0..120 { s.hour += 1; step_hour(&mut s, &d, &mut ev); }
        assert!((s.tallies[&TallyKey::Method(KillMethod::Torpedo)] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn intel_delay_cancel_and_reveals() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        record_kills(&mut s, &rid("r1"), &missile_kills(10), &d, &mut ev);
        let (r, c) = (rid("r1"), CardId::new("card.pd"));
        assert_eq!(spend_intel(&mut s, &IntelSpend::DelayCard { region: r.clone(), card: c.clone() }, &d, &mut ev), Err(TheatreReject::NotEnoughIntel));
        s.resources.intel = 7;
        spend_intel(&mut s, &IntelSpend::DelayCard { region: r.clone(), card: c.clone() }, &d, &mut ev).unwrap();
        assert_eq!(s.resources.intel, 5);
        assert_eq!(s.regions[&r].queued_cards[0].arrives_hour, 144);
        spend_intel(&mut s, &IntelSpend::CancelCard { region: r.clone(), card: c.clone() }, &d, &mut ev).unwrap();
        assert_eq!(s.resources.intel, 0);
        assert!(s.regions[&r].queued_cards.iter().all(|q| q.card != c));
        // Unknown targets are refused before Intel is charged.
        s.resources.intel = 9;
        assert_eq!(spend_intel(&mut s, &IntelSpend::CancelCard { region: r.clone(), card: c.clone() }, &d, &mut ev), Err(TheatreReject::UnknownTarget));
        assert_eq!(spend_intel(&mut s, &IntelSpend::RevealConvoys(rid("bogus")), &d, &mut ev), Err(TheatreReject::UnknownTarget));
        assert_eq!(spend_intel(&mut s, &IntelSpend::RevealAce(AceId::new("bogus")), &d, &mut ev), Err(TheatreReject::UnknownTarget));
        assert_eq!(s.resources.intel, 9);
        spend_intel(&mut s, &IntelSpend::RevealConvoys(r.clone()), &d, &mut ev).unwrap();
        assert_eq!(s.regions[&r].convoys_revealed_until_hour, Some(96));
        spend_intel(&mut s, &IntelSpend::RevealAce(AceId::new("ace.one")), &d, &mut ev).unwrap();
        assert_eq!(s.resources.intel, 5);
        assert_eq!(s.ops.aces_revealed, vec![AceId::new("ace.one")]);
    }
}
