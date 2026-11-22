//! Contract-owned (no work unit). The alert model. DESIGN.md L1676-1687 (priorities and state rules, after
//! MSC.302(87)), L1666 (alert budget), L1242-1247 (one sound pattern per sounded priority).
//! Behaviour lives in `crate::alerts` (unit `sys-alerts`).
use crate::ids::{AlertId, TrackId};
use content::enums::OfficerRole;
use content::ids::{CompartmentId, PartSlot};
use serde::{Deserialize, Serialize};
use sim_core::EntityId;

/// Ascending urgency. Each has its own glyph, word and synthesized sound (L1676-1685).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AlertPriority {
    /// Out of the ordinary. Steady badge, one tick, no acknowledgement, self-clears.
    #[default] Caution,
    /// May become hazardous. 1 Hz blink, chime, again at 120 s, acknowledgement required.
    Warning,
    /// Act now: weapon inbound, flooding, fire. Blinks, repeats until acknowledged or rectified.
    Alarm,
    /// Ship loss imminent. Blinks, continuous sound, acknowledgement required.
    Emergency,
}

/// Subsystem an alert belongs to; flood mode combines Warnings per subsystem (L1687).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Subsystem { Threat, Flooding, Fire, Stability, Power, Fuel, Propulsion, Steering, Weapons, Sensors, Data, Crew, Navigation, Mission, System }

/// What the alert is about. The priority of each kind and its escalation thresholds are fixed in
/// `crate::alerts::priority_of`; Warnings escalate by threshold, never by timer (L1687).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum AlertKind {
    WeaponInbound { track: TrackId },
    TorpedoInbound { track: TrackId },
    Flooding { compartment: CompartmentId },
    /// Compartment over 50% flooded (patching no longer possible, L642-646).
    FloodingSevere { compartment: CompartmentId },
    Fire { compartment: CompartmentId },
    MagazineFire { magazine: PartSlot, cookoff_in_s: f64 },
    /// Heel beyond the capsize threshold or reserve buoyancy nearly gone: ship loss imminent.
    CapsizeImminent,
    SinkingImminent,
    PropulsionLost,
    SteeringLost,
    PowerShortage { shed: u16 },
    ConduitCut { conduit: u16 },
    PartHeavyDamage { slot: PartSlot },
    PartDestroyed { slot: PartSlot },
    /// A part fell back to its buffer or to local control (L394).
    PartOnBuffer { slot: PartSlot, seconds_left: f64 },
    PartLocalControl { slot: PartSlot },
    FuelLow { fraction: f64 },
    AmmunitionLow { slot: PartSlot },
    /// "Enemy ESM has probably detected you" (L510).
    EsmWarning,
    NewHostileTrack { track: TrackId },
    ShoalAhead,
    Grounded,
    CrewCasualties,
    PassengersLost { count: u16 },
    ActionInterrupted,
    ScriptGreyedOut,
    /// Host overload ladder moved (L1019-1029); every change shows in HUD and log.
    HostOverload { rung: u8 },
    HunterGroupApproaching,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertState {
    #[default] Unacknowledged,
    /// Steady until rectified.
    Acknowledged,
    /// Rectified while unacknowledged: Emergency and Alarm show "Cleared"; lower ones go to the log unread.
    Cleared,
    /// Absorbed by a same-compartment Fire or Flooding Alarm, or suppressed by an Emergency; shelved for
    /// 5 minutes or the rest of the sortie (L1687).
    Shelved,
    /// Flood mode sent it to the log (Cautions) or combined it per subsystem (Warnings).
    LogOnly,
}

/// One alert as the strip and the log show it. The strip shows 5 plus "+N"; the log keeps all with times.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlertView {
    pub id: AlertId,
    pub vessel: EntityId,
    pub priority: AlertPriority,
    pub subsystem: Subsystem,
    pub kind: AlertKind,
    pub state: AlertState,
    pub raised_tick: u64,
    /// Officer whose station it belongs to, for the Bridge Board link.
    pub officer: Option<OfficerRole>,
    /// True for the one newest top Emergency or Alarm that blinks fast (L1687).
    pub blink_fast: bool,
}

/// Alert budget numbers (L1666, L1763 fallback): at most 20 new alerts per 20-minute engagement; more than 10
/// in 10 minutes is a flood.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertBudget { pub per_20_min: u16, pub flood_per_10_min: u16, pub silence_s: u16, pub warning_repeat_s: u16, pub shelve_s: u16, pub strip_len: u8 }
impl Default for AlertBudget { fn default() -> Self { Self { per_20_min: 20, flood_per_10_min: 10, silence_s: 30, warning_repeat_s: 120, shelve_s: 300, strip_len: 5 } } }
