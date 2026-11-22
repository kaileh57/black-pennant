//! Unit `gd-hud-alerts`. Alerts on `BpHud`. DESIGN.md L1676-1687 (priorities, states, strip of 5 plus "+N",
//! flood mode, silence), L1666 (alert budget), L1743 (the strip is an accessibility live region).
//! The alert rules run in the simulation (`sim_systems::alerts`); this unit only presents `AlertView`s.
//! Acknowledge and Silence are commands on `Orders`.
//!
//! Source: the core's alert book (every alert of the sortie as the events told it), with the fields the
//! simulation changes in place without an event (`blink_fast`, the kind's live values such as a magazine's
//! cook-off countdown) taken from the newest snapshot's list when it holds the alert.
//!
//! Decisions where the contract is silent:
//! - The strip shows Unacknowledged, Acknowledged and Cleared alerts (a rectified, unacknowledged Alarm or
//!   Emergency shows "Cleared" until the player acknowledges it, which sends it to the log). Shelved and
//!   log-only alerts are not on the strip. Order: highest priority, then oldest raise, then lowest id.
//! - `alerts(true)` and `alerts_for` list Unacknowledged and Acknowledged alerts only (the doc's "active").
//! - `alert_level` counts the same active set, so a strip holding only Cleared items reads as calm.
//! - `next_ack` mirrors `sim_systems::alerts::AlertManager::acknowledge` without an id: an Unacknowledged
//!   alert of Warning or above, or a Cleared one; highest priority, then oldest `(raised_tick, id)`.
//!   Cautions need no acknowledgement and are skipped.
//! - `alerts_for(compartment, ..)` also lists the alerts of parts that sit in that compartment and of
//!   conduits with an end in it, so the schematic marks a compartment for everything wrong inside it.
//! - `silenced_s` is computed at every call from the newest snapshot tick (it runs down with sortie time,
//!   and a pause holds it), so the cached strip never freezes it.
use super::{alert_dict, alert_subject, BpHud, Section};
use crate::convert::d;
use crate::enums::GdEnum;
use content::ids::{CompartmentId, PartSlot};
use godot::prelude::*;
use sim_systems::ids::AlertId;
use sim_systems::protocol::{AlertBudget, AlertKind, AlertPriority, AlertState, AlertView};

/// Alerts the strip shows before "+N" (L1687).
pub(crate) fn strip_len() -> usize { AlertBudget::default().strip_len.max(1) as usize }

/// On the strip: live alerts and Cleared ones waiting for acknowledgement.
pub(crate) fn on_strip(a: &AlertView) -> bool { matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged | AlertState::Cleared) }
/// Active: unacknowledged or acknowledged.
pub(crate) fn is_active(a: &AlertView) -> bool { matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged) }

/// Strip order: highest priority first, then the oldest raise, then the lowest id.
fn strip_key(a: &AlertView) -> (std::cmp::Reverse<AlertPriority>, u64, AlertId) { (std::cmp::Reverse(a.priority), a.raised_tick, a.id) }

/// The strip's items in display order (all of them; the caller shows the first `strip_len()`).
pub(crate) fn strip_items(list: &[AlertView]) -> Vec<AlertView> {
    let mut v: Vec<AlertView> = list.iter().filter(|a| on_strip(a)).copied().collect();
    v.sort_by_key(strip_key);
    v
}

/// The alert Acknowledge (no id) would take, as the simulation picks it.
pub(crate) fn next_ack(list: &[AlertView]) -> Option<AlertId> {
    list.iter()
        .filter(|a| (a.state == AlertState::Unacknowledged && a.priority >= AlertPriority::Warning) || a.state == AlertState::Cleared)
        .min_by_key(|a| strip_key(a))
        .map(|a| a.id)
}

/// Highest active priority; `None` when calm.
pub(crate) fn level(list: &[AlertView]) -> Option<AlertPriority> { list.iter().filter(|a| is_active(a)).map(|a| a.priority).max() }

