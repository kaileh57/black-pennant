//! Unit `sys-replay-play`. Playback: open, seek, step, and produce the same `Snapshot` and event stream a
//! live client would get, so the client's render path is identical for live play, replays, the kill cam and
//! golden captures. DESIGN.md L1043-1050.
//!
//! Opening decodes every segment (each within [`super::record::SEGMENT_MAX_BYTES`], the whole within
//! [`super::record::REPLAY_MAX_RAW_BYTES`]) and builds a tick index; nothing in a damaged or hostile file can
//! make playback panic, it is refused as [`ReplayError::Corrupt`] instead.
//!
//! What a replay snapshot carries: `tick`, `pacing`, `vessels`, `bodies`, `decoys`, `tracks` and
//! `objectives`. `ship`, `allies` and `alerts` stay empty: replays record the replicated world, not the
//! per-player HUD state (L1043).
use super::record::{decode_frames, extra_at, frame_tick, kill_cams_for, update_pairing, Roster, SideFilter, HEADER_MAX_BYTES, PREAMBLE, REPLAY_MAX_FRAMES, REPLAY_MAX_RAW_BYTES, SEGMENT_MAX_BYTES};
use super::{ReplayError, ReplayFrame, ReplicatedKeyframe, SideCut};
use crate::prelude::*;
use crate::protocol::event::{Audience, Lane, OutEvent, SimEvent};
use crate::protocol::snapshot::{BodyView, DecoyView, ObjectiveView, Pacing, Relevance, Snapshot, TrackView, TurretView, VesselView, Viewer};
use content::replay::{ReplayHeader, REPLAY_FORMAT, REPLAY_MAGIC};
use sim_core::events::PhysEvent;
use sim_core::TICK_HZ;

/// A vessel missing from the pose frames (the distant-hull fallback records it less often) is still drawn
/// from its last pose for this long.
pub const STALE_TICKS: u64 = (TICK_HZ / 4) as u64;

pub struct ReplayPlayer {
    pub(crate) header: ReplayHeader,
    pub(crate) frames: Vec<ReplayFrame>,
    /// Next frame to apply.
    pub(crate) cursor: usize,
    pub(crate) tick: u64,
    pub(crate) index: Index,
    pub(crate) state: PlayState,
    /// False until the first `seek` or `advance` has handed out the opening introductions.
    pub(crate) primed: bool,
}

/// Where things are in `frames`, built once at open.
#[derive(Clone, Debug, Default)]
pub(crate) struct Index {
    /// Record tick of each frame: non-decreasing (a `Shot` row takes the tick of the frame before it).
    pub ticks: Vec<u64>,
    pub keyframes: Vec<usize>,
    /// Frames carrying full or partial poses: `Poses` and `Keyframe`.
    pub poses: Vec<usize>,
    pub drawables: Vec<usize>,
    pub pictures: BTreeMap<SideId, Vec<usize>>,
    /// Kill cam windows each side was sent (killer, from, to).
    pub kill_cams: BTreeMap<SideId, Vec<(EntityId, u64, u64)>>,
}

/// State derived from the stream as of `cursor`.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlayState {
    pub roster: Roster,
    pub sides: Vec<SideCut>,
    pub pacing: Pacing,
    pub objectives: Vec<ObjectiveView>,
    pub pairing: BTreeMap<SideId, BTreeMap<EntityId, TrackId>>,
    pub sides_of: BTreeMap<EntityId, SideId>,
    pub exterior: BTreeMap<EntityId, Vec<PartSlot>>,
}

impl PlayState {
    fn reset(&mut self, k: &ReplicatedKeyframe) {
        self.roster = Roster::from_sides(&k.sides);
        self.sides = k.sides.iter().map(|s| SideCut { side: s.side, allied: s.allied, players: s.players.clone(), intros: Vec::new() }).collect();
        self.pacing = k.pacing.clone();
        self.objectives = k.objectives.clone();
        self.pairing = k.sides.iter().map(|s| (s.side, s.intros.iter().map(|i| (i.entity, i.track)).collect())).collect();
        for i in k.entities.iter().chain(k.sides.iter().flat_map(|s| s.intros.iter())) { self.sides_of.insert(i.entity, i.side); }
        self.exterior = k.exterior_parts.iter().cloned().collect();
    }
    fn apply(&mut self, f: &ReplayFrame) {
        match f {
            ReplayFrame::Keyframe(k) => self.reset(k),
            ReplayFrame::Event(e) => {
                match &e.event {
                    SimEvent::PacingChanged(p) => self.pacing = p.clone(),
                    SimEvent::ObjectiveChanged { id, state, progress } => {
                        if let Some(o) = self.objectives.iter_mut().find(|o| o.id == *id) { o.state = *state; o.progress = *progress; }
                    }
                    SimEvent::EntityRevealed(i) => { self.sides_of.insert(i.entity, i.side); }
                    _ => {}
                }
                let sides: Vec<SideId> = self.sides.iter().map(|s| s.side).collect();
                for s in sides {
                    if self.roster.admits_side(e.audience, s) { update_pairing(self.pairing.entry(s).or_default(), &e.event, s); }
                }
            }
            _ => {}
        }
    }
    fn allied(&self, side: SideId) -> SideMask { self.sides.iter().find(|s| s.side == side).map(|s| s.allied.with(side)).unwrap_or(SideMask::only(side)) }
}

/// Parse the preamble and header.
fn parse_header(bytes: &[u8]) -> Result<(ReplayHeader, usize), ReplayError> {
    use bincode::Options;
    if bytes.len() < 4 || bytes[0..4] != REPLAY_MAGIC { return Err(ReplayError::Magic); }
    if bytes.len() < PREAMBLE { return Err(ReplayError::Corrupt("file ends inside the preamble".into())); }
    let format = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if format != REPLAY_FORMAT { return Err(ReplayError::Format { found: format, expected: REPLAY_FORMAT }); }
    let len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if len > HEADER_MAX_BYTES || len > bytes.len() - PREAMBLE { return Err(ReplayError::Corrupt("header length out of range".into())); }
    let header: ReplayHeader = bincode::DefaultOptions::new().with_limit(HEADER_MAX_BYTES as u64).deserialize(&bytes[PREAMBLE..PREAMBLE + len]).map_err(|e| ReplayError::Corrupt(format!("header: {e}")))?;
    if header.magic != REPLAY_MAGIC || header.format != REPLAY_FORMAT { return Err(ReplayError::Corrupt("header disagrees with the preamble".into())); }
    Ok((header, PREAMBLE + len))
}

