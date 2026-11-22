//! Part of unit `gd-campaign`. The campaign's sentences: theatre events, interrupts, refusals, proposals,
//! briefing tags, ghost conditions and tallies as `Msg` values. Keys live in the `campaign.*` namespace, whose
//! English is `game/i18n/campaign.csv`; [`text_keys`] lists every key emitted here for the coverage test.
//! No English is produced in Rust: names of places, operations, cards, aces, parts and classes travel as nested
//! text keys, player names as untranslated names.
use content::campaign::{CampaignData, ConstraintTag, GhostKind, RangeBand, Reward, TallyKey};
use content::catalogue::Catalogue;
use content::ids::{AceId, CardId, ClassId, OperationId, PartId, PlayerSlot, RegionId, TextKey, TheatreNodeId};
use content::part::PartTag;
use content::save::{IntelSpend, TheatreAction};
use sim_systems::protocol::{Msg, MsgArg, Unit};
use sim_systems::theatre::{AlertCause, Interrupt, TheatreEvent, TheatreReject};

/// Looks up the text keys of the things a sentence names.
pub(crate) struct Names<'a> {
    pub campaign: &'a CampaignData,
    pub catalogue: &'a Catalogue,
    /// Player names by theatre slot.
    pub players: Vec<String>,
}

fn key(k: &TextKey) -> MsgArg { MsgArg::Key(k.clone()) }
fn word(k: &str) -> MsgArg { MsgArg::Key(TextKey::new(k)) }
fn m(k: &str) -> Msg { Msg::new(k) }
fn hours(v: u64) -> MsgArg { MsgArg::Num { value: v.min(1 << 40) as f64, unit: Unit::Hours } }
fn finite(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }

impl<'a> Names<'a> {
    pub fn node(&self, id: &TheatreNodeId) -> MsgArg {
        self.campaign.theatre.nodes.iter().find(|n| &n.id == id).map_or_else(|| MsgArg::Name(id.as_str().to_string()), |n| key(&n.name))
    }
    pub fn region(&self, id: &RegionId) -> MsgArg {
        self.campaign.theatre.regions.iter().find(|r| &r.id == id).map_or_else(|| MsgArg::Name(id.as_str().to_string()), |r| key(&r.name))
    }
    pub fn operation(&self, id: &OperationId) -> MsgArg {
        self.campaign.operations.get(id).map_or_else(|| MsgArg::Name(id.as_str().to_string()), |o| key(&o.name))
    }
    pub fn card(&self, id: &CardId) -> MsgArg {
        self.campaign.cards.get(id).map_or_else(|| MsgArg::Name(id.as_str().to_string()), |c| key(&c.name))
    }
    pub fn ace(&self, id: &AceId) -> MsgArg {
        self.campaign.aces.get(id).map_or_else(|| MsgArg::Name(id.as_str().to_string()), |a| key(&a.name))
    }
    pub fn part(&self, id: &PartId) -> MsgArg {
        self.catalogue.part(id).map_or_else(|| MsgArg::Name(id.as_str().to_string()), |p| key(&p.name))
    }
    pub fn class(&self, id: &ClassId) -> MsgArg {
        self.campaign.classes.get(id).map_or_else(|| MsgArg::Name(id.as_str().to_string()), |c| key(&c.name))
    }
    /// A player's name; an unnamed or unknown slot reads as its number.
    pub fn player(&self, p: PlayerSlot) -> MsgArg {
        match self.players.get(p.0 as usize).filter(|n| !n.trim().is_empty()) {
            Some(n) => MsgArg::Name(n.clone()),
            None => MsgArg::Int(p.0 as i64 + 1),
        }
    }
}

// ---- Refusals ----

