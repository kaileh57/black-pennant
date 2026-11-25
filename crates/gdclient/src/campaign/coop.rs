//! Part of unit `gd-campaign`. Co-op campaign traffic (DESIGN.md L832-845): the host's theatre answers
//! members' commands and broadcasts events, the chart, briefings, the after-action report, save copies and
//! 60 s checkpoint bundles; a member mirrors what arrives and keeps its own copy of the save.
//!
//! # Decisions where the contract is silent
//! - Session slots are not theatre slots: a member's commands are mapped to its `TheatreState::players`
//!   entry by client id. A member whose client id the campaign does not know joins it between operations
//!   with the default starter hull (at most four players, L832); during a sortie it waits.
//! - A member mirrors the ledger from the newest save copy (players, stores, difficulty, act facts); the
//!   chart, events, briefings and reports come as they happen. Time only runs on the host.
//! - The chart goes out at most every [`CHART_PERIOD_S`] while time animates, and at once otherwise.
//! - A member's copy replaces its file only when it is newer or the same save (`net::coop::compare_saves`);
//!   a forked copy is refused with `campaign.save.forked` and its file is kept, but the live session still
//!   shows the host's state, which is authoritative.
//! - If the host quits mid-operation, each member writes the last checkpoint bundle it received as its
//!   suspend slot (L842); a re-host from a checkpoint restores the theatre from the bundle's save.
use super::{store, Backend, BpCampaign, Sig};
use crate::enums::CampaignPhase;
use crate::seams::{ClientEvent, ClientMsg, HostEvent, ServerMsg, TransferKind};
use crate::services::Services;
use crate::session::host::{HostControl, HostNote};
use crate::session::Coop;
use content::ids::{OperationId, PlayerSlot};
use content::save::SaveGame;
use net::msg::CheckpointBundle;
use sim_systems::ids::PlayerMask;
use sim_systems::protocol::Msg;
use sim_systems::theatre::{AfterAction, TheatreCommand, TheatreEvent};
use std::cell::Cell;
use std::collections::BTreeMap;

/// Fewest seconds between two chart broadcasts while time animates.
pub(crate) const CHART_PERIOD_S: f64 = 0.25;
/// Largest save copy a member decodes (the bulk lane's own limit).
pub(crate) const MAX_COPY_BYTES: u64 = 64 << 20;

/// One remote member of the session, by session slot.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Member { pub client_id: u64, pub name: String, pub connected: bool }

#[derive(Default)]
pub(crate) struct CoopState {
    /// This install's client id, read once.
    pub local_id: Cell<Option<u64>>,
    /// Host: members by session slot.
    pub members: BTreeMap<PlayerSlot, Member>,
    /// Member: the newest copy of the host's save (also the mirrored ledger).
    pub copy: Option<SaveGame>,
    /// Member: the newest checkpoint bundle (the suspend slot if the host quits mid-operation).
    pub bundle: Option<Box<CheckpointBundle>>,
    /// Host: the chart changed since the last broadcast, and when that was.
    pub chart_dirty: bool,
    pub chart_sent_s: f64,
    /// Host: offers whose briefings the members have.
    pub briefed: Vec<OperationId>,
}

impl CoopState {
    pub fn client_id(&self) -> u64 {
        if let Some(id) = self.local_id.get() { return id; }
        let id = super::local_client_id();
        self.local_id.set(Some(id));
        id
    }
}

/// Bincode of a save copy (as `CoopHost::bundle` writes its bundles).
pub(crate) fn encode_copy(save: &SaveGame) -> Vec<u8> { bincode::serialize(save).unwrap_or_default() }

/// Decode a received save copy with a size limit; `None` for anything malformed.
pub(crate) fn decode_copy(bytes: &[u8]) -> Option<SaveGame> {
    use bincode::Options;
    if bytes.len() as u64 > MAX_COPY_BYTES { return None; }
    let save: SaveGame = bincode::DefaultOptions::new().with_fixint_encoding().allow_trailing_bytes().with_limit(MAX_COPY_BYTES).deserialize(bytes).ok()?;
    (save.header.format == content::save::SAVE_FORMAT).then_some(save)
}