/// Read a whole replay file: header and decoded segments, each starting with a keyframe.
pub(crate) fn read_file(bytes: &[u8]) -> Result<(ReplayHeader, Vec<Vec<ReplayFrame>>), ReplayError> {
    let (header, start) = parse_header(bytes)?;
    let body = &bytes[start..];
    let mut segments = Vec::new();
    let mut pos = 0usize;
    let mut raw_total = 0usize;
    let mut frames_total = 0usize;
    while pos < body.len() {
        if header.keyframe_offsets.get(segments.len()) != Some(&(pos as u64)) { return Err(ReplayError::Corrupt(format!("segment {} is not at its keyframe offset", segments.len()))); }
        let Some(lb) = body.get(pos..pos + 4) else { return Err(ReplayError::Corrupt("file ends inside a segment length".into())) };
        let len = u32::from_le_bytes([lb[0], lb[1], lb[2], lb[3]]) as usize;
        let Some(frame) = body.get(pos + 4..pos + 4 + len) else { return Err(ReplayError::Corrupt("file ends inside a segment".into())) };
        let raw = content::share::decompress_bounded(frame, SEGMENT_MAX_BYTES).map_err(|e| ReplayError::Corrupt(format!("segment {}: {e}", segments.len())))?;
        raw_total += raw.len();
        if raw_total > REPLAY_MAX_RAW_BYTES { return Err(ReplayError::Corrupt("replay expands past the size limit".into())); }
        let frames = decode_frames(&raw)?;
        frames_total += frames.len();
        if frames_total > REPLAY_MAX_FRAMES { return Err(ReplayError::Corrupt("replay holds too many frames".into())); }
        if !matches!(frames.first(), Some(ReplayFrame::Keyframe(_))) { return Err(ReplayError::Corrupt(format!("segment {} does not start with a keyframe", segments.len()))); }
        segments.push(frames);
        pos += 4 + len;
    }
    if segments.len() != header.keyframe_offsets.len() { return Err(ReplayError::Corrupt("keyframe offsets do not match the body".into())); }
    if segments.is_empty() { return Err(ReplayError::Corrupt("no keyframe".into())); }
    Ok((header, segments))
}

/// Events that only matter as they happen (sounds, messages, flashes): a seek does not replay them.
fn transient(e: &SimEvent) -> bool {
    match e {
        SimEvent::SortieStarted { .. } | SimEvent::LadderStep { .. } | SimEvent::CommandRejected { .. } | SimEvent::QueryReply { .. }
        | SimEvent::SolutionUnconverged { .. } | SimEvent::Seduced { .. } | SimEvent::ShotLogged(_) | SimEvent::OfficerMessage { .. }
        | SimEvent::AceMessage { .. } | SimEvent::TickNotReproducible { .. } | SimEvent::PhotoTaken { .. } | SimEvent::Reinforcements { .. }
        | SimEvent::KillCam { .. } => true,
        SimEvent::Phys(p) => matches!(p,
            PhysEvent::MountFired { .. } | PhysEvent::Splash { .. } | PhysEvent::ShellImpact { .. } | PhysEvent::BodyImpact { .. }
            | PhysEvent::BodyBurst { .. } | PhysEvent::BodyDestroyed { .. } | PhysEvent::BodyRecovered { .. } | PhysEvent::BodySpawned { .. }
            | PhysEvent::DecoyDeployed { .. } | PhysEvent::DecoyExpired { .. } | PhysEvent::CookOff { .. } | PhysEvent::JobDone { .. }
            | PhysEvent::PassengersLost { .. } | PhysEvent::Collision { .. } | PhysEvent::Grounded { .. }),
        _ => false,
    }
}

fn wrap_pi(a: f64) -> f64 { let t = core::f64::consts::TAU; (a + core::f64::consts::PI).rem_euclid(t) - core::f64::consts::PI }
fn lerp(a: f64, b: f64, u: f64) -> f64 { a + (b - a) * u }
fn lerp_angle(a: f32, b: f32, u: f64) -> f32 { (a as f64 + wrap_pi(b as f64 - a as f64) * u) as f32 }

/// Position between two samples: cubic Hermite on the recorded velocities, linear when they disagree with
/// the displacement (a teleport, a respawn) or are not finite.
fn hermite(p0: DVec3, v0: DVec3, p1: DVec3, v1: DVec3, u: f64, dt_s: f64) -> DVec3 {
    let linear = p0.lerp(p1, u);
    if !(p0.is_finite() && p1.is_finite() && v0.is_finite() && v1.is_finite()) || dt_s <= 0.0 { return if linear.is_finite() { linear } else { p0 }; }
    let span = p0.distance(p1);
    if span > 2.0 * dt_s * (v0.length() + v1.length()) + 1.0 { return linear; }
    let (u2, u3) = (u * u, u * u * u);
    let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
    let h10 = u3 - 2.0 * u2 + u;
    let h01 = -2.0 * u3 + 3.0 * u2;
    let h11 = u3 - u2;
    p0 * h00 + v0 * (h10 * dt_s) + p1 * h01 + v1 * (h11 * dt_s)
}

/// Normalised lerp on the shorter arc (no transcendental functions; indistinguishable from slerp at 15 Hz).
fn nlerp(a: DQuat, b: DQuat, u: f64) -> DQuat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    let q = DQuat::from_xyzw(lerp(a.x, b.x, u), lerp(a.y, b.y, u), lerp(a.z, b.z, u), lerp(a.w, b.w, u));
    let n = q.length();
    if n > 1e-12 && n.is_finite() { q / n } else { a }
}

fn interp_kin(a: &Kinematics, b: &Kinematics, u: f64, dt_s: f64) -> Kinematics {
    Kinematics {
        pose: Pose { pos: hermite(a.pose.pos, a.lin_vel, b.pose.pos, b.lin_vel, u, dt_s), rot: nlerp(a.pose.rot, b.pose.rot, u) },
        lin_vel: a.lin_vel.lerp(b.lin_vel, u),
        ang_vel: a.ang_vel.lerp(b.ang_vel, u),
    }
}

fn interp_vessel(a: &VesselView, b: &VesselView, u: f64, dt_s: f64) -> VesselView {
    let mut v = if u < 0.5 { a.clone() } else { b.clone() };
    v.kin = interp_kin(&a.kin, &b.kin, u, dt_s);
    v.rudder = lerp(a.rudder as f64, b.rudder as f64, u) as f32;
    v.power = lerp(a.power as f64, b.power as f64, u) as f32;
    v.turrets = a.turrets.iter().map(|t| match b.turrets.iter().find(|x| x.slot == t.slot) {
        Some(x) => TurretView { slot: t.slot, train_rad: lerp_angle(t.train_rad, x.train_rad, u), elevation_rad: lerp_angle(t.elevation_rad, x.elevation_rad, u) },
        None => *t,
    }).collect();
    v
}

fn interp_body(a: &BodyView, b: &BodyView, u: f64, dt_s: f64) -> BodyView {
    let mut o = if u < 0.5 { *a } else { *b };
    o.pos = hermite(a.pos, a.vel, b.pos, b.vel, u, dt_s);
    o.vel = a.vel.lerp(b.vel, u);
    o
}

fn interp_decoy(a: &DecoyView, b: &DecoyView, u: f64) -> DecoyView {
    let mut o = *a;
    o.pos = a.pos.lerp(b.pos, u);
    o.age_s = lerp(a.age_s as f64, b.age_s as f64, u) as f32;
    o
}

fn interp_track(a: &TrackView, b: &TrackView, u: f64) -> TrackView {
    let mut t = a.clone();
    if !a.last_known && !b.last_known && a.pos.is_finite() && b.pos.is_finite() {
        t.pos = a.pos.lerp(b.pos, u);
        t.vel = a.vel.lerp(b.vel, u);
    }
    t
}

/// Pose lists of a frame (`Poses` or a keyframe).
fn pose_lists(f: &ReplayFrame) -> Option<(&[VesselView], &[BodyView], &[DecoyView])> {
    match f {
        ReplayFrame::Poses { vessels, bodies, decoys, .. } => Some((vessels, bodies, decoys)),
        ReplayFrame::Keyframe(k) => Some((&k.vessels, &k.bodies, &k.decoys)),
        _ => None,
    }
}

