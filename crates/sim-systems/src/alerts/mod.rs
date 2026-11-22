//! Unit `sys-alerts`. The alert manager: priorities, states, absorption, flood mode and the alert budget.
//! DESIGN.md L1676-1687, L1666 (budget), L1763 (gate fallback: ceiling 15, flood at 8).
//!
//! Rules:
//! - states: unacknowledged, acknowledged (steady until rectified), rectified or cleared. A rectified
//!   unacknowledged Emergency or Alarm shows "Cleared"; lower ones go to the log unread;
//! - only the newest top Emergency or Alarm blinks fast;
//! - Silence all lasts 30 s; Acknowledge takes the highest-priority, oldest alert; there is no
//!   acknowledge-all; only the player acknowledges (officers cannot);
//! - Warnings escalate by threshold, never by timer;
//! - a Fire or Flooding Alarm absorbs same-compartment Warnings and Cautions; an Emergency suppresses both;
//!   absorbed or suppressed items shelve for 5 minutes or the rest of the sortie;
//! - flood mode (more than 10 alerts in 10 minutes): Cautions go to the log, Warnings combine per subsystem,
//!   Emergencies and Alarms never combine;
//! - only first annunciations count toward budgets (at most 20 new alerts per 20-minute engagement); the mix
//!   target is 80% Caution, 15% Warning, 5% Alarm, under 1% Emergency.
//!
//! Decisions where the spec is silent:
//! - an alert is identified by (vessel, kind variant, its id field); values such as `cookoff_in_s` update in
//!   place. `Flooding` and `FloodingSevere` of one compartment are one alert, as are `PartHeavyDamage` and
//!   `PartDestroyed` of one part, and `WeaponInbound` and `TorpedoInbound` of one track;
//! - a condition that returns within 30 s of being rectified reopens its old alert instead of raising a new
//!   one, so a chattering condition counts once against the budget;
//! - priorities only rise while a condition persists (no de-escalation flicker); an escalated alert needs a
//!   fresh acknowledgement;
//! - flood mode ends as soon as the 10-minute count is back at or under the limit; items it sent to the log
//!   stay there; log-only and shelved-at-birth items are not first annunciations and do not count;
//! - every live Hostile contact is a condition; its Caution moves to the log `NEW_TRACK_S` after it was
//!   raised and stays there while the track lives, so a track that turns Hostile late still announces once;
//! - `for_vessel` returns the live alerts and the Cleared ones; rectified history stays in `alerts` and
//!   reaches clients through the alert events;
//! - acknowledge without an id skips Cautions (they need none) and takes Cleared alerts too, which dismisses
//!   them to the log; Cautions can still be acknowledged by id;
//! - shelving lasts `AlertBudget::shelve_s`; at expiry an item whose absorber is still live shelves again,
//!   otherwise it returns to the state it had;
//! - Fire and Flooding Alarms absorb part, conduit and ammunition alerts of their compartment; the compartment
//!   of a part or conduit comes from [`AlertManager::note_layout`], which the Sim calls with each vessel.
use crate::prelude::*;
use crate::protocol::alert::{AlertBudget, AlertKind, AlertPriority, AlertState, AlertView, Subsystem};
use content::part::PartFunction;
use sim_core::networks::{ConduitState, LinkState};
use sim_core::parts::{PartState, TeamOrder};
use sim_core::time::TICK_HZ;
use sim_core::vessel::VesselStatus;

/// Escalation thresholds (L1687: by threshold, never by timer).
pub const MAGAZINE_EMERGENCY_S: f64 = 30.0;
pub const FUEL_LOW_FRACTION: f64 = 0.25;
pub const FUEL_WARNING_FRACTION: f64 = 0.10;
pub const BUFFER_WARNING_S: f64 = 10.0;
pub const SHED_WARNING_COUNT: u16 = 3;
/// Seconds within which a returning condition reopens its old alert.
pub const REOPEN_S: u64 = 30;
/// A Hostile-contact alert stays on the strip this long, then goes to the log while the track lives.
pub const NEW_TRACK_S: u64 = 30;
/// Magazines below this fraction of their rounds report low ammunition.
pub const AMMO_LOW_FRACTION: f64 = 0.2;
/// Heel at which capsize is reported imminent, as a fraction of the capsize heel (90 degrees gives 50).
pub const CAPSIZE_WARN_FRACTION: f64 = 0.555_6;
/// Fraction of internal volume flooded at which sinking is reported imminent.
pub const SINKING_FRACTION: f64 = 0.5;

