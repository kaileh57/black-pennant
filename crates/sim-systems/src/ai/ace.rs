//! Unit `sys-ai-ace`. Aces: seven named Combine captains with a signature ship, doctrine and radio callsign.
//! DESIGN.md L774 (returns are authored and fixed in script; no enemy remembers individual player actions),
//! L1706 (text with radio effects, never voiced).
//!
//! An ace is an ordinary captain plus authored lines on cues and the ace's doctrine cards, which are always
//! active for the group the ace leads. Whether an ace returns in a later operation is decided by the act's
//! data (`AceArc::operations`), never by what the player did.
//!
//! Cue facts come from the ace's own hull, its own shots and its side's picture; nothing here reads the
//! player's hull. Each line is the authored key with the ace's callsign as its first argument, so the client
//! can name the speaker on the radio panel.
use crate::prelude::*;
use crate::tracks::SidePicture;
use content::campaign::{AceCue, AceDef};
use sim_core::events::PhysEvent;
use sim_core::vessel::VesselStatus;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AceState {
    pub ace: AceId,
    pub vessel: EntityId,
    pub group: Option<GroupId>,
    /// Cues already spoken this sortie; each cue speaks once.
    pub spoken: Vec<AceCue>,
    pub beaten: bool,
    pub captured: bool,
}

/// Facts the cue test reads this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AceFacts { pub arrived: bool, pub holds_player_track: bool, pub fired: bool, pub took_damage: bool, pub player_damaged: bool, pub withdrawing: bool, pub disabled_or_lost: bool, pub captured: bool }

/// Cues in the order they are tested within one tick.
const CUE_ORDER: [AceCue; 8] = [AceCue::Arrives, AceCue::SightsPlayer, AceCue::FiresFirst, AceCue::TakesDamage, AceCue::PlayerDamaged, AceCue::Withdraws, AceCue::Defeated, AceCue::Captured];
/// An opposing contact counts as sighted by the ace inside this range of its ship, metres.
const SIGHT_RANGE_M: f64 = 30_000.0;

/// Lines to speak now: each cue of the ace's arc for this operation fires once when its fact first holds.
/// Unit: `sys-ai-ace`.
///
/// The arcs used are those listing `operation`; when none does, arcs with an empty operation list (generic
/// lines for sandbox, skirmish and splash) are used. A cue whose fact holds is marked spoken even when no line
/// is authored for it. Once the ace is defeated or captured, the cues of a live ace (arrival through
/// withdrawal) are closed without speaking. `beaten` and `captured` are set from the facts.
pub fn lines(state: &mut AceState, def: &AceDef, operation: &OperationId, facts: &AceFacts) -> Vec<(AceCue, Msg)> {
    let mut out = Vec::new();
    if def.id != state.ace { return out; }
    if facts.disabled_or_lost { state.beaten = true; }
    if facts.captured { state.captured = true; }
    let listed: Vec<&content::campaign::AceArc> = def.arcs.iter().filter(|a| a.operations.iter().any(|o| o == operation)).collect();
    let arcs: Vec<&content::campaign::AceArc> = if listed.is_empty() { def.arcs.iter().filter(|a| a.operations.is_empty()).collect() } else { listed };
    let done = facts.disabled_or_lost || facts.captured;
    for cue in CUE_ORDER {
        if state.spoken.contains(&cue) { continue; }
        let live_cue = !matches!(cue, AceCue::Defeated | AceCue::Captured);
        let holds = match cue {
            AceCue::Arrives => facts.arrived,
            AceCue::SightsPlayer => facts.holds_player_track,
            AceCue::FiresFirst => facts.fired,
            AceCue::TakesDamage => facts.took_damage,
            AceCue::PlayerDamaged => facts.player_damaged,
            AceCue::Withdraws => facts.withdrawing,
            AceCue::Defeated => facts.disabled_or_lost,
            AceCue::Captured => facts.captured,
        };
        if done && live_cue { state.spoken.push(cue); continue; }
        if !holds { continue; }
        state.spoken.push(cue);
        if let Some(line) = arcs.iter().flat_map(|a| a.lines.iter()).find(|l| l.cue == cue) {
            out.push((cue, Msg { key: line.text.clone(), args: vec![MsgArg::Key(def.callsign.clone())] }));
        }
    }
    out
}

