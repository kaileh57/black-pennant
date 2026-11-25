//! Unit `sys-replay-record`. Recording from the authoritative output stream. DESIGN.md L1043-1050.
//!
//! # File layout (replay format [`content::replay::REPLAY_FORMAT`])
//! ```text
//! magic "BPRP" | format u32 LE | header length u32 LE | header (bincode, varint)
//! body: segment*, each = length u32 LE | one compressed frame (content::share::compress)
//! ```
//! Each segment decompresses to a run of bincode-encoded (varint) [`ReplayFrame`]s that starts with a
//! [`ReplayFrame::Keyframe`] followed by a full [`ReplayFrame::Drawable`]; `ReplayHeader::keyframe_offsets`
//! holds the byte offset of each segment within the body, so a reader can reach a keyframe without decoding
//! what lies before it. Magic and format sit before the header so a build refuses another format version
//! without decoding a header whose shape may have changed (L1045).
//!
//! A segment closes every `keyframe_s` of sortie time, or earlier when its raw size reaches
//! [`SEGMENT_SPLIT_BYTES`], so no segment can exceed what the player will decompress
//! ([`SEGMENT_MAX_BYTES`]). A closed segment is compressed on its own background thread (level 19 is far too
//! slow to run inside a tick); `finish` joins them in order, so the output is the same bytes as compressing
//! inline.
//!
//! Recorded floats are quantised before encoding (positions, velocities and rotations keep 24 mantissa bits,
//! about 3 mm at 50 km; angles and fractions keep 15): playback only draws them, and the zeroed low bits are
//! what lets a 30-minute battle fit the 30 MB gate.
use super::{ReplayError, ReplayFrame, ReplicatedKeyframe, SideCut};
use crate::prelude::*;
use crate::protocol::config::EntityIntro;
use crate::protocol::event::{Audience, Lane, OutEvent, SimEvent};
use crate::protocol::snapshot::{BodyView, DecoyView, Relevance, SeekerPhase, TrackView, VesselView, Viewer};
use crate::sim::Sim;
use content::replay::{ReplayHeader, REPLAY_FORMAT, REPLAY_MAGIC};
use sim_core::TICK_HZ;

/// Recorder options (the size fallback at L1047).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordOptions { pub pose_hz: u32, pub distant_pose_hz: u32, pub distant_m: f64, pub keyframe_s: u32 }
impl Default for RecordOptions { fn default() -> Self { Self { pose_hz: content::replay::REPLAY_POSE_HZ, distant_pose_hz: content::replay::REPLAY_POSE_HZ, distant_m: 5_000.0, keyframe_s: content::replay::REPLAY_KEYFRAME_S } } }

impl RecordOptions {
    /// The size fallback of L1047: hulls more than 5 km from every player hull record at 10 Hz.
    pub fn size_fallback() -> Self { Self { distant_pose_hz: 10, ..Self::default() } }
}

/// Most bytes one segment may decompress to; the player refuses larger ones.
pub const SEGMENT_MAX_BYTES: usize = 64 << 20;
/// A segment closes early (with a fresh keyframe) once its raw size reaches this.
pub const SEGMENT_SPLIT_BYTES: usize = 48 << 20;
/// Most bytes a header may take.
pub const HEADER_MAX_BYTES: usize = 1 << 20;
/// Most decompressed bytes a whole replay may hold (several hours of battle). The recorder stops adding
/// frames before it gets there, so it never writes a file the player refuses.
pub const REPLAY_MAX_RAW_BYTES: usize = 1 << 30;
/// Most frames one segment may hold, and the recorder's early-split point for it.
pub const SEGMENT_MAX_FRAMES: usize = 1 << 20;
pub const SEGMENT_SPLIT_FRAMES: usize = 3 << 18;
/// Most frames a whole replay may hold.
pub const REPLAY_MAX_FRAMES: usize = 1 << 22;
/// Bytes before the header: magic, format, header length.
pub(crate) const PREAMBLE: usize = 12;

pub struct ReplayRecorder {
    pub(crate) header: ReplayHeader,
    pub(crate) options: RecordOptions,
    /// Frames of the current tick, not yet encoded into the open segment.
    pub(crate) frames: Vec<ReplayFrame>,
    /// The open segment's encoded frames.
    pub(crate) body: Vec<u8>,
    pub(crate) last_pose_tick: u64,
    pub(crate) last_keyframe_tick: u64,
    /// Closed segments, compressed or being compressed, in order.
    pub(crate) packed: Vec<Packed>,
    pub(crate) last_distant_tick: u64,
    pub(crate) last_picture_tick: u64,
    pub(crate) blueprints: BTreeSet<ContentHash>,
    /// Entities that have had an omniscient introduction.
    pub(crate) known: BTreeSet<EntityId>,
    pub(crate) last_seen: Vec<(EntityId, SideMask)>,
    /// Shot log rows already examined, and the unresolved ones among them (indices into `Sim::shot_log`).
    pub(crate) shots_scanned: usize,
    pub(crate) shots_open: Vec<usize>,
    /// Distant-hull cadence credit: `distant_pose_hz` per pose frame, spent `pose_hz` at a time, so far hulls
    /// land on exactly `distant_pose_hz` of the `pose_hz` frames each second.
    pub(crate) distant_credit: u32,
    /// Frames in the open segment, and raw bytes and frames of the closed ones.
    pub(crate) seg_frames: usize,
    pub(crate) raw_total: usize,
    pub(crate) frames_total: usize,
}