/// Why the theatre refused a decision.
pub(crate) fn reject_msg(r: TheatreReject) -> Msg {
    m(match r {
        TheatreReject::NotHost => "campaign.reject.not_host",
        TheatreReject::NotEnoughSalvage => "campaign.reject.not_enough_salvage",
        TheatreReject::NotEnoughIntel => "campaign.reject.not_enough_intel",
        TheatreReject::NotAtCove => "campaign.reject.not_at_cove",
        TheatreReject::HullNotReady => "campaign.reject.hull_not_ready",
        TheatreReject::OperationNotOffered => "campaign.reject.operation_not_offered",
        TheatreReject::ForcedOperationPending => "campaign.reject.forced_operation_pending",
        TheatreReject::IronmanLocked => "campaign.reject.ironman_locked",
        TheatreReject::OverTonnageCap => "campaign.reject.over_tonnage_cap",
        TheatreReject::InvalidBlueprint => "campaign.reject.invalid_blueprint",
        TheatreReject::NoSuchProposal => "campaign.reject.no_such_proposal",
        TheatreReject::UnknownTarget => "campaign.reject.unknown_target",
        TheatreReject::CampaignOver => "campaign.reject.campaign_over",
    })
}

// ---- Interrupts ----

pub(crate) fn interrupt_msg(i: &Interrupt, n: &Names) -> Msg {
    match i {
        Interrupt::Arrived(node) => m("campaign.interrupt.arrived").arg(n.node(node)),
        Interrupt::RefitDone => m("campaign.interrupt.refit_done"),
        Interrupt::WaitDone => m("campaign.interrupt.wait_done"),
        Interrupt::OperationOffered(op) => m("campaign.interrupt.operation_offered").arg(n.operation(op)),
        Interrupt::ForcedOperation(op) => m("campaign.interrupt.forced_operation").arg(n.operation(op)),
        Interrupt::HunterGroupWarning { hours: h } => m("campaign.interrupt.hunter_group_warning").arg(hours(*h as u64)),
        Interrupt::CardArriving { region, card, hours: h } => m("campaign.interrupt.card_arriving").arg(n.card(card)).arg(n.region(region)).arg(hours(*h as u64)),
        Interrupt::ClockLow { days_left } => m("campaign.interrupt.clock_low").num(finite(*days_left).max(0.0), Unit::None),
        Interrupt::ActEnded => m("campaign.interrupt.act_ended"),
        Interrupt::HullReady(p) => m("campaign.interrupt.hull_ready").arg(n.player(*p)),
        Interrupt::Paused => m("campaign.interrupt.paused"),
    }
}

/// `Bp.INTERRUPT_*` of an interrupt (the order of the constants in `crate::enums`).
pub(crate) fn interrupt_kind(i: &Interrupt) -> i64 {
    match i {
        Interrupt::Arrived(_) => 0,
        Interrupt::RefitDone => 1,
        Interrupt::WaitDone => 2,
        Interrupt::OperationOffered(_) => 3,
        Interrupt::ForcedOperation(_) => 4,
        Interrupt::HunterGroupWarning { .. } => 5,
        Interrupt::CardArriving { .. } => 6,
        Interrupt::ClockLow { .. } => 7,
        Interrupt::ActEnded => 8,
        Interrupt::HullReady(_) => 9,
        Interrupt::Paused => 10,
    }
}

// ---- Theatre events ----

/// The snake_case variant name of an event (`TheatreEventDict.kind`).
pub(crate) fn event_kind(e: &TheatreEvent) -> &'static str {
    use TheatreEvent as E;
    match e {
        E::HourAdvanced { .. } => "hour_advanced",
        E::Interrupted(_) => "interrupted",
        E::SextantMoved { .. } => "sextant_moved",
        E::LaneCut { .. } => "lane_cut",
        E::LaneRestored { .. } => "lane_restored",
        E::DepotEmptied { .. } => "depot_emptied",
        E::SupplyChanged { .. } => "supply_changed",
        E::PatrolDensityChanged { .. } => "patrol_density_changed",
        E::AlertChanged { .. } => "alert_changed",
        E::AlertEscalated { .. } => "alert_escalated",
        E::HunterGroupSortied { .. } => "hunter_group_sortied",
        E::HunterGroupArrived { .. } => "hunter_group_arrived",
        E::TallyChanged { .. } => "tally_changed",
        E::CardQueued { .. } => "card_queued",
        E::CardDelayed { .. } => "card_delayed",
        E::CardCancelled { .. } => "card_cancelled",
        E::CardArrived { .. } => "card_arrived",
        E::ClockPushedBack { .. } => "clock_pushed_back",
        E::ClockExpired => "clock_expired",
        E::ActLost { .. } => "act_lost",
        E::ActWon { .. } => "act_won",
        E::ActStarted { .. } => "act_started",
        E::OpenTheatreOpened => "open_theatre_opened",
        E::OperationOffered(_) => "operation_offered",
        E::OperationWithdrawn(_) => "operation_withdrawn",
        E::OperationUnlocked(_) => "operation_unlocked",
        E::TierOpened(_) => "tier_opened",
        E::PartUnlocked(_) => "part_unlocked",
        E::ResourcesChanged => "resources_changed",
        E::HullLost { .. } => "hull_lost",
        E::HullRebuilt { .. } => "hull_rebuilt",
        E::RefitStarted { .. } => "refit_started",
        E::SextantSunk { .. } => "sextant_sunk",
        E::CampaignOver => "campaign_over",
        E::GroupSighted { .. } => "group_sighted",
        E::ConvoyRevealed { .. } => "convoy_revealed",
        E::AceRevealed { .. } => "ace_revealed",
        E::ProposalMade(_) => "proposal_made",
        E::ProposalAnswered { .. } => "proposal_answered",
        E::ProposalCommitted { .. } => "proposal_committed",
        E::ProposalWithdrawn { .. } => "proposal_withdrawn",
        E::Saved { .. } => "saved",
    }
}