/// Whether two kinds are the same alert subject (same variant and id field), so the snapshot's live values
/// may replace the book's.
fn same_subject(a: &AlertKind, b: &AlertKind) -> bool { std::mem::discriminant(a) == std::mem::discriminant(b) && subject_key(a) == subject_key(b) }
fn subject_key(k: &AlertKind) -> (u32, i64) {
    use AlertKind as K;
    match k {
        K::WeaponInbound { track } | K::TorpedoInbound { track } | K::NewHostileTrack { track } => (1, track.0 as i64),
        K::Flooding { compartment } | K::FloodingSevere { compartment } | K::Fire { compartment } => (2, compartment.0 as i64),
        K::MagazineFire { magazine, .. } => (3, magazine.0 as i64),
        K::PartHeavyDamage { slot } | K::PartDestroyed { slot } | K::PartOnBuffer { slot, .. } | K::PartLocalControl { slot } | K::AmmunitionLow { slot } => (3, slot.0 as i64),
        K::ConduitCut { conduit } => (4, *conduit as i64),
        K::HostOverload { rung } => (5, *rung as i64),
        _ => (0, 0),
    }
}

/// The book's alerts of one vessel (all of them while the own hull is not known), with the in-place fields
/// of the newest snapshot merged in. Ascending id, as the book keeps them.
pub(crate) fn merged(book: &[AlertView], snapshot: &[AlertView], own: Option<sim_core::EntityId>) -> Vec<AlertView> {
    book.iter().filter(|a| own.is_none_or(|v| a.vessel == v)).map(|a| {
        let mut out = *a;
        match snapshot.iter().find(|s| s.id == a.id) {
            Some(s) => {
                if same_subject(&s.kind, &a.kind) && s.priority == a.priority { out.kind = s.kind; }
                // Events are newer than the sampled snapshot: only an alert both call unacknowledged blinks.
                out.blink_fast = s.blink_fast && a.state == AlertState::Unacknowledged && s.state == AlertState::Unacknowledged;
            }
            None => { if a.state != AlertState::Unacknowledged { out.blink_fast = false; } }
        }
        out
    }).collect()
}

/// Whether an alert concerns a compartment (directly, through a part inside it, or a conduit ending in it)
/// or a part. `part_comp` gives a part's compartment, `conduit_ends` a conduit's two compartments.
pub(crate) fn concerns(a: &AlertView, compartment: Option<CompartmentId>, part: Option<PartSlot>, part_comp: &dyn Fn(PartSlot) -> Option<CompartmentId>, conduit_ends: &dyn Fn(u16) -> Option<(CompartmentId, CompartmentId)>) -> bool {
    let (_, p, c) = alert_subject(a);
    if let Some(want) = part { if p == Some(want) { return true; } }
    if let Some(want) = compartment {
        if c == Some(want) { return true; }
        if let Some(p) = p { if part_comp(p) == Some(want) { return true; } }
        if let AlertKind::ConduitCut { conduit } = a.kind {
            if let Some((x, y)) = conduit_ends(conduit) { if x == want || y == want { return true; } }
        }
    }
    false
}

impl BpHud {
    /// The sortie's alerts for this viewer, merged with the newest snapshot. Ascending id.
    pub(crate) fn alert_views(&self) -> Vec<AlertView> {
        let snap = self.snap().map(|s| s.alerts.as_slice()).unwrap_or(&[]);
        merged(&self.core().alerts.history, snap, self.core().own_vessel)
    }

    /// Every alert of the sortie, oldest first, cached per alerts revision.
    fn alert_list(&self) -> Array<VarDictionary> {
        self.cached("alerts_all", 0, &[Section::Alerts, Section::Static], |h| {
            let mut v = h.alert_views();
            v.sort_by_key(|a| (a.raised_tick, a.id));
            v.iter().map(|a| alert_dict(h, a)).collect()
        })
    }
}

fn dict_i64(d: &VarDictionary, key: &str) -> i64 { d.get(key).and_then(|v| v.try_to::<i64>().ok()).unwrap_or(-1) }
fn dict_active(d: &VarDictionary) -> bool { AlertState::from_gd(dict_i64(d, "state")).is_some_and(|s| matches!(s, AlertState::Unacknowledged | AlertState::Acknowledged)) }