/// What to do with a host's save copy on a member (L843).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyVerdict { Keep, Refuse }

pub(crate) fn copy_verdict(ours: Option<&SaveGame>, theirs: &SaveGame) -> CopyVerdict {
    let Some(ours) = ours else { return CopyVerdict::Keep };
    match net::coop::compare_saves(&ours.header, &theirs.header) {
        net::coop::SaveCompare::Forked => CopyVerdict::Refuse,
        net::coop::SaveCompare::OursNewer => {
            // An older copy of the same chain is fine to mirror but must not overwrite the newer file.
            CopyVerdict::Refuse
        }
        _ => CopyVerdict::Keep,
    }
}

impl BpCampaign {
    fn host_send(&self, to: Option<PlayerSlot>, msg: ServerMsg) {
        let s = crate::services::services();
        let c = s.connection.borrow();
        if let Some(h) = c.as_ref().and_then(|c| c.host.as_ref()) { h.send(HostControl::Send { to, msg }); }
    }
    fn host_bulk(&self, to: Option<PlayerSlot>, kind: TransferKind, bytes: Vec<u8>) {
        let s = crate::services::services();
        let c = s.connection.borrow();
        if let Some(h) = c.as_ref().and_then(|c| c.host.as_ref()) { h.send(HostControl::SendBulk { to, kind, bytes }); }
    }
    /// This process hosts a co-op campaign session.
    pub(crate) fn hosting(&self) -> bool {
        let s = crate::services::services();
        let c = s.connection.borrow();
        c.as_ref().is_some_and(|c| c.net == crate::enums::NetRole::Host && matches!(c.coop, Some(Coop::Host(_))))
    }

    /// Theatre slots of every connected player (this machine's first).
    pub(crate) fn connected_slots(&self) -> Vec<PlayerSlot> {
        let Some(state) = self.state() else { return Vec::new() };
        let local = self.local_slot();
        if self.local().is_none() { return (0..state.players.len().min(8)).map(|i| PlayerSlot(i as u8)).collect(); }
        let mut out = vec![local];
        for m in self.run.coop.members.values().filter(|m| m.connected) {
            if let Some(i) = state.players.iter().position(|p| p.client_id == m.client_id) {
                let s = PlayerSlot(i as u8);
                if !out.contains(&s) { out.push(s); }
            }
        }
        out
    }
    pub(crate) fn connected_mask(&self) -> PlayerMask { self.connected_slots().into_iter().fold(PlayerMask(0), |m, s| m.with(s)) }
    /// More than one player decides: proposals wait for votes (the theatre's own rule).
    pub(crate) fn is_coop(&self) -> bool {
        match &self.backend {
            Some(Backend::Local(t)) => t.state.players.len() > 1 && self.connected_mask().count() > 1,
            Some(Backend::Remote { .. }) => true,
            None => false,
        }
    }

    /// A member's decision goes to the host.
    pub(crate) fn send_to_host(&mut self, cmd: TheatreCommand) -> bool {
        let s = crate::services::services();
        let mut c = s.connection.borrow_mut();
        match c.as_mut() {
            Some(conn) => { conn.link.send(ClientMsg::Theatre(cmd)); true }
            None => { drop(c); drop(s); self.reject(Msg::new("campaign.reject.member_waits")); false }
        }
    }

    // ---- Host broadcasts ----