/// Priority, subsystem and owning officer of an alert kind at its current values. Escalation thresholds
/// live here (for example a magazine fire is an Alarm and becomes an Emergency when cook-off is under 30 s).
/// Unit: `sys-alerts`.
pub fn classify(kind: &AlertKind) -> (AlertPriority, Subsystem, Option<OfficerRole>) {
    use AlertPriority::*;
    use OfficerRole as O;
    match *kind {
        AlertKind::WeaponInbound { .. } => (Alarm, Subsystem::Threat, Some(O::Gunnery)),
        AlertKind::TorpedoInbound { .. } => (Alarm, Subsystem::Threat, Some(O::Helm)),
        AlertKind::Flooding { .. } | AlertKind::FloodingSevere { .. } => (Alarm, Subsystem::Flooding, Some(O::DamageControl)),
        AlertKind::Fire { .. } => (Alarm, Subsystem::Fire, Some(O::DamageControl)),
        AlertKind::MagazineFire { cookoff_in_s, .. } => (if cookoff_in_s < MAGAZINE_EMERGENCY_S { Emergency } else { Alarm }, Subsystem::Fire, Some(O::DamageControl)),
        AlertKind::CapsizeImminent | AlertKind::SinkingImminent => (Emergency, Subsystem::Stability, Some(O::DamageControl)),
        AlertKind::PropulsionLost => (Warning, Subsystem::Propulsion, Some(O::Engineer)),
        AlertKind::SteeringLost => (Warning, Subsystem::Steering, Some(O::Helm)),
        AlertKind::PowerShortage { shed } => (if shed >= SHED_WARNING_COUNT { Warning } else { Caution }, Subsystem::Power, Some(O::Engineer)),
        AlertKind::ConduitCut { .. } => (Caution, Subsystem::Power, Some(O::DamageControl)),
        AlertKind::PartHeavyDamage { .. } | AlertKind::PartDestroyed { .. } => (Caution, Subsystem::System, Some(O::DamageControl)),
        AlertKind::PartOnBuffer { seconds_left, .. } => (if seconds_left < BUFFER_WARNING_S { Warning } else { Caution }, Subsystem::Power, Some(O::Engineer)),
        AlertKind::PartLocalControl { .. } => (Caution, Subsystem::Data, Some(O::Engineer)),
        AlertKind::FuelLow { fraction } => (if fraction < FUEL_WARNING_FRACTION { Warning } else { Caution }, Subsystem::Fuel, Some(O::Engineer)),
        AlertKind::AmmunitionLow { .. } => (Caution, Subsystem::Weapons, Some(O::Gunnery)),
        AlertKind::EsmWarning => (Caution, Subsystem::Sensors, Some(O::Sensors)),
        AlertKind::NewHostileTrack { .. } => (Caution, Subsystem::Threat, Some(O::Sensors)),
        AlertKind::ShoalAhead | AlertKind::Grounded => (Warning, Subsystem::Navigation, Some(O::Helm)),
        AlertKind::CrewCasualties => (Caution, Subsystem::Crew, Some(O::DamageControl)),
        AlertKind::PassengersLost { .. } => (Warning, Subsystem::Mission, None),
        AlertKind::ActionInterrupted => (Caution, Subsystem::Mission, None),
        AlertKind::ScriptGreyedOut | AlertKind::HostOverload { .. } => (Caution, Subsystem::System, None),
        AlertKind::HunterGroupApproaching => (Warning, Subsystem::Threat, Some(O::Sensors)),
    }
}

/// Identity of an alert within a vessel: kind family and its id field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct AlertKey(u8, u32);

fn key_of(kind: &AlertKind) -> AlertKey {
    match *kind {
        AlertKind::WeaponInbound { track } | AlertKind::TorpedoInbound { track } => AlertKey(0, track.0),
        AlertKind::Flooding { compartment } | AlertKind::FloodingSevere { compartment } => AlertKey(1, compartment.0 as u32),
        AlertKind::Fire { compartment } => AlertKey(2, compartment.0 as u32),
        AlertKind::MagazineFire { magazine, .. } => AlertKey(3, magazine.0 as u32),
        AlertKind::CapsizeImminent => AlertKey(4, 0),
        AlertKind::SinkingImminent => AlertKey(5, 0),
        AlertKind::PropulsionLost => AlertKey(6, 0),
        AlertKind::SteeringLost => AlertKey(7, 0),
        AlertKind::PowerShortage { .. } => AlertKey(8, 0),
        AlertKind::ConduitCut { conduit } => AlertKey(9, conduit as u32),
        AlertKind::PartHeavyDamage { slot } | AlertKind::PartDestroyed { slot } => AlertKey(10, slot.0 as u32),
        AlertKind::PartOnBuffer { slot, .. } => AlertKey(11, slot.0 as u32),
        AlertKind::PartLocalControl { slot } => AlertKey(12, slot.0 as u32),
        AlertKind::FuelLow { .. } => AlertKey(13, 0),
        AlertKind::AmmunitionLow { slot } => AlertKey(14, slot.0 as u32),
        AlertKind::EsmWarning => AlertKey(15, 0),
        AlertKind::NewHostileTrack { track } => AlertKey(16, track.0),
        AlertKind::ShoalAhead => AlertKey(17, 0),
        AlertKind::Grounded => AlertKey(18, 0),
        AlertKind::CrewCasualties => AlertKey(19, 0),
        AlertKind::PassengersLost { .. } => AlertKey(20, 0),
        AlertKind::ActionInterrupted => AlertKey(21, 0),
        AlertKind::ScriptGreyedOut => AlertKey(22, 0),
        AlertKind::HostOverload { .. } => AlertKey(23, 0),
        AlertKind::HunterGroupApproaching => AlertKey(24, 0),
    }
}

