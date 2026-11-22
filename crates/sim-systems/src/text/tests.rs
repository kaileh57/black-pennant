use super::*;
use super::csv_check::{check_msg, lint, parse, GLOSSARY};
use crate::emcon::EmconReason;

const CSV: &str = include_str!("../../../../game/i18n/sim.csv");

fn every_msg() -> Vec<Msg> {
    let t = TrackId(7);
    let mut v = Vec::new();
    let holds = [
        HoldReason::NoTarget, HoldReason::Stance { have: Stance::Suspect, need: Stance::Hostile },
        HoldReason::Id { have: IdRung::Domain, need: IdRung::Class }, HoldReason::Grade { have: TrackGrade::Coarse, need: TrackGrade::Fine },
        HoldReason::OutOfRange { range_m: 9_000.0, limit_m: 8_000.0 }, HoldReason::SuspectOutsideSelfDefence { range_m: 6_000.0, limit_m: 4_000.0 },
        HoldReason::NeutralInSeekerBasket { neutral: t }, HoldReason::TrainLimit { need_deg: 74.0, limit_deg: 70.0 },
        HoldReason::ElevationLimit { need_deg: 85.0, limit_deg: 80.0 }, HoldReason::Masked, HoldReason::Slewing,
        HoldReason::Reloading { seconds: f64::NAN }, HoldReason::NoAmmunition, HoldReason::Unpowered, HoldReason::Shed, HoldReason::Damaged,
        HoldReason::FriendInLine { friend: t }, HoldReason::SalvoInFlight, HoldReason::PermissionWithheld, HoldReason::PlayerControl,
        HoldReason::EmconSilent, HoldReason::NoSolution,
    ];
    v.extend(holds.iter().map(hold_reason));
    let fires = [FireReason::Doctrine, FireReason::PointDefence, FireReason::SelfDefence, FireReason::Designated, FireReason::Rule(3), FireReason::Manual, FireReason::Graph, FireReason::LocalControl];
    v.extend(fires.iter().map(fire_reason));
    for r in [RungReason::None, RungReason::AltitudeMeasured, RungReason::DepthMeasured, RungReason::EmitterMatch, RungReason::AcousticMatch, RungReason::RcsAndSpeed, RungReason::EmitterUniqueToClass, RungReason::VisualSighting, RungReason::CloseVisualSighting, RungReason::Photograph, RungReason::Briefing, RungReason::Datalink, RungReason::PlayerOverride] { v.push(rung_reason(r)); }
    for r in [StanceReason::Default, StanceReason::Datalink, StanceReason::FiredOnOwnSide, StanceReason::CombineEmitter, StanceReason::BriefingHostile, StanceReason::BriefingNeutral, StanceReason::LobbyOpposingSide, StanceReason::PlayerOverride, StanceReason::GraphOverride] { v.push(stance_reason(r)); }
    let c = CompartmentId(3);
    let s = PartSlot(4);
    let kinds = [
        AlertKind::WeaponInbound { track: t }, AlertKind::TorpedoInbound { track: t }, AlertKind::Flooding { compartment: c },
        AlertKind::FloodingSevere { compartment: c }, AlertKind::Fire { compartment: c }, AlertKind::MagazineFire { magazine: s, cookoff_in_s: 45.0 },
        AlertKind::CapsizeImminent, AlertKind::SinkingImminent, AlertKind::PropulsionLost, AlertKind::SteeringLost,
        AlertKind::PowerShortage { shed: 3 }, AlertKind::ConduitCut { conduit: 2 }, AlertKind::PartHeavyDamage { slot: s },
        AlertKind::PartDestroyed { slot: s }, AlertKind::PartOnBuffer { slot: s, seconds_left: 12.0 }, AlertKind::PartLocalControl { slot: s },
        AlertKind::FuelLow { fraction: 0.2 }, AlertKind::AmmunitionLow { slot: s }, AlertKind::EsmWarning, AlertKind::NewHostileTrack { track: t },
        AlertKind::ShoalAhead, AlertKind::Grounded, AlertKind::CrewCasualties, AlertKind::PassengersLost { count: 4 },
        AlertKind::ActionInterrupted, AlertKind::ScriptGreyedOut, AlertKind::HostOverload { rung: 2 }, AlertKind::HunterGroupApproaching,
    ];
    for k in &kinds { for p in [AlertPriority::Caution, AlertPriority::Warning, AlertPriority::Alarm, AlertPriority::Emergency] { v.push(alert(k, p)); } }
    for b in [ActionBlock::TooFast, ActionBlock::TooFar, ActionBlock::UnderFire, ActionBlock::TargetUnderWay, ActionBlock::NoLineOfSight, ActionBlock::OutsideFieldOfView, ActionBlock::BeyondVisibility, ActionBlock::OffLine, ActionBlock::TooSlow, ActionBlock::NoMinesLeft, ActionBlock::PartLost, ActionBlock::Cancelled, ActionBlock::TargetLost] { v.push(action_block(b)); }
    for b in [CompressionBlock::WeaponInbound, CompressionBlock::ThreatInOwnEnvelope, CompressionBlock::InsideThreatEnvelope, CompressionBlock::HostLoad, CompressionBlock::TimeScaleDegraded, CompressionBlock::NotAllPlayers, CompressionBlock::ModeForbids] { v.push(compression_block(b)); }
    for r in [RejectReason::NotAllowedInMode, RejectReason::NotAllowedByRuleset, RejectReason::OnlyInDryDockOrSplash, RejectReason::NoSuchPart, RejectReason::NoSuchTrack, RejectReason::FriendCannotBeEngaged, RejectReason::RoleNotHeld, RejectReason::HullLost, RejectReason::TooManyRules, RejectReason::InvalidValue, RejectReason::SinglePlayerOnly, RejectReason::ConditionsNotMet, RejectReason::Duplicate] { v.push(reject(r)); }
    for f in [ScriptFault::Error, ScriptFault::Memory, ScriptFault::Budget, ScriptFault::Backstop] { v.push(script_fault(f)); }
    for r in [EmconReason::SilentOrdered, EmconReason::PassiveNoTrigger, EmconReason::BurstTrackInside { track: t, range_m: 18_000.0 }, EmconReason::BurstWeaponInbound, EmconReason::BurstRefreshForWeapon, EmconReason::HoldingLockWeaponsInFlight, EmconReason::ActiveOrdered, EmconReason::PointDefenceException] { v.push(emcon_reason(&r)); }
    for s in [Stance::Friend, Stance::Neutral, Stance::Suspect, Stance::Hostile] { v.push(Msg { key: stance_word(s), args: vec![] }); }
    for r in [IdRung::Unknown, IdRung::Domain, IdRung::Type, IdRung::Class, IdRung::Exact] { v.push(Msg { key: rung_word(r), args: vec![] }); }
    for g in [TrackGrade::Bearing, TrackGrade::Coarse, TrackGrade::Fine, TrackGrade::Lock] { v.push(Msg { key: grade_word(g), args: vec![] }); }
    for o in OfficerRole::ALL { v.push(Msg { key: officer_word(o), args: vec![] }); }
    v
}