/// Every event kind, for the CSV coverage test.
pub(crate) const EVENT_KINDS: &[&str] = &[
    "hour_advanced", "sextant_moved", "lane_cut", "lane_restored", "depot_emptied", "supply_changed",
    "patrol_density_changed", "alert_changed", "alert_escalated", "hunter_group_sortied", "hunter_group_arrived",
    "tally_changed", "card_queued", "card_delayed", "card_cancelled", "card_arrived", "clock_pushed_back",
    "clock_expired", "act_lost", "act_won", "act_started", "open_theatre_opened", "operation_offered",
    "operation_withdrawn", "operation_unlocked", "tier_opened", "part_unlocked", "resources_changed", "hull_lost",
    "hull_rebuilt", "refit_started", "sextant_sunk", "campaign_over", "group_sighted", "convoy_revealed",
    "ace_revealed", "proposal_made", "proposal_committed", "proposal_withdrawn", "saved",
];

fn act_number(a: content::enums::Act) -> MsgArg { MsgArg::Int(crate::enums::GdEnum::to_gd(a) + 1) }

/// The log line of an event. `hour` is the theatre hour it happened at (for "for N days" spans).
pub(crate) fn event_msg(e: &TheatreEvent, hour: u64, n: &Names) -> Msg {
    use TheatreEvent as E;
    let ek = |k: &str| m(&format!("campaign.event.{k}"));
    match e {
        E::HourAdvanced { hour } => ek("hour_advanced").arg(MsgArg::Int(*hour as i64)),
        E::Interrupted(i) => interrupt_msg(i, n),
        E::SextantMoved { from, to } => {
            if from.as_str().is_empty() { ek("sextant_placed").arg(n.node(to)) } else { ek("sextant_moved").arg(n.node(from)).arg(n.node(to)) }
        }
        E::LaneCut { a, b, until_hour } => ek("lane_cut").arg(n.node(a)).arg(n.node(b)).arg(MsgArg::Int(until_hour.saturating_sub(hour).div_ceil(24) as i64)),
        E::LaneRestored { a, b } => ek("lane_restored").arg(n.node(a)).arg(n.node(b)),
        E::DepotEmptied { node } => ek("depot_emptied").arg(n.node(node)),
        E::SupplyChanged { region, supply } => ek("supply_changed").arg(n.region(region)).arg(MsgArg::Int(finite(*supply).clamp(0.0, 100.0).round() as i64)),
        E::PatrolDensityChanged { region, factor } => ek("patrol_density_changed").arg(n.region(region)).num((finite(*factor).max(0.0) * 100.0).round(), Unit::Percent),
        E::AlertChanged { region, alert, cause } => ek("alert_changed").arg(n.region(region)).arg(MsgArg::Int(sim_systems::theatre::alert::alert_floor(finite(*alert)) as i64)).arg(word(cause_key(*cause))),
        E::AlertEscalated { region } => ek("alert_escalated").arg(n.region(region)),
        E::HunterGroupSortied { arrives_hour, ace, .. } => match ace {
            Some(a) => ek("hunter_group_sortied_ace").arg(n.ace(a)).arg(hours(arrives_hour.saturating_sub(hour))),
            None => ek("hunter_group_sortied").arg(hours(arrives_hour.saturating_sub(hour))),
        },
        E::HunterGroupArrived { .. } => ek("hunter_group_arrived"),
        E::TallyChanged { key, value } => ek("tally_changed").arg(word(&tally_word(key))).num((finite(*value) * 10.0).round() / 10.0, Unit::None),
        E::CardQueued { region, card, arrives_hour } => ek("card_queued").arg(n.card(card)).arg(n.region(region)).arg(hours(arrives_hour.saturating_sub(hour))),
        E::CardDelayed { region, card, arrives_hour } => ek("card_delayed").arg(n.card(card)).arg(n.region(region)).arg(hours(arrives_hour.saturating_sub(hour))),
        E::CardCancelled { region, card } => ek("card_cancelled").arg(n.card(card)).arg(n.region(region)),
        E::CardArrived { region, card } => ek("card_arrived").arg(n.card(card)).arg(n.region(region)),
        E::ClockPushedBack { days, .. } => ek("clock_pushed_back").arg(hours((finite(*days).max(0.0) * 24.0).round() as u64)),
        E::ClockExpired => ek("clock_expired"),
        E::ActLost { act } => ek("act_lost").arg(act_number(*act)),
        E::ActWon { act } => ek("act_won").arg(act_number(*act)),
        E::ActStarted { act, retreat } => if *retreat { ek("act_started_retreat").arg(act_number(*act)) } else { ek("act_started").arg(act_number(*act)) },
        E::OpenTheatreOpened => ek("open_theatre_opened"),
        E::OperationOffered(op) => ek("operation_offered").arg(n.operation(op)),
        E::OperationWithdrawn(op) => ek("operation_withdrawn").arg(n.operation(op)),
        E::OperationUnlocked(op) => ek("operation_unlocked").arg(n.operation(op)),
        E::TierOpened(t) => ek("tier_opened").arg(MsgArg::Int(crate::enums::GdEnum::to_gd(*t) + 1)),
        E::PartUnlocked(p) => ek("part_unlocked").arg(n.part(p)),
        E::ResourcesChanged => ek("resources_changed"),
        E::HullLost { player } => ek("hull_lost").arg(n.player(*player)),
        E::HullRebuilt { player } => ek("hull_rebuilt").arg(n.player(*player)),
        E::RefitStarted { player, hours: h } => ek("refit_started").arg(n.player(*player)).arg(hours(*h as u64)),
        E::SextantSunk { relocated_to, ready_hour } => ek("sextant_sunk").arg(n.node(relocated_to)).arg(hours(ready_hour.saturating_sub(hour))),
        E::CampaignOver => ek("campaign_over"),
        E::GroupSighted { node, .. } => ek("group_sighted").arg(n.node(node)),
        E::ConvoyRevealed { .. } => ek("convoy_revealed"),
        E::AceRevealed { ace, node } => ek("ace_revealed").arg(n.ace(ace)).arg(n.node(node)),
        E::ProposalMade(p) => ek("proposal_made").arg(n.player(p.by)),
        E::ProposalAnswered { player, accept, .. } => if *accept { ek("proposal_accepted").arg(n.player(*player)) } else { ek("proposal_declined").arg(n.player(*player)) },
        E::ProposalCommitted { .. } => ek("proposal_committed"),
        E::ProposalWithdrawn { .. } => ek("proposal_withdrawn"),
        E::Saved { .. } => ek("saved"),
    }
}