impl ReplayPlayer {
    /// Open a replay: check magic and format (a build opens only its own format version), decompress within
    /// bounds. Unit: `sys-replay-play`.
    pub fn open(bytes: &[u8]) -> Result<ReplayPlayer, ReplayError> {
        let (mut header, segments) = read_file(bytes)?;
        let frames: Vec<ReplayFrame> = segments.into_iter().flatten().collect();
        let mut index = Index::default();
        let mut last = frames.first().and_then(frame_tick).unwrap_or(0);
        for (i, f) in frames.iter().enumerate() {
            let t = frame_tick(f).unwrap_or(last).max(last);
            last = t;
            index.ticks.push(t);
            match f {
                ReplayFrame::Keyframe(_) => { index.keyframes.push(i); index.poses.push(i); }
                ReplayFrame::Poses { .. } => index.poses.push(i),
                ReplayFrame::Drawable { .. } => index.drawables.push(i),
                ReplayFrame::Picture { side, .. } => index.pictures.entry(*side).or_default().push(i),
                _ => {}
            }
        }
        let mut sides: BTreeSet<SideId> = BTreeSet::new();
        for f in &frames { if let ReplayFrame::Keyframe(k) = f { sides.extend(k.sides.iter().map(|s| s.side)); } }
        for s in sides {
            let cams = kill_cams_for(&frames, s);
            if !cams.is_empty() { index.kill_cams.insert(s, cams); }
        }
        // The data is the authority on length.
        header.duration_ticks = last;
        let start = index.ticks.first().copied().unwrap_or(0);
        let mut player = ReplayPlayer { header, frames, cursor: 0, tick: start, index, state: PlayState::default(), primed: false };
        if let Some(ReplayFrame::Keyframe(k)) = player.frames.first() { let k = k.clone(); player.state.reset(&k); }
        Ok(player)
    }

    /// Read only the header (replay list UI). Unit: `sys-replay-play`.
    pub fn read_header(bytes: &[u8]) -> Result<ReplayHeader, ReplayError> { parse_header(bytes).map(|h| h.0) }
    pub fn header(&self) -> &ReplayHeader { &self.header }
    pub fn tick(&self) -> u64 { self.tick }

    fn start_tick(&self) -> u64 { self.index.ticks.first().copied().unwrap_or(0) }
    fn clamp(&self, tick: u64) -> u64 { tick.clamp(self.start_tick(), self.header.duration_ticks.max(self.start_tick())) }
    /// Last indexed frame (from `list`) whose record tick is at or before `tick`.
    fn last_at(&self, list: &[usize], tick: u64) -> Option<usize> {
        let n = list.partition_point(|&i| self.index.ticks[i] <= tick);
        if n == 0 { None } else { Some(n - 1) }
    }

    /// Jump to the keyframe at or before `tick`, then play forward to it. Returns the events the client must
    /// apply to rebuild state (entity introductions with damage so far). Unit: `sys-replay-play`.
    ///
    /// In order: `SortieStarted`, the keyframe's weather and wave seed, its pacing (all `Audience::All`);
    /// each side's own introductions, addressed to that side; the omniscient introductions of every entity,
    /// addressed `Sides(SideMask::NONE)` so no side admits them while an omniscient consumer (which takes
    /// every non-host event) ends on them; then every state-bearing event recorded between the keyframe and
    /// `tick` (damage, splits, losses, interest, tracks, control, alerts, objectives), plus the spawns of
    /// shells still in flight at `tick`. Momentary events (sounds, messages, impacts) are not repeated.
    /// `tick` is clamped to the recording.
    pub fn seek(&mut self, tick: u64) -> Vec<OutEvent> {
        let tick = self.clamp(tick);
        let Some(ki) = self.last_at(&self.index.keyframes, tick).map(|i| self.index.keyframes[i]) else { self.tick = tick; return Vec::new() };
        let ReplayFrame::Keyframe(k) = &self.frames[ki] else { return Vec::new() };
        let k = k.clone();
        self.state = PlayState::default();
        self.state.reset(&k);
        let ev = |audience: Audience, event: SimEvent| OutEvent { tick: k.tick, audience, lane: Lane::Reliable, event };
        let mut out = vec![
            ev(Audience::All, SimEvent::SortieStarted { seed: self.header.seed, tick_hz: TICK_HZ }),
            ev(Audience::All, SimEvent::Phys(PhysEvent::WeatherChanged { weather: k.weather.clone(), wave_seed: k.wave_seed })),
            ev(Audience::All, SimEvent::PacingChanged(k.pacing.clone())),
        ];
        for s in &k.sides { for i in &s.intros { out.push(ev(Audience::side(s.side), SimEvent::EntityRevealed(Box::new(i.clone())))); } }
        for i in &k.entities { out.push(ev(Audience::Sides(SideMask::NONE), SimEvent::EntityRevealed(Box::new(i.clone())))); }

        let mut later: Vec<OutEvent> = Vec::new();
        let mut closed: BTreeSet<sim_core::projectiles::ProjectileId> = BTreeSet::new();
        let mut i = ki + 1;
        while i < self.frames.len() && self.index.ticks[i] <= tick {
            let f = &self.frames[i];
            self.state.apply(f);
            if let ReplayFrame::Event(e) = f {
                match &e.event {
                    SimEvent::Phys(PhysEvent::ProjectileSpawned(_)) => later.push(e.clone()),
                    SimEvent::Phys(PhysEvent::Splash { projectile, .. }) | SimEvent::Phys(PhysEvent::ShellImpact { projectile, .. }) => { closed.insert(*projectile); }
                    ev if !transient(ev) => later.push(e.clone()),
                    _ => {}
                }
            }
            i += 1;
        }
        later.retain(|e| !matches!(&e.event, SimEvent::Phys(PhysEvent::ProjectileSpawned(p)) if closed.contains(&p.id)));
        out.extend(later);
        self.cursor = i;
        self.tick = tick;
        self.primed = true;
        out
    }

    /// Advance to `tick` and return the events in between, in recorded order. Unit: `sys-replay-play`.
    ///
    /// A `tick` before the current one seeks instead (and returns what `seek` returns); a `tick` past the end
    /// stops at the end. The first call after `open` also returns the opening introductions, as `seek` would
    /// at the first keyframe. Events of every audience are returned; a side or player consumer filters with
    /// `Audience::admits`. An omniscient consumer takes every non-host event but applies `EntityRevealed` and
    /// `EntityHidden` only when addressed `Sides(SideMask::NONE)` (the recorder's and `seek`'s omniscient
    /// introductions and removals); the sides' own reveals and hides would otherwise hide ships a side lost
    /// sight of. A replay cut for one side has no omniscient pair: view it as that side.
    pub fn advance(&mut self, tick: u64) -> Vec<OutEvent> {
        let tick = self.clamp(tick);
        if tick < self.tick { return self.seek(tick); }
        let mut out = if self.primed { Vec::new() } else { let t = self.tick; self.seek(t) };
        while self.cursor < self.frames.len() && self.index.ticks[self.cursor] <= tick {
            let f = &self.frames[self.cursor];
            self.state.apply(f);
            if let ReplayFrame::Event(e) = f { out.push(e.clone()); }
            self.cursor += 1;
        }
        self.tick = tick;
        out
    }