#[test]
fn every_message_has_a_row_with_matching_placeholders() {
    let rows = parse(CSV);
    let keys = text_keys();
    let mut emitted = BTreeSet::new();
    for msg in every_msg() {
        assert!(keys.contains(&msg.key.0.as_str()), "{} missing from text_keys()", msg.key.0);
        check_msg(&rows, &msg);
        emitted.insert(msg.key.0.clone());
    }
    // Every listed key is reachable and every CSV row is listed: no dead rows, no unlisted keys.
    for k in &keys { assert!(emitted.contains(*k), "{k} listed but never emitted"); }
    assert_eq!(rows.len(), keys.len(), "CSV rows vs text_keys()");
    assert!(rows.keys().all(|k| k.starts_with("sim.")));
}

#[test]
fn english_follows_the_copy_rules() {
    for (key, (_, en)) in parse(CSV) {
        // Reasons give cause and evidence under 60 characters (copy rule 4).
        lint(&key, &en, 59, GLOSSARY);
    }
}

#[test]
fn rule_numbers_count_from_one_and_values_stay_finite() {
    assert_eq!(fire_reason(&FireReason::Rule(0)).args, vec![MsgArg::Int(1)]);
    let m = hold_reason(&HoldReason::Reloading { seconds: f64::INFINITY });
    assert_eq!(m.args, vec![MsgArg::Num { value: 0.0, unit: Unit::Seconds }]);
    let m = alert(&AlertKind::FuelLow { fraction: 0.25 }, AlertPriority::Caution);
    assert_eq!(m.args, vec![MsgArg::Num { value: 25.0, unit: Unit::Percent }]);
    assert_eq!(alert(&AlertKind::MagazineFire { magazine: PartSlot(1), cookoff_in_s: 20.0 }, AlertPriority::Emergency).key.0, "sim.alert.magazine_fire_critical");
}