    pub(crate) fn coop_broadcast_events(&mut self, events: &[TheatreEvent]) {
        if events.is_empty() || !self.hosting() { return; }
        for e in events { self.host_send(None, ServerMsg::Theatre(e.clone())); }
        self.run.coop.chart_dirty = true;
        if events.iter().any(|e| matches!(e, TheatreEvent::OperationOffered(_) | TheatreEvent::OperationWithdrawn(_))) { self.run.coop.briefed.clear(); }
    }
    pub(crate) fn coop_send_chart(&mut self, to: Option<PlayerSlot>) {
        if !self.hosting() { return; }
        let Some(t) = self.local() else { return };
        let chart = Box::new(t.chart());
        self.host_send(to, ServerMsg::Chart(chart));
        if to.is_none() { self.run.coop.chart_dirty = false; self.run.coop.chart_sent_s = self.run.now_s; }
    }
    /// Briefings of every offer the members do not have yet.
    fn coop_send_briefings(&mut self, to: Option<PlayerSlot>) {
        let Some(t) = self.local() else { return };
        let offers: Vec<OperationId> = t.state.offered.clone();
        let fresh: Vec<OperationId> = if to.is_some() { offers.clone() } else { offers.iter().filter(|o| !self.run.coop.briefed.contains(o)).cloned().collect() };
        let briefs: Vec<_> = fresh.iter().filter_map(|o| t.briefing(o)).collect();
        for b in briefs { self.host_send(to, ServerMsg::Briefing(Box::new(b))); }
        if to.is_none() { self.run.coop.briefed = offers; }
    }
    pub(crate) fn coop_send_after_action(&mut self, aa: &AfterAction) {
        if !self.hosting() { return; }
        self.host_send(None, ServerMsg::AfterAction(Box::new(aa.clone())));
        self.coop_send_chart(None);
    }
    /// After every full save the members get a copy (L834).
    pub(crate) fn coop_send_copy(&mut self, save: &SaveGame, to: Option<PlayerSlot>) {
        if !self.hosting() { return; }
        self.host_bulk(to, TransferKind::SaveCopy(save.header.sequence), encode_copy(save));
    }
    pub(crate) fn coop_sortie_started(&mut self, services: &Services) {
        if let Some(Coop::Host(h)) = services.connection.borrow_mut().as_mut().and_then(|c| c.coop.as_mut()) { h.sortie_started(); }
    }

    // ---- Each frame ----

    pub(crate) fn coop_frame(&mut self, services: &Services) {
        let (net, kind) = services.connection.borrow().as_ref().map_or((crate::enums::NetRole::Offline, crate::enums::SessionKind::None), |c| (c.net, c.kind));
        let mut session: Vec<ClientEvent> = Vec::new();
        let mut host: Vec<HostEvent> = Vec::new();
        let mut checkpoints: Vec<content::save::SortieCheckpoint> = Vec::new();
        {
            let feed = services.feed.borrow();
            for e in &feed.session {
                if matches!(e, ClientEvent::Theatre(_) | ClientEvent::TheatreRejected(_) | ClientEvent::Chart(_) | ClientEvent::Briefing(_) | ClientEvent::AfterAction(_) | ClientEvent::Checkpoint(_) | ClientEvent::BulkReceived(_) | ClientEvent::Ended(_) | ClientEvent::SortieLoading(_)) {
                    session.push(e.clone());
                }
            }
            for n in &feed.host_notes {
                match n {
                    HostNote::Session(e @ (HostEvent::PlayerJoined { .. } | HostEvent::PlayerLeft { .. } | HostEvent::HoldExpired { .. } | HostEvent::Theatre { .. } | HostEvent::BulkReceived { .. })) => host.push(e.clone()),
                    HostNote::Checkpoint(c) => checkpoints.push((**c).clone()),
                    _ => {}
                }
            }
        }
        if net == crate::enums::NetRole::Offline { self.run.coop.members.clear(); }
        for e in host { self.on_host_event(e); }
        for c in checkpoints { self.bundle_checkpoint(services, c); }
        for e in session { self.on_client_event(e, net, kind); }
        let animating = self.advancing;
        if self.run.coop.chart_dirty && self.local().is_some() && (!animating || self.run.now_s - self.run.coop.chart_sent_s >= CHART_PERIOD_S) {
            self.coop_send_chart(None);
            self.coop_send_briefings(None);
        }
    }

