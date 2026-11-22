//! Unit `sys-datalink`. The logical side of the Data network. DESIGN.md L283 (Data carries track and command
//! messages), L394 (losing the Data link), L460 (own-side units share detections at 10 Hz), L500 (Friend over
//! datalink), L522 (midcourse updates ride the datalink with no horizon limit).
//!
//! The physical side (conduits, zones, `LinkState` per part) is sim-core's `networks`. This module answers:
//! which units are on a side's link, what a part may use when its own Data link is down, and whether a ship's
//! consoles still command its parts.
//!
//! Decisions where the spec is silent:
//! - Power `Dead` or `LocalControl` (no network, battery flat) or a shed consumer is a dead part (no power).
//!   A part that loses only its Data link goes straight to local control (L394: the buffer step is skipped
//!   for Data).
//! - A design with no console part at all keeps command (there is nothing to lose); a design whose consoles
//!   are all destroyed, unpowered or cut off from Data is isolated.
//! - [`friend_reports`] reports every live entity of the link's sides, not only the members: an isolated
//!   hull, a torpedo, a mine or a missile in its terminal run is still own-side and must never become a
//!   Suspect track that doctrine could engage (rule 1, L500). Membership only decides whose detections reach
//!   the picture and who reads it.
use crate::prelude::*;
use content::part::weapons::MunitionKind;
use content::part::PartFunction;
use sim_core::munitions::{BodyPhase, Bodies};
use sim_core::networks::{LinkState, PartLinks};
use sim_core::vessel::{Vessel, VesselStatus};

/// How a part is being commanded this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum PartCommand {
    /// On the ship's Data network: officers, tracks from the side picture.
    #[default] Networked,
    /// Data link lost (the buffer step is skipped for Data, L394): the part uses its own sensor with
    /// `NetworkTuning::local_control_error_factor` (3x) and the HUD says so.
    Local { error_factor: f64 },
    /// No power, or destroyed.
    Dead,
}

/// Command mode of a part from its link state. Unit: `sys-datalink`.
pub fn part_command(links: &PartLinks, works: bool, tuning: &content::tuning::NetworkTuning) -> PartCommand {
    if !works || links.shed || matches!(links.power, LinkState::Dead | LinkState::LocalControl) { return PartCommand::Dead; }
    match links.data {
        LinkState::Network | LinkState::Redundant | LinkState::Buffer => PartCommand::Networked,
        LinkState::LocalControl | LinkState::Dead => {
            let f = tuning.local_control_error_factor;
            PartCommand::Local { error_factor: if f.is_finite() && f >= 1.0 { f } else { 3.0 } }
        }
    }
}

/// One side's datalink membership this tick.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkPicture {
    /// Entities contributing detections to and reading the side picture, ascending: hulls with a working
    /// console on a live Data network, drones and helicopters in the air, missiles in midcourse.
    pub members: Vec<EntityId>,
    /// Own-side hulls cut off from the link (no working console): they fight on local control and their
    /// detections do not reach the picture.
    pub isolated: Vec<EntityId>,
    /// Sides sharing this link (the side and its allies), set by [`membership`] and read by
    /// [`friend_reports`].
    #[serde(default)]
    pub sides: SideMask,
}
impl LinkPicture {
    /// True when the entity's detections reach the picture.
    pub fn is_member(&self, e: EntityId) -> bool { self.members.binary_search(&e).is_ok() }
}

fn hull_alive(status: VesselStatus) -> bool { matches!(status, VesselStatus::Active | VesselStatus::Disabled) }

fn munition_facts(catalogue: &Catalogue, idx: MunitionIdx) -> Option<(MunitionKind, Domain)> {
    catalogue.munitions.get_index(idx.index()).map(|(_, d)| (d.kind, d.domain))
}

/// A small body that carries the link: drones, helicopters and aircraft while airborne, missiles in
/// midcourse (boost and cruise; a missile in its terminal run flies on its own seeker).
fn body_is_member(kind: MunitionKind, phase: BodyPhase) -> bool {
    match kind {
        MunitionKind::Drone | MunitionKind::Helicopter | MunitionKind::Aircraft => !matches!(phase, BodyPhase::Dead | BodyPhase::Passive),
        MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile => matches!(phase, BodyPhase::Boost | BodyPhase::Cruise),
        _ => false,
    }
}

