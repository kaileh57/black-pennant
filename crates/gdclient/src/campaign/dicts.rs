//! Part of unit `gd-campaign`. The campaign's Dictionary shapes (`crate::shapes`, "Campaign"), one builder
//! per shape. Keys exactly as specified there; ids leave as `StringName`, players as `int`, text as
//! `MsgDict`. Main thread only (Godot types).
use super::words::{self, Names};
use crate::convert::{d, msg_dict};
use crate::enums::GdEnum;
use content::ids::{CardId, RegionId};
use content::save::{IntelSpend, Resources, TheatreAction};
use godot::prelude::*;
use sim_systems::protocol::Msg;
use sim_systems::theatre::{AfterAction, Briefing, ChartContact, ChartView, Interrupt, OperationOffer, Proposal, RegionView, TheatreEvent};

fn sn(s: &str) -> StringName { StringName::from(s) }
fn gs(s: &str) -> GString { GString::from(s) }
fn arr(items: impl IntoIterator<Item = VarDictionary>) -> VarArray { items.into_iter().map(|d| d.to_variant()).collect() }
fn ids<T: AsRef<str>>(items: impl IntoIterator<Item = T>) -> VarArray { items.into_iter().map(|s| sn(s.as_ref()).to_variant()).collect() }
fn hour_gd(h: u64) -> i64 { h.min(i64::MAX as u64) as i64 }
fn f(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }

/// Extra chart-table facts the facade adds to the theatre's view.
pub(crate) struct ChartExtra { pub phase: i64, pub at_cove: bool, pub can_save: i64 }

/// `ResourcesDict`.
pub(crate) fn resources_dict(r: &Resources, max_tier: content::enums::Tier) -> VarDictionary {
    let mut munitions = VarDictionary::new();
    for (m, n) in &r.munitions { munitions.set(&sn(m.as_str()), *n as i64); }
    let mut store = VarDictionary::new();
    for (p, n) in &r.parts_store { store.set(&sn(p.as_str()), *n as i64); }
    d! {
        "salvage" => r.salvage as i64,
        "intel" => r.intel as i64,
        "fuel_l" => f(r.fuel_l).max(0.0),
        "ammo_kg" => r.ammo_kg.values().map(|v| f(*v).max(0.0)).sum::<f64>(),
        "munitions" => munitions,
        "parts_store" => store,
        "unlocked_parts" => r.unlocked_parts.len() as i64,
        "max_tier" => max_tier.to_gd(),
    }
}

/// `CardDict`.
pub(crate) fn card_dict(card: &CardId, region: &RegionId, arrives_hour: u64, n: &Names) -> VarDictionary {
    let def = n.campaign.cards.get(card);
    d! {
        "id" => sn(card.as_str()),
        "name" => def.map_or_else(|| sn(card.as_str()), |c| sn(c.name.as_str())),
        "text" => def.map_or_else(StringName::default, |c| sn(c.description.as_str())),
        "arrives_h" => hour_gd(arrives_hour),
        "region" => sn(region.as_str()),
    }
}

fn region_dict(r: &RegionView, chart: &ChartView, n: &Names) -> VarDictionary {
    let name = n.campaign.theatre.regions.iter().find(|d| d.id == r.region).map_or_else(|| sn(r.region.as_str()), |d| sn(d.name.as_str()));
    let weather = chart.weather.iter().find(|(id, _)| *id == r.region).map_or_else(|| crate::convert::weather_dict(&Default::default()), |(_, w)| crate::convert::weather_dict(w));
    // Patrols at sea now: the region's base count scaled by its Supply (L748), so a cut lane shows on the chart.
    let base = n.campaign.theatre.regions.iter().find(|d| d.id == r.region).map_or(0.0, |d| d.base_patrols as f64);
    let patrols = (base * f(r.patrol_factor).max(0.0)).round();
    d! {
        "id" => sn(r.region.as_str()),
        "patrols" => if patrols.is_finite() { patrols as i64 } else { 0 },
        "name" => name,
        "open" => r.open,
        "alert" => r.alert_floor as i64,
        "supply" => f(r.supply).clamp(0.0, 100.0),
        "patrol_factor" => f(r.patrol_factor).max(0.0),
        "combine_control" => r.combine_control,
        "cards" => arr(r.active_cards.iter().map(|c| card_dict(c, &r.region, 0, n))),
        "queued" => arr(r.queued_cards.iter().map(|(c, h)| card_dict(c, &r.region, *h, n))),
        "weather" => weather,
    }
}