/// One closed segment.
pub(crate) enum Packed { Ready(Vec<u8>), Pending(std::thread::JoinHandle<Vec<u8>>) }
impl Packed {
    /// Compress `raw` off the calling thread (inline when no thread can be started).
    fn spawn(raw: Vec<u8>) -> Packed {
        let shared = std::sync::Arc::new(raw);
        let job = shared.clone();
        match std::thread::Builder::new().name("replay-pack".into()).spawn(move || content::share::compress(&job)) {
            Ok(h) => Packed::Pending(h),
            Err(_) => Packed::Ready(content::share::compress(&shared)),
        }
    }
    /// The compressed frame; `None` only if its thread died (the segment is then left out).
    fn join(self) -> Option<Vec<u8>> { match self { Packed::Ready(v) => Some(v), Packed::Pending(h) => h.join().ok() } }
}

/// Keep the top `keep` mantissa bits of an f64 (sign and exponent untouched; non-finite values unchanged).
fn q64(x: f64, keep: u32) -> f64 { if !x.is_finite() { return x; } f64::from_bits(x.to_bits() & !((1u64 << (52 - keep.min(52))) - 1)) }
fn q32(x: f32, keep: u32) -> f32 { if !x.is_finite() { return x; } f32::from_bits(x.to_bits() & !((1u32 << (23 - keep.min(23))) - 1)) }
fn qv(v: DVec3) -> DVec3 { DVec3::new(q64(v.x, POS_BITS), q64(v.y, POS_BITS), q64(v.z, POS_BITS)) }
const POS_BITS: u32 = 24;
const ANGLE_BITS: u32 = 15;
pub(crate) fn quantise_vessel(v: &mut VesselView) {
    v.kin.pose.pos = qv(v.kin.pose.pos);
    let r = v.kin.pose.rot;
    v.kin.pose.rot = DQuat::from_xyzw(q64(r.x, POS_BITS), q64(r.y, POS_BITS), q64(r.z, POS_BITS), q64(r.w, POS_BITS));
    v.kin.lin_vel = qv(v.kin.lin_vel);
    v.kin.ang_vel = qv(v.kin.ang_vel);
    for t in &mut v.turrets { t.train_rad = q32(t.train_rad, ANGLE_BITS); t.elevation_rad = q32(t.elevation_rad, ANGLE_BITS); }
    v.rudder = q32(v.rudder, ANGLE_BITS);
    v.power = q32(v.power, ANGLE_BITS);
}
pub(crate) fn quantise_body(b: &mut BodyView) { b.pos = qv(b.pos); b.vel = qv(b.vel); }
pub(crate) fn quantise_decoy(d: &mut DecoyView) { d.pos = qv(d.pos); d.age_s = q32(d.age_s, ANGLE_BITS); }
pub(crate) fn quantise_track(t: &mut TrackView) { t.pos = qv(t.pos); t.vel = qv(t.vel); }
fn quantise(frame: &mut ReplayFrame) {
    match frame {
        ReplayFrame::Poses { vessels, bodies, decoys, .. } => {
            vessels.iter_mut().for_each(quantise_vessel); bodies.iter_mut().for_each(quantise_body); decoys.iter_mut().for_each(quantise_decoy);
        }
        ReplayFrame::Picture { tracks, .. } => tracks.iter_mut().for_each(quantise_track),
        ReplayFrame::Keyframe(k) => {
            k.vessels.iter_mut().for_each(quantise_vessel); k.bodies.iter_mut().for_each(quantise_body); k.decoys.iter_mut().for_each(quantise_decoy);
            for p in &mut k.pictures { p.1.iter_mut().for_each(quantise_track); }
        }
        _ => {}
    }
}

/// Elapsed-tick due check: robust to several ticks passing between calls and to a tick going backwards.
pub(crate) fn due(now: u64, last: u64, interval: u64) -> bool { now < last || now - last >= interval.max(1) }
pub(crate) fn interval_ticks(hz: u32) -> u64 { (TICK_HZ / hz.clamp(1, TICK_HZ)) as u64 }

fn opts() -> impl bincode::Options { use bincode::Options; bincode::DefaultOptions::new().with_limit(REPLAY_MAX_RAW_BYTES as u64).allow_trailing_bytes() }

/// Encode one frame and append it to `out`.
pub(crate) fn encode_frame(frame: &ReplayFrame, out: &mut Vec<u8>) {
    use bincode::Options;
    // Serialising plain data into a Vec fails only on allocation failure; a frame that somehow does not encode
    // is left out rather than taking the server down.
    let mark = out.len();
    if opts().serialize_into(&mut *out, frame).is_err() { out.truncate(mark); }
}