/// Events the theatre log keeps. Hour ticks, interrupts (their own signal), store changes (the resources
/// bar shows them) and supply and patrol drift (the chart animates them) would bury the log otherwise.
pub(crate) fn logged(e: &TheatreEvent) -> bool {
    !matches!(e, TheatreEvent::HourAdvanced { .. } | TheatreEvent::Interrupted(_) | TheatreEvent::ResourcesChanged | TheatreEvent::SupplyChanged { .. } | TheatreEvent::PatrolDensityChanged { .. } | TheatreEvent::TallyChanged { .. })
}

fn cause_key(c: AlertCause) -> &'static str {
    match c {
        AlertCause::ObservedKill => "campaign.cause.observed_kill",
        AlertCause::UnobservedKillNoticed => "campaign.cause.unobserved_kill_noticed",
        AlertCause::Sighting => "campaign.cause.sighting",
        AlertCause::Evidence => "campaign.cause.evidence",
        AlertCause::QuietDecay => "campaign.cause.quiet_decay",
        AlertCause::ActStart => "campaign.cause.act_start",
        AlertCause::SextantSunk => "campaign.cause.sextant_sunk",
    }
}

// ---- Tallies ----

fn range_word(b: RangeBand) -> &'static str {
    match b { RangeBand::Close => "close", RangeBand::Medium => "medium", RangeBand::Long => "long", RangeBand::BeyondHorizon => "beyond_horizon" }
}

