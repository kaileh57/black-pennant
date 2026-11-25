//! Unit `net-coop`. Campaign co-op session continuity. DESIGN.md L834 (host writes the save; every member
//! keeps a copy), L842 (the host writes an operation checkpoint, a full keyframe plus theatre state, every
//! 60 s and sends it to members; if the host drops any member can re-host and the operation resumes from the
//! last checkpoint; if the host quits mid-operation the checkpoint becomes every member's suspend slot), L843
//! (save copies carry a sequence and the previous save's hash: highest sequence wins, a forked copy is refused
//! with an explanation), L1005 (each member maps its own port in the background after joining; the host sends a
//! succession list of members whose mapping worked, in join order, with external endpoints and a fresh secret;
//! after 10 s without a packet from the host the first member on the list re-hosts from the last checkpoint and
//! the others connect to it automatically; if nobody's mapping worked each member gets a Re-host button that
//! produces a new join code to share by hand).
//!
//! The host app calls [`CoopHost`] next to its `ServerSession`; members call [`CoopMember`] next to their
//! `ClientSession`. Neither owns a session, a Sim or a Theatre: they decide, the app acts.
//!
//! Choices this unit makes where the spec is silent (design decisions):
//! - The host itself is never on the succession list; the app calls [`CoopHost::member_joined`] for remote
//!   members only. A member whose mapping reported double NAT, or an unspecified address or port 0, is left
//!   off: its endpoint is not reachable from outside.
//! - The first checkpoint of a sortie is due at once (a host lost in the first minute still leaves something
//!   to resume), then every [`CHECKPOINT_TICKS`]. A tick lower than the last checkpoint's means a new sortie.
//! - A member keeps a received checkpoint unless it is older than the one held for the same sortie (same
//!   config bytes and save sequence, lower tick) or comes from an earlier save sequence. Sortie ticks restart
//!   every operation, so a bare tick comparison would pin the previous operation's checkpoint.
//! - If the first member on the list has no checkpoint (host lost between operations), it gets `Manual`: it
//!   re-hosts from its save copy through the normal flow. The others still try to connect to it.
//! - [`compare_saves`] works on headers alone: a different sequence means the higher is newer; at one
//!   sequence the copies are `Same` only when the headers are identical, since two different saves made from
//!   one parent share `prev_hash`. With both saves at hand, `content::save::reconcile` also checks the chain
//!   across sequences.
use crate::join::LobbySecret;
use crate::msg::{CheckpointBundle, MappingReport, SuccessionEntry, SuccessionList};
use content::ids::PlayerSlot;
use content::save::{SaveGame, SaveHeader, SortieCheckpoint};
use std::net::SocketAddr;
use std::time::Duration;

/// Largest checkpoint bundle a member decodes (the bulk lane's checkpoint limit).
pub const MAX_BUNDLE_BYTES: u64 = crate::blueprints::MAX_CHECKPOINT_BYTES as u64;

fn fresh_secret() -> LobbySecret { LobbySecret(renet2_netcode::generate_random_bytes()) }

fn reachable(report: &MappingReport) -> Option<SocketAddr> {
    let addr = report.external?;
    (!report.double_nat && addr.port() != 0 && !addr.ip().is_unspecified() && !addr.ip().is_multicast()).then_some(addr)
}

/// Decode a received `TransferKind::Checkpoint` payload (bincode 1, as [`CoopHost::bundle`] writes it), with a
/// size limit. `None` for anything malformed.
pub fn decode_bundle(bytes: &[u8]) -> Option<CheckpointBundle> {
    use bincode::Options;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES { return None; }
    bincode::DefaultOptions::new().with_fixint_encoding().allow_trailing_bytes().with_limit(MAX_BUNDLE_BYTES).deserialize(bytes).ok()
}

/// Checkpoint cadence in ticks (60 s).
pub const CHECKPOINT_TICKS: u64 = sim_core::time::CHECKPOINT_TICKS;
/// Host silence before re-hosting.
pub const HOST_SILENCE: Duration = Duration::from_secs(10);