#[godot_api(secondary)]
impl BpHud {
    /// Returns `AlertStripDict`. On change, `alerts_changed`.
    #[func]
    fn alert_strip(&self) -> VarDictionary {
        let cached = self.cached("alert_strip", 0, &[Section::Alerts, Section::Static], |h| {
            let views = h.alert_views();
            let items = strip_items(&views);
            let n = strip_len();
            let shown: Array<VarDictionary> = items.iter().take(n).map(|a| alert_dict(h, a)).collect();
            d! {
                "shown" => shown,
                "more" => items.len().saturating_sub(n) as i64,
                "flood" => h.core().alerts.flood,
                "silenced_s" => 0.0f64,
                "next_ack" => next_ack(&views).map_or(-1, |id| id.0 as i64),
            }
        });
        let mut out = cached.duplicate_shallow();
        let core = self.core();
        let left = core.alerts.silenced_s(core.tick);
        out.set("silenced_s", if left.is_finite() { left.max(0.0) } else { 0.0 });
        out
    }
    /// Every alert of the sortie with its time, oldest first (`Array[AlertDict]`). With `active_only` true,
    /// only unacknowledged and acknowledged ones.
    #[func]
    fn alerts(&self, active_only: bool) -> Array<VarDictionary> {
        if !active_only { return self.alert_list(); }
        self.cached("alerts_active", 0, &[Section::Alerts, Section::Static], |h| h.alert_list().iter_shared().filter(dict_active).collect())
    }
    /// One `AlertDict`, or `null`.
    #[func]
    fn alert(&self, id: i64) -> Variant {
        if id < 0 { return Variant::nil(); }
        self.alert_list().iter_shared().find(|d| dict_i64(d, "id") == id).map_or(Variant::nil(), |d| d.to_variant())
    }
    /// Highest active priority (`Bp.PRIORITY_*`), or `Bp.NONE` when calm. The calm state shows zero alert
    /// colour pixels (rule 1).
    #[func]
    fn alert_level(&self) -> i64 { level(&self.alert_views()).map_or(-1, |p| p.to_gd()) }
    /// Active alerts for one compartment or part, for the damage-control schematic: `Array[AlertDict]`.
    /// Pass `Bp.NONE` for the one that does not apply.
    #[func]
    fn alerts_for(&self, compartment: i64, part: i64) -> Array<VarDictionary> {
        let comp = u16::try_from(compartment).ok().map(CompartmentId);
        let slot = u16::try_from(part).ok().map(PartSlot);
        if comp.is_none() && slot.is_none() { return Array::new(); }
        let arg = (comp.map_or(-1, |c| c.0 as i64) << 20) ^ slot.map_or(0xF_FFFF, |s| s.0 as i64);
        self.cached("alerts_for", arg, &[Section::Alerts, Section::Static], |h| {
            let design = h.design();
            let part_comp = |s: PartSlot| design.and_then(|d| d.prefab.parts.get(s.index())).and_then(|p| p.compartment);
            let conduit_ends = |c: u16| design.and_then(|d| d.prefab.networks.conduits.get(c as usize)).map(|k| (k.from, k.to));
            let mut v: Vec<AlertView> = h.alert_views().into_iter().filter(|a| is_active(a) && concerns(a, comp, slot, &part_comp, &conduit_ends)).collect();
            v.sort_by_key(strip_key);
            v.iter().map(|a| alert_dict(h, a)).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::enums::OfficerRole;
    use sim_core::EntityId;
    use sim_systems::ids::TrackId;
    use sim_systems::protocol::Subsystem;
    use AlertPriority::*;
    use AlertState::*;

    fn own() -> EntityId { EntityId::from_u64(7) }
    fn a(id: u32, p: AlertPriority, tick: u64, state: AlertState) -> AlertView {
        AlertView { id: AlertId(id), vessel: own(), priority: p, subsystem: Subsystem::Fire, kind: AlertKind::Fire { compartment: CompartmentId(id as u16) }, state, raised_tick: tick, officer: Some(OfficerRole::DamageControl), blink_fast: false }
    }

    #[test]
    fn strip_orders_by_priority_then_age_and_hides_shelved() {
        let list = vec![a(0, Caution, 10, Unacknowledged), a(1, Alarm, 50, Unacknowledged), a(2, Alarm, 20, Acknowledged), a(3, Emergency, 90, Cleared), a(4, Warning, 5, Shelved), a(5, Warning, 6, LogOnly)];
        let ids: Vec<u32> = strip_items(&list).iter().map(|x| x.id.0).collect();
        assert_eq!(ids, vec![3, 2, 1, 0]);
        assert_eq!(strip_len(), 5);
    }

    #[test]
    fn next_ack_matches_the_simulation_rule() {
        // Cautions need no acknowledgement; acknowledged ones are done; Cleared ones are taken.
        assert_eq!(next_ack(&[a(0, Caution, 1, Unacknowledged)]), None);
        assert_eq!(next_ack(&[a(0, Caution, 1, Unacknowledged), a(1, Warning, 9, Unacknowledged), a(2, Warning, 3, Unacknowledged)]), Some(AlertId(2)));
        assert_eq!(next_ack(&[a(1, Alarm, 9, Acknowledged), a(2, Warning, 3, Unacknowledged)]), Some(AlertId(2)));
        assert_eq!(next_ack(&[a(1, Warning, 1, Unacknowledged), a(2, Emergency, 30, Cleared)]), Some(AlertId(2)));
        // Same priority and tick: lowest id.
        assert_eq!(next_ack(&[a(4, Alarm, 3, Unacknowledged), a(2, Alarm, 3, Unacknowledged)]), Some(AlertId(2)));
        assert_eq!(next_ack(&[]), None);
    }

    #[test]
    fn level_is_calm_without_active_alerts() {
        assert_eq!(level(&[]), None);
        assert_eq!(level(&[a(0, Emergency, 1, Cleared), a(1, Alarm, 1, LogOnly), a(2, Warning, 1, Shelved)]), None);
        assert_eq!(level(&[a(0, Caution, 1, Unacknowledged), a(1, Alarm, 1, Acknowledged)]), Some(Alarm));
    }

    #[test]
    fn snapshot_supplies_blink_and_live_values_only() {
        let mut book = a(0, Alarm, 1, Unacknowledged);
        book.kind = AlertKind::MagazineFire { magazine: PartSlot(4), cookoff_in_s: 90.0 };
        let mut snap = book;
        snap.kind = AlertKind::MagazineFire { magazine: PartSlot(4), cookoff_in_s: 61.0 };
        snap.blink_fast = true;
        let m = merged(&[book], &[snap], Some(own()));
        assert!(m[0].blink_fast);
        assert_eq!(m[0].kind, snap.kind);
        // The book's newer state wins: an acknowledged alert stops blinking even if the snapshot lags.
        let mut acked = book;
        acked.state = Acknowledged;
        let m = merged(&[acked], &[snap], Some(own()));
        assert!(!m[0].blink_fast && m[0].state == Acknowledged);
        // Other vessels are left out once the own hull is known.
        let mut other = book;
        other.vessel = EntityId::from_u64(99);
        assert!(merged(&[other], &[], Some(own())).is_empty());
        assert_eq!(merged(&[other], &[], None).len(), 1);
        // A different subject never takes the snapshot's kind.
        let mut s2 = snap;
        s2.kind = AlertKind::WeaponInbound { track: TrackId(3) };
        assert_eq!(merged(&[book], &[s2], Some(own()))[0].kind, book.kind);
    }

    #[test]
    fn concerns_compartment_through_parts_and_conduits() {
        let part_comp = |s: PartSlot| if s == PartSlot(3) { Some(CompartmentId(1)) } else { None };
        let ends = |c: u16| if c == 2 { Some((CompartmentId(1), CompartmentId(4))) } else { None };
        let fire = a(0, Alarm, 1, Unacknowledged);
        assert!(concerns(&fire, Some(CompartmentId(0)), None, &part_comp, &ends));
        assert!(!concerns(&fire, Some(CompartmentId(1)), None, &part_comp, &ends));
        let mut part = fire;
        part.kind = AlertKind::PartHeavyDamage { slot: PartSlot(3) };
        assert!(concerns(&part, Some(CompartmentId(1)), None, &part_comp, &ends));
        assert!(concerns(&part, None, Some(PartSlot(3)), &part_comp, &ends));
        assert!(!concerns(&part, None, Some(PartSlot(2)), &part_comp, &ends));
        let mut cut = fire;
        cut.kind = AlertKind::ConduitCut { conduit: 2 };
        assert!(concerns(&cut, Some(CompartmentId(4)), None, &part_comp, &ends));
        assert!(!concerns(&cut, None, None, &part_comp, &ends));
    }
}