/// Decode a run of frames (one decompressed segment).
pub(crate) fn decode_frames(mut bytes: &[u8]) -> Result<Vec<ReplayFrame>, ReplayError> {
    use bincode::Options;
    let mut frames = Vec::new();
    while !bytes.is_empty() {
        if frames.len() >= SEGMENT_MAX_FRAMES { return Err(ReplayError::Corrupt("segment holds too many frames".into())); }
        let f: ReplayFrame = opts().deserialize_from(&mut bytes).map_err(|e| ReplayError::Corrupt(format!("frame {}: {e}", frames.len())))?;
        frames.push(f);
    }
    Ok(frames)
}

pub(crate) fn encode_header(header: &ReplayHeader) -> Vec<u8> {
    use bincode::Options;
    bincode::DefaultOptions::new().serialize(header).unwrap_or_default()
}

/// Write a replay file from a header and raw segments (compressed in parallel, output in order).
pub(crate) fn write_file(header: ReplayHeader, segments: &[&[u8]]) -> Vec<u8> {
    use rayon::prelude::*;
    let packed: Vec<Vec<u8>> = segments.par_iter().filter(|s| !s.is_empty()).map(|s| content::share::compress(s)).collect();
    write_packed(header, &packed)
}

/// Write a replay file from a header and compressed segments; fills `keyframe_offsets`, magic and format.
pub(crate) fn write_packed(mut header: ReplayHeader, segments: &[Vec<u8>]) -> Vec<u8> {
    header.magic = REPLAY_MAGIC;
    header.format = REPLAY_FORMAT;
    let mut body = Vec::new();
    header.keyframe_offsets.clear();
    for frame in segments {
        header.keyframe_offsets.push(body.len() as u64);
        body.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        body.extend_from_slice(&frame);
    }
    let head = encode_header(&header);
    let mut out = Vec::with_capacity(PREAMBLE + head.len() + body.len());
    out.extend_from_slice(&REPLAY_MAGIC);
    out.extend_from_slice(&REPLAY_FORMAT.to_le_bytes());
    out.extend_from_slice(&(head.len() as u32).to_le_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(&body);
    out
}

impl ReplayRecorder {
    /// A recorder with no frames: the pure core that `start` fills from a Sim.
    pub(crate) fn empty(header: ReplayHeader, options: RecordOptions, tick: u64) -> ReplayRecorder {
        ReplayRecorder {
            header, options, frames: Vec::new(), body: Vec::new(), last_pose_tick: tick, last_keyframe_tick: tick, packed: Vec::new(),
            last_distant_tick: tick, last_picture_tick: tick, blueprints: BTreeSet::new(), known: BTreeSet::new(), last_seen: Vec::new(), shots_scanned: 0,
            shots_open: Vec::new(), distant_credit: 0, seg_frames: 0, raw_total: 0, frames_total: 0,
        }
    }

    /// Queue a frame for the open segment, noting the blueprint hashes it names and the duration.
    pub(crate) fn push(&mut self, mut frame: ReplayFrame) {
        quantise(&mut frame);
        let bps = &mut self.blueprints;
        let mut note = |i: &EntityIntro| { if !i.blueprint.is_empty() { bps.insert(i.blueprint); } };
        match &frame {
            ReplayFrame::Event(OutEvent { event: SimEvent::EntityRevealed(i), .. }) => note(i),
            ReplayFrame::Keyframe(k) => { k.entities.iter().for_each(&mut note); k.sides.iter().flat_map(|s| s.intros.iter()).for_each(&mut note); }
            _ => {}
        }
        if let Some(t) = frame_tick(&frame) { self.header.duration_ticks = self.header.duration_ticks.max(t); }
        self.frames.push(frame);
    }

    /// Encode queued frames into the open segment.
    pub(crate) fn flush(&mut self) {
        let frames = core::mem::take(&mut self.frames);
        self.seg_frames += frames.len();
        for f in frames { encode_frame(&f, &mut self.body); }
    }

    fn close_segment(&mut self) {
        self.flush();
        if !self.body.is_empty() {
            self.raw_total += self.body.len();
            self.frames_total += self.seg_frames;
            self.seg_frames = 0;
            self.packed.push(Packed::spawn(core::mem::take(&mut self.body)));
        }
    }

    /// True once the recording is as long as a replay may be: later ticks are not recorded, and the file
    /// written so far stays valid.
    pub(crate) fn full(&self) -> bool {
        self.raw_total + self.body.len() + SEGMENT_MAX_BYTES >= REPLAY_MAX_RAW_BYTES
            || self.frames_total + self.seg_frames + SEGMENT_MAX_FRAMES >= REPLAY_MAX_FRAMES
    }

    /// Close the open segment (if it holds anything) and start a new one with this keyframe and the full
    /// drawable masks.
    pub(crate) fn begin_segment(&mut self, keyframe: ReplicatedKeyframe, seen: Vec<(EntityId, SideMask)>) {
        self.close_segment();
        let tick = keyframe.tick;
        for e in &keyframe.entities { self.known.insert(e.entity); }
        self.push(ReplayFrame::Keyframe(Box::new(keyframe)));
        self.push(ReplayFrame::Drawable { tick, seen: seen.clone() });
        self.last_seen = seen;
        self.last_keyframe_tick = tick;
        self.flush();
    }

    /// Record the drawable masks of this tick if they changed. An entity that left the world gets an
    /// omniscient `EntityHidden` addressed `Sides(SideMask::NONE)`, pairing the omniscient reveal.
    pub(crate) fn note_seen(&mut self, tick: u64, seen: Vec<(EntityId, SideMask)>) {
        if seen != self.last_seen {
            let gone: Vec<EntityId> = self.last_seen.iter().map(|x| x.0).filter(|e| seen.binary_search_by_key(e, |x| x.0).is_err()).collect();
            for entity in gone {
                self.known.remove(&entity);
                self.push(ReplayFrame::Event(OutEvent { tick, audience: Audience::Sides(SideMask::NONE), lane: Lane::Reliable, event: SimEvent::EntityHidden { entity } }));
            }
            self.push(ReplayFrame::Drawable { tick, seen: seen.clone() });
            self.last_seen = seen;
        }
    }

    /// Whether a keyframe is due at `tick`: every `keyframe_s`, or early when the open segment is large.
    pub(crate) fn keyframe_due(&self, tick: u64) -> bool {
        let interval = self.options.keyframe_s.max(1) as u64 * TICK_HZ as u64;
        due(tick, self.last_keyframe_tick, interval) || self.body.len() >= SEGMENT_SPLIT_BYTES || self.seg_frames >= SEGMENT_SPLIT_FRAMES
    }

    /// Queue a pose frame if due; vessels for which `near` is false only on the distant cadence.
    pub(crate) fn poses_due(&self, tick: u64) -> bool { due(tick, self.last_pose_tick, interval_ticks(self.options.pose_hz)) }
    pub(crate) fn push_poses(&mut self, tick: u64, mut vessels: Vec<VesselView>, bodies: Vec<BodyView>, decoys: Vec<DecoyView>, near: impl Fn(&VesselView) -> bool) {
        let pose_hz = self.options.pose_hz.clamp(1, TICK_HZ);
        let distant_due = if self.options.distant_pose_hz >= pose_hz { true } else {
            self.distant_credit += self.options.distant_pose_hz;
            if self.distant_credit >= pose_hz { self.distant_credit -= pose_hz; true } else { false }
        };
        if distant_due { self.last_distant_tick = tick; } else { vessels.retain(|v| near(v)); }
        self.push(ReplayFrame::Poses { tick, vessels, bodies, decoys });
        self.last_pose_tick = tick;
    }

    /// Start recording a sortie: fills the header from the config and writes the first keyframe.
    /// Unit: `sys-replay-record`.
    pub fn start(sim: &Sim, game_version: &str, options: RecordOptions) -> ReplayRecorder {
        let config = sim.config();
        let tick = sim.tick().0;
        let mut names: Vec<(PlayerSlot, String)> = sim.players().iter().map(|p| (p.slot, p.name.clone())).collect();
        names.sort_by_key(|n| n.0);
        let header = ReplayHeader {
            magic: REPLAY_MAGIC,
            format: REPLAY_FORMAT,
            game_version: game_version.to_string(),
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            tuning_version: sim.deps.catalogue.tuning.version,
            mode: config.mode,
            seed: config.seed,
            weather: sim.world().weather().clone(),
            duration_ticks: tick,
            blueprints: Vec::new(),
            player_names: names.into_iter().map(|n| n.1).collect(),
            operation: config.operation.as_ref().map(|o| o.to_string()).unwrap_or_default(),
            pinned: false,
            keyframe_offsets: Vec::new(),
        };
        let mut rec = ReplayRecorder::empty(header, options, tick);
        rec.begin_segment(keyframe(sim), seen_masks(sim));
        rec
    }

    /// Call once per tick after `Sim::step` with the events just drained (all audiences): appends events, a
    /// pose frame when due, pictures at 1 Hz and a keyframe every 60 s. Unit: `sys-replay-record`.
    ///
    /// Due checks count elapsed ticks, so a call after several compressed-time steps still records one frame
    /// of each kind that fell due. An entity seen for the first time also gets an omniscient `EntityRevealed`
    /// addressed `Sides(SideMask::NONE)`: no side admits it, so it never survives a PvP cut, while an omniscient
    /// consumer (which takes every non-host event) ends the tick on the full introduction.
    ///
    /// The omniscient pair is the convention for omniscient playback: a consumer drawing
    /// `Viewer::Omniscient` applies `EntityRevealed` and `EntityHidden` only when addressed
    /// `Sides(SideMask::NONE)`, since the sides' own reveals and hides describe what each side could see.
    pub fn record_tick(&mut self, sim: &Sim, events: &[OutEvent]) {
        if self.full() { return; }
        let tick = sim.tick().0;
        for e in events { self.push(ReplayFrame::Event(e.clone())); }

        let seen = seen_masks(sim);
        for &(e, _) in &seen {
            if self.known.insert(e) {
                if let Some(intro) = sim.world().locate(e).and_then(|l| crate::sim::snapshot::entity_intro(sim, e, l.2)) {
                    self.push(ReplayFrame::Event(OutEvent { tick, audience: Audience::Sides(SideMask::NONE), lane: Lane::Reliable, event: SimEvent::EntityRevealed(Box::new(intro)) }));
                }
            }
        }
        self.note_seen(tick, seen);

        if self.poses_due(tick) {
            let snap = sim.snapshot(Viewer::Omniscient);
            let hulls: Vec<DVec3> = sim.players().iter().filter_map(|p| p.vessel).filter_map(|v| sim.world().vessel(v)).map(|v| v.kin.pose.pos).collect();
            let d = self.options.distant_m;
            let near = |v: &VesselView| hulls.is_empty() || !v.kin.pose.pos.is_finite() || hulls.iter().any(|h| h.distance(v.kin.pose.pos) <= d);
            self.push_poses(tick, snap.vessels, snap.bodies, snap.decoys, near);
        }

        if due(tick, self.last_picture_tick, TICK_HZ as u64) {
            for (side, tracks) in pictures(sim) { self.push(ReplayFrame::Picture { tick, side, tracks }); }
            self.last_picture_tick = tick;
        }

        self.record_shots(sim.shot_log(), sim.is_over());
        self.flush();
        if self.keyframe_due(tick) { self.begin_segment(keyframe(sim), seen_masks(sim)); }
    }

    /// Shot log rows as they resolve (every remaining row once the sortie is over).
    pub(crate) fn record_shots(&mut self, log: &[crate::protocol::event::ShotLogEntry], over: bool) {
        if self.shots_scanned > log.len() { self.shots_scanned = log.len(); self.shots_open.retain(|&i| i < log.len()); }
        for i in self.shots_scanned..log.len() { self.shots_open.push(i); }
        self.shots_scanned = log.len();
        let mut still = Vec::new();
        for i in core::mem::take(&mut self.shots_open) {
            let row = log[i];
            if row.hit.is_some() || over { self.push(ReplayFrame::Shot(row)); } else { still.push(i); }
        }
        self.shots_open = still;
    }

    /// Finish: header (with keyframe offsets and duration) plus compressed body. Unit: `sys-replay-record`.
    pub fn finish(mut self) -> Vec<u8> {
        self.close_segment();
        let packed: Vec<Vec<u8>> = core::mem::take(&mut self.packed).into_iter().filter_map(Packed::join).collect();
        self.header.blueprints = self.blueprints.iter().copied().collect();
        write_packed(self.header.clone(), &packed)
    }
}

/// The tick a frame belongs to (`Shot` rows carry their firing tick, which is not their record tick).
pub(crate) fn frame_tick(f: &ReplayFrame) -> Option<u64> {
    match f {
        ReplayFrame::Poses { tick, .. } | ReplayFrame::Picture { tick, .. } | ReplayFrame::Drawable { tick, .. } => Some(*tick),
        ReplayFrame::Event(e) => Some(e.tick),
        ReplayFrame::Keyframe(k) => Some(k.tick),
        ReplayFrame::Shot(_) => None,
    }
}

/// Every live entity with the sides that may draw it, ascending.
fn seen_masks(sim: &Sim) -> Vec<(EntityId, SideMask)> {
    let mut v: Vec<(EntityId, SideMask)> = sim.world().entities().iter().map(|e| (e, sim.interest.sides_seeing(e))).collect();
    v.sort_by_key(|x| x.0);
    v
}

fn side_ids(sim: &Sim) -> Vec<SideId> {
    let mut sides: Vec<SideId> = sim.config().sides.iter().map(|s| s.id).collect();
    sides.sort();
    sides.dedup();
    sides
}

/// Each side's picture as its players see it, ascending side.
fn pictures(sim: &Sim) -> Vec<(SideId, Vec<TrackView>)> { side_ids(sim).into_iter().map(|s| (s, sim.snapshot(Viewer::Side(s)).tracks)).collect() }

/// Replicated state now.
fn keyframe(sim: &Sim) -> ReplicatedKeyframe {
    use crate::sim::snapshot::entity_intro;
    let world = sim.world();
    let mut ents: Vec<EntityId> = world.entities().iter().collect();
    ents.sort();
    let entities: Vec<EntityIntro> = ents.iter().filter_map(|&e| { let side = world.locate(e)?.2; entity_intro(sim, e, side) }).collect();
    let snap = sim.snapshot(Viewer::Omniscient);
    let allied = crate::sim::events::allied_masks(sim.config());
    let sides: Vec<SideCut> = side_ids(sim).into_iter().map(|s| {
        let mut players: Vec<(PlayerSlot, Option<EntityId>)> = sim.players().iter().filter(|p| p.side == s).map(|p| (p.slot, p.vessel)).collect();
        players.sort_by_key(|p| p.0);
        let drawable = sim.interest.drawable.get(s.index()).cloned().unwrap_or_default();
        let intros = drawable.iter().filter_map(|&e| entity_intro(sim, e, s)).collect();
        SideCut { side: s, allied: allied.get(s.index()).copied().unwrap_or(SideMask::NONE).with(s), players, intros }
    }).collect();
    let exterior_parts = if crate::sim::events::is_pvp(sim.config().mode) {
        ents.iter().filter(|&&e| world.vessel(e).is_some()).map(|&e| {
            let parts = match sim.interest.exterior.get(&e) {
                Some(ix) => ix.parts.clone(),
                None => crate::sim::events::current_exterior(sim, e).map(|x| x.0.parts).unwrap_or_default(),
            };
            (e, parts)
        }).collect()
    } else { Vec::new() };
    ReplicatedKeyframe {
        tick: sim.tick().0,
        weather: world.weather().clone(),
        wave_seed: world.waves().seed,
        entities,
        vessels: snap.vessels,
        bodies: snap.bodies,
        decoys: snap.decoys,
        pictures: pictures(sim),
        pacing: snap.pacing,
        objectives: snap.objectives,
        sides,
        exterior_parts,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Per-side view rules, shared by the PvP cut and `Viewer::Side` playback.
// ---------------------------------------------------------------------------------------------------------

/// Who is on which side, from the latest keyframe.
#[derive(Clone, Debug, Default)]
pub(crate) struct Roster { pub players: BTreeMap<PlayerSlot, SideId> }
impl Roster {
    pub(crate) fn from_sides(sides: &[SideCut]) -> Roster {
        let mut r = Roster::default();
        for s in sides { for p in &s.players { r.players.insert(p.0, s.side); } }
        r
    }
    /// True when the side, or any player on it, is admitted. `Host` admits no side.
    pub(crate) fn admits_side(&self, audience: Audience, side: SideId) -> bool {
        match audience {
            Audience::All => true,
            Audience::Sides(m) => m.has(side),
            Audience::Players(m) => self.players.iter().any(|(p, s)| *s == side && m.has(*p)),
            Audience::Host => false,
        }
    }
    pub(crate) fn side_of(&self, p: PlayerSlot) -> Option<SideId> { self.players.get(&p).copied() }
}

/// What a side may draw of the true poses, as live play draws it (`sim::snapshot::build`).
pub(crate) struct SideFilter<'a> {
    pub side: SideId,
    pub allied: SideMask,
    pub pvp: bool,
    /// Drawable masks; `None` when the caller has already chosen what is drawable.
    pub seen: Option<&'a [(EntityId, SideMask)]>,
    pub exterior: &'a BTreeMap<EntityId, Vec<PartSlot>>,
    pub pairing: &'a BTreeMap<EntityId, TrackId>,
    pub sides_of: &'a BTreeMap<EntityId, SideId>,
    /// Entities drawn although not drawable: the killer during a kill cam window the side was sent (L1049).
    pub extra: &'a [EntityId],
    /// The viewer's own hull and its position (`Viewer::Player`), for `Relevance::Own` and the range bands.
    pub own: Option<EntityId>,
    pub own_pos: Option<DVec3>,
}
impl SideFilter<'_> {
    pub(crate) fn can_draw(&self, e: EntityId) -> bool {
        if self.extra.contains(&e) { return true; }
        match self.seen {
            None => true,
            Some(seen) => seen.binary_search_by_key(&e, |x| x.0).map(|i| seen[i].1.has(self.side)).unwrap_or(false),
        }
    }
    fn allied_entity(&self, e: EntityId) -> bool { self.sides_of.get(&e).map(|s| self.allied.has(*s)).unwrap_or(false) }
    pub(crate) fn vessel(&self, v: &VesselView) -> Option<VesselView> {
        if !self.can_draw(v.entity) { return None; }
        let mut v = v.clone();
        v.track = self.pairing.get(&v.entity).copied().unwrap_or(TrackId::NONE);
        if Some(v.entity) == self.own {
            v.relevance = Relevance::Own;
        } else if self.allied_entity(v.entity) {
            v.relevance = Relevance::OwnSide;
        } else {
            v.relevance = crate::sim::snapshot::relevance(self.own_pos, v.kin.pose.pos, v.kin.lin_vel, false, false, false);
            if self.pvp {
                // Fail closed: a vessel with no recorded exterior shows no turrets.
                let allowed = self.exterior.get(&v.entity);
                v.turrets.retain(|t| allowed.map(|a| a.binary_search(&t.slot).is_ok()).unwrap_or(false));
            }
        }
        Some(v)
    }
    pub(crate) fn body(&self, b: &BodyView) -> Option<BodyView> {
        if !self.can_draw(b.entity) { return None; }
        let mut b = *b;
        b.track = self.pairing.get(&b.entity).copied().unwrap_or(TrackId::NONE);
        if self.allied.has(b.side) {
            b.relevance = Relevance::OwnSide;
        } else {
            b.seeker = SeekerPhase::Off;
            b.relevance = crate::sim::snapshot::relevance(self.own_pos, b.pos, b.vel, false, false, true);
        }
        Some(b)
    }
    pub(crate) fn decoy(&self, d: &DecoyView) -> Option<DecoyView> {
        if !self.can_draw(d.entity) { return None; }
        let mut d = *d;
        if !self.can_draw(d.owner) { d.owner = EntityId::NONE; }
        Some(d)
    }
}

/// Apply an event a side received to its entity-to-track pairing.
pub(crate) fn update_pairing(pairing: &mut BTreeMap<EntityId, TrackId>, event: &SimEvent, side: SideId) {
    match event {
        SimEvent::EntityRevealed(i) => { pairing.insert(i.entity, i.track); }
        SimEvent::TrackMerged { side: s, from, into } if *s == side => { for t in pairing.values_mut() { if *t == *from { *t = *into; } } }
        _ => {}
    }
}

/// Kill cam windows sent to a side: (killer, from tick, to tick).
pub(crate) fn kill_cams_for(frames: &[ReplayFrame], side: SideId) -> Vec<(EntityId, u64, u64)> {
    let mut roster = Roster::default();
    let mut out = Vec::new();
    for f in frames {
        match f {
            ReplayFrame::Keyframe(k) => roster = Roster::from_sides(&k.sides),
            ReplayFrame::Event(OutEvent { audience, event: SimEvent::KillCam { killer: Some(k), from_tick, to_tick, .. }, .. }) if roster.admits_side(*audience, side) => out.push((*k, *from_tick, *to_tick)),
            _ => {}
        }
    }
    out
}
pub(crate) fn extra_at(kill_cams: &[(EntityId, u64, u64)], tick: u64) -> Vec<EntityId> { kill_cams.iter().filter(|c| c.1 <= tick && tick <= c.2).map(|c| c.0).collect() }

/// Cut a finished replay to what one side may see, with exterior-only blueprints for the other team (PvP
/// release after the match, L1049). Unit: `sys-replay-record`.
///
/// Kept: events the side or one of its players was sent; of `Host` rows only the side's own shot log
/// (`ShotLogged` events and `Shot` rows of its allies' players), released with the replay after the match;
/// pictures of the side and its allies; poses of what the side could draw at that moment (from the
/// `Drawable` frames) with other-team turrets limited to their weather-deck parts, other-team seekers off,
/// track pairing redone for the side and undrawable decoy owners scrubbed; keyframes rebuilt from the side's
/// own introductions; the header's blueprint list rebuilt from what survives. The killer of one of the side's
/// hulls stays drawn over a kill cam window the side was sent. Everything else is dropped: the cut fails
/// closed.
pub fn cut_for_side(replay: &[u8], side: SideId) -> Result<Vec<u8>, super::ReplayError> {
    let (mut header, segments) = super::play::read_file(replay)?;
    let pvp = crate::sim::events::is_pvp(header.mode);
    let all: Vec<ReplayFrame> = segments.into_iter().flatten().collect();
    let kill_cams = kill_cams_for(&all, side);
    let mut roster = Roster::default();
    let mut allied = SideMask::only(side);
    let mut exterior: BTreeMap<EntityId, Vec<PartSlot>> = BTreeMap::new();
    let mut pairing: BTreeMap<EntityId, TrackId> = BTreeMap::new();
    let mut sides_of: BTreeMap<EntityId, SideId> = BTreeMap::new();
    let mut seen: Vec<(EntityId, SideMask)> = Vec::new();
    let mut blueprints: BTreeSet<ContentHash> = BTreeSet::new();
    let mut out_segments: Vec<Vec<u8>> = Vec::new();
    let mut raw: Vec<u8> = Vec::new();
    for f in all {
        let kept: Option<ReplayFrame> = match f {
            ReplayFrame::Keyframe(k) => {
                if !raw.is_empty() { out_segments.push(core::mem::take(&mut raw)); }
                let mut k = *k;
                roster = Roster::from_sides(&k.sides);
                let mine = k.sides.iter().find(|s| s.side == side).cloned().unwrap_or(SideCut { side, allied: SideMask::only(side), ..Default::default() });
                allied = mine.allied.with(side);
                exterior = k.exterior_parts.iter().cloned().collect();
                pairing = mine.intros.iter().map(|i| (i.entity, i.track)).collect();
                for i in k.entities.iter().chain(mine.intros.iter()) { sides_of.insert(i.entity, i.side); }
                let drawn: BTreeSet<EntityId> = mine.intros.iter().map(|i| i.entity).collect();
                let extra = extra_at(&kill_cams, k.tick);
                let drawable = |e: EntityId| drawn.contains(&e) || extra.contains(&e);
                let sf = SideFilter { side, allied, pvp, seen: None, exterior: &exterior, pairing: &pairing, sides_of: &sides_of, extra: &extra, own: None, own_pos: None };
                k.vessels = k.vessels.iter().filter(|v| drawable(v.entity)).filter_map(|v| sf.vessel(v)).collect();
                k.bodies = k.bodies.iter().filter(|b| drawable(b.entity)).filter_map(|b| sf.body(b)).collect();
                k.decoys = k.decoys.iter().filter(|d| drawable(d.entity)).map(|d| { let mut d = *d; if !drawable(d.owner) { d.owner = EntityId::NONE; } d }).collect();
                k.entities = mine.intros.clone();
                k.sides.retain(|s| allied.has(s.side));
                for s in &mut k.sides { if s.side != side { s.intros.retain(|i| drawn.contains(&i.entity)); } }
                for i in k.sides.iter().flat_map(|s| s.intros.iter()) { if !i.blueprint.is_empty() { blueprints.insert(i.blueprint); } }
                k.pictures.retain(|p| allied.has(p.0));
                k.exterior_parts.retain(|(e, _)| drawable(*e));
                Some(ReplayFrame::Keyframe(Box::new(k)))
            }
            ReplayFrame::Drawable { tick, seen: s } => {
                seen = s;
                // Only entities the side may draw, and of each mask only the side's and its allies' bits.
                let mine = seen.iter().filter(|x| x.1.has(side)).map(|x| (x.0, SideMask(x.1 .0 & allied.0).with(side))).collect();
                Some(ReplayFrame::Drawable { tick, seen: mine })
            }
            ReplayFrame::Poses { tick, vessels, bodies, decoys } => {
                let extra = extra_at(&kill_cams, tick);
                let sf = SideFilter { side, allied, pvp, seen: Some(&seen), exterior: &exterior, pairing: &pairing, sides_of: &sides_of, extra: &extra, own: None, own_pos: None };
                Some(ReplayFrame::Poses {
                    tick,
                    vessels: vessels.iter().filter_map(|v| sf.vessel(v)).collect(),
                    bodies: bodies.iter().filter_map(|b| sf.body(b)).collect(),
                    decoys: decoys.iter().filter_map(|d| sf.decoy(d)).collect(),
                })
            }
            ReplayFrame::Picture { tick, side: s, tracks } => if allied.has(s) { Some(ReplayFrame::Picture { tick, side: s, tracks }) } else { None },
            ReplayFrame::Shot(row) => if roster.side_of(row.player).map(|s| allied.has(s)).unwrap_or(false) { Some(ReplayFrame::Shot(row)) } else { None },
            ReplayFrame::Event(e) => {
                if let SimEvent::EntityRevealed(i) = &e.event { sides_of.insert(i.entity, i.side); }
                let keep = match (&e.audience, &e.event) {
                    (Audience::Host, SimEvent::ShotLogged(row)) => roster.side_of(row.player).map(|s| allied.has(s)).unwrap_or(false),
                    (a, _) => roster.admits_side(*a, side),
                };
                if keep {
                    update_pairing(&mut pairing, &e.event, side);
                    if let SimEvent::EntityRevealed(i) = &e.event { if !i.blueprint.is_empty() { blueprints.insert(i.blueprint); } }
                    Some(ReplayFrame::Event(e))
                } else { None }
            }
        };
        if let Some(f) = kept { encode_frame(&f, &mut raw); }
    }
    if !raw.is_empty() { out_segments.push(raw); }
    header.blueprints = blueprints.into_iter().collect();
    header.pinned = false;
    let segs: Vec<&[u8]> = out_segments.iter().map(|s| s.as_slice()).collect();
    Ok(write_file(header, &segs))
}

/// Which stored replays to delete to keep the 20 most recent plus pinned ones (L1047): input is
/// (file name, recorded order, pinned); output the names to delete. Unit: `sys-replay-record`.
///
/// Pinned replays never count against the 20, so pinning one never pushes a recent replay out. Ties in
/// recorded order break by file name (the later name counts as more recent). A name listed twice is decided
/// once, by its most recent order, and a name pinned in any entry is kept. Output ascends by name.
pub fn retention(files: &[(String, u64, bool)]) -> Vec<String> {
    let mut best: BTreeMap<&str, (u64, bool)> = BTreeMap::new();
    for (name, order, pinned) in files {
        let e = best.entry(name.as_str()).or_insert((*order, *pinned));
        e.0 = e.0.max(*order);
        e.1 |= *pinned;
    }
    let mut unpinned: Vec<(u64, &str)> = best.iter().filter(|(_, v)| !v.1).map(|(n, v)| (v.0, *n)).collect();
    unpinned.sort_by(|a, b| b.cmp(a));
    let mut out: Vec<String> = unpinned.into_iter().skip(content::replay::REPLAY_KEEP).map(|(_, n)| n.to_string()).collect();
    out.sort();
    out
}
