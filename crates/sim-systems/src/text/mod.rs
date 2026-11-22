//! Unit `sys-text`. Turns the protocol's typed reasons and kinds into [`Msg`] values (translation key plus
//! arguments). Owns the key namespace `sim.*` and the English file `game/i18n/sim.csv`.
//! DESIGN.md L351-353 (reasons), L1695-1706 (copy rules: sentence case; capitals only for glossary codes;
//! stance, rung and billet words keep an initial capital; officer lines fit two 40-character lines),
//! L1608 (banned words and copy tics; CI lints the CSV).
//!
//! Key namespaces are disjoint, one owner and one CSV each, and each owner exposes a `text_keys()` list for
//! the CI check that every key has a row:
//! `sim.*` this unit (`game/i18n/sim.csv`); `officer.*` `sys-officer-board` (`officers.csv`); `tuning.*`
//! `sys-officer-tuning` (`tuning.csv`); `graph.*` `sys-graph-lib` (`graph.csv`); `notes.*` `sys-notes`
//! (`notes.csv`); `suggest.*` `sys-suggest` (`suggest.csv`); `splash.*` `sys-splash` (`splash.csv`);
//! `objective.*` `sys-objectives` (`objectives.csv`); `tutorial.*` the `data-tutorial` unit (`tutorial.csv`).
//! Briefing, operation, ace, part and class texts are keys authored by content's data units. Typed protocol
//! and theatre enums that have no function here (`LobbyIssue`, `TheatreReject`, `Interrupt`, `TheatreEvent`,
//! `Outcome`, `content::validate::Issue`) are worded by the client in its own namespace.
//! No English text is produced here. Each function returns a key such as `sim.hold.out_of_range` and the
//! typed arguments in the order the CSV row's placeholders expect.
use crate::prelude::*;
use crate::protocol::alert::{AlertKind, AlertPriority};
use crate::protocol::event::{RejectReason, ScriptFault};
use crate::protocol::snapshot::{ActionBlock, CompressionBlock};

fn m(key: &str) -> Msg { Msg::new(key) }
fn metres(msg: Msg, v: f64) -> Msg { msg.num(finite(v), Unit::Metres) }
fn degrees(msg: Msg, v: f64) -> Msg { msg.num(finite(v), Unit::Degrees) }
fn seconds(msg: Msg, v: f64) -> Msg { msg.num(finite(v).max(0.0), Unit::Seconds) }
/// NaN or infinite values from a faulty stage never reach the wire as such.
fn finite(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }

/// "Holding: reason" (L407). Unit: `sys-text`.
pub fn hold_reason(reason: &HoldReason) -> Msg {
    match *reason {
        HoldReason::NoTarget => m("sim.hold.no_target"),
        HoldReason::Stance { have, need } => m("sim.hold.stance").arg(MsgArg::Stance(have)).arg(MsgArg::Stance(need)),
        HoldReason::Id { have, need } => m("sim.hold.id").arg(MsgArg::Rung(have)).arg(MsgArg::Rung(need)),
        HoldReason::Grade { have, need } => m("sim.hold.grade").arg(MsgArg::Grade(have)).arg(MsgArg::Grade(need)),
        HoldReason::OutOfRange { range_m, limit_m } => metres(metres(m("sim.hold.out_of_range"), range_m), limit_m),
        HoldReason::SuspectOutsideSelfDefence { range_m, limit_m } => metres(metres(m("sim.hold.suspect_outside_self_defence"), range_m), limit_m),
        HoldReason::NeutralInSeekerBasket { neutral } => m("sim.hold.neutral_in_seeker_basket").arg(MsgArg::Track(neutral)),
        HoldReason::TrainLimit { need_deg, limit_deg } => degrees(degrees(m("sim.hold.train_limit"), need_deg), limit_deg),
        HoldReason::ElevationLimit { need_deg, limit_deg } => degrees(degrees(m("sim.hold.elevation_limit"), need_deg), limit_deg),
        HoldReason::Masked => m("sim.hold.masked"),
        HoldReason::Slewing => m("sim.hold.slewing"),
        HoldReason::Reloading { seconds: s } => seconds(m("sim.hold.reloading"), s),
        HoldReason::NoAmmunition => m("sim.hold.no_ammunition"),
        HoldReason::Unpowered => m("sim.hold.unpowered"),
        HoldReason::Shed => m("sim.hold.shed"),
        HoldReason::Damaged => m("sim.hold.damaged"),
        HoldReason::FriendInLine { friend } => m("sim.hold.friend_in_line").arg(MsgArg::Track(friend)),
        HoldReason::SalvoInFlight => m("sim.hold.salvo_in_flight"),
        HoldReason::PermissionWithheld => m("sim.hold.permission_withheld"),
        HoldReason::PlayerControl => m("sim.hold.player_control"),
        HoldReason::EmconSilent => m("sim.hold.emcon_silent"),
        HoldReason::NoSolution => m("sim.hold.no_solution"),
        HoldReason::ShoreSite => m("sim.hold.shore_site"),
    }
}