fn contact_dict(c: &ChartContact) -> VarDictionary {
    d! {
        "id" => c.id as i64,
        "node" => sn(c.node.as_str()),
        "destination" => c.destination.as_ref().map_or_else(StringName::default, |x| sn(x.as_str())),
        "convoy" => c.convoy,
        "hulls" => c.hulls as i64,
        "ace" => c.ace.as_ref().map_or_else(StringName::default, |x| sn(x.as_str())),
        "last_seen_h" => hour_gd(c.last_seen_hour),
    }
}

/// `OfferDict`.
pub(crate) fn offer_dict(o: &OperationOffer) -> VarDictionary {
    d! {
        "id" => sn(o.operation.as_str()),
        "kind" => super::model::operation_kind_gd(&o.kind),
        "template" => o.template.map_or(-1, |t| t.to_gd()),
        "name" => sn(o.name.as_str()),
        "node" => sn(o.node.as_str()),
        "region" => sn(o.region.as_str()),
        "forced" => o.forced,
        "transit_h" => o.transit_hours as i64,
    }
}

/// `IntelDict`.
pub(crate) fn intel_dict(s: &IntelSpend, cost: u8) -> VarDictionary {
    let (kind, region, card, ace) = match s {
        IntelSpend::DelayCard { region, card } => (0, region.as_str(), card.as_str(), ""),
        IntelSpend::CancelCard { region, card } => (1, region.as_str(), card.as_str(), ""),
        IntelSpend::RevealConvoys(region) => (2, region.as_str(), "", ""),
        IntelSpend::RevealAce(ace) => (3, "", "", ace.as_str()),
    };
    d! { "kind" => kind as i64, "region" => sn(region), "card" => sn(card), "ace" => sn(ace), "cost" => cost as i64 }
}

/// Intel price of a spend under the campaign rules.
pub(crate) fn intel_cost(s: &IntelSpend, rules: &content::campaign::CampaignRules) -> u8 {
    match s {
        IntelSpend::DelayCard { .. } => rules.intel_delay_card,
        IntelSpend::CancelCard { .. } => rules.intel_cancel_card,
        IntelSpend::RevealConvoys(_) => rules.intel_reveal_convoys,
        IntelSpend::RevealAce(_) => rules.intel_reveal_ace,
    }
}

/// `TheatreActionDict`.
pub(crate) fn action_dict(a: &TheatreAction, rules: &content::campaign::CampaignRules) -> VarDictionary {
    let none = StringName::default;
    let (kind, node, hours, op, player, intel) = match a {
        TheatreAction::Transit(n) => (0, sn(n.as_str()), 0, none(), -1, Variant::nil()),
        TheatreAction::Refit => (1, none(), 0, none(), -1, Variant::nil()),
        TheatreAction::Wait { hours } => (2, none(), *hours as i64, none(), -1, Variant::nil()),
        TheatreAction::AcceptOperation(o) => (3, none(), 0, sn(o.as_str()), -1, Variant::nil()),
        TheatreAction::DeclineOperation(o) => (4, none(), 0, sn(o.as_str()), -1, Variant::nil()),
        TheatreAction::SpendIntel(s) => (5, none(), 0, none(), -1, intel_dict(s, intel_cost(s, rules)).to_variant()),
        TheatreAction::RebuildHull { player } => (6, none(), 0, none(), *player as i64, Variant::nil()),
    };
    d! { "kind" => kind as i64, "node" => node, "hours" => hours, "operation" => op, "player" => player, "intel" => intel }
}