    fn on_host_event(&mut self, e: HostEvent) {
        match e {
            HostEvent::PlayerJoined { slot, client_id, name, .. } => {
                self.run.coop.members.insert(slot, Member { client_id, name: name.clone(), connected: true });
                if self.local().is_none() { return; }
                self.admit_member(client_id, &name);
                self.coop_send_chart(Some(slot));
                self.coop_send_briefings(Some(slot));
                let root = super::saves::saves_root();
                if let Some(id) = self.local().map(|t| t.header().campaign_id) {
                    if let Some(save) = store::latest_save(&root, id) { self.coop_send_copy(&save, Some(slot)); }
                }
                self.run.autosave_due = true;
                self.run.sigs.push(Sig::Chart);
            }
            HostEvent::PlayerLeft { client_id, .. } | HostEvent::HoldExpired { client_id, .. } => {
                for m in self.run.coop.members.values_mut() { if m.client_id == client_id { m.connected = false; } }
                self.run.sigs.push(Sig::Chart);
            }
            HostEvent::Theatre { slot, command } => {
                let Some(client_id) = self.run.coop.members.get(&slot).map(|m| m.client_id) else { return };
                let Some(t) = self.local() else { return };
                let Some(i) = t.state.players.iter().position(|p| p.client_id == client_id) else {
                    self.host_send(Some(slot), ServerMsg::TheatreRejected(sim_systems::theatre::TheatreReject::UnknownTarget));
                    return;
                };
                if let TheatreCommand::PauseClock(p) = command { self.run.clock_paused = p; }
                let mask = self.connected_mask();
                let r = self.local_mut().map(|t| t.command(PlayerSlot(i as u8), false, mask, command));
                self.pump();
                match r {
                    Some(Ok(())) => self.run.autosave_due = true,
                    Some(Err(e)) => self.host_send(Some(slot), ServerMsg::TheatreRejected(e)),
                    None => {}
                }
            }
            HostEvent::BulkReceived { slot, completed } => {
                let TransferKind::SaveCopy(_) = completed.kind else { return };
                let Some(theirs) = decode_copy(&completed.bytes) else { return };
                self.reconcile_member_copy(slot, theirs);
            }
            _ => {}
        }
    }

    /// A member the campaign does not know joins it between operations (at most four players).
    fn admit_member(&mut self, client_id: u64, name: &str) {
        let Some(t) = self.local_mut() else { return };
        if t.state.players.iter().any(|p| p.client_id == client_id) { return; }
        if t.active().is_some() || t.state.players.len() >= sim_systems::theatre::MAX_CAMPAIGN_PLAYERS { return; }
        let Ok(bp) = content::campaign::load::stock_blueprint(super::DEFAULT_STARTER) else { return };
        // Missing store slots read as full (`economy`), so the new hull sails fully loaded.
        t.state.players.push(content::save::PlayerRecord {
            client_id, name: name.to_string(), blueprint: bp, damage: Default::default(), fuel_fraction: 1.0, ammo: BTreeMap::new(), hull_lost: false, ready_at_hour: None, veterancy: 0.0,
        });
        self.bump();
    }

    /// A member's own copy at session start (L843): a newer copy of this campaign is adopted between
    /// operations; any other answer is our copy, which the member checks for itself.
    fn reconcile_member_copy(&mut self, slot: PlayerSlot, theirs: SaveGame) {
        let root = super::saves::saves_root();
        let Some(t) = self.local() else { return };
        let mine = store::latest_save(&root, t.header().campaign_id);
        let between = t.active().is_none() && !self.advancing && self.phase != CampaignPhase::Sortie;
        let take = match &mine { Some(m) => sim_systems::theatre::saves::reconcile_copies(m, &theirs) == content::save::Reconcile::TakeTheirs, None => false };
        if take && between {
            let data = super::theatre_data();
            let _ = store::write_save(&root, &theirs, super::store::AUTOSAVE_STEM);
            self.backend = Some(Backend::Local(Box::new(sim_systems::theatre::Theatre::from_save(theirs.clone(), data))));
            self.run.log.clear();
            self.run.proposals.clear();
            self.bump();
            self.run.sigs.push(Sig::Chart);
            self.run.sigs.push(Sig::Resources);
            self.coop_send_copy(&theirs, None);
            self.coop_send_chart(None);
        } else if let Some(m) = mine {
            self.coop_send_copy(&m, Some(slot));
        }
    }