/// Why a mount fired (L353). Unit: `sys-text`.
pub fn fire_reason(reason: &FireReason) -> Msg {
    match *reason {
        FireReason::Doctrine => m("sim.fire.doctrine"),
        FireReason::PointDefence => m("sim.fire.point_defence"),
        FireReason::SelfDefence => m("sim.fire.self_defence"),
        FireReason::Designated => m("sim.fire.designated"),
        // Rules are numbered from 1 in the rules table the player sees.
        FireReason::Rule(i) => m("sim.fire.rule").arg(MsgArg::Int(i as i64 + 1)),
        FireReason::Manual => m("sim.fire.manual"),
        FireReason::Graph => m("sim.fire.graph"),
        FireReason::LocalControl => m("sim.fire.local_control"),
    }
}

/// What moved a track up a rung (L477). Unit: `sys-text`.
pub fn rung_reason(reason: RungReason) -> Msg {
    m(match reason {
        RungReason::None => "sim.rung.none",
        RungReason::AltitudeMeasured => "sim.rung.altitude_measured",
        RungReason::DepthMeasured => "sim.rung.depth_measured",
        RungReason::EmitterMatch => "sim.rung.emitter_match",
        RungReason::AcousticMatch => "sim.rung.acoustic_match",
        RungReason::RcsAndSpeed => "sim.rung.rcs_and_speed",
        RungReason::EmitterUniqueToClass => "sim.rung.emitter_unique_to_class",
        RungReason::VisualSighting => "sim.rung.visual_sighting",
        RungReason::CloseVisualSighting => "sim.rung.close_visual_sighting",
        RungReason::Photograph => "sim.rung.photograph",
        RungReason::Briefing => "sim.rung.briefing",
        RungReason::Datalink => "sim.rung.datalink",
        RungReason::PlayerOverride => "sim.rung.player_override",
    })
}

/// Why a track holds its stance (L503). Unit: `sys-text`.
pub fn stance_reason(reason: StanceReason) -> Msg {
    m(match reason {
        StanceReason::Default => "sim.stance.default",
        StanceReason::Datalink => "sim.stance.datalink",
        StanceReason::FiredOnOwnSide => "sim.stance.fired_on_own_side",
        StanceReason::CombineEmitter => "sim.stance.combine_emitter",
        StanceReason::BriefingHostile => "sim.stance.briefing_hostile",
        StanceReason::BriefingNeutral => "sim.stance.briefing_neutral",
        StanceReason::LobbyOpposingSide => "sim.stance.lobby_opposing_side",
        StanceReason::PlayerOverride => "sim.stance.player_override",
        StanceReason::GraphOverride => "sim.stance.graph_override",
    })
}