/// A `TheatreActionDict` from GDScript, checked against the shape; `None` when a key the kind needs is
/// missing or out of range.
pub(crate) fn action_from_dict(dict: &VarDictionary) -> Option<TheatreAction> {
    use crate::convert::{get_dict, get_i64, get_text};
    let text = |k: &str| get_text(dict, k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(match get_i64(dict, "kind")? {
        0 => TheatreAction::Transit(content::ids::TheatreNodeId::new(text("node")?)),
        1 => TheatreAction::Refit,
        2 => TheatreAction::Wait { hours: u32::try_from(get_i64(dict, "hours")?).ok().filter(|h| *h >= 1)? },
        3 => TheatreAction::AcceptOperation(content::ids::OperationId::new(text("operation")?)),
        4 => TheatreAction::DeclineOperation(content::ids::OperationId::new(text("operation")?)),
        5 => TheatreAction::SpendIntel(intel_from_dict(&get_dict(dict, "intel")?)?),
        6 => TheatreAction::RebuildHull { player: u8::try_from(get_i64(dict, "player")?).ok()? },
        _ => return None,
    })
}

/// An `IntelDict` from GDScript.
pub(crate) fn intel_from_dict(dict: &VarDictionary) -> Option<IntelSpend> {
    use crate::convert::{get_i64, get_text};
    let text = |k: &str| get_text(dict, k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(match get_i64(dict, "kind")? {
        0 => IntelSpend::DelayCard { region: RegionId::new(text("region")?), card: CardId::new(text("card")?) },
        1 => IntelSpend::CancelCard { region: RegionId::new(text("region")?), card: CardId::new(text("card")?) },
        2 => IntelSpend::RevealConvoys(RegionId::new(text("region")?)),
        3 => IntelSpend::RevealAce(content::ids::AceId::new(text("ace")?)),
        _ => return None,
    })
}

/// `ProposalDict`.
pub(crate) fn proposal_dict(p: &Proposal, n: &Names) -> VarDictionary {
    let accepted: PackedInt32Array = (0..8u8).filter(|i| p.accepted.has(content::ids::PlayerSlot(*i))).map(|i| i as i32).collect();
    d! {
        "id" => p.id.0 as i64,
        "by" => p.by.0 as i64,
        "action" => action_dict(&p.action, &n.campaign.rules),
        "accepted" => accepted,
        "made_h" => hour_gd(p.made_hour),
        "text" => msg_dict(&words::action_msg(&p.action, n)),
    }
}

/// `TallyDict`.
pub(crate) fn tally_dict(k: &content::campaign::TallyKey, value: f64) -> VarDictionary {
    d! { "text" => msg_dict(&words::tally_msg(k)), "value" => f(value).max(0.0), "key" => gs(&words::tally_key_string(k)) }
}

/// `ChartDict`.
pub(crate) fn chart_dict(c: &ChartView, extra: &ChartExtra, n: &Names) -> VarDictionary {
    let cut: VarArray = arr(c.cut_lanes.iter().map(|(a, b, u)| d! { "a" => sn(a.as_str()), "b" => sn(b.as_str()), "until_h" => hour_gd(*u) }));
    d! {
        "hour" => hour_gd(c.hour),
        "day" => hour_gd(c.hour / 24 + 1),
        "act" => c.act.to_gd(),
        "retreat" => c.retreat,
        "open_theatre" => c.open_theatre,
        "clock_hours_left" => c.clock_hours_left.map_or(-1.0, |h| f(h).max(0.0)),
        "sextant" => sn(c.sextant.as_str()),
        "coves" => ids(c.coves.iter().map(|x| x.as_str())),
        "regions" => arr(c.regions.iter().map(|r| region_dict(r, c, n))),
        "cut_lanes" => cut,
        "contacts" => arr(c.contacts.iter().map(contact_dict)),
        "offers" => arr(c.offers.iter().map(offer_dict)),
        "proposals" => arr(c.proposals.iter().map(|p| proposal_dict(p, n))),
        "hunter_warning_h" => c.hunter_warning_hours.map_or(-1, |h| h as i64),
        "tallies" => arr(c.tallies.iter().map(|(k, v)| tally_dict(k, *v))),
        "time_of_day" => c.time_of_day.to_gd(),
        "phase" => extra.phase,
        "at_cove" => extra.at_cove,
        "can_save" => extra.can_save,
    }
}

fn threat_dict(t: &sim_systems::theatre::ThreatEntry, n: &Names) -> VarDictionary {
    let name = match (&t.class, t.role) {
        (Some(c), _) => n.campaign.classes.get(c).map_or_else(|| Msg::new("campaign.threat.unknown"), |d| Msg::new(d.name.as_str())),
        (None, Some(r)) => Msg::new(crate::content_api::model::role_key(r)),
        (None, None) => Msg::new("campaign.threat.unknown"),
    };
    d! {
        "class_id" => t.class.as_ref().map_or_else(StringName::default, |c| sn(c.as_str())),
        "name" => msg_dict(&name),
        "count_min" => t.count_min as i64,
        "count_max" => t.count_max.max(t.count_min) as i64,
        "confidence" => t.confidence.to_gd(),
        "ace" => t.ace.as_ref().map_or_else(StringName::default, |a| sn(a.as_str())),
    }
}

/// `BriefingDict`. `dtg` is the date-time group of the commit hour, already formatted.
pub(crate) fn briefing_dict(b: &Briefing, n: &Names, dtg: &str) -> VarDictionary {
    let actions: PackedInt32Array = b.actions.iter().map(|a| a.to_gd() as i32).collect();
    let route: PackedVector2Array = b.route.iter().map(|p| Vector2::new(f(p[0]) as f32, f(p[1]) as f32)).collect();
    d! {
        "operation" => sn(b.operation.as_str()),
        "kind" => super::model::operation_kind_gd(&b.kind),
        "name" => sn(b.name.as_str()),
        "text" => sn(b.text.as_str()),
        "region" => sn(b.region.as_str()),
        "node" => sn(b.node.as_str()),
        "weather" => crate::convert::weather_dict(&b.weather),
        "time_of_day" => b.time_of_day.to_gd(),
        "distance_km" => f(b.distance_km).max(0.0),
        "threats" => arr(b.threats.iter().map(|t| threat_dict(t, n))),
        "tags" => arr(b.tags.iter().map(|t| msg_dict(&words::tag_msg(t)))),
        "ghosts" => arr(b.ghosts.iter().map(|g| d! { "text" => msg_dict(&words::ghost_msg(&g.kind)), "reward" => msg_dict(&words::reward_msg(&g.reward, n)) })),
        "actions" => actions,
        "recommended_emcon" => b.recommended_emcon.map_or(-1, |e| e.to_gd()),
        "route" => route,
        "map" => crate::convert::map_dict(&b.map),
        "committed_hulls" => b.committed_hulls as i64,
        "committed_tonnage_t" => f(b.committed_tonnage_t).max(0.0),
        "pushback_days" => f(b.pushback_days).max(0.0),
        "forced" => b.forced,
        "expires_h" => b.expires_hour.map_or(-1, hour_gd),
        "dtg" => gs(dtg),
    }
}

/// `InterruptDict`. `region` and `card` are filled for a card arriving (and empty otherwise).
pub(crate) fn interrupt_dict(i: &Interrupt, n: &Names) -> VarDictionary {
    let none = StringName::default;
    let (node, op, hours, player, region, card) = match i {
        Interrupt::Arrived(x) => (sn(x.as_str()), none(), 0, -1, none(), none()),
        Interrupt::OperationOffered(o) | Interrupt::ForcedOperation(o) => (none(), sn(o.as_str()), 0, -1, none(), none()),
        Interrupt::HunterGroupWarning { hours } => (none(), none(), *hours as i64, -1, none(), none()),
        Interrupt::CardArriving { region, card, hours } => (none(), none(), *hours as i64, -1, sn(region.as_str()), sn(card.as_str())),
        Interrupt::ClockLow { days_left } => (none(), none(), (f(*days_left).max(0.0) * 24.0).round() as i64, -1, none(), none()),
        Interrupt::HullReady(p) => (none(), none(), 0, p.0 as i64, none(), none()),
        Interrupt::RefitDone | Interrupt::WaitDone | Interrupt::ActEnded | Interrupt::Paused => (none(), none(), 0, -1, none(), none()),
    };
    d! {
        "kind" => words::interrupt_kind(i),
        "text" => msg_dict(&words::interrupt_msg(i, n)),
        "node" => node, "operation" => op, "hours" => hours, "player" => player, "region" => region, "card" => card,
    }
}

/// `TheatreEventDict`: kind, text and hour, plus the variant's own fields.
pub(crate) fn event_dict(e: &TheatreEvent, hour: u64, n: &Names) -> VarDictionary {
    use TheatreEvent as E;
    let mut out = d! { "kind" => sn(words::event_kind(e)), "text" => msg_dict(&words::event_msg(e, hour, n)), "hour" => hour_gd(hour) };
    let mut put = |k: &str, v: Variant| { out.set(k, &v); };
    let id = |s: &str| sn(s).to_variant();
    match e {
        E::HourAdvanced { hour } => put("hour", hour_gd(*hour).to_variant()),
        E::Interrupted(i) => put("interrupt", interrupt_dict(i, n).to_variant()),
        E::SextantMoved { from, to } => { put("from", id(from.as_str())); put("to", id(to.as_str())); }
        E::LaneCut { a, b, until_hour } => { put("a", id(a.as_str())); put("b", id(b.as_str())); put("until_hour", hour_gd(*until_hour).to_variant()); }
        E::LaneRestored { a, b } => { put("a", id(a.as_str())); put("b", id(b.as_str())); }
        E::DepotEmptied { node } => put("node", id(node.as_str())),
        E::SupplyChanged { region, supply } => { put("region", id(region.as_str())); put("supply", f(*supply).to_variant()); }
        E::PatrolDensityChanged { region, factor } => { put("region", id(region.as_str())); put("factor", f(*factor).to_variant()); }
        E::AlertChanged { region, alert, cause } => { put("region", id(region.as_str())); put("alert", f(*alert).to_variant()); put("cause", cause.to_gd().to_variant()); }
        E::AlertEscalated { region } => put("region", id(region.as_str())),
        E::HunterGroupSortied { group, arrives_hour, ace } => { put("group", (*group as i64).to_variant()); put("arrives_hour", hour_gd(*arrives_hour).to_variant()); put("ace", id(ace.as_ref().map_or("", |a| a.as_str()))); }
        E::HunterGroupArrived { group } => put("group", (*group as i64).to_variant()),
        E::TallyChanged { key, value } => { put("key", gs(&words::tally_key_string(key)).to_variant()); put("value", f(*value).to_variant()); }
        E::CardQueued { region, card, arrives_hour } | E::CardDelayed { region, card, arrives_hour } => { put("region", id(region.as_str())); put("card", id(card.as_str())); put("arrives_hour", hour_gd(*arrives_hour).to_variant()); }
        E::CardCancelled { region, card } | E::CardArrived { region, card } => { put("region", id(region.as_str())); put("card", id(card.as_str())); }
        E::ClockPushedBack { days, hours_left } => { put("days", f(*days).to_variant()); put("hours_left", f(*hours_left).to_variant()); }
        E::ActLost { act } | E::ActWon { act } => put("act", act.to_gd().to_variant()),
        E::ActStarted { act, retreat } => { put("act", act.to_gd().to_variant()); put("retreat", retreat.to_variant()); }
        E::OperationOffered(o) | E::OperationWithdrawn(o) | E::OperationUnlocked(o) => put("operation", id(o.as_str())),
        E::TierOpened(t) => put("tier", t.to_gd().to_variant()),
        E::PartUnlocked(p) => put("part", id(p.as_str())),
        E::HullLost { player } | E::HullRebuilt { player } => put("player", (player.0 as i64).to_variant()),
        E::RefitStarted { player, hours } => { put("player", (player.0 as i64).to_variant()); put("hours", (*hours as i64).to_variant()); }
        E::SextantSunk { relocated_to, ready_hour } => { put("relocated_to", id(relocated_to.as_str())); put("ready_hour", hour_gd(*ready_hour).to_variant()); }
        E::GroupSighted { group, node } => { put("group", (*group as i64).to_variant()); put("node", id(node.as_str())); }
        E::ConvoyRevealed { convoy } => put("convoy", (*convoy as i64).to_variant()),
        E::AceRevealed { ace, node } => { put("ace", id(ace.as_str())); put("node", id(node.as_str())); }
        E::ProposalMade(p) => put("proposal", proposal_dict(p, n).to_variant()),
        E::ProposalAnswered { proposal, player, accept } => { put("proposal", (proposal.0 as i64).to_variant()); put("player", (player.0 as i64).to_variant()); put("accept", accept.to_variant()); }
        E::ProposalCommitted { proposal } | E::ProposalWithdrawn { proposal } => put("proposal", (proposal.0 as i64).to_variant()),
        E::Saved { sequence } => put("sequence", hour_gd(*sequence).to_variant()),
        E::ClockExpired | E::OpenTheatreOpened | E::ResourcesChanged | E::CampaignOver => {}
    }
    out
}

/// `AfterActionDict`.
/// `start_hour` is the theatre hour the report's first events happened at (the sortie's commit hour).
pub(crate) fn after_action_dict(a: &AfterAction, start_hour: u64, n: &Names) -> VarDictionary {
    let p = &a.payout;
    let payout = d! {
        "salvage_wrecks" => p.salvage_wrecks as i64, "salvage_prizes" => p.salvage_prizes as i64, "intel" => p.intel as i64,
        "blueprints" => ids(p.blueprints.iter().map(|b| b.as_str())), "fuel_l" => f(p.fuel_l).max(0.0), "ammo_kg" => f(p.ammo_kg).max(0.0),
    };
    let belief: PackedVector3Array = a.belief.iter().map(|b| Vector3::new(b[0], b[1], b[2])).filter(|v| v.is_finite()).collect();
    let lost: PackedInt32Array = a.hulls_lost.iter().map(|s| s.0 as i32).collect();
    let mut at = start_hour;
    let events: VarArray = a.events.iter().filter_map(|e| {
        if let TheatreEvent::HourAdvanced { hour } = e { at = *hour; return None; }
        words::logged(e).then(|| event_dict(e, at, n).to_variant())
    }).collect();
    d! {
        "operation" => a.operation.as_ref().map_or_else(StringName::default, |o| sn(o.as_str())),
        "outcome" => a.outcome.to_gd(),
        "stats" => crate::convert::stats_dict(&a.stats),
        "payout" => payout,
        "tallies" => arr(a.tallies.iter().map(|(k, v)| tally_dict(k, *v))),
        "kills_by_method" => arr(a.kills_by_method.iter().map(|(m, c)| d! { "method" => m.to_gd(), "count" => *c as i64 })),
        "queued_cards" => arr(a.queued_cards.iter().map(|(r, c, h)| card_dict(c, r, *h, n))),
        "alert_changes" => arr(a.alert_changes.iter().map(|(r, v, c)| d! { "region" => sn(r.as_str()), "alert" => f(*v), "cause" => c.to_gd() })),
        "clock_pushback_days" => f(a.clock_pushback_days).max(0.0),
        "belief" => belief,
        "ghosts" => arr(a.ghosts.iter().map(|(g, met)| d! { "text" => msg_dict(&words::ghost_msg(&g.kind)), "met" => *met, "reward" => msg_dict(&words::reward_msg(&g.reward, n)) })),
        "repair_cost" => arr(a.repair_cost.iter().map(|(p, s)| d! { "player" => p.0 as i64, "salvage" => *s as i64 })),
        "hulls_lost" => lost,
        "events" => events,
    }
}

/// `SaveSlotDict`.
pub(crate) fn save_slot_dict(s: &super::store::SlotInfo) -> VarDictionary {
    let h = s.header.as_ref();
    let players: PackedStringArray = s.players.iter().map(|p| gs(p)).collect();
    d! {
        "path" => gs(&super::store::user_path(&s.rel)),
        "slot_name" => gs(h.map_or("", |h| h.slot_name.as_str())),
        "campaign_id" => gs(&h.map_or_else(String::new, |h| super::store::campaign_dir_name(h.campaign_id))),
        "sequence" => h.map_or(0, |h| h.sequence.min(i64::MAX as u64) as i64),
        "ironman" => h.is_some_and(|h| h.ironman),
        "played_s" => h.map_or(0, |h| h.played_s.min(i64::MAX as u64) as i64),
        "act" => s.act.map_or(-1, |a| a.to_gd()),
        "day" => if s.header.is_some() { hour_gd(s.hour / 24 + 1) } else { 0 },
        "game_version" => gs(h.map_or("", |h| h.game_version.as_str())),
        "modified_unix" => s.modified_unix,
        "is_suspend" => s.is_suspend,
        "loadable" => s.problem.is_none() && s.header.is_some(),
        "problem" => s.problem.as_ref().map_or_else(crate::convert::empty_msg, msg_dict),
        "players" => players,
    }
}

/// `RefitDiffDict`.
pub(crate) fn refit_diff_dict(diff: &content::stats::RefitDiff, cost: &content::stats::RefitCost, affordable: bool, catalogue: &content::catalogue::Catalogue) -> VarDictionary {
    use content::stats::PartChange;
    let cell = |c: &content::cell::CellPos| Vector3i::new(c.x as i32, c.y as i32, c.z as i32);
    let name = |p: &content::ids::PartId| catalogue.part(p).map_or_else(|| sn(p.as_str()), |d| sn(d.name.as_str()));
    let changes = arr(diff.changes.iter().map(|c| match c {
        PartChange::Added(p) => d! { "kind" => sn("added"), "part" => sn(p.part.as_str()), "name" => name(&p.part), "cell" => cell(&p.pos), "from_cell" => Variant::nil() },
        PartChange::Removed(p) => d! { "kind" => sn("removed"), "part" => sn(p.part.as_str()), "name" => name(&p.part), "cell" => cell(&p.pos), "from_cell" => Variant::nil() },
        PartChange::Moved { before, after } => d! { "kind" => sn("moved"), "part" => sn(after.part.as_str()), "name" => name(&after.part), "cell" => cell(&after.pos), "from_cell" => cell(&before.pos) },
    }));
    let mut store = VarDictionary::new();
    for (p, n) in &cost.from_store { store.set(&sn(p.as_str()), *n as i64); }
    d! {
        "changes" => changes,
        "structure_delta_t" => f(diff.structure_delta_t).max(0.0),
        "salvage" => cost.salvage as i64,
        "hours" => cost.hours as i64,
        "from_store" => store,
        "affordable" => affordable,
    }
}