    /// Snapshot at the current tick for a viewer: poses interpolated between the surrounding pose frames;
    /// `Viewer::Side` shows that side's recorded picture, `Viewer::Omniscient` shows everything.
    /// Unit: `sys-replay-play`.
    ///
    /// `Viewer::Side` and `Viewer::Player` see what that side could draw at the time, filtered as live play
    /// filters it (track pairing, other-team seekers off, PvP turrets limited to the exterior); a player also
    /// gets `Relevance::Own` and range bands from their hull. A player slot the replay does not know gets an
    /// empty snapshot, as live play gives one. `Viewer::Omniscient` plots the lowest side that had players.
    pub fn snapshot(&self, viewer: Viewer) -> Snapshot {
        let t = self.tick;
        let mut snap = Snapshot { tick: t, pacing: self.state.pacing.clone(), objectives: self.state.objectives.clone(), ..Default::default() };
        let (side, own) = match viewer {
            Viewer::Omniscient => (None, None),
            Viewer::Side(s) => (Some(s), None),
            Viewer::Player(p) => match self.state.roster.side_of(p) {
                Some(s) => (Some(s), self.state.sides.iter().find(|c| c.side == s).and_then(|c| c.players.iter().find(|x| x.0 == p)).and_then(|x| x.1)),
                None => return snap,
            },
        };
        let seen: &[(EntityId, SideMask)] = match self.last_at(&self.index.drawables, t).map(|i| &self.frames[self.index.drawables[i]]) {
            Some(ReplayFrame::Drawable { seen, .. }) => seen,
            _ => &[],
        };
        let extra = side.and_then(|s| self.index.kill_cams.get(&s)).map(|c| extra_at(c, t)).unwrap_or_default();
        let alive = |e: EntityId| seen.is_empty() || extra.contains(&e) || seen.binary_search_by_key(&e, |x| x.0).is_ok();
        let (vessels, bodies, decoys) = self.interpolated(t, &alive);

        let plot_side = side.unwrap_or_else(|| {
            self.state.sides.iter().filter(|s| !s.players.is_empty()).map(|s| s.side).min()
                .or_else(|| self.state.sides.iter().map(|s| s.side).min()).unwrap_or(SideId(0))
        });
        snap.tracks = self.picture(plot_side, t);

        match side {
            None => { snap.vessels = vessels; snap.bodies = bodies; snap.decoys = decoys; }
            Some(s) => {
                let empty = BTreeMap::new();
                let pairing = self.state.pairing.get(&s).unwrap_or(&empty);
                let own_pos = own.and_then(|o| vessels.iter().find(|v| v.entity == o)).map(|v| v.kin.pose.pos);
                let sf = SideFilter {
                    side: s, allied: self.state.allied(s), pvp: crate::sim::events::is_pvp(self.header.mode), seen: Some(seen), exterior: &self.state.exterior,
                    pairing, sides_of: &self.state.sides_of, extra: &extra, own, own_pos,
                };
                let engaged: BTreeSet<TrackId> = snap.tracks.iter().filter(|t| t.inbound || t.engaged_by > 0).map(|t| t.id).collect();
                let bump = |r: &mut Relevance, track: TrackId| if !track.is_none() && engaged.contains(&track) && *r < Relevance::Engaged { *r = Relevance::Engaged; };
                snap.vessels = vessels.iter().filter_map(|v| sf.vessel(v)).map(|mut v| { bump(&mut v.relevance, v.track); v }).collect();
                snap.bodies = bodies.iter().filter_map(|b| sf.body(b)).map(|mut b| { bump(&mut b.relevance, b.track); b }).collect();
                snap.decoys = decoys.iter().filter_map(|d| sf.decoy(d)).collect();
            }
        }
        snap
    }

    /// Poses at `t`, interpolated, ascending entity.
    fn interpolated(&self, t: u64, alive: &dyn Fn(EntityId) -> bool) -> (Vec<VesselView>, Vec<BodyView>, Vec<DecoyView>) {
        let poses = &self.index.poses;
        let Some(a) = self.last_at(poses, t) else { return Default::default() };
        let fa = poses[a];
        let ta = self.index.ticks[fa];
        let Some((_, ba, da)) = pose_lists(&self.frames[fa]) else { return Default::default() };
        let next = poses.get(a + 1).map(|&i| (self.index.ticks[i], i));

        // Vessels: the latest pose of each at or before t (looking back over the fallback's gaps), paired with
        // its next pose within the stale window.
        let mut latest: BTreeMap<EntityId, (u64, &VesselView)> = BTreeMap::new();
        let mut j = a as isize;
        while j >= 0 {
            let fi = poses[j as usize];
            let tj = self.index.ticks[fi];
            if t.saturating_sub(tj) > STALE_TICKS && j != a as isize { break; }
            if let Some((vs, _, _)) = pose_lists(&self.frames[fi]) { for v in vs { latest.entry(v.entity).or_insert((tj, v)); } }
            j -= 1;
        }
        let mut vessels = Vec::with_capacity(latest.len());
        for (e, (t0, v0)) in latest {
            if !alive(e) { continue; }
            let mut found = None;
            for &fi in &poses[a + 1..] {
                let t1 = self.index.ticks[fi];
                if t1 > t.saturating_add(STALE_TICKS) || t1 > t0.saturating_add(2 * STALE_TICKS) { break; }
                if let Some((vs, _, _)) = pose_lists(&self.frames[fi]) {
                    if let Ok(k) = vs.binary_search_by_key(&e, |v| v.entity) { found = Some((t1, &vs[k])); break; }
                }
            }
            vessels.push(match found {
                Some((t1, v1)) if t1 > t0 => interp_vessel(v0, v1, (t - t0) as f64 / (t1 - t0) as f64, (t1 - t0) as f64 / TICK_HZ as f64),
                // No later pose yet: dead-reckon on the recorded velocity over the short gap.
                _ => {
                    let mut v = v0.clone();
                    let p = v.kin.pose.pos + v.kin.lin_vel * ((t - t0) as f64 / TICK_HZ as f64);
                    if p.is_finite() { v.kin.pose.pos = p; }
                    v
                }
            });
        }

        // Bodies and decoys are in every pose frame: pair frame a with the next one.
        let (vb, vd): (&[BodyView], &[DecoyView]) = match next.and_then(|(_, i)| pose_lists(&self.frames[i])) { Some((_, b, d)) => (b, d), None => (&[], &[]) };
        let (u, dt_s) = match next { Some((tb, _)) if tb > ta => ((t - ta) as f64 / (tb - ta) as f64, (tb - ta) as f64 / TICK_HZ as f64), _ => (0.0, 0.0) };
        let bodies = ba.iter().filter(|b| alive(b.entity)).map(|b| match vb.binary_search_by_key(&b.entity, |x| x.entity) {
            Ok(k) if dt_s > 0.0 => interp_body(b, &vb[k], u, dt_s),
            _ => *b,
        }).collect();
        let decoys = da.iter().filter(|d| alive(d.entity)).map(|d| match vd.binary_search_by_key(&d.entity, |x| x.entity) {
            Ok(k) if dt_s > 0.0 => interp_decoy(d, &vd[k], u),
            _ => *d,
        }).collect();
        (vessels, bodies, decoys)
    }

