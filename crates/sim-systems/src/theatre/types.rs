//! Contract-owned (no work unit). Campaign-layer types shared by the theatre units, net (co-op campaign) and
//! the client (chart table, briefing, after-action report). DESIGN.md L62-73 (core loop), L729-896.
//! Persistent state itself is `content::save::TheatreState`; these are views, commands and events around it.
use crate::prelude::*;
use crate::protocol::config::{Outcome, SortieStats};
use content::campaign::{ConstraintTag, GhostCondition, KillMethod, MapDef, MissionType, OperationKind, StationAction, TallyKey};
use content::campaign::ClassRole;
use content::save::{IntelSpend, TheatreAction};
use content::weather::WeatherState;

/// How sure the briefing is about a threat entry (threat picture graded by confidence, L66).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence { #[default] Possible, Probable, Confirmed }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreatEntry { pub class: Option<ClassId>, pub role: Option<ClassRole>, pub count_min: u8, pub count_max: u8, pub confidence: Confidence, pub ace: Option<AceId> }

/// What the briefing screen shows and Fit Check reads (L66, L743, L801).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Briefing {
    pub operation: OperationId,
    pub kind: OperationKind,
    pub name: TextKey,
    pub text: TextKey,
    pub region: RegionId,
    pub node: TheatreNodeId,
    /// Forecast = the weather the sortie will have: the region's weather at the commit hour (L743).
    pub weather: WeatherState,
    pub time_of_day: TimeOfDay,
    /// Approach distance from spawn to the first objective in km.
    pub distance_km: f64,
    pub threats: Vec<ThreatEntry>,
    pub tags: Vec<ConstraintTag>,
    pub ghosts: Vec<GhostCondition>,
    pub actions: Vec<StationAction>,
    pub recommended_emcon: Option<Emcon>,
    /// Chart of the sortie area and the briefing route (stock Helm follows it in the winnability gate).
    pub map: MapDef,
    pub route: Vec<[f64; 2]>,
    /// Hostile hulls and tonnage committed (at least 5x hulls and 4x tonnage, L94).
    pub committed_hulls: u16,
    pub committed_tonnage_t: f64,
    pub pushback_days: f64,
    pub forced: bool,
    /// Theatre hour after which the offer lapses; `None` = stays.
    pub expires_hour: Option<u64>,
    /// Seed the sortie will use, so Fit Check and the sortie see the same sea.
    pub seed: u64,
}

/// One operation on offer at the chart table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationOffer { pub operation: OperationId, pub kind: OperationKind, pub template: Option<MissionType>, pub name: TextKey, pub node: TheatreNodeId, pub region: RegionId, pub forced: bool, pub transit_hours: u32 }

/// Why theatre time stopped (L741: time runs in 1-hour steps until the next decision or interrupt).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Interrupt { Arrived(TheatreNodeId), RefitDone, WaitDone, OperationOffered(OperationId), ForcedOperation(OperationId), HunterGroupWarning { hours: u32 }, CardArriving { region: RegionId, card: CardId, hours: u32 }, ClockLow { days_left: f64 },
    /// The act ended: `won` false when its clock ran out (`TheatreEvent::ActLost`), true for a win or the open
    /// theatre opening. An act lost in this build's last act starts again in retreat, which the banner says.
    ActEnded { won: bool },
    HullReady(PlayerSlot), Paused }

/// Why regional Alert moved (L749).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertCause { ObservedKill, UnobservedKillNoticed, Sighting, Evidence, QuietDecay, ActStart, SextantSunk }

/// Everything that happens in theatre time, for the chart animation, the log and co-op members.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TheatreEvent {
    HourAdvanced { hour: u64 },
    Interrupted(Interrupt),
    SextantMoved { from: TheatreNodeId, to: TheatreNodeId },
    LaneCut { a: TheatreNodeId, b: TheatreNodeId, until_hour: u64 },
    LaneRestored { a: TheatreNodeId, b: TheatreNodeId },
    DepotEmptied { node: TheatreNodeId },
    SupplyChanged { region: RegionId, supply: f64 },
    PatrolDensityChanged { region: RegionId, factor: f64 },
    AlertChanged { region: RegionId, alert: f64, cause: AlertCause },
    /// Alert reached 3: escorts, air patrols and decoy convoys appear.
    AlertEscalated { region: RegionId },
    HunterGroupSortied { group: u32, arrives_hour: u64, ace: Option<AceId> },
    HunterGroupArrived { group: u32 },
    TallyChanged { key: TallyKey, value: f64 },
    CardQueued { region: RegionId, card: CardId, arrives_hour: u64 },
    CardDelayed { region: RegionId, card: CardId, arrives_hour: u64 },
    CardCancelled { region: RegionId, card: CardId },
    CardArrived { region: RegionId, card: CardId },
    ClockPushedBack { days: f64, hours_left: f64 },
    ClockExpired,
    /// The act was lost: the next act starts in the retreat configuration (L750).
    ActLost { act: Act },
    ActWon { act: Act },
    ActStarted { act: Act, retreat: bool },
    OpenTheatreOpened,
    OperationOffered(OperationId),
    OperationWithdrawn(OperationId),
    OperationUnlocked(OperationId),
    TierOpened(Tier),
    PartUnlocked(PartId),
    ResourcesChanged,
    HullLost { player: PlayerSlot },
    HullRebuilt { player: PlayerSlot },
    RefitStarted { player: PlayerSlot, hours: u32 },
    SextantSunk { relocated_to: TheatreNodeId, ready_hour: u64 },
    /// Ironman: no player hull remains (L828).
    CampaignOver,
    GroupSighted { group: u32, node: TheatreNodeId },
    ConvoyRevealed { convoy: u32 },
    AceRevealed { ace: AceId, node: TheatreNodeId },
    ProposalMade(Proposal),
    ProposalAnswered { proposal: ProposalId, player: PlayerSlot, accept: bool },
    ProposalCommitted { proposal: ProposalId },
    ProposalWithdrawn { proposal: ProposalId },
    Saved { sequence: u64 },
}

