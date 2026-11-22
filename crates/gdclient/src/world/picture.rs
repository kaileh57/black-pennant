//! Part of unit `gd-world-view`. What the bridge view derives from the side picture, with no Godot in it:
//! track marker placement and declutter rank, the manual aim solution, the part under the cursor, and which
//! map a sortie is fought on. Tests run headless.
// Client presentation, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
use super::camera::Lens;
use crate::interp::ViewFrame;
use content::campaign::{CampaignData, MapDef};
use content::cell::CellPos;
use content::enums::{Domain, GameMode, IdRung, Stance};
use content::ids::PartSlot;
use glam::{DQuat, DVec3, Vec3};
use sim_core::math::Pose;
use sim_systems::doctrine::MountInfo;
use sim_systems::ids::TrackId;
use sim_systems::protocol::{ControlRole, ShipStatus, Snapshot, TrackView};

/// Floats per track in `BpWorldView.track_markers`.
pub const MARKER_FLOATS: usize = 6;
/// Markers keep this far inside the viewport edge (canvas units), so an edge marker's 24 px symbol and its
/// label stay on screen.
pub const MARKER_EDGE_PX: f32 = 28.0;
/// A marker sits this far above the top of the hull it belongs to, in metres plus a share of the range, so
/// the symbol never covers the model it names (rule 13: no model is an icon, and no icon hides a model).
const MARKER_LIFT_M: f32 = 4.0;
const MARKER_LIFT_PER_M: f32 = 0.012;

/// Declutter rank of a track, lowest kept longest (rule 14, L1670): the designated track, inbound weapons,
/// Hostile inside the longest own weapon envelope, Hostile outside it, Suspect, tracks still at the
/// Unknown rung, then Neutral and Friend.
pub fn track_rank(track: &TrackView, designated: TrackId, range_m: f64, own_envelope_m: f64) -> u8 {
    if !designated.is_none() && track.id == designated { return 0; }
    if track.domain == Domain::Weapon && track.inbound { return 1; }
    match track.stance {
        Stance::Hostile => if own_envelope_m > 0.0 && range_m <= own_envelope_m { 2 } else { 3 },
        Stance::Suspect => if track.rung == IdRung::Unknown { 5 } else { 4 },
        Stance::Neutral | Stance::Friend => 6,
    }
}

/// One marker before packing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker { pub track: TrackId, pub screen: [f32; 2], pub on_screen: bool, pub range_m: f32, pub rank: u8, pub pos: Vec3 }

/// Markers of every live track, ascending track number. A track paired with a drawn vessel sits above that
/// vessel's box (`top` gives the box's top centre in render space); any other sits at its estimate, carried
/// forward to render time. Range is from the viewer's hull, or from the camera without one.
pub fn track_markers(view: &ViewFrame, snapshot: Option<&Snapshot>, lens: &Lens, top: &dyn Fn(sim_core::EntityId) -> Option<Vec3>, out: &mut Vec<Marker>) {
    out.clear();
    let own = view.own.and_then(|i| view.vessels.get(i as usize));
    let from = own.map_or(lens.pos, |v| Vec3::from(v.pos));
    let (designated, envelope) = snapshot.and_then(|s| s.ship.as_ref()).map_or((TrackId::default(), 0.0), |s| (s.designated, s.longest_envelope_m as f64));
    for t in &view.tracks {
        if t.id.is_none() { continue; }
        let info = snapshot.and_then(|s| s.tracks.binary_search_by(|x| x.id.cmp(&t.id)).ok().map(|i| &s.tracks[i]));
        // A dropped track keeps its mark on the plot only; the bridge view shows what is held now.
        if info.is_some_and(|i| i.last_known) { continue; }
        // Own-side weapons in flight are not contacts (the track list drops them too); the spec's HUD rules
        // give them no marker of their own.
        if info.is_some_and(|i| !crate::hud::tracks::listed(i)) { continue; }
        let mut pos = Vec3::from(t.pos);
        if !pos.is_finite() { continue; }
        let drawn = view.vessels.iter().find(|v| v.track == t.id && !v.own);
        if let Some(p) = drawn.and_then(|v| top(v.entity)).filter(|p| p.is_finite()) { pos = p; }
        let range = (Vec3::from(drawn.map_or(t.pos, |v| v.pos)) - from).length();
        let lift = if drawn.is_some() { MARKER_LIFT_M + MARKER_LIFT_PER_M * (pos - lens.pos).length() } else { 0.0 };
        pos.y += lift;
        let (screen, on_screen) = lens.project_clamped(pos, MARKER_EDGE_PX);
        let rank = info.map_or(5, |i| track_rank(i, designated, range as f64, envelope));
        out.push(Marker { track: t.id, screen, on_screen, range_m: range, rank, pos });
    }
    out.sort_by_key(|m| m.track);
}