    /// A side's picture at `t`: the latest recorded one (or the keyframe's), positions interpolated toward the
    /// next for tracks in both.
    fn picture(&self, side: SideId, t: u64) -> Vec<TrackView> {
        let list = self.index.pictures.get(&side).map(|v| v.as_slice()).unwrap_or(&[]);
        let from_pic = |i: usize| -> Option<&Vec<TrackView>> { match &self.frames[i] { ReplayFrame::Picture { tracks, .. } => Some(tracks), _ => None } };
        let kf = self.last_at(&self.index.keyframes, t).map(|i| self.index.keyframes[i]);
        let a = self.last_at(list, t).map(|i| (i, list[i]));
        // Prefer the keyframe's copy when it is newer than the latest picture frame.
        let (base_tick, base): (u64, &[TrackView]) = match (a, kf) {
            (Some((_, fi)), Some(ki)) if self.index.ticks[ki] > self.index.ticks[fi] => (self.index.ticks[ki], self.kf_picture(ki, side)),
            (Some((_, fi)), _) => (self.index.ticks[fi], from_pic(fi).map(|v| v.as_slice()).unwrap_or(&[])),
            (None, Some(ki)) => (self.index.ticks[ki], self.kf_picture(ki, side)),
            (None, None) => return Vec::new(),
        };
        let next = match a { Some((i, _)) => list.get(i + 1).copied(), None => list.first().copied() };
        let Some(ni) = next.filter(|&ni| self.index.ticks[ni] > base_tick) else { return base.to_vec() };
        let tb = self.index.ticks[ni];
        let nb = from_pic(ni).map(|v| v.as_slice()).unwrap_or(&[]);
        let u = ((t.saturating_sub(base_tick)) as f64 / (tb - base_tick) as f64).clamp(0.0, 1.0);
        base.iter().map(|tr| match nb.binary_search_by_key(&tr.id, |x| x.id) { Ok(k) => interp_track(tr, &nb[k], u), Err(_) => tr.clone() }).collect()
    }
    fn kf_picture(&self, ki: usize, side: SideId) -> &[TrackView] {
        match &self.frames[ki] { ReplayFrame::Keyframe(k) => k.pictures.iter().find(|p| p.0 == side).map(|p| p.1.as_slice()).unwrap_or(&[]), _ => &[] }
    }