/// A chart-table proposal in co-op: any player proposes; it commits when all accept or the host confirms
/// (L71, L839). In single-player a proposal commits at once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proposal { pub id: ProposalId, pub by: PlayerSlot, pub action: TheatreAction, pub accepted: PlayerMask, pub made_hour: u64 }

/// Client to host at the chart table and in the dry dock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TheatreCommand {
    Propose(TheatreAction),
    Respond { proposal: ProposalId, accept: bool },
    /// Host only.
    Confirm(ProposalId),
    Withdraw(ProposalId),
    /// Commit a refit of the sender's hull to this design (cost and time from `content::stats::refit_cost`).
    Refit { blueprint: Box<content::blueprint::Blueprint> },
    Repair { full: bool },
    Rearm,
    Rebuild,
    SpendIntel(IntelSpend),
    /// Pause or resume the theatre time animation (not a simulation pause).
    PauseClock(bool),
    /// Between operations, except under Ironman, which locks all six at the start (L860).
    SetDifficulty(content::campaign::DifficultySliders),
    Save,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TheatreReject { NotHost, NotEnoughSalvage, NotEnoughIntel, NotAtCove, HullNotReady, OperationNotOffered, ForcedOperationPending, IronmanLocked, OverTonnageCap, InvalidBlueprint, NoSuchProposal, UnknownTarget, CampaignOver }

/// What the chart table may show about one Combine group or convoy (only what Intel, sightings or reveals
/// allow; the ledger's truth stays on the host in PvP-free co-op too, though save data is not hidden, L845).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartContact { pub id: u32, pub node: TheatreNodeId, pub destination: Option<TheatreNodeId>, pub convoy: bool, pub hulls: u8, pub ace: Option<AceId>, pub last_seen_hour: u64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegionView { pub region: RegionId, pub open: bool, pub alert_floor: u8, pub supply: f64, pub patrol_factor: f64, pub active_cards: Vec<CardId>, /// Doctrine bulletin: cards on their way with arrival hour (L772).
    pub queued_cards: Vec<(CardId, u64)>, pub combine_control: bool }

/// Everything the chart table draws (L71, L741).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartView {
    pub hour: u64,
    pub act: Act,
    pub retreat: bool,
    pub open_theatre: bool,
    pub clock_hours_left: Option<f64>,
    pub sextant: TheatreNodeId,
    pub coves: Vec<TheatreNodeId>,
    pub regions: Vec<RegionView>,
    /// Cut lanes with the hour they reopen.
    pub cut_lanes: Vec<(TheatreNodeId, TheatreNodeId, u64)>,
    pub contacts: Vec<ChartContact>,
    pub offers: Vec<OperationOffer>,
    pub proposals: Vec<Proposal>,
    pub hunter_warning_hours: Option<u32>,
    pub tallies: Vec<(TallyKey, f64)>,
    pub weather: Vec<(RegionId, WeatherState)>,
    pub time_of_day: TimeOfDay,
}

/// Salvage, Intel and blueprint changes of one sortie with their sources (L811-822).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Payout { pub salvage_wrecks: u32, pub salvage_prizes: u32, pub intel: u32, pub blueprints: Vec<PartId>, pub fuel_l: f64, pub ammo_kg: f64 }

/// The after-action report (L70): fleet-level only, no per-enemy memory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AfterAction {
    pub operation: Option<OperationId>,
    pub outcome: Outcome,
    pub stats: SortieStats,
    pub payout: Payout,
    /// Win-method tallies tracked by doctrine cards, after this sortie.
    pub tallies: Vec<(TallyKey, f64)>,
    pub kills_by_method: Vec<(KillMethod, u16)>,
    /// Counters newly queued on the doctrine bulletin.
    pub queued_cards: Vec<(RegionId, CardId, u64)>,
    pub alert_changes: Vec<(RegionId, f64, AlertCause)>,
    pub clock_pushback_days: f64,
    /// Task-group belief-map overlay samples (x, z, weight) on the sortie map.
    pub belief: Vec<[f32; 3]>,
    pub ghosts: Vec<(GhostCondition, bool)>,
    /// Repair cost outstanding per player in Salvage.
    pub repair_cost: Vec<(PlayerSlot, u32)>,
    pub hulls_lost: Vec<PlayerSlot>,
    pub events: Vec<TheatreEvent>,
    /// Why the sortie ended ([`SortieResult::end_reason`](crate::protocol::config::SortieResult::end_reason)),
    /// for the after-action report's Result table. Last field: co-op sends the report by bincode.
    #[serde(default)]
    pub end_reason: Option<crate::protocol::config::EndReason>,
}

/// One row of the local operation log (L80, L881): user data folder only, never sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationLogRow { pub operation: String, pub template: Option<MissionType>, pub difficulty: String, pub briefing_open_s: f64, pub refit_s: f64, pub splash_s: f64, pub sortie_s: f64, pub recover_s: f64, pub chart_s: f64, pub first_attempt: bool, pub outcome: Outcome, pub income: u32, pub spend: u32, pub ghosts_met: u8 }