pub struct CoopHost {
    pub(crate) campaign_id: u64,
    pub(crate) join_order: Vec<(u64, PlayerSlot)>,
    pub(crate) mappings: std::collections::BTreeMap<u64, MappingReport>,
    pub(crate) next_secret: LobbySecret,
    pub(crate) last_checkpoint_tick: Option<u64>,
    pub(crate) dirty: bool,
}

impl CoopHost {
    /// Unit: `net-coop`.
    pub fn new(campaign_id: u64) -> CoopHost {
        CoopHost { campaign_id, join_order: Vec::new(), mappings: Default::default(), next_secret: fresh_secret(), last_checkpoint_tick: None, dirty: true }
    }
    /// Record a member in join order. Unit: `net-coop`.
    pub fn member_joined(&mut self, client_id: u64, slot: PlayerSlot) {
        match self.join_order.iter_mut().find(|(id, _)| *id == client_id) {
            Some(entry) => { if entry.1 != slot { entry.1 = slot; self.dirty = true; } }
            None => { self.join_order.push((client_id, slot)); self.dirty = true; }
        }
    }
    /// Unit: `net-coop`.
    pub fn member_left(&mut self, client_id: u64) {
        let before = self.join_order.len();
        self.join_order.retain(|(id, _)| *id != client_id);
        let mapped = self.mappings.remove(&client_id).is_some();
        if mapped || self.join_order.len() != before { self.dirty = true; }
    }
    /// Unit: `net-coop`.
    pub fn on_mapping(&mut self, client_id: u64, report: MappingReport) {
        // Only members still in the session; a late report from someone who left is ignored.
        if !self.join_order.iter().any(|(id, _)| *id == client_id) { return; }
        if self.mappings.get(&client_id) != Some(&report) {
            self.mappings.insert(client_id, report);
            self.dirty = true;
        }
    }
    /// The list to broadcast when it changed since the last call (fresh `next_secret` each time). Unit: `net-coop`.
    pub fn take_succession(&mut self) -> Option<SuccessionList> {
        if !self.dirty { return None; }
        self.dirty = false;
        self.next_secret = fresh_secret();
        let entries = self.join_order.iter()
            .filter_map(|(id, slot)| self.mappings.get(id).and_then(reachable).map(|external| SuccessionEntry { client_id: *id, slot: *slot, external }))
            .collect();
        Some(SuccessionList { entries, next_secret: self.next_secret.0, campaign_id: self.campaign_id })
    }
    /// True when a checkpoint is due at this sim tick. Unit: `net-coop`.
    pub fn checkpoint_due(&self, tick: u64) -> bool {
        match self.last_checkpoint_tick {
            None => true,
            Some(last) => tick < last || tick - last >= CHECKPOINT_TICKS,
        }
    }
    /// A new sortie starts: its first checkpoint is due at once.
    pub fn sortie_started(&mut self) { self.last_checkpoint_tick = None; }
    /// Bundle a checkpoint (from `Sim::checkpoint`) with the current save for sending on `Bulk` as
    /// `TransferKind::Checkpoint(tick)`. Unit: `net-coop`.
    ///
    /// Bytes are bincode 1 with the crate's default options; [`decode_bundle`] reads them.
    pub fn bundle(&mut self, checkpoint: SortieCheckpoint, save: SaveGame) -> (u64, Vec<u8>) {
        let tick = checkpoint.tick;
        self.last_checkpoint_tick = Some(tick);
        let bundle = CheckpointBundle { checkpoint, save };
        // Plain data with no maps keyed by unsupported types: serialising cannot fail. Were it ever to, the
        // empty payload fails to decode on the member, who keeps its previous checkpoint.
        let bytes = bincode::serialize(&bundle).unwrap_or_default();
        (tick, bytes)
    }
}