    /// True at the end of the recording.
    pub fn finished(&self) -> bool { self.tick >= self.header.duration_ticks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::config::EntityIntro;
    use crate::protocol::event::ShotLogEntry;
    use crate::protocol::snapshot::{SeekerPhase, UncertaintyShape};
    use crate::replay::record::{cut_for_side, retention, RecordOptions, ReplayRecorder};
    use content::cell::CellChange;
    use content::enums::GameMode;
    use sim_core::projectiles::{ProjectileId, ProjectileSpawn};
    use sim_core::vessel::VesselStatus;

    const S0: SideId = SideId(0);
    const S1: SideId = SideId(1);
    fn ent(i: u32) -> EntityId { EntityId { index: i, generation: 1 } }
    fn hash(b: u8) -> ContentHash { ContentHash([b; 32]) }

    fn header(mode: GameMode) -> ReplayHeader {
        ReplayHeader { magic: REPLAY_MAGIC, format: REPLAY_FORMAT, game_version: "test".into(), protocol_version: 1, tuning_version: 1, mode, seed: 7, weather: WeatherState::default(), duration_ticks: 0, blueprints: Vec::new(), player_names: vec!["a".into(), "b".into()], operation: String::new(), pinned: false, keyframe_offsets: Vec::new() }
    }
    fn intro(e: EntityId, side: SideId, bp: ContentHash, track: TrackId) -> EntityIntro {
        EntityIntro { entity: e, side, blueprint: bp, exterior_only: false, class: None, name: String::new(), player: None, status: VesselStatus::default(), damage: vec![CellChange { cell: CellIndex(3), bucket: 2 }], removed: Vec::new(), parent: None, track }
    }
    fn vessel(e: EntityId, x: f64, vx: f64) -> VesselView {
        let kin = Kinematics { pose: Pose { pos: DVec3::new(x, 0.0, 0.0), rot: DQuat::IDENTITY }, lin_vel: DVec3::new(vx, 0.0, 0.0), ang_vel: DVec3::ZERO };
        VesselView { entity: e, kin, status: VesselStatus::default(), flags: Default::default(), turrets: vec![TurretView { slot: PartSlot(1), train_rad: 3.0, elevation_rad: 0.0 }, TurretView { slot: PartSlot(2), train_rad: 0.0, elevation_rad: 0.0 }], rudder: 0.0, power: 0.5, track: TrackId(99), relevance: Relevance::Distant }
    }
    fn body(e: EntityId, side: SideId, x: f64) -> BodyView {
        BodyView { entity: e, kind: MunitionIdx(0), side, pos: DVec3::new(x, 10.0, 0.0), vel: DVec3::new(60.0, 0.0, 0.0), phase: Default::default(), seeker: SeekerPhase::Locked, track: TrackId(98), relevance: Relevance::Distant }
    }
    fn track(id: u32, x: f64) -> TrackView {
        TrackView { id: TrackId(id), domain: Default::default(), stance: Default::default(), stance_reason: Default::default(), rung: Default::default(), rung_reason: Default::default(), grade: Default::default(), pos: DVec3::new(x, 0.0, 0.0), vel: DVec3::ZERO, shape: UncertaintyShape::Circle { radius_m: 50.0 }, last_update_tick: 0, last_known: false, class: None, candidates: 3, sources: Default::default(), trend: Default::default(), inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Relevance::Distant, struck: false }
    }
    fn ev(tick: u64, audience: Audience, event: SimEvent) -> ReplayFrame { ReplayFrame::Event(OutEvent { tick, audience, lane: Lane::Reliable, event }) }

    /// Own hull A (side 0, player 0), enemy B (side 1, player 1) that side 0 sees from tick 0, and enemy H
    /// (side 1, index 777) that side 0 never sees. Keyframes at ticks 0 and 60.
    const A: u32 = 1;
    const B: u32 = 2;
    const H: u32 = 777;
    fn keyframe(tick: u64, x: f64, mode: GameMode) -> ReplicatedKeyframe {
        let sides = vec![
            SideCut { side: S0, allied: SideMask::only(S0), players: vec![(PlayerSlot(0), Some(ent(A)))], intros: vec![intro(ent(A), S0, hash(1), TrackId::NONE), EntityIntro { exterior_only: true, blueprint: hash(20), ..intro(ent(B), S1, hash(20), TrackId(5)) }] },
            SideCut { side: S1, allied: SideMask::only(S1), players: vec![(PlayerSlot(1), Some(ent(B)))], intros: vec![intro(ent(B), S1, hash(2), TrackId::NONE), intro(ent(H), S1, hash(77), TrackId::NONE)] },
        ];
        ReplicatedKeyframe {
            tick, weather: WeatherState::default(), wave_seed: 11,
            entities: vec![intro(ent(A), S0, hash(1), TrackId::NONE), intro(ent(B), S1, hash(2), TrackId::NONE), intro(ent(H), S1, hash(77), TrackId::NONE)],
            vessels: vec![vessel(ent(A), x, 10.0), vessel(ent(B), 1000.0 + x, 10.0), vessel(ent(H), 9000.0, 0.0)],
            bodies: vec![body(ent(H + 1), S1, 0.0)], decoys: vec![DecoyView { entity: ent(H + 2), kind: content::part::DecoyKind::Chaff, owner: ent(H), pos: DVec3::ZERO, age_s: 0.0 }],
            pictures: vec![(S0, vec![track(5, 1000.0)]), (S1, vec![track(6, 0.0), track(7, 9000.0)])],
            pacing: Pacing::default(), objectives: Vec::new(), sides,
            exterior_parts: if crate::sim::events::is_pvp(mode) { vec![(ent(A), vec![PartSlot(1)]), (ent(B), vec![PartSlot(1)]), (ent(H), vec![])] } else { Vec::new() },
        }
    }
    fn seen() -> Vec<(EntityId, SideMask)> {
        let both = SideMask::only(S0).with(S1);
        vec![(ent(A), both), (ent(B), both), (ent(H), SideMask::only(S1)), (ent(H + 1), SideMask::only(S1)), (ent(H + 2), SideMask::only(S1))]
    }
    fn build(mode: GameMode) -> Vec<u8> {
        let mut r = ReplayRecorder::empty(header(mode), RecordOptions::default(), 0);
        r.begin_segment(keyframe(0, 0.0, mode), seen());
        for tick in 1..=90u64 {
            if tick == 10 {
                r.push(ev(tick, Audience::All, SimEvent::Phys(PhysEvent::ProjectileSpawned(ProjectileSpawn { id: ProjectileId(1), tick, origin: DVec3::ZERO, velocity: DVec3::X, calibre_mm: 76, band: content::part::weapons::CalibreBand::Auto, shooter: ent(A), mount: PartSlot(1), rounds: 1, point_defence: false }))));
                r.push(ev(tick, Audience::All, SimEvent::Phys(PhysEvent::ProjectileSpawned(ProjectileSpawn { id: ProjectileId(2), tick, origin: DVec3::ZERO, velocity: DVec3::X, calibre_mm: 76, band: content::part::weapons::CalibreBand::Auto, shooter: ent(A), mount: PartSlot(1), rounds: 1, point_defence: false }))));
                r.push(ev(tick, Audience::side(S0), SimEvent::OfficerMessage { vessel: ent(A), role: OfficerRole::ALL[0], text: Msg::default() }));
                r.push(ev(tick, Audience::side(S1), SimEvent::Phys(PhysEvent::CellsDamaged { vessel: ent(H), changes: vec![CellChange { cell: CellIndex(9), bucket: 1 }], cause: sim_core::damage::DamageCause::Shell, attacker: ent(B) })));
                r.push(ev(tick, Audience::Host, SimEvent::EnemyAlert { side: S1, level: 2 }));
                r.push(ev(tick, Audience::Players(crate::ids::PlayerMask(1 << 1)), SimEvent::TrackCreated { side: S1, track: TrackId(7), domain: Default::default() }));
            }
            if tick == 20 {
                r.push(ev(tick, Audience::All, SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(1), pos: DVec3::ZERO, calibre_mm: 76, shooter: ent(A) })));
                r.push(ev(tick, Audience::side(S0), SimEvent::Phys(PhysEvent::CellsDamaged { vessel: ent(B), changes: vec![CellChange { cell: CellIndex(4), bucket: 3 }], cause: sim_core::damage::DamageCause::Shell, attacker: ent(A) })));
                r.push(ev(tick, Audience::Host, SimEvent::ShotLogged(ShotLogEntry { tick: 10, player: PlayerSlot(0), mount: PartSlot(1), shot: crate::protocol::command::ShotDesignation::Direction { dir_local: DVec3::X }, rtt_ms: 40, client_render_tick: 9, target_speed_mps: 5.0, hit: Some(false), binding: Default::default() })));
                r.push(ev(tick, Audience::Host, SimEvent::ShotLogged(ShotLogEntry { tick: 10, player: PlayerSlot(1), mount: PartSlot(1), shot: crate::protocol::command::ShotDesignation::Direction { dir_local: DVec3::X }, rtt_ms: 40, client_render_tick: 9, target_speed_mps: 5.0, hit: Some(true), binding: Default::default() })));
            }
            r.note_seen(tick, seen());
            if r.poses_due(tick) {
                let x = tick as f64 * 10.0 / TICK_HZ as f64;
                r.push_poses(tick, vec![vessel(ent(A), x, 10.0), vessel(ent(B), 1000.0 + x, 10.0), vessel(ent(H), 9000.0, 0.0)], vec![body(ent(H + 1), S1, tick as f64)], Vec::new(), |_| true);
            }
            if tick % 60 == 30 { r.push(ReplayFrame::Picture { tick, side: S0, tracks: vec![track(5, 1000.0 + tick as f64)] }); r.push(ReplayFrame::Picture { tick, side: S1, tracks: vec![track(6, 0.0), track(7, 9000.0)] }); }
            r.flush();
            if tick == 60 { r.begin_segment(keyframe(60, 10.0, mode), seen()); }
        }
        r.record_shots(&[ShotLogEntry { tick: 10, player: PlayerSlot(1), mount: PartSlot(1), shot: crate::protocol::command::ShotDesignation::Direction { dir_local: DVec3::X }, rtt_ms: 40, client_render_tick: 9, target_speed_mps: 5.0, hit: None, binding: Default::default() }], true);
        r.finish()
    }

    #[test]
    fn round_trip_keeps_every_frame() {
        let bytes = build(GameMode::Skirmish);
        let p = ReplayPlayer::open(&bytes).unwrap();
        assert_eq!(p.header().keyframe_offsets.len(), 2);
        assert_eq!(p.header().duration_ticks, 90);
        assert_eq!(p.index.keyframes.len(), 2);
        assert!(p.frames.iter().any(|f| matches!(f, ReplayFrame::Shot(_))));
        assert!(p.header().blueprints.contains(&hash(77)));
        // Encoding the decoded frames again gives the same bytes.
        let (h, segs) = read_file(&bytes).unwrap();
        let raw: Vec<Vec<u8>> = segs.iter().map(|s| { let mut v = Vec::new(); for f in s { crate::replay::record::encode_frame(f, &mut v); } v }).collect();
        let again = crate::replay::record::write_file(h, &raw.iter().map(|v| v.as_slice()).collect::<Vec<_>>());
        assert_eq!(again, bytes);
        assert_eq!(ReplayPlayer::read_header(&bytes).unwrap(), *p.header());
    }

    #[test]
    fn refuses_other_formats_and_damage_without_panicking() {
        let bytes = build(GameMode::Skirmish);
        assert!(matches!(ReplayPlayer::open(b"nope"), Err(ReplayError::Magic)));
        let mut other = bytes.clone();
        other[4..8].copy_from_slice(&(REPLAY_FORMAT + 1).to_le_bytes());
        assert!(matches!(ReplayPlayer::open(&other), Err(ReplayError::Format { found, expected }) if found == REPLAY_FORMAT + 1 && expected == REPLAY_FORMAT));
        assert!(matches!(ReplayPlayer::read_header(&other), Err(ReplayError::Format { .. })));
        for n in (0..bytes.len()).step_by(7) {
            assert!(ReplayPlayer::open(&bytes[..n]).is_err());
            assert!(cut_for_side(&bytes[..n], S0).is_err());
        }
        for i in (PREAMBLE..bytes.len()).step_by(13) {
            let mut b = bytes.clone();
            b[i] ^= 0x5A;
            // Either refused or opened; never a panic, and an opened file plays.
            if let Ok(mut p) = ReplayPlayer::open(&b) { let _ = p.advance(u64::MAX); let _ = p.snapshot(Viewer::Side(S0)); let _ = p.seek(30); }
            let _ = cut_for_side(&b, S1);
        }
    }

    #[test]
    fn seek_rebuilds_state_without_momentary_events() {
        let bytes = build(GameMode::Skirmish);
        let mut p = ReplayPlayer::open(&bytes).unwrap();
        let out = p.seek(45);
        assert_eq!(p.tick(), 45);
        assert!(matches!(out[0].event, SimEvent::SortieStarted { seed: 7, .. }));
        assert!(matches!(out[1].event, SimEvent::Phys(PhysEvent::WeatherChanged { wave_seed: 11, .. })));
        // Side intros to their side, then omniscient intros no side admits.
        let reveals: Vec<&OutEvent> = out.iter().filter(|e| matches!(e.event, SimEvent::EntityRevealed(_))).collect();
        assert_eq!(reveals.len(), 4 + 3);
        assert!(reveals[4..].iter().all(|e| e.audience == Audience::Sides(SideMask::NONE) && !e.audience.admits(PlayerSlot(0), S0) && !e.audience.admits(PlayerSlot(1), S1)));
        // Damage after the keyframe is kept; officer chatter and splashes are not; only the unclosed shell.
        assert!(out.iter().any(|e| matches!(&e.event, SimEvent::Phys(PhysEvent::CellsDamaged { vessel, .. }) if *vessel == ent(B))));
        assert!(!out.iter().any(|e| matches!(e.event, SimEvent::OfficerMessage { .. } | SimEvent::Phys(PhysEvent::Splash { .. }))));
        let shells: Vec<u32> = out.iter().filter_map(|e| match &e.event { SimEvent::Phys(PhysEvent::ProjectileSpawned(s)) => Some(s.id.0), _ => None }).collect();
        assert_eq!(shells, vec![2]);
        // From the second keyframe only its own state.
        let out = p.seek(75);
        assert!(!out.iter().any(|e| matches!(e.event, SimEvent::Phys(PhysEvent::CellsDamaged { .. }) | SimEvent::Phys(PhysEvent::ProjectileSpawned(_)))));
        // Clamped past the end.
        p.seek(10_000);
        assert_eq!(p.tick(), 90);
        assert!(p.finished());
    }

    #[test]
    fn advance_returns_events_in_order_and_backwards_seeks() {
        let bytes = build(GameMode::Skirmish);
        let mut p = ReplayPlayer::open(&bytes).unwrap();
        let first = p.advance(5);
        assert!(matches!(first[0].event, SimEvent::SortieStarted { .. }), "first advance primes");
        let mid = p.advance(20);
        let ticks: Vec<u64> = mid.iter().map(|e| e.tick).collect();
        assert!(ticks.windows(2).all(|w| w[0] <= w[1]) && ticks.iter().all(|&t| (6..=20).contains(&t)));
        assert!(mid.iter().any(|e| matches!(e.event, SimEvent::OfficerMessage { .. })));
        assert!(p.advance(20).is_empty());
        let back = p.advance(3);
        assert!(matches!(back[0].event, SimEvent::SortieStarted { .. }));
        assert_eq!(p.tick(), 3);
    }

    #[test]
    fn snapshot_interpolates_and_filters_per_side() {
        let bytes = build(GameMode::Skirmish);
        let mut p = ReplayPlayer::open(&bytes).unwrap();
        p.seek(10);
        // Poses every 4 ticks at 10 m/s: x = 10 * t / 60 exactly, between frames too.
        let s = p.snapshot(Viewer::Omniscient);
        let a = s.vessels.iter().find(|v| v.entity == ent(A)).unwrap();
        assert!((a.kin.pose.pos.x - 10.0 * 10.0 / 60.0).abs() < 1e-5, "{}", a.kin.pose.pos.x);
        assert_eq!(s.vessels.len(), 3);
        assert_eq!(s.bodies.len(), 1);
        assert!((s.bodies[0].pos.x - 10.0).abs() < 1.0);
        let side = p.snapshot(Viewer::Side(S0));
        assert_eq!(side.vessels.iter().map(|v| v.entity).collect::<Vec<_>>(), vec![ent(A), ent(B)]);
        assert!(side.bodies.is_empty() && side.decoys.is_empty());
        let b = side.vessels.iter().find(|v| v.entity == ent(B)).unwrap();
        assert_eq!(b.track, TrackId(5));
        assert_eq!(side.vessels[0].relevance, Relevance::OwnSide);
        assert_eq!(side.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), vec![TrackId(5)]);
        let me = p.snapshot(Viewer::Player(PlayerSlot(0)));
        assert_eq!(me.vessels[0].relevance, Relevance::Own);
        assert_eq!(me.vessels[1].relevance, Relevance::Near);
        assert!(p.snapshot(Viewer::Player(PlayerSlot(5))).vessels.is_empty());
        // Picture interpolation between the keyframe (x 1000 at 0) and the picture at 30 (x 1030).
        p.seek(15);
        let tr = &p.snapshot(Viewer::Side(S0)).tracks[0];
        assert!((tr.pos.x - 1015.0).abs() < 1e-9);
        // The enemy seeker is hidden from the other side.
        let s1 = p.snapshot(Viewer::Side(S1));
        assert_eq!(s1.bodies[0].seeker, SeekerPhase::Locked);
    }

    #[test]
    fn pvp_cut_leaks_nothing_about_a_hidden_enemy() {
        let bytes = build(GameMode::PvpElimination);
        let cut = cut_for_side(&bytes, S0).unwrap();
        let p = ReplayPlayer::open(&cut).unwrap();
        let dump = format!("{:?}", p.frames);
        for i in [H, H + 1, H + 2] { assert!(!dump.contains(&format!("index: {i},")), "entity {i} leaked"); }
        assert!(!dump.contains("EnemyAlert") && !dump.contains("TrackCreated"), "other side's rows leaked");
        assert!(!dump.contains("hit: Some(true)") && dump.contains("hit: Some(false)"), "only own shot log rows");
        assert!(!p.header().blueprints.contains(&hash(77)) && !p.header().blueprints.contains(&hash(2)));
        assert!(p.header().blueprints.contains(&hash(1)) && p.header().blueprints.contains(&hash(20)));
        for f in &p.frames {
            let vs: &[VesselView] = match f { ReplayFrame::Poses { vessels, .. } => vessels, ReplayFrame::Keyframe(k) => &k.vessels, _ => &[] };
            for v in vs {
                if v.entity == ent(B) { assert_eq!(v.turrets.iter().map(|t| t.slot).collect::<Vec<_>>(), vec![PartSlot(1)]); assert_eq!(v.track, TrackId(5)); }
                if v.entity == ent(A) { assert_eq!(v.turrets.len(), 2); }
            }
            if let ReplayFrame::Keyframe(k) = f {
                assert!(k.sides.iter().all(|s| s.side == S0));
                assert!(k.pictures.iter().all(|p| p.0 == S0));
                assert!(k.entities.iter().any(|i| i.entity == ent(B) && i.exterior_only));
            }
        }
        // The cut opens and plays as side 0.
        let mut p = p;
        p.seek(40);
        assert_eq!(p.snapshot(Viewer::Omniscient).vessels.len(), 2);
    }

    #[test]
    fn kill_cam_keeps_the_killer_drawn() {
        let mut r = ReplayRecorder::empty(header(GameMode::PvpElimination), RecordOptions::default(), 0);
        r.begin_segment(keyframe(0, 0.0, GameMode::PvpElimination), seen());
        for tick in 1..=20u64 {
            if r.poses_due(tick) { r.push_poses(tick, vec![vessel(ent(A), 0.0, 0.0), vessel(ent(H), 9000.0, 0.0)], Vec::new(), Vec::new(), |_| true); }
            if tick == 20 { r.push(ev(tick, Audience::side(S0), SimEvent::KillCam { victim: ent(A), killer: Some(ent(H)), from_tick: 4, to_tick: 12 })); }
            r.flush();
        }
        let bytes = r.finish();
        let mut p = ReplayPlayer::open(&cut_for_side(&bytes, S0).unwrap()).unwrap();
        p.seek(8);
        let s = p.snapshot(Viewer::Side(S0));
        let h = s.vessels.iter().find(|v| v.entity == ent(H)).expect("killer drawn in the window");
        assert!(h.turrets.is_empty(), "exterior only");
        p.seek(16);
        assert!(p.snapshot(Viewer::Side(S0)).vessels.iter().all(|v| v.entity != ent(H)));
    }

    #[test]
    fn distant_hulls_record_less_often_and_still_draw() {
        let mut r = ReplayRecorder::empty(header(GameMode::Skirmish), RecordOptions::size_fallback(), 0);
        r.begin_segment(keyframe(0, 0.0, GameMode::Skirmish), seen());
        let mut far_frames = 0;
        for tick in 1..=60u64 {
            if r.poses_due(tick) {
                let x = tick as f64;
                r.push_poses(tick, vec![vessel(ent(A), x, 60.0), vessel(ent(H), 9000.0 + x, 60.0)], Vec::new(), Vec::new(), |v| v.entity == ent(A));
                if matches!(r.frames.last(), Some(ReplayFrame::Poses { vessels, .. }) if vessels.len() == 2) { far_frames += 1; }
            }
            r.flush();
        }
        // 15 pose frames in a second; the far hull is in at most 10 of them.
        assert_eq!(far_frames, 10);
        let mut p = ReplayPlayer::open(&r.finish()).unwrap();
        // From the first distant pose on (the fixture keyframe holds the far hull still).
        for t in 8..=60 {
            p.seek(t);
            let s = p.snapshot(Viewer::Omniscient);
            let h = s.vessels.iter().find(|v| v.entity == ent(H)).expect("far hull drawn at every tick");
            assert!((h.kin.pose.pos.x - (9000.0 + t as f64)).abs() < 1e-6, "tick {t}: {}", h.kin.pose.pos.x);
        }
    }

    #[test]
    fn retention_keeps_twenty_recent_plus_pinned() {
        let mut files: Vec<(String, u64, bool)> = (0..25).map(|i| (format!("r{i:02}"), i as u64, false)).collect();
        files.push(("old_pin".into(), 0, true));
        files.push(("r03".into(), 3, true)); // pinned in a second entry
        let del = retention(&files);
        assert_eq!(del, vec!["r00", "r01", "r02", "r04"]);
        assert!(retention(&files[..20]).is_empty());
        assert!(retention(&[]).is_empty());
    }

    /// Size gate probe (L1047, 30 MB for a 30-minute battle): one 60 s segment of 20 turning hulls with four
    /// turrets each and 12 bodies in flight, then the extrapolation. Run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn size_gate_probe() {
        let mut r = ReplayRecorder::empty(header(GameMode::Skirmish), RecordOptions::default(), 0);
        r.begin_segment(keyframe(0, 0.0, GameMode::Skirmish), seen());
        for tick in 1..=60 * TICK_HZ as u64 {
            if r.poses_due(tick) {
                let ts = tick as f64 / TICK_HZ as f64;
                let vessels = (0..20u32).map(|i| {
                    let h = 0.01 * ts * (1.0 + i as f64 * 0.1) + i as f64;
                    let (c, sn) = (libm::cos(h), libm::sin(h));
                    let mut v = vessel(ent(i + 1), 2_000.0 * i as f64 + 500.0 * sn, 9.0 * c);
                    v.kin.pose.pos.z = 300.0 * i as f64 + 500.0 * c;
                    v.kin.pose.pos.y = 0.3 * libm::sin(ts * 0.7 + i as f64);
                    v.kin.pose.rot = DQuat::from_xyzw(0.01 * sn, libm::sin(h / 2.0), 0.0, libm::cos(h / 2.0)).normalize();
                    v.kin.ang_vel = DVec3::new(0.01 * c, 0.01, 0.002 * sn);
                    v.turrets = (0..4u16).map(|k| TurretView { slot: PartSlot(k), train_rad: (h + k as f64) as f32, elevation_rad: 0.1 * k as f32 }).collect();
                    v.power = 0.7;
                    v.rudder = (0.2 * sn) as f32;
                    v
                }).collect();
                let bodies = (0..12u32).map(|i| body(ent(500 + i), S1, 250.0 * ts + i as f64 * 37.0)).collect();
                r.push_poses(tick, vessels, bodies, Vec::new(), |_| true);
            }
            if tick % 60 == 0 { for side in [S0, S1] { r.push(ReplayFrame::Picture { tick, side, tracks: (0..15).map(|i| track(i + 1, tick as f64 + i as f64)).collect() }); } }
            r.flush();
        }
        let raw = r.body.len();
        let t0 = std::time::Instant::now();
        let packed = content::share::compress(&r.body).len();
        let dt = t0.elapsed();
        println!("60 s raw {raw} B, compressed {packed} B in {dt:?}; 30 min ~ {:.1} MB, compress ~ {:.0} s", packed as f64 * 30.0 / 1e6, dt.as_secs_f64() * 30.0);
    }

    #[test]
    fn omniscient_pair_hides_entities_that_leave_the_world() {
        let mut r = ReplayRecorder::empty(header(GameMode::Skirmish), RecordOptions::default(), 0);
        r.begin_segment(keyframe(0, 0.0, GameMode::Skirmish), seen());
        let mut less = seen();
        less.retain(|x| x.0 != ent(H + 1));
        r.note_seen(5, less.clone());
        r.note_seen(6, less);
        let hides: Vec<&OutEvent> = r.frames.iter().filter_map(|f| match f { ReplayFrame::Event(e) => Some(e), _ => None }).collect();
        assert_eq!(hides.len(), 1);
        assert!(matches!(hides[0].event, SimEvent::EntityHidden { entity } if entity == ent(H + 1)));
        assert_eq!(hides[0].audience, Audience::Sides(SideMask::NONE));
        assert!(!r.known.contains(&ent(H + 1)));
    }

    #[test]
    fn due_checks_count_elapsed_ticks() {
        use crate::replay::record::due;
        assert!(!due(3, 0, 4) && due(4, 0, 4) && due(9, 0, 4));
        assert!(due(2, 10, 4), "a tick going backwards is due");
    }
}