fn body_alive(bodies: &Bodies, i: usize) -> bool {
    bodies.phase.get(i).is_some_and(|&p| p != BodyPhase::Dead) && !bodies.hp.get(i).is_some_and(|&hp| hp == 0)
}

/// World-free membership core: hulls as (entity, side, alive, has command), bodies from the table.
pub(crate) fn membership_core(hulls: &[(EntityId, SideId, bool, bool)], bodies: &Bodies, catalogue: &Catalogue, sides: SideMask) -> LinkPicture {
    let mut pic = LinkPicture { sides, ..Default::default() };
    for &(e, s, alive, command) in hulls {
        if !alive || !sides.has(s) { continue; }
        if command { pic.members.push(e) } else { pic.isolated.push(e) }
    }
    for i in 0..bodies.len() {
        let (Some(&e), Some(&s), Some(&k), Some(&ph)) = (bodies.entity.get(i), bodies.side.get(i), bodies.kind.get(i), bodies.phase.get(i)) else { continue };
        if !sides.has(s) || !body_alive(bodies, i) { continue; }
        if let Some((kind, _)) = munition_facts(catalogue, k) { if body_is_member(kind, ph) { pic.members.push(e); } }
    }
    pic.members.sort_unstable();
    pic.members.dedup();
    pic.isolated.sort_unstable();
    pic.isolated.dedup();
    pic
}

/// Recompute membership for a side (allied sides share one link, `SideDef::allied`). Unit: `sys-datalink`.
pub fn membership(world: &World, side: SideId, allied: SideMask) -> LinkPicture {
    let sides = allied.with(side);
    let cat = world.catalogue();
    let hulls: Vec<(EntityId, SideId, bool, bool)> = world.vessels()
        .filter(|(_, v)| sides.has(v.side))
        .map(|(_, v)| { let alive = hull_alive(v.status); (v.id, v.side, alive, alive && has_command(v, cat)) })
        .collect();
    membership_core(&hulls, world.bodies(), cat, sides)
}

/// Whether one console part still serves the Data network.
fn console_serves(p: &sim_core::parts::PartRuntime) -> bool {
    p.works() && !p.links.shed && !matches!(p.links.power, LinkState::Dead | LinkState::LocalControl)
        && matches!(p.links.data, LinkState::Network | LinkState::Redundant | LinkState::Buffer)
}

/// True when a vessel still has a working console on its Data network (priority 1 consumer, L291).
/// Unit: `sys-datalink`.
pub fn has_command(vessel: &Vessel, catalogue: &Catalogue) -> bool {
    if !hull_alive(vessel.status) { return false; }
    let mut consoles = 0u32;
    for p in &vessel.parts {
        let is_console = catalogue.parts.get_index(p.def.index()).is_some_and(|(_, d)| matches!(d.function, PartFunction::Console));
        if !is_console { continue; }
        consoles += 1;
        if console_serves(p) { return true; }
    }
    consoles == 0
}

/// World-free friend report core over hulls (entity, side, alive, domain, pos, vel) and the body table.
pub(crate) fn friend_reports_core(hulls: &[(EntityId, SideId, bool, Domain, DVec3, DVec3)], bodies: &Bodies, catalogue: &Catalogue, sides: SideMask) -> Vec<(EntityId, Domain, DVec3, DVec3)> {
    let mut out = Vec::new();
    for &(e, s, alive, d, p, v) in hulls {
        if alive && sides.has(s) && p.is_finite() { out.push((e, d, p, if v.is_finite() { v } else { DVec3::ZERO })); }
    }
    for i in 0..bodies.len() {
        let (Some(&e), Some(&s), Some(&k), Some(&p)) = (bodies.entity.get(i), bodies.side.get(i), bodies.kind.get(i), bodies.pos.get(i)) else { continue };
        if !sides.has(s) || !body_alive(bodies, i) || !p.is_finite() { continue; }
        let Some((_, domain)) = munition_facts(catalogue, k) else { continue };
        let v = bodies.vel.get(i).copied().filter(|v| v.is_finite()).unwrap_or(DVec3::ZERO);
        out.push((e, domain, p, v));
    }
    out.sort_by_key(|o| o.0);
    out.dedup_by_key(|o| o.0);
    out
}