/// Pack markers as `[track, x, y, on_screen, range_m, rank]` each.
pub fn pack_markers(markers: &[Marker], out: &mut Vec<f32>) {
    out.clear();
    out.reserve(markers.len() * MARKER_FLOATS);
    for m in markers { out.extend_from_slice(&[m.track.0 as f32, m.screen[0], m.screen[1], if m.on_screen { 1.0 } else { 0.0 }, m.range_m, m.rank as f32]); }
}

/// The part whose footprint holds `cell`, by the design's placements (`size_of` gives a part's footprint in
/// cells at rotation 0; unknown parts are skipped). The lowest slot wins where footprints overlap.
pub fn part_at_cell(parts: &[(PartSlot, content::blueprint::PartPlacement)], cell: [i16; 3], size_of: &dyn Fn(&content::ids::PartId) -> Option<[u8; 3]>) -> Option<PartSlot> {
    let mut best: Option<PartSlot> = None;
    for (slot, p) in parts {
        let Some(size) = size_of(&p.part) else { continue };
        let r = content::kit::rotated_size(size, p.rot);
        let lo = [p.pos.x as i32, p.pos.y as i32, p.pos.z as i32];
        let inside = (0..3).all(|a| (cell[a] as i32) >= lo[a] && (cell[a] as i32) < lo[a] + r[a] as i32);
        if inside && best.is_none_or(|b| slot.0 < b.0) { best = Some(*slot); }
    }
    best
}

/// The mount under the player's hand: the held mount itself, else the first mount of the held weapon
/// group. `None` when no weapon role is held or the design has no such mount.
pub fn held_mount<'a>(ship: &ShipStatus, mounts: &'a [MountInfo]) -> Option<&'a MountInfo> {
    for role in &ship.held {
        let found = match role {
            ControlRole::Mount(slot) => mounts.iter().find(|m| m.slot == *slot),
            ControlRole::WeaponGroup(g) => mounts.iter().find(|m| m.group == *g && m.muzzle_mps > 0.0).or_else(|| mounts.iter().find(|m| m.group == *g)),
            _ => None,
        };
        if found.is_some() { return found; }
    }
    None
}

/// What the reticle is on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AimAt {
    /// A point on the sea or on a hull, world metres, and the track of the hull if it is one.
    Point { world: DVec3, track: TrackId },
    /// Nothing within reach: the ray itself (above the horizon).
    Sky { dir: DVec3 },
}

/// The manual aim solution shown beside the reticle (`BpWorldView.aim_readout`) and sent as the aim
/// direction of the held mount.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AimReadout {
    pub valid: bool,
    pub map: [f64; 2],
    pub range_m: f64,
    pub bearing_deg: f64,
    pub track: TrackId,
    pub in_arc: bool,
    pub time_of_flight_s: f64,
    /// Unit direction from the mount to the aim point in the vessel-local frame (+X bow, +Y up, +Z
    /// starboard): `PlayerInput::aim_local` and `ShotDesignation::Direction`. Zero when there is none.
    pub aim_local: [f32; 3],
}
impl Default for AimReadout {
    fn default() -> Self { Self { valid: false, map: [0.0; 2], range_m: 0.0, bearing_deg: 0.0, track: TrackId::default(), in_arc: false, time_of_flight_s: 0.0, aim_local: [0.0; 3] } }
}