    /// Bundle the 60 s checkpoint with the theatre as of sortie start and send it to the members (L842).
    fn bundle_checkpoint(&mut self, services: &Services, checkpoint: content::save::SortieCheckpoint) {
        let Some(t) = self.local() else { return };
        let suspend = sim_systems::theatre::saves::make_suspend(t, checkpoint.clone(), env!("CARGO_PKG_VERSION"));
        let save = SaveGame { header: suspend.header, theatre: suspend.theatre };
        let mut conn = services.connection.borrow_mut();
        let Some(c) = conn.as_mut() else { return };
        let Some(Coop::Host(h)) = c.coop.as_mut() else { return };
        let (tick, bytes) = h.bundle(checkpoint, save);
        if let Some(host) = c.host.as_ref() { host.send(HostControl::SendBulk { to: None, kind: TransferKind::Checkpoint(tick), bytes }); }
    }

    // ---- Member side ----

    #[allow(clippy::type_complexity)]
    fn remote_mut(&mut self) -> Option<(&mut Option<Box<sim_systems::theatre::ChartView>>, &mut Vec<sim_systems::theatre::Briefing>, &mut content::save::Resources)> {
        match &mut self.backend { Some(Backend::Remote { chart, briefings, resources }) => Some((chart, briefings, resources)), _ => None }
    }

    fn on_client_event(&mut self, e: ClientEvent, net: crate::enums::NetRole, kind: crate::enums::SessionKind) {
        let member = net == crate::enums::NetRole::Client;
        match e {
            // A re-host from a checkpoint: this process now runs the operation.
            ClientEvent::Checkpoint(b) if net == crate::enums::NetRole::Host && self.local().is_none() => self.restore_from_bundle(*b),
            ClientEvent::Checkpoint(b) if member => self.run.coop.bundle = Some(b),
            ClientEvent::Chart(c) if member => {
                if self.backend.is_none() {
                    if kind != crate::enums::SessionKind::Campaign && kind != crate::enums::SessionKind::None { return; }
                    self.backend = Some(Backend::Remote { chart: None, briefings: Vec::new(), resources: Default::default() });
                    self.set_phase(CampaignPhase::Chart);
                }
                if let Some((chart, _, _)) = self.remote_mut() { *chart = Some(c); }
                self.bump();
                self.run.sigs.push(Sig::Chart);
            }
            ClientEvent::Briefing(b) if member => {
                if let Some((_, briefings, _)) = self.remote_mut() {
                    briefings.retain(|x| x.operation != b.operation);
                    briefings.push(*b);
                    if briefings.len() > 32 { briefings.remove(0); }
                }
                self.bump();
            }
            ClientEvent::Theatre(ev) if member && self.local().is_none() && self.backend.is_some() => {
                let hour = self.remote_hour(&ev);
                match &ev {
                    TheatreEvent::HourAdvanced { .. } if self.phase == CampaignPhase::Chart => self.set_phase(CampaignPhase::Advancing),
                    TheatreEvent::Interrupted(_) if self.phase == CampaignPhase::Advancing => self.set_phase(CampaignPhase::Chart),
                    _ => {}
                }
                self.observe_event(&ev, hour);
            }
            ClientEvent::TheatreRejected(r) if member && self.local().is_none() => self.reject_typed(r),
            ClientEvent::AfterAction(aa) if member && self.local().is_none() && self.backend.is_some() => {
                self.run.report_hour = self.remote_chart_hour();
                self.after_action = Some(aa);
                self.run.sigs.push(Sig::AfterAction);
                self.set_phase(CampaignPhase::AfterAction);
            }
            ClientEvent::SortieLoading(_) if member && self.backend.is_some() => self.set_phase(CampaignPhase::Sortie),
            ClientEvent::BulkReceived(done) if member => {
                let TransferKind::SaveCopy(_) = done.kind else { return };
                let Some(theirs) = decode_copy(&done.bytes) else { return };
                self.take_copy(theirs);
            }
            ClientEvent::Ended(end) if member && self.local().is_none() && self.backend.is_some() => {
                if end == net::error::SessionEnd::HostQuit && self.phase == CampaignPhase::Sortie {
                    if let Some(b) = self.run.coop.bundle.take() {
                        let root = super::saves::saves_root();
                        let s = content::save::SuspendSave { header: b.save.header.clone(), theatre: b.save.theatre.clone(), sortie: b.checkpoint.clone() };
                        if store::write_suspend(&root, &s).is_err() { self.reject(Msg::new("campaign.save.suspend_failed")); }
                    }
                }
                // The host is gone (a re-host restores from its checkpoint): leave the mirror.
                if end != net::error::SessionEnd::HostLost { self.close_mirror(); }
            }
            _ => {}
        }
    }

