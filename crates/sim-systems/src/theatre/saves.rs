//! Unit `sys-theatre-saves`. Saving rules: coves, the suspend slot, Ironman, co-op save copies and checkpoints.
//! DESIGN.md L112, L826-828, L843-845.
//!
//! Full saves happen at coves and between operations. Quitting mid-sortie writes a suspend save from the
//! latest 60 s operation checkpoint to the single suspend slot; loading it deletes it. Ironman (a separate
//! toggle): the same suspend slot, one rolling full save, hull loss permanent; the campaign ends when no
//! player hull remains; in co-op Ironman a lost player keeps the chart-table vote and spectates. Co-op: the
//! host writes the save and every member keeps a copy; the host writes an operation checkpoint (full keyframe
//! plus theatre state) every 60 s and sends it to members so any member can re-host; if the host quits
//! mid-operation the checkpoint becomes the suspend slot in every copy. Copies carry a sequence number and
//! the hash of the previous save: on disagreement the highest sequence wins and a forked copy is refused with
//! an explanation; loading the suspend slot advances the sequence. This is protection against accidents, not
//! against cheating. File I/O itself is the caller's (gdclient or server): everything here is bytes in,
//! bytes out, through `content::save`.
//!
//! # Rules where the spec is silent
//! - "At a cove and between operations" = no sortie is active and no committed transit, refit or wait is
//!   still running. Those timed actions live only in the `Theatre` wrapper, so refusing a
//!   save while one runs means no unsaved state can be lost. SEXTANT is at a cove whenever none runs.
//! - A suspend save is itself a link in the chain: its header is the next header after the last full save
//!   (sequence + 1, previous hash = that save's hash). Resuming it advances the sequence once more, and the
//!   resumed theatre chains its next full save to the hash of the suspend record (header plus state).
//! - Copies of different campaigns never reconcile: they read as forked.
//! - Outside Ironman the campaign never ends (L828); in Ironman it ends once every player hull is lost.
use super::{ActiveOperation, Theatre};
use crate::prelude::*;
use content::save::{Reconcile, SaveGame, SaveHeader, SortieCheckpoint, SuspendSave};

/// Where a full save may be written now (L828).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SaveGate { Allowed, InSortie, NotAtCoveOrBetweenOperations }

/// May the campaign be fully saved now? Unit: `sys-theatre-saves`.
pub fn can_save(theatre: &Theatre) -> SaveGate {
    if theatre.active.is_some() { return SaveGate::InSortie; }
    if theatre.pending.is_some() { return SaveGate::NotAtCoveOrBetweenOperations; }
    SaveGate::Allowed
}

/// Next header in the chain for a state about to be saved: sequence + 1, previous hash, played time.
/// Unit: `sys-theatre-saves`.
pub fn next_header(previous: &SaveHeader, previous_hash: ContentHash, game_version: &str, played_s: u64) -> SaveHeader {
    SaveHeader {
        format: content::save::SAVE_FORMAT,
        game_version: game_version.to_string(),
        tuning_version: previous.tuning_version,
        sequence: previous.sequence.saturating_add(1),
        prev_hash: previous_hash,
        campaign_id: previous.campaign_id,
        slot_name: previous.slot_name.clone(),
        ironman: previous.ironman,
        played_s: played_s.max(previous.played_s),
    }
}

/// Build the suspend save from a checkpoint and the theatre state as of sortie start.
/// Unit: `sys-theatre-saves`.
///
/// Theatre time is frozen during a sortie, so the live state is the state at sortie start.
pub fn make_suspend(theatre: &Theatre, checkpoint: SortieCheckpoint, game_version: &str) -> SuspendSave {
    let prev = theatre.last_hash.unwrap_or(ContentHash::EMPTY);
    let mut header = next_header(&theatre.header, prev, game_version, theatre.header.played_s);
    header.tuning_version = theatre.data.catalogue.tuning.version;
    SuspendSave { header, theatre: theatre.state.clone(), sortie: checkpoint }
}

/// Consume a suspend save: returns the theatre to continue from and the checkpoint to restore the Sim; the
/// caller deletes the slot. Advances the sequence. Unit: `sys-theatre-saves`.
///
/// The active operation is rebuilt from the checkpoint's sortie config (operation, region, seed, hour) and
/// the operation's placement; a config that does not decode leaves no active operation, and the result is
/// then placed from `SortieResult::operation` and `region` when it arrives.
pub fn resume_suspend(save: SuspendSave, data: super::TheatreData) -> (Theatre, SortieCheckpoint) {
    let SuspendSave { header, theatre: state, sortie } = save;
    let as_save = SaveGame { header, theatre: state };
    let hash = content::save::save_hash(&as_save);
    let SaveGame { mut header, theatre: state } = as_save;
    header.sequence = header.sequence.saturating_add(1);
    let mut t = Theatre::wrap(state, header, data, Some(hash));
    if let Ok(config) = crate::sim::keyframe::decode_config(&sortie.config) {
        if let Some(op) = config.operation.clone() {
            let placed = super::opgen::placement(&t.state, &t.data, &op);
            let region = config.region.clone().or_else(|| placed.as_ref().map(|p| p.0.clone()));
            let node = placed.map(|p| p.1);
            if let (Some(region), Some(node)) = (region, node) {
                t.active = Some(ActiveOperation { operation: op, region, node, seed: config.seed, started_hour: config.theatre_hour });
            }
        }
    }
    (t, sortie)
}

/// Decide between the host's and a member's copy when a co-op session starts. Unit: `sys-theatre-saves`.
pub fn reconcile_copies(mine: &SaveGame, theirs: &SaveGame) -> Reconcile {
    if mine.header.campaign_id != theirs.header.campaign_id { return Reconcile::Forked; }
    content::save::reconcile(&mine.header, content::save::save_hash(mine), &theirs.header, content::save::save_hash(theirs))
}

/// Ironman: has the campaign ended (no player hull remains)? Unit: `sys-theatre-saves`.
pub fn campaign_over(theatre: &Theatre) -> bool {
    theatre.header.ironman && !theatre.state.players.is_empty() && theatre.state.players.iter().all(|p| p.hull_lost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> SaveHeader {
        SaveHeader { format: 1, game_version: "0.1.0".into(), tuning_version: 3, sequence: 7, prev_hash: ContentHash::EMPTY, campaign_id: 99, slot_name: "a".into(), ironman: true, played_s: 600 }
    }

    #[test]
    fn next_header_chains() {
        let h = header();
        let prev = ContentHash([5; 32]);
        let n = next_header(&h, prev, "0.2.0", 900);
        assert_eq!(n.sequence, 8);
        assert_eq!(n.prev_hash, prev);
        assert_eq!(n.game_version, "0.2.0");
        assert_eq!(n.played_s, 900);
        assert_eq!((n.campaign_id, n.ironman, n.tuning_version), (99, true, 3));
        // Played time never runs backwards; the sequence saturates.
        assert_eq!(next_header(&h, prev, "x", 10).played_s, 600);
        let mut top = h.clone();
        top.sequence = u64::MAX;
        assert_eq!(next_header(&top, prev, "x", 0).sequence, u64::MAX);
    }
}