/// Manager bookkeeping per alert, parallel to `AlertManager::alerts`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AlertMeta {
    key: AlertKey,
    /// The condition is still reported.
    live: bool,
    /// The player acknowledged it at its current priority.
    acked: bool,
    rectified_tick: u64,
    /// State to return to when a shelving ends.
    prior: AlertState,
}

/// Compartments of parts and conduits on one vessel, for absorption.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Layout { vessel: EntityId, parts: Vec<(PartSlot, CompartmentId)>, conduits: Vec<(u16, CompartmentId)> }

/// A condition a system reports as present this tick. The manager raises on first sight, updates values,
/// escalates across thresholds and rectifies when a condition stops being reported.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Condition { pub vessel: EntityId, pub kind: AlertKind }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AlertManager {
    pub budget: AlertBudget,
    /// Every alert of the sortie, ascending id (the log keeps all with times).
    pub alerts: Vec<AlertView>,
    pub next_id: u32,
    /// Ticks of first annunciations per vessel, for the budget and flood tests.
    pub raised_ticks: Vec<(EntityId, u64)>,
    pub flood: Vec<EntityId>,
    pub silenced_until: Vec<(EntityId, u64)>,
    /// Shelved until tick, per alert.
    pub shelved_until: Vec<(AlertId, u64)>,
    /// One entry per alert, same order as `alerts`.
    #[serde(default)]
    pub(crate) meta: Vec<AlertMeta>,
    /// Indices into `alerts` of live alerts and of alerts rectified in the last `REOPEN_S`, ascending.
    #[serde(default)]
    pub(crate) live: Vec<u32>,
    #[serde(default)]
    pub(crate) recent: Vec<u32>,
    #[serde(default)]
    pub(crate) layouts: Vec<Layout>,
}

/// Change the manager made this tick; the Sim turns each into a `SimEvent` for the vessel's player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlertChange { Raised(AlertView), Escalated(AlertView), State(AlertId, AlertState), Flood(EntityId, bool), Silenced(EntityId, u64) }

impl AlertManager {
    /// Reconcile this tick's conditions (ascending vessel, then kind order) with the alert list.
    /// Unit: `sys-alerts`.
    pub fn update(&mut self, conditions: &[Condition], tick: u64, out: &mut Vec<AlertChange>) {
        self.ensure_meta();
        let hz = TICK_HZ as u64;
        let window_20 = 20 * 60 * hz;
        self.raised_ticks.retain(|&(_, t)| t + window_20 > tick);
        self.silenced_until.retain(|&(_, until)| until > tick);
        self.recent.retain(|&i| self.meta.get(i as usize).map(|m| !m.live && m.rectified_tick + REOPEN_S * hz >= tick).unwrap_or(false));

        // Present conditions, one per identity; a duplicate keeps the higher priority.
        let mut seen: BTreeMap<(EntityId, AlertKey), AlertKind> = BTreeMap::new();
        for c in conditions {
            let k = (c.vessel, key_of(&c.kind));
            match seen.get(&k) { Some(old) if classify(old).0 >= classify(&c.kind).0 => {} _ => { seen.insert(k, c.kind); } }
        }
        let mut touched: BTreeSet<EntityId> = BTreeSet::new();

        // Live alerts: update, escalate or rectify.
        let live = std::mem::take(&mut self.live);
        let mut still_live = Vec::with_capacity(live.len());
        for i in live {
            let idx = i as usize;
            let (vessel, key) = (self.alerts[idx].vessel, self.meta[idx].key);
            match seen.remove(&(vessel, key)) {
                Some(kind) => {
                    self.alerts[idx].kind = kind;
                    let (p, sub, officer) = classify(&kind);
                    if p > self.alerts[idx].priority { self.escalate(idx, p, sub, officer, out); }
                    still_live.push(i);
                }
                None => {
                    self.rectify(idx, tick, out);
                    self.recent.push(i);
                }
            }
            touched.insert(vessel);
        }
        self.live = still_live;
        // Hostile-contact Cautions announce a contact; after `NEW_TRACK_S` they stay live in the log only.
        let aged = NEW_TRACK_S * hz;
        for i in self.live.clone() {
            let idx = i as usize;
            let a = self.alerts[idx];
            if matches!(a.kind, AlertKind::NewHostileTrack { .. }) && matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged) && a.raised_tick + aged <= tick {
                self.set_state(idx, AlertState::LogOnly, out);
            }
        }