/// Alert line for the strip and the log. The priority picks the wording only where a kind changes meaning
/// when it escalates (a magazine fire past its Emergency threshold). Unit: `sys-text`.
pub fn alert(kind: &AlertKind, priority: AlertPriority) -> Msg {
    match *kind {
        AlertKind::WeaponInbound { track } => m("sim.alert.weapon_inbound").arg(MsgArg::Track(track)),
        AlertKind::TorpedoInbound { track } => m("sim.alert.torpedo_inbound").arg(MsgArg::Track(track)),
        AlertKind::Flooding { compartment } => m("sim.alert.flooding").arg(MsgArg::Compartment(compartment)),
        AlertKind::FloodingSevere { compartment } => m("sim.alert.flooding_severe").arg(MsgArg::Compartment(compartment)),
        AlertKind::Fire { compartment } => m("sim.alert.fire").arg(MsgArg::Compartment(compartment)),
        AlertKind::MagazineFire { magazine, cookoff_in_s } => {
            let key = if priority >= AlertPriority::Emergency { "sim.alert.magazine_fire_critical" } else { "sim.alert.magazine_fire" };
            seconds(m(key).arg(MsgArg::Part(magazine)), cookoff_in_s)
        }
        AlertKind::CapsizeImminent => m("sim.alert.capsize_imminent"),
        AlertKind::SinkingImminent => m("sim.alert.sinking_imminent"),
        AlertKind::PropulsionLost => m("sim.alert.propulsion_lost"),
        AlertKind::SteeringLost => m("sim.alert.steering_lost"),
        AlertKind::PowerShortage { shed } => m("sim.alert.power_shortage").arg(MsgArg::Int(shed as i64)),
        AlertKind::ConduitCut { conduit } => m("sim.alert.conduit_cut").arg(MsgArg::Int(conduit as i64 + 1)),
        AlertKind::PartHeavyDamage { slot } => m("sim.alert.part_heavy_damage").arg(MsgArg::Part(slot)),
        AlertKind::PartDestroyed { slot } => m("sim.alert.part_destroyed").arg(MsgArg::Part(slot)),
        AlertKind::PartOnBuffer { slot, seconds_left } => seconds(m("sim.alert.part_on_buffer").arg(MsgArg::Part(slot)), seconds_left),
        AlertKind::PartLocalControl { slot } => m("sim.alert.part_local_control").arg(MsgArg::Part(slot)),
        AlertKind::FuelLow { fraction } => m("sim.alert.fuel_low").num(finite(fraction).clamp(0.0, 1.0) * 100.0, Unit::Percent),
        AlertKind::AmmunitionLow { slot } => m("sim.alert.ammunition_low").arg(MsgArg::Part(slot)),
        AlertKind::EsmWarning => m("sim.alert.esm_warning"),
        AlertKind::NewHostileTrack { track } => m("sim.alert.hostile_track").arg(MsgArg::Track(track)),
        AlertKind::ShoalAhead => m("sim.alert.shoal_ahead"),
        AlertKind::Grounded => m("sim.alert.grounded"),
        AlertKind::CrewCasualties => m("sim.alert.crew_casualties"),
        AlertKind::PassengersLost { count } => m("sim.alert.passengers_lost").arg(MsgArg::Int(count as i64)),
        AlertKind::ActionInterrupted => m("sim.alert.action_interrupted"),
        AlertKind::ScriptGreyedOut => m("sim.alert.script_greyed_out"),
        AlertKind::HostOverload { rung } => m("sim.alert.host_overload").arg(MsgArg::Int(rung as i64)),
        AlertKind::HunterGroupApproaching => m("sim.alert.hunter_group_approaching"),
    }
}

pub fn action_block(block: ActionBlock) -> Msg {
    m(match block {
        ActionBlock::TooFast => "sim.action.too_fast",
        ActionBlock::TooFar => "sim.action.too_far",
        ActionBlock::UnderFire => "sim.action.under_fire",
        ActionBlock::TargetUnderWay => "sim.action.target_under_way",
        ActionBlock::NoLineOfSight => "sim.action.no_line_of_sight",
        ActionBlock::OutsideFieldOfView => "sim.action.outside_field_of_view",
        ActionBlock::BeyondVisibility => "sim.action.beyond_visibility",
        ActionBlock::OffLine => "sim.action.off_line",
        ActionBlock::TooSlow => "sim.action.too_slow",
        ActionBlock::NoMinesLeft => "sim.action.no_mines_left",
        ActionBlock::PartLost => "sim.action.part_lost",
        ActionBlock::Cancelled => "sim.action.cancelled",
        ActionBlock::TargetLost => "sim.action.target_lost",
    })
}

pub fn compression_block(block: CompressionBlock) -> Msg {
    m(match block {
        CompressionBlock::WeaponInbound => "sim.compress.weapon_inbound",
        CompressionBlock::ThreatInOwnEnvelope => "sim.compress.threat_in_own_envelope",
        CompressionBlock::InsideThreatEnvelope => "sim.compress.inside_threat_envelope",
        CompressionBlock::HostLoad => "sim.compress.host_load",
        CompressionBlock::TimeScaleDegraded => "sim.compress.time_scale_degraded",
        CompressionBlock::NotAllPlayers => "sim.compress.not_all_players",
        CompressionBlock::ModeForbids => "sim.compress.mode_forbids",
    })
}