/// The word key naming what a tally counts.
fn tally_word(k: &TallyKey) -> String {
    use crate::enums::GdEnum;
    match k {
        TallyKey::Method(x) => crate::text::word_key_of("kill", x.to_gd()).unwrap_or_default(),
        TallyKey::Range(b) => format!("campaign.word.range.{}", range_word(*b)),
        TallyKey::Time(t) => crate::text::word_key_of("time", t.to_gd()).unwrap_or_default(),
        TallyKey::Emission(e) => crate::text::word_key_of("emcon", e.to_gd()).unwrap_or_default(),
    }
}

/// `TallyDict.text`: what is tallied, as a sentence fragment.
pub(crate) fn tally_msg(k: &TallyKey) -> Msg {
    let w = word(&tally_word(k));
    match k {
        TallyKey::Method(_) => m("campaign.tally.method").arg(w),
        TallyKey::Range(_) => m("campaign.tally.range").arg(w),
        TallyKey::Time(_) => m("campaign.tally.time").arg(w),
        TallyKey::Emission(_) => m("campaign.tally.emission").arg(w),
    }
}

/// `TallyDict.key`: a stable string for the tally (`method:missile`, `range:long`, `time:night`,
/// `emission:silent`).
pub(crate) fn tally_key_string(k: &TallyKey) -> String {
    use crate::enums::GdEnum;
    let last = |s: Option<String>| s.and_then(|s| s.rsplit('.').next().map(str::to_string)).unwrap_or_default();
    match k {
        TallyKey::Method(x) => format!("method:{}", last(crate::text::word_key_of("kill", x.to_gd()))),
        TallyKey::Range(b) => format!("range:{}", range_word(*b)),
        TallyKey::Time(t) => format!("time:{}", last(crate::text::word_key_of("time", t.to_gd()))),
        TallyKey::Emission(e) => format!("emission:{}", last(crate::text::word_key_of("emcon", e.to_gd()))),
    }
}

// ---- Proposals and actions ----

/// A chart-table action as an imperative sentence (`ProposalDict.text`).
pub(crate) fn action_msg(a: &TheatreAction, n: &Names) -> Msg {
    let rules = &n.campaign.rules;
    match a {
        TheatreAction::Transit(node) => m("campaign.action.transit").arg(n.node(node)),
        TheatreAction::Refit => m("campaign.action.refit"),
        TheatreAction::Wait { hours: h } => m("campaign.action.wait").arg(hours(*h as u64)),
        TheatreAction::AcceptOperation(op) => m("campaign.action.accept").arg(n.operation(op)),
        TheatreAction::DeclineOperation(op) => m("campaign.action.decline").arg(n.operation(op)),
        TheatreAction::SpendIntel(s) => match s {
            IntelSpend::DelayCard { region, card } => m("campaign.action.delay_card").num(rules.intel_delay_card as f64, Unit::Intel).arg(n.card(card)).arg(n.region(region)),
            IntelSpend::CancelCard { region, card } => m("campaign.action.cancel_card").num(rules.intel_cancel_card as f64, Unit::Intel).arg(n.card(card)).arg(n.region(region)),
            IntelSpend::RevealConvoys(region) => m("campaign.action.reveal_convoys").num(rules.intel_reveal_convoys as f64, Unit::Intel).arg(n.region(region)),
            IntelSpend::RevealAce(ace) => m("campaign.action.reveal_ace").num(rules.intel_reveal_ace as f64, Unit::Intel).arg(n.ace(ace)),
        },
        TheatreAction::RebuildHull { player } => m("campaign.action.rebuild").arg(n.player(PlayerSlot(*player))),
    }
}