fn unit_rot(q: DQuat) -> DQuat { if q.is_finite() && q.length_squared() > 1e-12 { q.normalize() } else { DQuat::IDENTITY } }

/// True when mount angles lie inside the limits (a span of a full circle, or none, has no dead arc).
pub fn within_limits(train_rad: f64, elevation_rad: f64, limits: &content::graph::MountLimits) -> bool {
    use std::f64::consts::TAU;
    let d2r = |d: f64| if d.is_finite() { d.to_radians() } else { 0.0 };
    let (tmin, tmax) = (d2r(limits.train_min_deg), d2r(limits.train_max_deg));
    let (emin, emax) = (d2r(limits.elevation_min_deg), d2r(limits.elevation_max_deg));
    const EPS: f64 = 1e-6;
    if emin <= emax && !(elevation_rad >= emin - EPS && elevation_rad <= emax + EPS) { return false; }
    if tmax - tmin >= TAU - 1e-9 || tmax <= tmin { return true; }
    // Any representation of the angle inside the arc will do.
    let t = tmin + (train_rad - tmin).rem_euclid(TAU);
    t <= tmax + EPS
}

/// Solve the readout for one mount. `pose` and `vel` are the firing hull's (world), `drag_k` the gun's band
/// drag. The simulation lays a hand-aimed gun on the point its aim ray reaches from the muzzle
/// (`sim_systems::input`), so the direction sent is from the mount to the point under the reticle, not the
/// camera's ray: with the camera 100 m off the mount the two differ by degrees.
pub fn aim_readout(pose: &Pose, mount: &MountInfo, drag_k: f64, at: AimAt) -> AimReadout {
    let rot = unit_rot(pose.rot);
    let muzzle = pose.pos + rot * if mount.pos_local.is_finite() { mount.pos_local } else { DVec3::ZERO };
    let local = |world_dir: DVec3| { let l = rot.inverse() * world_dir; [l.x as f32, l.y as f32, l.z as f32] };
    match at {
        AimAt::Sky { dir } => {
            let dir = dir.normalize_or_zero();
            if !dir.is_finite() || dir == DVec3::ZERO { return AimReadout::default(); }
            AimReadout { aim_local: local(dir), bearing_deg: crate::convert::heading_deg(dir.x.atan2(-dir.z)), ..AimReadout::default() }
        }
        AimAt::Point { world, track } => {
            let d = world - muzzle;
            if !d.is_finite() || d.length_squared() < 1e-6 { return AimReadout::default(); }
            let range = d.x.hypot(d.z);
            let bearing = crate::convert::heading_deg(d.x.atan2(-d.z));
            let mut out = AimReadout { valid: true, map: [world.x, world.z], range_m: range, bearing_deg: bearing, track, aim_local: local(d.normalize()), ..AimReadout::default() };
            // The gun is laid ballistically on the point: its elevation and time of flight decide reach.
            let solved = if mount.muzzle_mps > 0.0 && range >= 1.0 { sim_systems::guidance::gun::elevation_for(range, d.y, mount.muzzle_mps, drag_k) } else { None };
            let fire_dir = match solved {
                Some((elevation, tof)) if elevation.is_finite() && tof.is_finite() => {
                    out.time_of_flight_s = tof;
                    let (s, c) = elevation.sin_cos();
                    Some(DVec3::new(d.x / range * c, s, d.z / range * c))
                }
                _ => None,
            };
            if let Some(dir) = fire_dir {
                let (train, elevation) = sim_systems::guidance::gun::world_dir_to_mount(pose, mount.zero_rad, dir);
                out.in_arc = within_limits(train, elevation, &mount.limits);
            }
            out
        }
    }
}