pub fn reject(reason: RejectReason) -> Msg {
    m(match reason {
        RejectReason::NotAllowedInMode => "sim.reject.not_allowed_in_mode",
        RejectReason::NotAllowedByRuleset => "sim.reject.not_allowed_by_ruleset",
        RejectReason::OnlyInDryDockOrSplash => "sim.reject.only_in_dry_dock_or_splash",
        RejectReason::NoSuchPart => "sim.reject.no_such_part",
        RejectReason::NoSuchTrack => "sim.reject.no_such_track",
        RejectReason::FriendCannotBeEngaged => "sim.reject.friend_cannot_be_engaged",
        RejectReason::RoleNotHeld => "sim.reject.role_not_held",
        RejectReason::HullLost => "sim.reject.hull_lost",
        RejectReason::TooManyRules => "sim.reject.too_many_rules",
        RejectReason::InvalidValue => "sim.reject.invalid_value",
        RejectReason::SinglePlayerOnly => "sim.reject.single_player_only",
        RejectReason::ConditionsNotMet => "sim.reject.conditions_not_met",
        RejectReason::Duplicate => "sim.reject.duplicate",
    })
}

pub fn script_fault(fault: ScriptFault) -> Msg {
    m(match fault {
        ScriptFault::Error => "sim.script.error",
        ScriptFault::Memory => "sim.script.memory",
        ScriptFault::Budget => "sim.script.budget",
        ScriptFault::Backstop => "sim.script.backstop",
    })
}

pub fn emcon_reason(reason: &crate::emcon::EmconReason) -> Msg {
    use crate::emcon::EmconReason as R;
    match *reason {
        R::SilentOrdered => m("sim.emcon.silent_ordered"),
        R::PassiveNoTrigger => m("sim.emcon.passive_no_trigger"),
        R::BurstTrackInside { track, range_m } => metres(m("sim.emcon.burst_track_inside").arg(MsgArg::Track(track)), range_m),
        R::BurstWeaponInbound => m("sim.emcon.burst_weapon_inbound"),
        R::BurstRefreshForWeapon => m("sim.emcon.burst_refresh_for_weapon"),
        R::HoldingLockWeaponsInFlight => m("sim.emcon.holding_lock_weapons_in_flight"),
        R::ActiveOrdered => m("sim.emcon.active_ordered"),
        R::PointDefenceException => m("sim.emcon.point_defence_exception"),
        R::SelfDefenceUnderFire => m("sim.emcon.self_defence"),
    }
}

/// Word for a stance, rung, grade or officer billet (glossary entries). Unit: `sys-text`.
pub fn stance_word(stance: Stance) -> TextKey {
    TextKey::new(match stance { Stance::Friend => "sim.word.stance.friend", Stance::Neutral => "sim.word.stance.neutral", Stance::Suspect => "sim.word.stance.suspect", Stance::Hostile => "sim.word.stance.hostile" })
}
pub fn rung_word(rung: IdRung) -> TextKey {
    TextKey::new(match rung { IdRung::Unknown => "sim.word.rung.unknown", IdRung::Domain => "sim.word.rung.domain", IdRung::Type => "sim.word.rung.type", IdRung::Class => "sim.word.rung.class", IdRung::Exact => "sim.word.rung.exact" })
}
pub fn grade_word(grade: TrackGrade) -> TextKey {
    TextKey::new(match grade { TrackGrade::Bearing => "sim.word.grade.bearing", TrackGrade::Coarse => "sim.word.grade.coarse", TrackGrade::Fine => "sim.word.grade.fine", TrackGrade::Lock => "sim.word.grade.lock" })
}
pub fn officer_word(role: OfficerRole) -> TextKey {
    TextKey::new(match role {
        OfficerRole::Helm => "sim.word.officer.helm",
        OfficerRole::Engineer => "sim.word.officer.engineer",
        OfficerRole::Sensors => "sim.word.officer.sensors",
        OfficerRole::Gunnery => "sim.word.officer.gunnery",
        OfficerRole::DamageControl => "sim.word.officer.damage_control",
    })
}

/// Every `sim.*` key this unit can emit, for the CI check that each has a row in `game/i18n/sim.csv`.
/// Unit: `sys-text`.
pub fn text_keys() -> Vec<&'static str> { KEYS.to_vec() }