// ---- Briefing ----

fn part_tag_word(t: PartTag) -> &'static str {
    match t {
        PartTag::Crane => "campaign.part_tag.crane",
        PartTag::MineRail => "campaign.part_tag.mine_rail",
        PartTag::RhibDavit => "campaign.part_tag.rhib_davit",
        PartTag::BoardingKit => "campaign.part_tag.boarding_kit",
        PartTag::RecoveryNet => "campaign.part_tag.recovery_net",
        PartTag::CameraMast => "campaign.part_tag.camera_mast",
        PartTag::PassengerBerths => "campaign.part_tag.passenger_berths",
        PartTag::PointDefence => "campaign.part_tag.point_defence",
        PartTag::Magazine => "campaign.part_tag.magazine",
        PartTag::FuelStore => "campaign.part_tag.fuel_store",
        PartTag::Propulsion => "campaign.part_tag.propulsion",
        PartTag::Steering => "campaign.part_tag.steering",
        PartTag::Generation => "campaign.part_tag.generation",
        PartTag::LowSignature => "campaign.part_tag.low_signature",
    }
}
const PART_TAGS: &[PartTag] = &[PartTag::Crane, PartTag::MineRail, PartTag::RhibDavit, PartTag::BoardingKit, PartTag::RecoveryNet, PartTag::CameraMast, PartTag::PassengerBerths, PartTag::PointDefence, PartTag::Magazine, PartTag::FuelStore, PartTag::Propulsion, PartTag::Steering, PartTag::Generation, PartTag::LowSignature];

/// A briefing constraint tag as a sentence.
pub(crate) fn tag_msg(t: &ConstraintTag) -> Msg {
    match t {
        // Drafts differ by tenths of a metre, which the metres unit would round away.
        ConstraintTag::DraftLimit { max_m } => m("campaign.tag.draft_limit").arg(MsgArg::Name(format!("{:.1}", finite(*max_m).max(0.0)))),
        ConstraintTag::RcsLimit { max_m2 } => m("campaign.tag.rcs_limit").num(finite(*max_m2), Unit::None),
        ConstraintTag::Range { min_km } => m("campaign.tag.range").num(finite(*min_km), Unit::Kilometres),
        ConstraintTag::Passengers { count, allowed_losses } => m("campaign.tag.passengers").arg(MsgArg::Int(*count as i64)).arg(MsgArg::Int(*allowed_losses as i64)),
        ConstraintTag::Requires(tag) => m("campaign.tag.requires").arg(word(part_tag_word(*tag))),
        ConstraintTag::MinSpeed { kn } => m("campaign.tag.min_speed").num(finite(*kn), Unit::Knots),
    }
}

/// A ghost condition as a sentence.
pub(crate) fn ghost_msg(g: &GhostKind) -> Msg {
    match g {
        GhostKind::NoRadarEmissions => m("campaign.ghost.no_radar_emissions"),
        GhostKind::NoLosses => m("campaign.ghost.no_losses"),
        GhostKind::NeverDetected => m("campaign.ghost.never_detected"),
        GhostKind::CaptureTheAce => m("campaign.ghost.capture_the_ace"),
        GhostKind::NoKills => m("campaign.ghost.no_kills"),
        GhostKind::UnderTime { seconds } => m("campaign.ghost.under_time").num((*seconds as f64 / 60.0).ceil(), Unit::Minutes),
        GhostKind::NoHullDamage => m("campaign.ghost.no_hull_damage"),
    }
}

/// What a ghost condition pays.
pub(crate) fn reward_msg(r: &Reward, n: &Names) -> Msg {
    match r {
        Reward::Intel(v) => m("campaign.reward.intel").num(*v as f64, Unit::Intel),
        Reward::Blueprint(p) => m("campaign.reward.blueprint").arg(n.part(p)),
        Reward::Salvage(v) => m("campaign.reward.salvage").num(*v as f64, Unit::Salvage),
    }
}