/// The map a sortie is fought on, as a client works it out (it is not sent, see `session`): the operation's
/// own map, else the map of the theatre node the operation is authored at; the test basin for Splash, the
/// tutorial and trials; otherwise the mode's seeded default for the member count.
pub fn sortie_map(campaign: &CampaignData, mode: GameMode, seed: u64, operation: &str, players: usize) -> MapDef {
    if !operation.is_empty() {
        if let Some(op) = campaign.operations.get(&content::ids::OperationId::new(operation)) {
            if let Some(map) = &op.map { return map.clone(); }
            if let Some(node) = op.node.as_ref().and_then(|id| campaign.theatre.nodes.iter().find(|n| &n.id == id)) { return node.map.clone(); }
        }
    }
    match mode {
        GameMode::Splash | GameMode::Tutorial | GameMode::Trial => campaign.theatre.basin.clone(),
        _ => sim_systems::modes::default_map(mode, seed, players.max(1)),
    }
}

/// Convenience for cell positions from a raycast hit.
pub fn cell_pos(cell: [i16; 3]) -> CellPos { CellPos::new(cell[0], cell[1], cell[2]) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{RenderOrigin, TrackPose, VesselPose};
    use content::blueprint::PartPlacement;
    use content::graph::MountLimits;
    use content::ids::{PartId, PartIdx};
    use sim_core::EntityId;

    fn track(id: u32, stance: Stance) -> TrackView {
        TrackView {
            id: TrackId(id), domain: Domain::Surface, stance, stance_reason: Default::default(), rung: IdRung::Type, rung_reason: Default::default(), grade: Default::default(),
            pos: DVec3::ZERO, vel: DVec3::ZERO, shape: sim_systems::protocol::UncertaintyShape::Circle { radius_m: 10.0 }, last_update_tick: 0, last_known: false, class: None, candidates: 0,
            sources: Default::default(), trend: Default::default(), inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Default::default(), struck: false,
        }
    }

    #[test]
    fn rank_follows_the_declutter_order() {
        let designated = TrackId(7);
        let mut weapon = track(3, Stance::Hostile);
        weapon.domain = Domain::Weapon;
        weapon.inbound = true;
        let mut unknown = track(5, Stance::Suspect);
        unknown.rung = IdRung::Unknown;
        let ranks = [
            track_rank(&track(7, Stance::Neutral), designated, 9_000.0, 8_000.0),
            track_rank(&weapon, designated, 9_000.0, 8_000.0),
            track_rank(&track(1, Stance::Hostile), designated, 5_000.0, 8_000.0),
            track_rank(&track(1, Stance::Hostile), designated, 9_000.0, 8_000.0),
            track_rank(&track(1, Stance::Suspect), designated, 100.0, 8_000.0),
            track_rank(&unknown, designated, 100.0, 8_000.0),
            track_rank(&track(1, Stance::Friend), designated, 100.0, 8_000.0),
        ];
        assert_eq!(ranks, [0, 1, 2, 3, 4, 5, 6]);
        // No designation and no weapons: nothing is inside an envelope of zero.
        assert_eq!(track_rank(&track(1, Stance::Hostile), TrackId::default(), 10.0, 0.0), 3);
    }

    #[test]
    fn markers_sit_on_their_hulls_and_clamp_to_the_edge() {
        let lens = Lens::look(Vec3::new(0.0, 20.0, 100.0), Vec3::new(0.0, 5.0, 0.0), 0.0, 60.0, [1920.0, 1080.0]);
        let own = VesselPose { entity: EntityId::from_u64(1), pos: [0.0, 0.0, 0.0], own: true, ..Default::default() };
        let enemy = VesselPose { entity: EntityId::from_u64(2), pos: [40.0, 0.0, -300.0], track: TrackId(12), ..Default::default() };
        let view = ViewFrame {
            origin: RenderOrigin::default(), vessels: vec![own, enemy], own: Some(0),
            tracks: vec![
                TrackPose { id: TrackId(30), pos: [-200.0, 0.0, 900.0], world_xz: [-200.0, 900.0] },
                TrackPose { id: TrackId(12), pos: [44.0, 0.0, -305.0], world_xz: [44.0, -305.0] },
                TrackPose { id: TrackId(20), pos: [-2_000.0, 0.0, -4_000.0], world_xz: [-2_000.0, -4_000.0] },
            ],
            ..Default::default()
        };
        let mut snap = Snapshot {
            tracks: vec![track(12, Stance::Hostile), track(20, Stance::Suspect), track(30, Stance::Neutral)],
            ship: Some(ShipStatus { designated: TrackId(20), longest_envelope_m: 8_000.0, ..Default::default() }),
            ..Default::default()
        };
        let top = |e: EntityId| if e == EntityId::from_u64(2) { Some(Vec3::new(42.0, 14.0, -300.0)) } else { None };
        let mut out = Vec::new();
        track_markers(&view, Some(&snap), &lens, &top, &mut out);
        assert_eq!(out.iter().map(|m| m.track.0).collect::<Vec<_>>(), [12, 20, 30]);
        // Track 12 is the drawn enemy: its marker is above the hull box, not at the track estimate.
        let above = lens.project(Vec3::new(42.0, 14.0, -300.0)).unwrap();
        assert!(out[0].on_screen && (out[0].screen[0] - above[0]).abs() < 1.0 && out[0].screen[1] < above[1]);
        assert!((out[0].range_m - (40.0f32 * 40.0 + 300.0 * 300.0).sqrt()).abs() < 0.5);
        assert_eq!((out[0].rank, out[1].rank, out[2].rank), (2, 0, 6));
        // Track 30 is behind the camera: clamped to the inset edge and flagged.
        assert!(!out[2].on_screen);
        let s = out[2].screen;
        let on_edge = (s[0] - MARKER_EDGE_PX).abs() < 0.01 || (s[0] - (1920.0 - MARKER_EDGE_PX)).abs() < 0.01 || (s[1] - MARKER_EDGE_PX).abs() < 0.01 || (s[1] - (1080.0 - MARKER_EDGE_PX)).abs() < 0.01;
        assert!(on_edge, "{s:?}");
        let mut packed = Vec::new();
        pack_markers(&out, &mut packed);
        assert_eq!(packed.len(), 3 * MARKER_FLOATS);
        assert_eq!((packed[0], packed[3], packed[5]), (12.0, 1.0, 2.0));
        // A dropped track has no bridge marker; without a snapshot every track is still placed.
        snap.tracks[2].last_known = true;
        track_markers(&view, Some(&snap), &lens, &top, &mut out);
        assert_eq!(out.len(), 2);
        track_markers(&view, None, &lens, &top, &mut out);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn own_weapons_in_flight_get_no_contact_marker() {
        let lens = Lens::look(Vec3::new(0.0, 20.0, 100.0), Vec3::new(0.0, 5.0, 0.0), 0.0, 60.0, [1920.0, 1080.0]);
        let view = ViewFrame {
            origin: RenderOrigin::default(), vessels: Vec::new(), own: None,
            tracks: vec![
                TrackPose { id: TrackId(6), pos: [0.0, 30.0, -1_400.0], world_xz: [0.0, -1_400.0] },
                TrackPose { id: TrackId(7), pos: [10.0, 30.0, -900.0], world_xz: [10.0, -900.0] },
                TrackPose { id: TrackId(8), pos: [-50.0, 0.0, -600.0], world_xz: [-50.0, -600.0] },
            ],
            ..Default::default()
        };
        let mut own_missile = track(6, Stance::Friend);
        own_missile.domain = Domain::Weapon;
        let mut inbound = track(7, Stance::Hostile);
        inbound.domain = Domain::Weapon;
        inbound.inbound = true;
        let snap = Snapshot { tracks: vec![own_missile, inbound, track(8, Stance::Friend)], ..Default::default() };
        let mut out = Vec::new();
        track_markers(&view, Some(&snap), &lens, &|_| None, &mut out);
        // The own missile (Friend, Weapon) is dropped; a hostile weapon and a friendly hull keep theirs.
        assert_eq!(out.iter().map(|m| m.track.0).collect::<Vec<_>>(), [7, 8]);
    }

    #[test]
    fn the_part_under_the_cursor_is_found_by_footprint() {
        let place = |x: i16, rot: u8| PartPlacement { part: PartId::new("gun"), pos: CellPos::new(x, 8, 4), rot: content::kit::PartRot { yaw: rot, tilt: 0 }, kit: None, label: None, load: None };
        let parts = vec![(PartSlot(4), place(10, 0)), (PartSlot(2), place(30, 1)), (PartSlot(9), PartPlacement { part: PartId::new("unknown"), ..place(0, 0) })];
        let size_of = |id: &PartId| if id.as_str() == "gun" { Some([4u8, 2, 2]) } else { None };
        assert_eq!(part_at_cell(&parts, [11, 9, 5], &size_of), Some(PartSlot(4)));
        assert_eq!(part_at_cell(&parts, [14, 9, 5], &size_of), None);
        assert_eq!(part_at_cell(&parts, [0, 8, 4], &size_of), None);
        // The second gun is turned a quarter: its 4-cell length runs along Z.
        let r = content::kit::rotated_size([4, 2, 2], parts[1].1.rot);
        let far = [30 + r[0] as i16 - 1, 8, 4 + r[2] as i16 - 1];
        assert_eq!(part_at_cell(&parts, far, &size_of), Some(PartSlot(2)));
        assert_eq!(part_at_cell(&parts, [30 + r[0] as i16, 8, 4], &size_of), None);
    }

    fn mount(limits: MountLimits) -> MountInfo {
        MountInfo { slot: PartSlot(3), part: PartIdx(0), weapon: content::graph::WeaponClass::Gun, munition: None, pos_local: DVec3::new(20.0, 6.0, 0.0), zero_rad: 0.0, limits, effective_range_m: 8_000.0, director: None, kit: None, group: 1, muzzle_mps: 900.0, point_defence: false }
    }

    #[test]
    fn the_aim_direction_runs_from_the_mount_not_the_camera() {
        // Heading north at the origin; the mount is 20 m forward and 6 m up.
        let pose = Pose::from_heading(DVec3::new(100.0, 0.0, 200.0), 0.0);
        let full = MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: -5.0, elevation_max_deg: 60.0 };
        let m = mount(full);
        let muzzle = pose.to_world(m.pos_local);
        assert!((muzzle - DVec3::new(100.0, 6.0, 180.0)).length() < 1e-9);
        // A point 3 km due east of the muzzle on the sea.
        let target = DVec3::new(muzzle.x + 3_000.0, 0.0, muzzle.z);
        let r = aim_readout(&pose, &m, 2.0e-5, AimAt::Point { world: target, track: TrackId(5) });
        assert!(r.valid && r.in_arc && r.track == TrackId(5));
        assert!((r.range_m - 3_000.0).abs() < 1e-6 && (r.bearing_deg - 90.0).abs() < 1e-6);
        assert_eq!(r.map, [target.x, target.z]);
        // Vessel-local: east of a north-heading ship is to starboard (+Z), and slightly down to the sea.
        assert!(r.aim_local[2] > 0.99 && r.aim_local[0].abs() < 1e-3 && r.aim_local[1] < 0.0);
        // Time of flight is the solver's: between the vacuum time and twice it.
        assert!(r.time_of_flight_s > 3_000.0 / 900.0 && r.time_of_flight_s < 2.0 * 3_000.0 / 900.0, "{}", r.time_of_flight_s);
        // A mount that only bears forward cannot reach the beam; far beyond gun range nothing can.
        let forward = mount(MountLimits { train_min_deg: -30.0, train_max_deg: 30.0, ..full });
        assert!(!aim_readout(&pose, &forward, 2.0e-5, AimAt::Point { world: target, track: TrackId::default() }).in_arc);
        let ahead = DVec3::new(muzzle.x, 0.0, muzzle.z - 3_000.0);
        assert!(aim_readout(&pose, &forward, 2.0e-5, AimAt::Point { world: ahead, track: TrackId::default() }).in_arc);
        let beyond = aim_readout(&pose, &m, 2.0e-5, AimAt::Point { world: DVec3::new(muzzle.x + 500_000.0, 0.0, muzzle.z), track: TrackId::default() });
        assert!(beyond.valid && !beyond.in_arc && beyond.time_of_flight_s == 0.0);
        // Above the horizon: a direction only.
        let sky = aim_readout(&pose, &m, 2.0e-5, AimAt::Sky { dir: DVec3::new(0.0, 0.5, -1.0) });
        assert!(!sky.valid && sky.aim_local[0] > 0.8 && sky.aim_local[1] > 0.4);
        assert_eq!(aim_readout(&pose, &m, 2.0e-5, AimAt::Sky { dir: DVec3::NAN }), AimReadout::default());
        assert_eq!(aim_readout(&pose, &m, 2.0e-5, AimAt::Point { world: DVec3::NAN, track: TrackId::default() }), AimReadout::default());
    }

    #[test]
    fn limits_and_held_mounts() {
        let arc = MountLimits { train_min_deg: -135.0, train_max_deg: 135.0, elevation_min_deg: -5.0, elevation_max_deg: 70.0 };
        assert!(within_limits(0.0, 0.2, &arc) && within_limits(-2.3, 0.2, &arc) && within_limits(2.3 - std::f64::consts::TAU, 0.2, &arc));
        assert!(!within_limits(3.0, 0.2, &arc) && !within_limits(0.0, 1.4, &arc) && !within_limits(0.0, -0.2, &arc));
        let all = MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, ..arc };
        assert!(within_limits(3.0, 0.2, &all) && within_limits(-17.0, 0.2, &all));
        let mounts = vec![mount(arc), MountInfo { slot: PartSlot(8), group: 2, ..mount(arc) }];
        let held = |roles: Vec<ControlRole>| ShipStatus { held: roles, ..Default::default() };
        assert_eq!(held_mount(&held(vec![ControlRole::Helm]), &mounts).map(|m| m.slot), None);
        assert_eq!(held_mount(&held(vec![ControlRole::Helm, ControlRole::Mount(PartSlot(8))]), &mounts).map(|m| m.slot), Some(PartSlot(8)));
        assert_eq!(held_mount(&held(vec![ControlRole::WeaponGroup(1)]), &mounts).map(|m| m.slot), Some(PartSlot(3)));
        assert_eq!(held_mount(&held(vec![ControlRole::WeaponGroup(7)]), &mounts).map(|m| m.slot), None);
    }

    #[test]
    fn the_sortie_map_is_the_operations_else_the_modes_default() {
        let mut campaign = CampaignData::default();
        campaign.theatre.basin = MapDef { size_m: [4_000.0, 4_000.0], seed: 99, ..Default::default() };
        assert_eq!(sortie_map(&campaign, GameMode::Splash, 1, "", 1).seed, 99);
        let skirmish = sortie_map(&campaign, GameMode::Skirmish, 42, "", 1);
        assert_eq!(skirmish, sim_systems::modes::default_map(GameMode::Skirmish, 42, 1));
        // An unknown operation key falls through to the default rather than failing.
        assert_eq!(sortie_map(&campaign, GameMode::Skirmish, 42, "op.no_such", 0), skirmish);
    }
}