/// Friend positions a side knows by datalink: (entity, domain, position, velocity), ascending entity. Input to
/// `crate::tracks::fuse`. Unit: `sys-datalink`.
///
/// Every live hull and small body of the link's sides (see the module notes): members report themselves,
/// and isolated hulls, torpedoes, mines and terminal missiles are still known own-side units. When
/// `link.sides` is empty (a picture not built by [`membership`]) the sides of the listed entities are used.
pub fn friend_reports(world: &World, link: &LinkPicture) -> Vec<(EntityId, Domain, DVec3, DVec3)> {
    let mut sides = link.sides;
    if sides.is_empty() {
        let bodies = world.bodies();
        for &e in link.members.iter().chain(link.isolated.iter()) {
            if let Some(v) = world.vessel(e) { sides = sides.with(v.side); }
            else if let Some(&s) = bodies.row_of(e).and_then(|i| bodies.side.get(i)) { sides = sides.with(s); }
        }
    }
    let hulls: Vec<(EntityId, SideId, bool, Domain, DVec3, DVec3)> = world.vessels()
        .filter(|(_, v)| sides.has(v.side))
        .map(|(_, v)| (v.id, v.side, hull_alive(v.status), if v.submersible { Domain::Subsurface } else { Domain::Surface }, v.kin.pose.pos, v.kin.lin_vel))
        .collect();
    friend_reports_core(&hulls, world.bodies(), world.catalogue(), sides)
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::tuning::NetworkTuning;

    fn links(power: LinkState, data: LinkState) -> PartLinks { PartLinks { power, data, ..Default::default() } }
    fn e(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }

    #[test]
    fn part_command_follows_links() {
        let t = NetworkTuning::default();
        assert_eq!(part_command(&links(LinkState::Network, LinkState::Network), true, &t), PartCommand::Networked);
        assert_eq!(part_command(&links(LinkState::Buffer, LinkState::Redundant), true, &t), PartCommand::Networked);
        assert_eq!(part_command(&links(LinkState::Network, LinkState::LocalControl), true, &t), PartCommand::Local { error_factor: 3.0 });
        assert_eq!(part_command(&links(LinkState::Network, LinkState::Dead), true, &t), PartCommand::Local { error_factor: 3.0 });
        assert_eq!(part_command(&links(LinkState::Dead, LinkState::Network), true, &t), PartCommand::Dead);
        assert_eq!(part_command(&links(LinkState::Network, LinkState::Network), false, &t), PartCommand::Dead);
        let mut shed = links(LinkState::Network, LinkState::Network);
        shed.shed = true;
        assert_eq!(part_command(&shed, true, &t), PartCommand::Dead);
        let bad = NetworkTuning { local_control_error_factor: f64::NAN, ..t };
        assert_eq!(part_command(&links(LinkState::Network, LinkState::Dead), true, &bad), PartCommand::Local { error_factor: 3.0 });
    }

    #[test]
    fn membership_splits_hulls_and_filters_sides() {
        let cat = Catalogue::default();
        let hulls = vec![(e(3), SideId(0), true, true), (e(1), SideId(0), true, false), (e(2), SideId(1), true, true), (e(4), SideId(2), true, true), (e(5), SideId(0), false, false)];
        let p = membership_core(&hulls, &Bodies::default(), &cat, SideMask::only(SideId(0)).with(SideId(1)));
        assert_eq!(p.members, vec![e(2), e(3)]);
        assert_eq!(p.isolated, vec![e(1)]);
        assert!(p.is_member(e(3)) && !p.is_member(e(1)));
        let f = friend_reports_core(&[(e(9), SideId(0), true, Domain::Surface, DVec3::X, DVec3::NAN), (e(8), SideId(3), true, Domain::Surface, DVec3::X, DVec3::ZERO), (e(7), SideId(1), false, Domain::Surface, DVec3::X, DVec3::ZERO)], &Bodies::default(), &cat, p.sides);
        assert_eq!(f, vec![(e(9), Domain::Surface, DVec3::X, DVec3::ZERO)]);
    }

    #[test]
    fn member_bodies_by_kind_and_phase() {
        assert!(body_is_member(MunitionKind::Drone, BodyPhase::Loiter));
        assert!(body_is_member(MunitionKind::AntiShipMissile, BodyPhase::Cruise));
        assert!(!body_is_member(MunitionKind::AntiShipMissile, BodyPhase::Terminal));
        assert!(!body_is_member(MunitionKind::Torpedo, BodyPhase::Cruise));
        assert!(!body_is_member(MunitionKind::Drone, BodyPhase::Dead));
    }
}