        // New conditions: reopen a recently rectified alert, else raise.
        for ((vessel, key), kind) in seen {
            touched.insert(vessel);
            let reopen = self.recent.iter().position(|&i| { let a = &self.alerts[i as usize]; a.vessel == vessel && self.meta[i as usize].key == key });
            if let Some(pos) = reopen {
                let i = self.recent.remove(pos);
                let idx = i as usize;
                let (p, sub, officer) = classify(&kind);
                self.alerts[idx].kind = kind;
                self.meta[idx].live = true;
                self.insert_live(i);
                if p > self.alerts[idx].priority { self.escalate(idx, p, sub, officer, out); } else {
                    let state = if self.meta[idx].acked { AlertState::Acknowledged } else { AlertState::Unacknowledged };
                    self.set_state(idx, state, out);
                }
                continue;
            }
            self.raise(vessel, key, kind, tick, out);
        }

        // Flood mode, shelving and blink per vessel.
        for &v in self.flood.clone().iter() { touched.insert(v); }
        for v in touched {
            self.update_flood(v, tick, out);
            self.update_shelving(v, tick, out);
            self.update_blink(v);
        }
    }

    /// Acknowledge a named alert, or the highest-priority, oldest unacknowledged one of the vessel.
    /// Unit: `sys-alerts`.
    pub fn acknowledge(&mut self, vessel: EntityId, id: Option<AlertId>, out: &mut Vec<AlertChange>) -> bool {
        self.ensure_meta();
        let idx = match id {
            Some(id) => match self.index_of(id) { Some(i) if self.alerts[i].vessel == vessel => i, _ => return false },
            None => {
                let mut best: Option<usize> = None;
                for (i, a) in self.alerts.iter().enumerate() {
                    if a.vessel != vessel { continue; }
                    let wants = (a.state == AlertState::Unacknowledged && self.meta[i].live && a.priority >= AlertPriority::Warning) || a.state == AlertState::Cleared;
                    if !wants { continue; }
                    let better = match best { None => true, Some(b) => { let o = &self.alerts[b]; a.priority > o.priority || (a.priority == o.priority && (a.raised_tick, a.id) < (o.raised_tick, o.id)) } };
                    if better { best = Some(i); }
                }
                match best { Some(i) => i, None => return false }
            }
        };
        let done = match self.alerts[idx].state {
            AlertState::Unacknowledged if self.meta[idx].live => { self.meta[idx].acked = true; self.set_state(idx, AlertState::Acknowledged, out); true }
            AlertState::Cleared => { self.meta[idx].acked = true; self.set_state(idx, AlertState::LogOnly, out); true }
            _ => false,
        };
        if done { self.update_blink(vessel); }
        done
    }

    pub fn silence(&mut self, vessel: EntityId, tick: u64, out: &mut Vec<AlertChange>) {
        let until = tick + self.budget.silence_s as u64 * TICK_HZ as u64;
        match self.silenced_until.iter_mut().find(|(v, _)| *v == vessel) { Some(e) => e.1 = until, None => self.silenced_until.push((vessel, until)) }
        self.silenced_until.sort();
        out.push(AlertChange::Silenced(vessel, until));
    }

    /// Alerts a vessel's strip and log show now (shelved and log-only included, flagged by state).
    /// Unit: `sys-alerts`.
    /// Rectified history is not repeated here (it reached the client as events when it happened), so the
    /// list stays bounded however long the sortie runs.
    pub fn for_vessel(&self, vessel: EntityId) -> Vec<AlertView> {
        let mut v: Vec<AlertView> = self.alerts.iter().enumerate()
            .filter(|(i, a)| a.vessel == vessel && (self.meta.get(*i).map(|m| m.live).unwrap_or(true) || a.state == AlertState::Cleared))
            .map(|(_, a)| *a).collect();
        v.sort_by_key(|a| a.id);
        v
    }

    /// First annunciations for a vessel in the last `window_s` seconds (budget checks in scripted battles).
    /// Annunciations older than 20 minutes are pruned, so longer windows count the last 20 minutes.
    /// Unit: `sys-alerts`.
    pub fn raised_in_window(&self, vessel: EntityId, tick: u64, window_s: f64) -> u32 {
        if !(window_s > 0.0) { return 0; }
        let window = libm::ceil(window_s.min(1.0e9) * TICK_HZ as f64) as u64;
        self.raised_ticks.iter().filter(|&&(v, t)| v == vessel && t <= tick && t + window > tick).count() as u32
    }

    /// Record which compartment each part and conduit of a vessel sits in, so Fire and Flooding Alarms can
    /// absorb the part, conduit and ammunition alerts of their compartment. The Sim calls this with each
    /// crewed vessel before `update` (cheap; refreshes after compartment recomputes).
    pub fn note_layout(&mut self, vessel: &sim_core::vessel::Vessel) {
        let mut parts: Vec<(PartSlot, CompartmentId)> = vessel.parts.iter().filter_map(|p| p.compartment.map(|c| (p.slot, c))).collect();
        parts.sort();
        let conduits = vessel.networks.conduits.iter().enumerate().take(u16::MAX as usize).map(|(i, k)| (i as u16, k.from.min(k.to))).collect();
        let layout = Layout { vessel: vessel.id, parts, conduits };
        match self.layouts.binary_search_by(|l| l.vessel.cmp(&vessel.id)) { Ok(i) => self.layouts[i] = layout, Err(i) => self.layouts.insert(i, layout) }
    }

    // ---- internals ----

    fn ensure_meta(&mut self) {
        let n = self.alerts.len() as u32;
        if self.meta.len() == self.alerts.len() && self.live.iter().chain(&self.recent).all(|&i| i < n) { return; }
        // Restored from data without bookkeeping: rebuild it from the views.
        self.meta = self.alerts.iter().map(|a| AlertMeta { key: key_of(&a.kind), live: matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged | AlertState::Shelved), acked: a.state == AlertState::Acknowledged, rectified_tick: 0, prior: AlertState::Unacknowledged }).collect();
        self.live = (0..self.alerts.len() as u32).filter(|&i| self.meta[i as usize].live).collect();
        self.recent.clear();
        self.next_id = self.next_id.max(self.alerts.iter().map(|a| a.id.0 + 1).max().unwrap_or(0));
    }

    fn index_of(&self, id: AlertId) -> Option<usize> { self.alerts.binary_search_by(|a| a.id.cmp(&id)).ok() }

    fn insert_live(&mut self, i: u32) { if let Err(p) = self.live.binary_search(&i) { self.live.insert(p, i); } }

    fn set_state(&mut self, idx: usize, state: AlertState, out: &mut Vec<AlertChange>) {
        if self.alerts[idx].state == state { return; }
        if self.alerts[idx].state == AlertState::Shelved { let id = self.alerts[idx].id; self.shelved_until.retain(|&(s, _)| s != id); }
        self.alerts[idx].state = state;
        if state != AlertState::Unacknowledged { self.alerts[idx].blink_fast = false; }
        out.push(AlertChange::State(self.alerts[idx].id, state));
    }

    fn escalate(&mut self, idx: usize, p: AlertPriority, sub: Subsystem, officer: Option<OfficerRole>, out: &mut Vec<AlertChange>) {
        if self.alerts[idx].state == AlertState::Shelved { let id = self.alerts[idx].id; self.shelved_until.retain(|&(s, _)| s != id); }
        let a = &mut self.alerts[idx];
        a.priority = p;
        a.subsystem = sub;
        a.officer = officer;
        a.state = AlertState::Unacknowledged;
        self.meta[idx].acked = false;
        out.push(AlertChange::Escalated(*a));
    }

    fn rectify(&mut self, idx: usize, tick: u64, out: &mut Vec<AlertChange>) {
        self.meta[idx].live = false;
        self.meta[idx].rectified_tick = tick;
        let a = &self.alerts[idx];
        let state = match a.state {
            AlertState::Unacknowledged if a.priority >= AlertPriority::Alarm => AlertState::Cleared,
            AlertState::Cleared => AlertState::Cleared,
            _ => AlertState::LogOnly,
        };
        self.set_state(idx, state, out);
    }

    fn raise(&mut self, vessel: EntityId, key: AlertKey, kind: AlertKind, tick: u64, out: &mut Vec<AlertChange>) {
        let (priority, subsystem, officer) = classify(&kind);
        let id = AlertId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let flood = self.flood.contains(&vessel);
        let combined = flood && match priority {
            AlertPriority::Caution => true,
            AlertPriority::Warning => self.live.iter().any(|&i| { let a = &self.alerts[i as usize]; a.vessel == vessel && a.subsystem == subsystem && a.priority == AlertPriority::Warning && matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged) }),
            _ => false,
        };
        let shelved = !combined && priority <= AlertPriority::Warning && self.absorbed(vessel, &kind);
        let state = if combined { AlertState::LogOnly } else if shelved { AlertState::Shelved } else { AlertState::Unacknowledged };
        let view = AlertView { id, vessel, priority, subsystem, kind, state, raised_tick: tick, officer, blink_fast: false };
        let idx = self.alerts.len();
        self.alerts.push(view);
        self.meta.push(AlertMeta { key, live: true, acked: false, rectified_tick: 0, prior: AlertState::Unacknowledged });
        self.insert_live(idx as u32);
        if shelved { self.shelved_until.push((id, tick + self.shelve_ticks())); }
        if state == AlertState::Unacknowledged { self.raised_ticks.push((vessel, tick)); }
        out.push(AlertChange::Raised(view));
    }

    fn shelve_ticks(&self) -> u64 { self.budget.shelve_s as u64 * TICK_HZ as u64 }

    /// Compartment of an alert, for absorption.
    fn compartment_of(&self, vessel: EntityId, kind: &AlertKind) -> Option<CompartmentId> {
        let layout = || self.layouts.binary_search_by(|l| l.vessel.cmp(&vessel)).ok().map(|i| &self.layouts[i]);
        let part = |s: PartSlot| layout().and_then(|l| l.parts.binary_search_by(|p| p.0.cmp(&s)).ok().map(|i| l.parts[i].1));
        match *kind {
            AlertKind::Flooding { compartment } | AlertKind::FloodingSevere { compartment } | AlertKind::Fire { compartment } => Some(compartment),
            AlertKind::MagazineFire { magazine, .. } => part(magazine),
            AlertKind::PartHeavyDamage { slot } | AlertKind::PartDestroyed { slot } | AlertKind::PartOnBuffer { slot, .. } | AlertKind::PartLocalControl { slot } | AlertKind::AmmunitionLow { slot } => part(slot),
            AlertKind::ConduitCut { conduit } => layout().and_then(|l| l.conduits.iter().find(|c| c.0 == conduit).map(|c| c.1)),
            _ => None,
        }
    }

    /// True when a live Emergency suppresses this vessel's lower alerts, or a live Fire or Flooding Alarm of
    /// the same compartment absorbs it.
    fn absorbed(&self, vessel: EntityId, kind: &AlertKind) -> bool {
        let c = self.compartment_of(vessel, kind);
        self.live.iter().any(|&i| {
            let a = &self.alerts[i as usize];
            if a.vessel != vessel { return false; }
            if a.priority == AlertPriority::Emergency { return true; }
            a.priority >= AlertPriority::Alarm && c.is_some() && matches!(a.kind, AlertKind::Fire { .. } | AlertKind::Flooding { .. } | AlertKind::FloodingSevere { .. }) && self.compartment_of(vessel, &a.kind) == c
        })
    }

    fn update_flood(&mut self, vessel: EntityId, tick: u64, out: &mut Vec<AlertChange>) {
        let window = 10 * 60 * TICK_HZ as u64;
        let count = self.raised_ticks.iter().filter(|&&(v, t)| v == vessel && t + window > tick).count();
        let on = count > self.budget.flood_per_10_min as usize;
        let was = self.flood.contains(&vessel);
        if on == was { return; }
        if !on {
            self.flood.retain(|&v| v != vessel);
            out.push(AlertChange::Flood(vessel, false));
            return;
        }
        self.flood.push(vessel);
        self.flood.sort();
        out.push(AlertChange::Flood(vessel, true));
        // Cautions go to the log; Warnings combine per subsystem into the oldest visible one.
        let mut kept: Vec<Subsystem> = Vec::new();
        for i in self.live.clone() {
            let idx = i as usize;
            let a = self.alerts[idx];
            if a.vessel != vessel || !matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged) { continue; }
            match a.priority {
                AlertPriority::Caution => self.set_state(idx, AlertState::LogOnly, out),
                AlertPriority::Warning => if kept.contains(&a.subsystem) { self.set_state(idx, AlertState::LogOnly, out) } else { kept.push(a.subsystem) },
                _ => {}
            }
        }
    }

    fn update_shelving(&mut self, vessel: EntityId, tick: u64, out: &mut Vec<AlertChange>) {
        for i in self.live.clone() {
            let idx = i as usize;
            let a = self.alerts[idx];
            if a.vessel != vessel || a.priority > AlertPriority::Warning { continue; }
            match a.state {
                AlertState::Unacknowledged | AlertState::Acknowledged if self.absorbed(vessel, &a.kind) => {
                    self.meta[idx].prior = a.state;
                    self.set_state(idx, AlertState::Shelved, out);
                    self.shelved_until.push((a.id, tick + self.shelve_ticks()));
                }
                AlertState::Shelved => {
                    let until = self.shelved_until.iter().find(|s| s.0 == a.id).map(|s| s.1).unwrap_or(0);
                    if until > tick { continue; }
                    if self.absorbed(vessel, &a.kind) {
                        let next = tick + self.shelve_ticks();
                        match self.shelved_until.iter_mut().find(|s| s.0 == a.id) { Some(s) => s.1 = next, None => self.shelved_until.push((a.id, next)) }
                    } else {
                        let prior = self.meta[idx].prior;
                        self.set_state(idx, prior, out);
                    }
                }
                _ => {}
            }
        }
        // Rectified shelved items leave the shelf.
        let alerts = &self.alerts;
        let meta = &self.meta;
        self.shelved_until.retain(|&(id, _)| alerts.binary_search_by(|a| a.id.cmp(&id)).ok().map(|i| meta[i].live && alerts[i].state == AlertState::Shelved).unwrap_or(false));
        self.shelved_until.sort();
    }

    /// Only the newest top unacknowledged Emergency or Alarm blinks fast (L1687).
    fn update_blink(&mut self, vessel: EntityId) {
        let mut best: Option<usize> = None;
        for &i in &self.live {
            let idx = i as usize;
            let a = &self.alerts[idx];
            if a.vessel != vessel { continue; }
            if a.blink_fast { self.alerts[idx].blink_fast = false; }
            let a = &self.alerts[idx];
            if a.state != AlertState::Unacknowledged || a.priority < AlertPriority::Alarm { continue; }
            let better = match best { None => true, Some(b) => { let o = &self.alerts[b]; a.priority > o.priority || (a.priority == o.priority && (a.raised_tick, a.id) > (o.raised_tick, o.id)) } };
            if better { best = Some(idx); }
        }
        if let Some(b) = best { self.alerts[b].blink_fast = true; }
    }
}