// ---- Keys ----

/// Keys of messages composed in the facade itself (transit preview, saves, new campaign, sorties).
pub(crate) const FACADE_KEYS: &[&str] = &[
    "campaign.reject.no_campaign", "campaign.reject.busy", "campaign.reject.no_design", "campaign.reject.hull_count_locked",
    "campaign.reject.unknown_service", "campaign.reject.bad_action", "campaign.reject.no_briefing", "campaign.reject.not_accepted",
    "campaign.reject.member_waits", "campaign.reject.no_dock",
    "campaign.transit.hours", "campaign.transit.hours_cut", "campaign.transit.here", "campaign.transit.not_cove",
    "campaign.transit.no_route", "campaign.transit.forced", "campaign.transit.sextant_refit", "campaign.transit.under_way",
    "campaign.sortie.failed", "campaign.sortie.lost_result",
    "campaign.new.bad_difficulty", "campaign.new.bad_starter", "campaign.new.busy", "campaign.new.save_failed",
    "campaign.save.write_failed", "campaign.save.not_found", "campaign.save.outside", "campaign.save.forked",
    "campaign.save.other_campaign", "campaign.save.suspend_failed", "campaign.save.not_allowed", "campaign.save.copy_refused",
    "campaign.save.unreadable", "campaign.threat.unknown",
];

/// Every `campaign.*` key this unit emits.
pub(crate) fn text_keys() -> Vec<String> {
    let mut keys: Vec<String> = FACADE_KEYS.iter().map(|k| k.to_string()).collect();
    for r in [TheatreReject::NotHost, TheatreReject::NotEnoughSalvage, TheatreReject::NotEnoughIntel, TheatreReject::NotAtCove, TheatreReject::HullNotReady, TheatreReject::OperationNotOffered, TheatreReject::ForcedOperationPending, TheatreReject::IronmanLocked, TheatreReject::OverTonnageCap, TheatreReject::InvalidBlueprint, TheatreReject::NoSuchProposal, TheatreReject::UnknownTarget, TheatreReject::CampaignOver] {
        keys.push(reject_msg(r).key.as_str().to_string());
    }
    for k in ["arrived", "refit_done", "wait_done", "operation_offered", "forced_operation", "hunter_group_warning", "card_arriving", "clock_low", "act_ended", "hull_ready", "paused"] {
        keys.push(format!("campaign.interrupt.{k}"));
    }
    for k in EVENT_KINDS { keys.push(format!("campaign.event.{k}")); }
    for k in ["sextant_placed", "hunter_group_sortied_ace", "act_started_retreat", "proposal_accepted", "proposal_declined"] { keys.push(format!("campaign.event.{k}")); }
    for c in [AlertCause::ObservedKill, AlertCause::UnobservedKillNoticed, AlertCause::Sighting, AlertCause::Evidence, AlertCause::QuietDecay, AlertCause::ActStart, AlertCause::SextantSunk] { keys.push(cause_key(c).to_string()); }
    for b in [RangeBand::Close, RangeBand::Medium, RangeBand::Long, RangeBand::BeyondHorizon] { keys.push(format!("campaign.word.range.{}", range_word(b))); }
    for k in ["method", "range", "time", "emission"] { keys.push(format!("campaign.tally.{k}")); }
    for k in ["transit", "refit", "wait", "accept", "decline", "delay_card", "cancel_card", "reveal_convoys", "reveal_ace", "rebuild"] { keys.push(format!("campaign.action.{k}")); }
    for t in PART_TAGS { keys.push(part_tag_word(*t).to_string()); }
    for k in ["draft_limit", "rcs_limit", "range", "passengers", "requires", "min_speed"] { keys.push(format!("campaign.tag.{k}")); }
    for k in ["no_radar_emissions", "no_losses", "never_detected", "capture_the_ace", "no_kills", "under_time", "no_hull_damage"] { keys.push(format!("campaign.ghost.{k}")); }
    for k in ["intel", "blueprint", "salvage"] { keys.push(format!("campaign.reward.{k}")); }
    keys
}