/// Gather the facts for an ace from the world and its side's picture. Unit: `sys-ai-ace`.
///
/// - `arrived`: the ace's ship is present and afloat.
/// - `holds_player_track`: the side holds a live Suspect or Hostile surface or subsurface contact, graded
///   Coarse or better, within 30 km of the ace's ship.
/// - `fired`: the ace's ship fired a gun or launched this tick.
/// - `took_damage`: the ace's ship took weapon damage this tick.
/// - `player_damaged`: a shot of the ace damaged a hull that is not of the ace's side this tick.
/// - `withdrawing`: the ace's hull meets the captain's hurt rule (`unit::wants_withdraw` with ace
///   thresholds); `sys-ai` may also OR in `CaptainState::is_withdrawing`.
/// - `disabled_or_lost`: the ship is gone, disabled, struck or lost.
/// - `captured`: the ship now belongs to another side (a prize).
pub fn facts(state: &AceState, world: &World, picture: &SidePicture, events: &[PhysEvent]) -> AceFacts {
    let me = state.vessel;
    let mut f = AceFacts::default();
    let vessel = if me.is_none() { None } else { world.vessel(me) };
    let Some(v) = vessel else {
        f.disabled_or_lost = true;
        f.captured = state.captured;
        return f;
    };
    let side = picture.side;
    f.captured = state.captured || v.side != side;
    f.disabled_or_lost = v.status != VesselStatus::Active || v.struck;
    f.arrived = !f.disabled_or_lost && !f.captured;
    let pos = DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z);
    f.holds_player_track = f.arrived && picture.tracks.iter().any(|t| {
        t.is_live() && matches!(t.domain, Domain::Surface | Domain::Subsurface) && matches!(t.stance, Stance::Suspect | Stance::Hostile) && t.grade >= TrackGrade::Coarse
            && t.pos.x.is_finite() && t.pos.z.is_finite() && DVec2::new(t.pos.x, t.pos.z).distance(pos) <= SIGHT_RANGE_M
    });
    for e in events {
        match e {
            PhysEvent::MountFired { vessel, .. } if *vessel == me => f.fired = true,
            PhysEvent::BodySpawned { launcher, .. } if *launcher == me => f.fired = true,
            PhysEvent::CellsDamaged { vessel, changes, cause, attacker } => {
                let weapon = !matches!(cause, sim_core::damage::DamageCause::Collision | sim_core::damage::DamageCause::Grounding | sim_core::damage::DamageCause::Fire);
                if *vessel == me && weapon && !changes.is_empty() { f.took_damage = true; }
                if *attacker == me && *vessel != me && !changes.is_empty() && world.vessel(*vessel).map_or(true, |o| o.side != side) { f.player_damaged = true; }
            }
            PhysEvent::Breach { vessel, .. } if *vessel == me => f.took_damage = true,
            PhysEvent::VesselLost { vessel, .. } if *vessel == me => { f.disabled_or_lost = true; f.arrived = false; }
            _ => {}
        }
    }
    if f.arrived {
        let h = super::unit::hull_health(v, world.catalogue());
        f.withdrawing = super::unit::wants_withdraw(&h, super::unit::Behaviour::Combatant, true);
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::campaign::{AceArc, AceLine};
    use content::enums::Act;

    fn def() -> AceDef {
        AceDef {
            id: AceId::new("ace.1"), name: TextKey::new("ace.1.name"), callsign: TextKey::new("ace.1.callsign"), ship: ClassId::new("class.a"), doctrine: vec![],
            arcs: vec![
                AceArc { act: Act::Act1, operations: vec![OperationId::new("op.1")], lines: vec![
                    AceLine { cue: AceCue::Arrives, text: TextKey::new("ace.1.op1.arrives") },
                    AceLine { cue: AceCue::Defeated, text: TextKey::new("ace.1.op1.defeated") },
                    AceLine { cue: AceCue::Withdraws, text: TextKey::new("ace.1.op1.withdraws") },
                ] },
                AceArc { act: Act::Act1, operations: vec![], lines: vec![AceLine { cue: AceCue::Arrives, text: TextKey::new("ace.1.any.arrives") }] },
            ],
        }
    }
    fn state() -> AceState { AceState { ace: AceId::new("ace.1"), vessel: EntityId::from_u64(5), ..Default::default() } }

    #[test]
    fn each_cue_speaks_once_with_callsign() {
        let d = def();
        let mut s = state();
        let op = OperationId::new("op.1");
        let f = AceFacts { arrived: true, ..Default::default() };
        let l = lines(&mut s, &d, &op, &f);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].0, AceCue::Arrives);
        assert_eq!(l[0].1.key, TextKey::new("ace.1.op1.arrives"));
        assert_eq!(l[0].1.args, vec![MsgArg::Key(TextKey::new("ace.1.callsign"))]);
        assert!(lines(&mut s, &d, &op, &f).is_empty());
        // A cue with no authored line closes silently.
        let f2 = AceFacts { arrived: true, fired: true, ..Default::default() };
        assert!(lines(&mut s, &d, &op, &f2).is_empty());
        assert!(s.spoken.contains(&AceCue::FiresFirst));
    }

    #[test]
    fn generic_arc_when_operation_not_listed() {
        let d = def();
        let mut s = state();
        let l = lines(&mut s, &d, &OperationId::new("op.other"), &AceFacts { arrived: true, ..Default::default() });
        assert_eq!(l[0].1.key, TextKey::new("ace.1.any.arrives"));
    }

    #[test]
    fn defeat_closes_live_cues_and_marks_beaten() {
        let d = def();
        let mut s = state();
        let op = OperationId::new("op.1");
        let l = lines(&mut s, &d, &op, &AceFacts { arrived: true, withdrawing: true, disabled_or_lost: true, ..Default::default() });
        assert_eq!(l.iter().map(|x| x.0).collect::<Vec<_>>(), vec![AceCue::Defeated]);
        assert!(s.beaten && !s.captured);
        assert!(lines(&mut s, &d, &op, &AceFacts { withdrawing: true, ..Default::default() }).is_empty());
        // Wrong definition: nothing.
        let mut other = state();
        other.ace = AceId::new("ace.2");
        assert!(lines(&mut other, &d, &op, &AceFacts { arrived: true, ..Default::default() }).is_empty());
    }
}