/// Collect the conditions present on one ship this tick from its vessel state, stage states and picture:
/// inbound weapons, flooding, fires, magazine fires, stability, propulsion and steering loss, power shortage,
/// cut conduits, part damage, buffers and local control, fuel and ammunition, ESM warning, new Hostile tracks,
/// shoal ahead, grounded, passengers lost, interrupted actions, greyed scripts. Unit: `sys-alerts`.
///
/// Shoal ahead, grounded, interrupted station actions, host overload and hunter groups need state this
/// function is not given (terrain, special operations, the overload ladder, the theatre): the Sim pushes
/// those conditions itself into the same list before `AlertManager::update`.
pub fn conditions(ship: &crate::ship::Ship, vessel: &sim_core::vessel::Vessel, picture: &crate::tracks::SidePicture, ctx: &SimCtx<'_>, out: &mut Vec<Condition>) {
    if matches!(vessel.status, VesselStatus::Lost(_) | VesselStatus::Wreck) { return; }
    let start = out.len();
    let tuning = ctx.tuning;
    let tick = ctx.tick.0;
    let id = vessel.id;
    let mut push = |kind: AlertKind| out.push(Condition { vessel: id, kind });
    let func = |p: &sim_core::parts::PartRuntime| ctx.catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function);

    // Inbound weapons: tracks the engagement stage marked inbound whose closest approach to this hull is
    // within the inbound distance inside the horizon. A track estimated under water is a torpedo.
    let own = vessel.kin.pose.pos;
    let own_v = vessel.kin.lin_vel;
    let reach = tuning.doctrine.inbound_cpa_m.max(vessel.dims[0] * 0.5);
    for t in picture.tracks.iter().filter(|t| t.is_live() && t.inbound && t.domain == Domain::Weapon) {
        let age = tick.saturating_sub(t.last_update_tick) as f64 / TICK_HZ as f64;
        let pos = t.pos + t.vel * age;
        if let Some((d, _)) = cpa(pos - own, t.vel - own_v, tuning.doctrine.inbound_horizon_s) {
            if d <= reach { push(if pos.y < -1.0 { AlertKind::TorpedoInbound { track: t.id } } else { AlertKind::WeaponInbound { track: t.id } }); }
        }
    }

    // Flooding and fire per compartment. Water let in on purpose (open flood valve, flooded magazine) is not
    // an alarm.
    let flooding_mags: BTreeSet<PartSlot> = vessel.controls.flood_magazines.iter().copied().collect();
    let mag_flooded = |s: PartSlot| flooding_mags.contains(&s) || vessel.parts.iter().find(|p| p.slot == s).map(|p| matches!(p.state, PartState::Magazine { flooded: true, .. }) || !p.works()).unwrap_or(true);
    for c in &vessel.compartments.compartments {
        let intentional = c.flood_valve_open || c.magazine.map(|m| mag_flooded(m)).unwrap_or(false);
        if !intentional && (c.fill_fraction() >= 0.05 || c.water_m3 >= 2.0) {
            push(if c.fill_fraction() > tuning.flooding.flooded_fraction { AlertKind::FloodingSevere { compartment: c.id } } else { AlertKind::Flooding { compartment: c.id } });
        }
        if c.is_burning() {
            match c.magazine.filter(|&m| !mag_flooded(m)) {
                Some(m) => {
                    let teams = vessel.crew.teams.iter().filter(|t| t.order == TeamOrder::FightFire(c.id)).count().min(255) as u8;
                    let left = sim_core::fire::cookoff_forecast(vessel, m, teams, &tuning.fire).map(|f| f.0)
                        .unwrap_or(tuning.fire.cookoff_delay_s - c.fire_ticks as f64 / TICK_HZ as f64);
                    push(AlertKind::MagazineFire { magazine: m, cookoff_in_s: if left.is_finite() { left.max(0.0) } else { 0.0 } });
                }
                None => push(AlertKind::Fire { compartment: c.id }),
            }
        }
    }

    // Stability.
    let (heel, _) = vessel.heel_trim();
    let heel_deg = libm::fabs(heel) * 180.0 / core::f64::consts::PI;
    if vessel.capsize_ticks > 0 || heel_deg >= tuning.stability.capsize_heel_deg * CAPSIZE_WARN_FRACTION { push(AlertKind::CapsizeImminent); }
    let (water, volume) = vessel.compartments.compartments.iter().fold((0.0, 0.0), |(w, v), c| (w + c.water_m3.max(0.0), v + c.volume_m3.max(0.0)));
    if volume > 0.0 && water / volume >= SINKING_FRACTION { push(AlertKind::SinkingImminent); }

    // Propulsion and steering.
    let (mut engines, mut engines_ok, mut props, mut props_ok, mut steer, mut steer_ok) = (0, 0, 0, 0, 0, 0);
    let powered = |p: &sim_core::parts::PartRuntime| p.links.power != LinkState::Dead && !p.links.shed;
    for p in &vessel.parts {
        match func(p) {
            Some(PartFunction::Engine(e)) => {
                engines += 1;
                let fed = if e.kind == content::part::EngineKind::ElectricMotor { powered(p) } else { p.links.fuel != LinkState::Dead };
                if p.works() && fed { engines_ok += 1; }
            }
            Some(PartFunction::Propulsor(d)) => {
                props += 1;
                if p.works() { props_ok += 1; }
                if d.steer_max_deg > 0.0 { steer += 1; if p.works() { steer_ok += 1; } }
            }
            Some(PartFunction::Rudder { .. }) => { steer += 1; if p.works() && powered(p) { steer_ok += 1; } }
            _ => {}
        }
    }
    if vessel.status == VesselStatus::Disabled && !vessel.struck || (props > 0 && props_ok == 0) || (engines > 0 && engines_ok == 0) { push(AlertKind::PropulsionLost); }
    if steer > 0 && steer_ok == 0 { push(AlertKind::SteeringLost); }

    // Power, conduits, part state.
    let shed = vessel.parts.iter().filter(|p| p.links.shed).count().min(u16::MAX as usize) as u16;
    if shed > 0 { push(AlertKind::PowerShortage { shed }); }
    for (i, k) in vessel.networks.conduits.iter().enumerate().take(u16::MAX as usize) {
        if k.state == ConduitState::Cut { push(AlertKind::ConduitCut { conduit: i as u16 }); }
    }
    for p in &vessel.parts {
        let Some(f) = func(p) else { continue };
        let reportable = !matches!(f, PartFunction::Structure | PartFunction::Ballast { .. } | PartFunction::Door | PartFunction::Berths { .. } | PartFunction::PassengerBerths { .. });
        if reportable {
            match p.level { DamageLevel::Heavy => push(AlertKind::PartHeavyDamage { slot: p.slot }), DamageLevel::Destroyed => push(AlertKind::PartDestroyed { slot: p.slot }), _ => {} }
        }
        if !p.works() { continue; }
        let mut buffer = f64::INFINITY;
        if p.links.power == LinkState::Buffer { buffer = buffer.min(p.links.battery_s); }
        if p.links.fuel == LinkState::Buffer { buffer = buffer.min(p.links.day_tank_s); }
        if buffer.is_finite() { push(AlertKind::PartOnBuffer { slot: p.slot, seconds_left: buffer.max(0.0) }); }
        if p.links.data == LinkState::LocalControl { push(AlertKind::PartLocalControl { slot: p.slot }); }
        match (f, &p.state) {
            (PartFunction::Magazine { rounds: cap, .. }, PartState::Magazine { rounds, flooded: false, .. }) if *cap > 0 && (*rounds as f64) < *cap as f64 * AMMO_LOW_FRACTION => push(AlertKind::AmmunitionLow { slot: p.slot }),
            (PartFunction::Launcher(l), PartState::Launcher { ready: 0, .. }) if l.reload_s <= 0.0 => push(AlertKind::AmmunitionLow { slot: p.slot }),
            _ => {}
        }
    }
    // Read the tanks directly: the Engineer's state is not refreshed when a player subgraph replaces it.
    let fuel = crate::officers::engineer::fuel_fraction(&vessel.parts, ctx.catalogue);
    if fuel.is_finite() && fuel < FUEL_LOW_FRACTION { push(AlertKind::FuelLow { fraction: fuel.max(0.0) }); }

    // Sensors and picture (player knowledge only).
    if ship.stages.emcon.esm_warning { push(AlertKind::EsmWarning); }
    // Every live Hostile contact is a condition; the manager moves it to the log once it is no longer new.
    for t in picture.tracks.iter().filter(|t| t.is_live() && t.stance == Stance::Hostile && t.domain != Domain::Weapon) {
        push(AlertKind::NewHostileTrack { track: t.id });
    }

    // Crew, passengers, scripts.
    if vessel.crew.surviving_berths < vessel.crew.original_berths { push(AlertKind::CrewCasualties); }
    if vessel.crew.passengers_lost > 0 { push(AlertKind::PassengersLost { count: vessel.crew.passengers_lost }); }
    if !ship.graph_state.greyed.is_empty() { push(AlertKind::ScriptGreyedOut); }

    out[start..].sort_by_key(|c| key_of(&c.kind));
}

/// Closest approach of a relative position and velocity within a horizon: (distance, time).
fn cpa(rel: DVec3, vel: DVec3, horizon_s: f64) -> Option<(f64, f64)> {
    let v2 = vel.length_squared();
    let t = if v2 > 1e-9 { (-rel.dot(vel) / v2).clamp(0.0, horizon_s.max(0.0)) } else { 0.0 };
    let d = (rel + vel * t).length();
    if d.is_finite() { Some((d, t)) } else { None }
}

#[cfg(test)]
mod tests;