/// What a member should do when the host goes silent.
#[derive(Clone, Debug, PartialEq)]
pub enum RehostPlan {
    /// This member is first on the succession list: host on its mapped port with `secret`, restoring from
    /// `checkpoint` (the app rebuilds Theatre from `save` and the Sim with `Sim::from_checkpoint`).
    HostHere { secret: LobbySecret, checkpoint: Box<CheckpointBundle> },
    /// Connect to the member who re-hosts, with the list's secret; retry for up to 30 s.
    ConnectTo { addr: SocketAddr, secret: LobbySecret },
    /// No member mapped: show the Re-host button (a new join code to share by hand).
    Manual { checkpoint: Option<Box<CheckpointBundle>> },
}

pub struct CoopMember {
    pub(crate) client_id: u64,
    pub(crate) checkpoint: Option<Box<CheckpointBundle>>,
    pub(crate) succession: Option<SuccessionList>,
}

impl CoopMember {
    pub fn new(client_id: u64) -> Self { Self { client_id, checkpoint: None, succession: None } }
    /// Keep the newest checkpoint (by tick). Unit: `net-coop`.
    pub fn on_checkpoint(&mut self, bundle: CheckpointBundle) {
        let keep_old = self.checkpoint.as_deref().is_some_and(|cur| {
            let (old, new) = (&cur.save.header, &bundle.save.header);
            if old.campaign_id != new.campaign_id { return false; }
            if new.sequence != old.sequence { return new.sequence < old.sequence; }
            cur.checkpoint.config == bundle.checkpoint.config && bundle.checkpoint.tick < cur.checkpoint.tick
        });
        if !keep_old { self.checkpoint = Some(Box::new(bundle)); }
    }
    pub fn on_succession(&mut self, list: SuccessionList) { self.succession = Some(list); }
    pub fn checkpoint(&self) -> Option<&CheckpointBundle> { self.checkpoint.as_deref() }
    /// True after [`HOST_SILENCE`] without a host packet.
    pub fn host_lost(&self, since_last_packet: Duration) -> bool { since_last_packet >= HOST_SILENCE }
    /// Decide the re-host. Unit: `net-coop`.
    pub fn rehost_plan(&self) -> RehostPlan {
        let manual = || RehostPlan::Manual { checkpoint: self.checkpoint.clone() };
        let Some(list) = &self.succession else { return manual() };
        let Some(first) = list.entries.first() else { return manual() };
        let secret = LobbySecret(list.next_secret);
        if first.client_id == self.client_id {
            match &self.checkpoint {
                Some(cp) => RehostPlan::HostHere { secret, checkpoint: cp.clone() },
                None => manual(),
            }
        } else {
            RehostPlan::ConnectTo { addr: first.external, secret }
        }
    }
}

/// How two copies of one campaign's save relate (L843).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveCompare { Same, TheirsNewer, OursNewer, /// Different saves at a sequence reached by both: refused.
    Forked, DifferentCampaign }

/// Compare headers by campaign id, sequence and previous-save hash chain. Unit: `net-coop`.
pub fn compare_saves(ours: &SaveHeader, theirs: &SaveHeader) -> SaveCompare {
    if ours.campaign_id != theirs.campaign_id { return SaveCompare::DifferentCampaign; }
    match theirs.sequence.cmp(&ours.sequence) {
        std::cmp::Ordering::Greater => SaveCompare::TheirsNewer,
        std::cmp::Ordering::Less => SaveCompare::OursNewer,
        std::cmp::Ordering::Equal => {
            // The slot name is each member's own label for its copy; everything else must agree.
            let same = ours.prev_hash == theirs.prev_hash && ours.played_s == theirs.played_s && ours.ironman == theirs.ironman
                && ours.format == theirs.format && ours.tuning_version == theirs.tuning_version && ours.game_version == theirs.game_version;
            if same { SaveCompare::Same } else { SaveCompare::Forked }
        }
    }
}

#[cfg(test)]
mod tests;