    fn remote_chart_hour(&self) -> u64 {
        match &self.backend { Some(Backend::Remote { chart: Some(c), .. }) => c.hour, _ => self.state().map_or(0, |s| s.hour) }
    }
    fn remote_hour(&self, e: &TheatreEvent) -> u64 {
        if let TheatreEvent::HourAdvanced { hour } = e { *hour } else { self.remote_chart_hour() }
    }

    /// A save copy from the host: mirrored always, written to disk unless it is older or forked (L843).
    fn take_copy(&mut self, theirs: SaveGame) {
        let root = super::saves::saves_root();
        let ours = store::latest_save(&root, theirs.header.campaign_id);
        match copy_verdict(ours.as_ref(), &theirs) {
            CopyVerdict::Keep => { if store::write_save(&root, &theirs, store::COPY_STEM).is_err() { self.reject(Msg::new("campaign.save.write_failed")); } }
            CopyVerdict::Refuse => {
                let forked = ours.as_ref().is_some_and(|o| net::coop::compare_saves(&o.header, &theirs.header) == net::coop::SaveCompare::Forked);
                if forked { self.reject(Msg::new("campaign.save.forked")); }
            }
        }
        if let Some((_, _, resources)) = self.remote_mut() { *resources = theirs.theatre.resources.clone(); }
        self.run.coop.copy = Some(theirs);
        self.bump();
        self.run.sigs.push(Sig::Resources);
        self.run.sigs.push(Sig::Chart);
    }

    /// Become the host of a re-hosted operation from a checkpoint bundle (L842).
    fn restore_from_bundle(&mut self, b: CheckpointBundle) {
        let s = content::save::SuspendSave { header: b.save.header, theatre: b.save.theatre, sortie: b.checkpoint };
        let (t, _checkpoint) = sim_systems::theatre::saves::resume_suspend(s, super::theatre_data());
        self.backend = Some(Backend::Local(Box::new(t)));
        self.run.coop.copy = None;
        self.run.sortie = None;
        self.bump();
        self.set_phase(CampaignPhase::Sortie);
        self.run.sigs.push(Sig::Resources);
    }

    /// Leave a member's mirror (the session ended).
    pub(crate) fn close_mirror(&mut self) {
        self.backend = None;
        self.after_action = None;
        self.run.coop.copy = None;
        self.run.coop.bundle = None;
        self.run.proposals.clear();
        self.run.log.clear();
        self.bump();
        self.set_phase(CampaignPhase::None);
    }
}