const KEYS: &[&str] = &[
    "sim.hold.no_target", "sim.hold.stance", "sim.hold.id", "sim.hold.grade", "sim.hold.out_of_range",
    "sim.hold.suspect_outside_self_defence", "sim.hold.neutral_in_seeker_basket", "sim.hold.train_limit",
    "sim.hold.elevation_limit", "sim.hold.masked", "sim.hold.slewing", "sim.hold.reloading", "sim.hold.no_ammunition",
    "sim.hold.unpowered", "sim.hold.shed", "sim.hold.damaged", "sim.hold.friend_in_line", "sim.hold.salvo_in_flight",
    "sim.hold.permission_withheld", "sim.hold.player_control", "sim.hold.emcon_silent", "sim.hold.no_solution", "sim.hold.shore_site",
    "sim.fire.doctrine", "sim.fire.point_defence", "sim.fire.self_defence", "sim.fire.designated", "sim.fire.rule",
    "sim.fire.manual", "sim.fire.graph", "sim.fire.local_control",
    "sim.rung.none", "sim.rung.altitude_measured", "sim.rung.depth_measured", "sim.rung.emitter_match",
    "sim.rung.acoustic_match", "sim.rung.rcs_and_speed", "sim.rung.emitter_unique_to_class", "sim.rung.visual_sighting",
    "sim.rung.close_visual_sighting", "sim.rung.photograph", "sim.rung.briefing", "sim.rung.datalink",
    "sim.rung.player_override",
    "sim.stance.default", "sim.stance.datalink", "sim.stance.fired_on_own_side", "sim.stance.combine_emitter",
    "sim.stance.briefing_hostile", "sim.stance.briefing_neutral", "sim.stance.lobby_opposing_side",
    "sim.stance.player_override", "sim.stance.graph_override",
    "sim.alert.weapon_inbound", "sim.alert.torpedo_inbound", "sim.alert.flooding", "sim.alert.flooding_severe",
    "sim.alert.fire", "sim.alert.magazine_fire", "sim.alert.magazine_fire_critical", "sim.alert.capsize_imminent",
    "sim.alert.sinking_imminent", "sim.alert.propulsion_lost", "sim.alert.steering_lost", "sim.alert.power_shortage",
    "sim.alert.conduit_cut", "sim.alert.part_heavy_damage", "sim.alert.part_destroyed", "sim.alert.part_on_buffer",
    "sim.alert.part_local_control", "sim.alert.fuel_low", "sim.alert.ammunition_low", "sim.alert.esm_warning",
    "sim.alert.hostile_track", "sim.alert.shoal_ahead", "sim.alert.grounded", "sim.alert.crew_casualties",
    "sim.alert.passengers_lost", "sim.alert.action_interrupted", "sim.alert.script_greyed_out",
    "sim.alert.host_overload", "sim.alert.hunter_group_approaching",
    "sim.action.too_fast", "sim.action.too_far", "sim.action.under_fire", "sim.action.target_under_way",
    "sim.action.no_line_of_sight", "sim.action.outside_field_of_view", "sim.action.beyond_visibility",
    "sim.action.off_line", "sim.action.too_slow", "sim.action.no_mines_left", "sim.action.part_lost",
    "sim.action.cancelled", "sim.action.target_lost",
    "sim.compress.weapon_inbound", "sim.compress.threat_in_own_envelope", "sim.compress.inside_threat_envelope",
    "sim.compress.host_load", "sim.compress.time_scale_degraded", "sim.compress.not_all_players",
    "sim.compress.mode_forbids",
    "sim.reject.not_allowed_in_mode", "sim.reject.not_allowed_by_ruleset", "sim.reject.only_in_dry_dock_or_splash",
    "sim.reject.no_such_part", "sim.reject.no_such_track", "sim.reject.friend_cannot_be_engaged",
    "sim.reject.role_not_held", "sim.reject.hull_lost", "sim.reject.too_many_rules", "sim.reject.invalid_value",
    "sim.reject.single_player_only", "sim.reject.conditions_not_met", "sim.reject.duplicate",
    "sim.script.error", "sim.script.memory", "sim.script.budget", "sim.script.backstop",
    "sim.emcon.silent_ordered", "sim.emcon.passive_no_trigger", "sim.emcon.burst_track_inside",
    "sim.emcon.burst_weapon_inbound", "sim.emcon.burst_refresh_for_weapon", "sim.emcon.holding_lock_weapons_in_flight",
    "sim.emcon.active_ordered", "sim.emcon.point_defence_exception", "sim.emcon.self_defence",
    "sim.word.stance.friend", "sim.word.stance.neutral", "sim.word.stance.suspect", "sim.word.stance.hostile",
    "sim.word.rung.unknown", "sim.word.rung.domain", "sim.word.rung.type", "sim.word.rung.class", "sim.word.rung.exact",
    "sim.word.grade.bearing", "sim.word.grade.coarse", "sim.word.grade.fine", "sim.word.grade.lock",
    "sim.word.officer.helm", "sim.word.officer.engineer", "sim.word.officer.sensors", "sim.word.officer.gunnery",
    "sim.word.officer.damage_control",
];

#[cfg(test)]
pub(crate) mod csv_check;

#[cfg(test)]
mod tests;
