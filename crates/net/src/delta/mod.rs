//! Unit `net-delta` (hard: baselines, loss, and the bandwidth gate depend on it). Per-client snapshot frames:
//! delta encoding against the newest acknowledged baseline, and client-side reconstruction.
//! DESIGN.md L920 (quantised poses), L924 (priority fill), L930-933 (20 bytes or less per vessel update on
//! average; 256 kbps p95 per client with 41 hulls), L939 (decoding is identical on every platform).
//!
//! Frame layout (bit packed, `schema::BitWriter`): header (sequence, sim tick, baseline sequence or none, item
//! counts), removed item keys since the baseline, then items in priority order, each coded with
//! `schema::Replicate` against the receiver's copy in the baseline. Vessel, body and decoy items use their
//! quantised wire forms; tracks and the slow sections (`ShipStatus`, allies, alerts, objectives, pacing) are
//! sent whole (bincode) when changed since the baseline and due by the accumulator.
//!
//! Baselines: the server keeps the last [`HISTORY`] frames it sent to the client together with the client's
//! full reconstructed state after each frame; a frame is a valid baseline only once acked
//! (`msg::SnapshotAck`). With no valid baseline it writes full items. The decoder keeps the same history and
//! rebuilds the full state; frames older than the newest decoded one are ignored (unreliable channel).
//!
//! # Baselines under loss and ack lag (carry-forward)
//! Acks arrive about one RTT late, so several frames in flight share one baseline. The decoder therefore
//! builds each frame's state from its own newest state ("carry-forward") and applies only the frame's items
//! and removals; an item is delta coded against the decoder's copy in the named baseline frame `B`. The
//! encoder keeps, per item, the last touch (write or removal) the client is known to have decoded and the
//! touches whose fate is still unknown. An item is delta coded only when its value in the client's state
//! after `B` is certain: the newest touch at or before `B` was decoded, and no touch with unknown fate lies
//! at or before `B`. Otherwise it is written in full behind one flag bit. Removals repeat in every frame
//! until a frame carrying them is acked, and the decoder applies them as remove-if-present. Tracks, bodies,
//! decoys and sections that are due but whose every possible client value already equals the current value
//! are not written (they count as sent); vessels are always written when due, so the interpolator gets a
//! fresh sample. A lost update is repaired at the item's next due time; nothing ever regresses to an older
//! value, because the decoder never rebuilds from the baseline copy.
//!
//! After [`SnapshotEncoder::reset`], frames carry the epoch start (the first post-reset sequence) until one
//! of them is acked; a decoder whose newest frame predates the epoch starts that frame from an empty state,
//! so nothing from the previous sortie lingers.
//!
//! Vessel and body positions are delta coded against a prediction from the base's velocity over the ticks
//! between the base and the frame, and vessel rotations against the base rotation advanced by its angular
//! velocity, so a hull under way costs a few bits per axis. Both ends compute the prediction from the same
//! stored wire values with the same IEEE operations (no transcendentals), so it is identical everywhere.
//!
//! Frame layout: seq (varint), tick (varint), has-baseline bit, has-epoch bit, `seq - B` (varint),
//! `seq - epoch` (varint), removed count and keys, item count and items. An item is a 3-bit kind (vessel,
//! body, decoy, track, section), a 3-bit section index for sections, a delta bit when the frame has a
//! baseline, the key when delta coded (entity index and generation, or track id; a full item carries its own
//! id), then the `schema::Replicate` payload.
use crate::error::NetError;
use crate::msg::SnapshotAck;
use crate::schema::replicate::{Coder, Elems, OptSub, WireEq};
use crate::schema::quant::{dequantize_rot, dequantize_vec, quantize_rot};
use crate::schema::replicate::{track_pos_dq, track_pos_q, track_vel_q, TRACK_POS_UNITS_PER_M, TRACK_VEL_UNITS_PER_MPS};
use crate::schema::{BitReader, BitWriter, Replicate, WireBody, WireDecoy, WireShipSlow, WireVessel, ANG_VEL_UNITS_PER_RADPS};
use glam::DQuat;
use serde::{Deserialize, Serialize};
use sim_core::EntityId;
use sim_systems::ids::TrackId;
use sim_systems::protocol::alert::AlertView;
use sim_systems::protocol::snapshot::{AllyStatus, ObjectiveView, Pacing, Relevance, ShipStatus, Snapshot, TrackView};
use std::collections::{BTreeMap, VecDeque};

/// Frames of history kept on both ends (a little over 2 s at 30 Hz).
pub const HISTORY: usize = 72;

/// One replicated item of a viewer's snapshot. Order is the tie-break order of the accumulator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ItemKey { Vessel(EntityId), Body(EntityId), Decoy(EntityId), Track(TrackId), Section(Section) }

/// The non-list parts of a `Snapshot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Section { Ship, Allies, Alerts, Objectives, Pacing, ShipSlow }
impl Section {
    /// Every section in key order.
    pub const ALL: [Section; 6] = [Section::Ship, Section::Allies, Section::Alerts, Section::Objectives, Section::Pacing, Section::ShipSlow];
    fn from_index(i: u64) -> Option<Section> { Section::ALL.get(i as usize).copied() }
}

/// Byte counts of one frame by item kind, for the bandwidth gate (L930-931) and the connection log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameBytes {
    pub header: u32, pub vessels: u32, pub vessel_items: u32, pub bodies: u32, pub tracks: u32, pub sections: u32,
    /// Items written of bodies and decoys, and of tracks.
    #[serde(default)]
    pub body_items: u32,
    #[serde(default)]
    pub track_items: u32,
    /// Bytes per section, in [`Section::ALL`] order (they sum to `sections`, give or take rounding).
    #[serde(default)]
    pub section_bytes: [u32; 6],
    /// Removed-key list bytes (part of `header`).
    #[serde(default)]
    pub removals: u32,
}

/// What the encoder produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EncodedFrame {
    pub seq: u32,
    pub bytes: Vec<u8>,
    /// Items written, in order; the session passes them to `PriorityAccumulator::sent`.
    pub sent: Vec<ItemKey>,
    /// Items due that did not fit the budget.
    pub deferred: u32,
    pub stats: FrameBytes,
}

/// One item's value as both ends hold it (wire forms for poses, decoded views for the rest). Track views
/// dominate the size; boxing them would cost an allocation per item per frame for no gain.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ItemVal {
    Vessel(WireVessel),
    Body(WireBody),
    Decoy(WireDecoy),
    Track(TrackView),
    Ship(Option<ShipStatus>),
    ShipSlow(Option<WireShipSlow>),
    Allies(Vec<AllyStatus>),
    Alerts(Vec<AlertView>),
    Objectives(Vec<ObjectiveView>),
    Pacing(Pacing),
}

/// Item kind on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind { Vessel, Body, Decoy, Track, Section(Section) }
const KIND_BITS: u32 = 3;
impl Kind {
    fn of(key: &ItemKey) -> Kind {
        match key { ItemKey::Vessel(_) => Kind::Vessel, ItemKey::Body(_) => Kind::Body, ItemKey::Decoy(_) => Kind::Decoy, ItemKey::Track(_) => Kind::Track, ItemKey::Section(s) => Kind::Section(*s) }
    }
    fn tag(self) -> u64 { match self { Kind::Vessel => 0, Kind::Body => 1, Kind::Decoy => 2, Kind::Track => 3, Kind::Section(_) => 4 } }
}

/// Longest prediction interval: beyond 10 s the base is too old to be worth extrapolating.
const MAX_PREDICT_TICKS: u64 = 600;
/// Ticks per second over the position units per velocity unit (512 / 256): position advance in units is
/// `vel_units * ticks / 30`.
const PREDICT_DIV: i64 = (sim_core::TICK_HZ as i64) / 2;

fn div_round(n: i64, d: i64) -> i64 { if n >= 0 { (n + d / 2) / d } else { -((-n + d / 2) / d) } }
fn advance(pos: [i32; 3], vel: [i32; 3], dt: u64) -> [i32; 3] {
    let mut out = pos;
    for i in 0..3 { out[i] = (pos[i] as i64 + div_round(vel[i] as i64 * dt as i64, PREDICT_DIV)).clamp(i32::MIN as i64, i32::MAX as i64) as i32; }
    out
}
/// The base as the frame's tick would see it if nothing accelerated.
fn predict_vessel(b: &WireVessel, dt: u64) -> WireVessel {
    let dt = dt.min(MAX_PREDICT_TICKS);
    let mut p = b.clone();
    if dt == 0 { return p; }
    p.pose.pos = advance(b.pose.pos, b.lin_vel, dt);
    if b.ang_vel != [0; 3] {
        let half = dequantize_vec(b.ang_vel, ANG_VEL_UNITS_PER_RADPS) * (dt as f64 / sim_core::TICK_HZ as f64 * 0.5);
        let q = dequantize_rot(b.pose.rot);
        // First-order rotation by the world-frame angular velocity: dq = (half, 1) normalised, q' = dq * q.
        let n = (half.x * half.x + half.y * half.y + half.z * half.z + 1.0).sqrt();
        let dq = DQuat::from_xyzw(half.x / n, half.y / n, half.z / n, 1.0 / n);
        p.pose.rot = quantize_rot(qmul(dq, q));
    }
    p
}
/// Quaternion product written out, so both ends use the same operation order whatever glam does.
fn qmul(a: DQuat, b: DQuat) -> DQuat {
    DQuat::from_xyzw(
        a.w * b.x + a.x * b.w + a.y * b.z - a.z * b.y,
        a.w * b.y - a.x * b.z + a.y * b.w + a.z * b.x,
        a.w * b.z + a.x * b.y - a.y * b.x + a.z * b.w,
        a.w * b.w - a.x * b.x - a.y * b.y - a.z * b.z,
    )
}
/// A track estimate as the frame's tick would see it: the snapshot extrapolates live tracks to its own tick
/// (`Track::predict`), so between filter updates the position moves by exactly the velocity it carries.
/// Computed on the quantised grid with integer maths, identical on every platform.
fn predict_track(b: &TrackView, dt: u64) -> TrackView {
    let mut p = b.clone();
    let dt = dt.min(MAX_PREDICT_TICKS);
    if dt == 0 || b.vel == glam::DVec3::ZERO { return p; }
    let (pos, vel) = (track_pos_q(b.pos), track_vel_q(b.vel));
    // Position units per velocity unit-tick: (8 / 32) / 60 = 1 / 240.
    const DIV: i64 = (TRACK_VEL_UNITS_PER_MPS / TRACK_POS_UNITS_PER_M) as i64 * sim_core::TICK_HZ as i64;
    let mut q = pos;
    for i in 0..3 { q[i] = (pos[i] as i64 + div_round(vel[i] as i64 * dt as i64, DIV)).clamp(i32::MIN as i64, i32::MAX as i64) as i32; }
    p.pos = track_pos_dq(q);
    p
}
fn predict_body(b: &WireBody, dt: u64) -> WireBody {
    let mut p = *b;
    p.pos = advance(b.pos, b.vel, dt.min(MAX_PREDICT_TICKS));
    p
}

impl ItemVal {
    fn kind(&self) -> Kind {
        match self {
            ItemVal::Vessel(_) => Kind::Vessel, ItemVal::Body(_) => Kind::Body, ItemVal::Decoy(_) => Kind::Decoy, ItemVal::Track(_) => Kind::Track,
            ItemVal::Ship(_) => Kind::Section(Section::Ship), ItemVal::ShipSlow(_) => Kind::Section(Section::ShipSlow), ItemVal::Allies(_) => Kind::Section(Section::Allies), ItemVal::Alerts(_) => Kind::Section(Section::Alerts),
            ItemVal::Objectives(_) => Kind::Section(Section::Objectives), ItemVal::Pacing(_) => Kind::Section(Section::Pacing),
        }
    }
    /// Key of a value read in full (entity and track items carry their own id).
    fn key(&self) -> ItemKey {
        match self {
            ItemVal::Vessel(v) => ItemKey::Vessel(EntityId::from_u64(v.entity)),
            ItemVal::Body(v) => ItemKey::Body(EntityId::from_u64(v.entity)),
            ItemVal::Decoy(v) => ItemKey::Decoy(EntityId::from_u64(v.entity)),
            ItemVal::Track(t) => ItemKey::Track(t.id),
            ItemVal::Ship(_) => ItemKey::Section(Section::Ship),
            ItemVal::ShipSlow(_) => ItemKey::Section(Section::ShipSlow),
            ItemVal::Allies(_) => ItemKey::Section(Section::Allies),
            ItemVal::Alerts(_) => ItemKey::Section(Section::Alerts),
            ItemVal::Objectives(_) => ItemKey::Section(Section::Objectives),
            ItemVal::Pacing(_) => ItemKey::Section(Section::Pacing),
        }
    }
    fn wire_eq(&self, o: &ItemVal) -> bool {
        match (self, o) {
            (ItemVal::Vessel(a), ItemVal::Vessel(b)) => a.wire_eq(b),
            (ItemVal::Body(a), ItemVal::Body(b)) => a.wire_eq(b),
            (ItemVal::Decoy(a), ItemVal::Decoy(b)) => a.wire_eq(b),
            (ItemVal::Track(a), ItemVal::Track(b)) => a.wire_eq(b),
            (ItemVal::Ship(a), ItemVal::Ship(b)) => <OptSub as Coder<Option<ShipStatus>>>::same(a, b),
            (ItemVal::ShipSlow(a), ItemVal::ShipSlow(b)) => <OptSub as Coder<Option<WireShipSlow>>>::same(a, b),
            (ItemVal::Allies(a), ItemVal::Allies(b)) => <Elems as Coder<Vec<AllyStatus>>>::same(a, b),
            (ItemVal::Alerts(a), ItemVal::Alerts(b)) => <Elems as Coder<Vec<AlertView>>>::same(a, b),
            (ItemVal::Objectives(a), ItemVal::Objectives(b)) => <Elems as Coder<Vec<ObjectiveView>>>::same(a, b),
            (ItemVal::Pacing(a), ItemVal::Pacing(b)) => a.wire_eq(b),
            _ => false,
        }
    }
    /// Payload only. `base` must be the same kind (callers check).
    fn put(&self, base: Option<&ItemVal>, dt: u64, w: &mut BitWriter) {
        match (self, base) {
            (ItemVal::Vessel(v), Some(ItemVal::Vessel(b))) => v.write(Some(&predict_vessel(b, dt)), w),
            (ItemVal::Vessel(v), _) => v.write(None, w),
            (ItemVal::Body(v), Some(ItemVal::Body(b))) => v.write(Some(&predict_body(b, dt)), w),
            (ItemVal::Body(v), _) => v.write(None, w),
            (ItemVal::Decoy(v), Some(ItemVal::Decoy(b))) => v.write(Some(b), w),
            (ItemVal::Decoy(v), _) => v.write(None, w),
            (ItemVal::Track(v), Some(ItemVal::Track(b))) => v.write(Some(&predict_track(b, dt)), w),
            (ItemVal::Track(v), _) => v.write(None, w),
            (ItemVal::Ship(v), b) => OptSub::put(v, match b { Some(ItemVal::Ship(b)) => Some(b), _ => None }, w),
            (ItemVal::ShipSlow(v), b) => OptSub::put(v, match b { Some(ItemVal::ShipSlow(b)) => Some(b), _ => None }, w),
            (ItemVal::Allies(v), b) => Elems::put(v, match b { Some(ItemVal::Allies(b)) => Some(b), _ => None }, w),
            (ItemVal::Alerts(v), b) => Elems::put(v, match b { Some(ItemVal::Alerts(b)) => Some(b), _ => None }, w),
            (ItemVal::Objectives(v), b) => Elems::put(v, match b { Some(ItemVal::Objectives(b)) => Some(b), _ => None }, w),
            (ItemVal::Pacing(v), Some(ItemVal::Pacing(b))) => v.write(Some(b), w),
            (ItemVal::Pacing(v), _) => v.write(None, w),
        }
    }
    fn get(kind: Kind, base: Option<&ItemVal>, dt: u64, r: &mut BitReader<'_>) -> Result<ItemVal, NetError> {
        if let Some(b) = base { if b.kind() != kind { return Err(NetError::Decode("baseline item of another kind".into())); } }
        Ok(match kind {
            Kind::Vessel => ItemVal::Vessel(WireVessel::read(match base { Some(ItemVal::Vessel(b)) => Some(predict_vessel(b, dt)), _ => None }.as_ref(), r)?),
            Kind::Body => ItemVal::Body(WireBody::read(match base { Some(ItemVal::Body(b)) => Some(predict_body(b, dt)), _ => None }.as_ref(), r)?),
            Kind::Decoy => ItemVal::Decoy(WireDecoy::read(match base { Some(ItemVal::Decoy(b)) => Some(b), _ => None }, r)?),
            Kind::Track => ItemVal::Track(TrackView::read(match base { Some(ItemVal::Track(b)) => Some(predict_track(b, dt)), _ => None }.as_ref(), r)?),
            Kind::Section(Section::Ship) => ItemVal::Ship(OptSub::get(match base { Some(ItemVal::Ship(b)) => Some(b), _ => None }, r)?),
            Kind::Section(Section::ShipSlow) => ItemVal::ShipSlow(OptSub::get(match base { Some(ItemVal::ShipSlow(b)) => Some(b), _ => None }, r)?),
            Kind::Section(Section::Allies) => ItemVal::Allies(Elems::get(match base { Some(ItemVal::Allies(b)) => Some(b), _ => None }, r)?),
            Kind::Section(Section::Alerts) => ItemVal::Alerts(Elems::get(match base { Some(ItemVal::Alerts(b)) => Some(b), _ => None }, r)?),
            Kind::Section(Section::Objectives) => ItemVal::Objectives(Elems::get(match base { Some(ItemVal::Objectives(b)) => Some(b), _ => None }, r)?),
            Kind::Section(Section::Pacing) => ItemVal::Pacing(Pacing::read(match base { Some(ItemVal::Pacing(b)) => Some(b), _ => None }, r)?),
        })
    }
}

/// Every item of a snapshot as a wire value.
fn items_of(snap: &Snapshot) -> BTreeMap<ItemKey, ItemVal> {
    let mut m = BTreeMap::new();
    for v in &snap.vessels { m.insert(ItemKey::Vessel(v.entity), ItemVal::Vessel(WireVessel::from_view(v))); }
    for b in &snap.bodies { m.insert(ItemKey::Body(b.entity), ItemVal::Body(WireBody::from_view(b))); }
    for d in &snap.decoys { m.insert(ItemKey::Decoy(d.entity), ItemVal::Decoy(WireDecoy::from_view(d))); }
    for t in &snap.tracks { m.insert(ItemKey::Track(t.id), ItemVal::Track(t.clone())); }
    let mut ship = snap.ship.clone();
    let slow = ship.as_mut().map(WireShipSlow::split);
    m.insert(ItemKey::Section(Section::Ship), ItemVal::Ship(ship));
    m.insert(ItemKey::Section(Section::ShipSlow), ItemVal::ShipSlow(slow));
    m.insert(ItemKey::Section(Section::Allies), ItemVal::Allies(snap.allies.clone()));
    m.insert(ItemKey::Section(Section::Alerts), ItemVal::Alerts(snap.alerts.clone()));
    m.insert(ItemKey::Section(Section::Objectives), ItemVal::Objectives(snap.objectives.clone()));
    m.insert(ItemKey::Section(Section::Pacing), ItemVal::Pacing(snap.pacing.clone()));
    m
}

fn put_key(key: &ItemKey, w: &mut BitWriter) {
    w.write_bits(Kind::of(key).tag(), KIND_BITS);
    match key {
        ItemKey::Vessel(e) | ItemKey::Body(e) | ItemKey::Decoy(e) => { w.write_varint(e.index as u64); w.write_varint(e.generation as u64); }
        ItemKey::Track(t) => w.write_varint(t.0 as u64),
        ItemKey::Section(s) => w.write_bits(*s as u64, 3),
    }
}
fn get_kind(r: &mut BitReader<'_>) -> Result<Kind, NetError> {
    Ok(match r.read_bits(KIND_BITS)? {
        0 => Kind::Vessel, 1 => Kind::Body, 2 => Kind::Decoy, 3 => Kind::Track,
        4 => Kind::Section(Section::from_index(r.read_bits(3)?).ok_or_else(|| NetError::Decode("bad section".into()))?),
        k => return Err(NetError::Decode(format!("bad item kind {k}"))),
    })
}
fn get_id(kind: Kind, r: &mut BitReader<'_>) -> Result<ItemKey, NetError> {
    Ok(match kind {
        Kind::Vessel | Kind::Body | Kind::Decoy => {
            let e = EntityId { index: r.read_varint_u32()?, generation: r.read_varint_u32()? };
            match kind { Kind::Vessel => ItemKey::Vessel(e), Kind::Body => ItemKey::Body(e), _ => ItemKey::Decoy(e) }
        }
        Kind::Track => ItemKey::Track(TrackId(r.read_varint_u32()?)),
        Kind::Section(s) => ItemKey::Section(s),
    })
}
fn get_key(r: &mut BitReader<'_>) -> Result<ItemKey, NetError> { let k = get_kind(r)?; get_id(k, r) }

/// One write or removal of an item in a sent frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Touch {
    pub(crate) seq: u32,
    pub(crate) val: Option<ItemVal>,
    /// Sim tick of the frame that carried it: the time the value describes, from which a later frame
    /// predicts (both ends keep it, so the prediction interval is exact however long ago the item went).
    pub(crate) tick: u64,
}

/// What the encoder knows about the client's copy of one item.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ItemLog {
    /// Newest touch the client is known to have decoded.
    pub(crate) anchor: Option<Touch>,
    /// Newer touches whose fate is not known yet, ascending.
    pub(crate) pending: Vec<Touch>,
    /// Pending touches were dropped up to this sequence (a client that stopped acking): the client's copy
    /// is unknown until a newer touch is decoded.
    pub(crate) unknown_upto: u32,
}

/// What the client holds for an item in its state after a given frame.
enum Known<'a> { Value(&'a ItemVal, u64), Absent, Unknown }

impl ItemLog {
    fn anchor_seq(&self) -> u32 { self.anchor.as_ref().map_or(0, |t| t.seq) }
    fn tainted(&self) -> bool { self.unknown_upto > self.anchor_seq() }
    /// The client's copy in its state after frame `b` (newest acked, so every decoded touch is at or before
    /// it).
    fn at(&self, b: u32) -> Known<'_> {
        if (self.tainted() && self.unknown_upto <= b) || self.pending.iter().any(|t| t.seq <= b) { return Known::Unknown; }
        match &self.anchor { Some(Touch { val: Some(v), tick, .. }) => Known::Value(v, *tick), _ => Known::Absent }
    }
    /// The client may hold some value for this item now.
    fn might_hold(&self) -> bool {
        self.tainted() || matches!(self.anchor, Some(Touch { val: Some(_), .. })) || self.pending.iter().any(|t| t.val.is_some())
    }
    /// Every value the client could hold now equals `cur`.
    fn surely_equal(&self, cur: &ItemVal) -> bool {
        if self.tainted() { return false; }
        let anchor_ok = matches!(&self.anchor, Some(Touch { val: Some(v), .. }) if v.wire_eq(cur));
        anchor_ok && self.pending.iter().all(|t| t.val.as_ref().is_some_and(|v| v.wire_eq(cur)))
    }
    fn push(&mut self, t: Touch) {
        if self.pending.len() >= HISTORY {
            let dropped = self.pending.remove(0);
            self.unknown_upto = self.unknown_upto.max(dropped.seq);
        }
        self.pending.push(t);
    }
    /// Apply what an ack says about frame fates.
    fn resolve(&mut self, fate: impl Fn(u32) -> Option<bool>) {
        let mut newest_decoded: Option<usize> = None;
        let mut i = 0;
        while i < self.pending.len() {
            match fate(self.pending[i].seq) {
                Some(false) => { self.pending.remove(i); continue; }
                Some(true) => newest_decoded = Some(i),
                None => {}
            }
            i += 1;
        }
        if let Some(i) = newest_decoded {
            let t = self.pending[i].clone();
            if t.seq > self.anchor_seq() { self.anchor = Some(t); }
            self.pending.drain(..=i);
        }
    }
}

/// One frame sent: its sequence and tick (the prediction base time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameRec { pub(crate) seq: u32, pub(crate) tick: u64 }

/// Server side, one per client.
pub struct SnapshotEncoder {
    pub(crate) next_seq: u32,
    pub(crate) acked: SnapshotAck,
    /// The last [`HISTORY`] frames sent.
    pub(crate) history: VecDeque<FrameRec>,
    /// Per item, what the client holds (see the module docs).
    pub(crate) items: BTreeMap<ItemKey, ItemLog>,
    /// First sequence after the last reset, and whether a frame at or after it has been acked.
    pub(crate) epoch: u32,
    pub(crate) epoch_acked: bool,
}

impl SnapshotEncoder {
    pub fn new() -> Self { Self { next_seq: 1, acked: SnapshotAck::default(), history: Default::default(), items: Default::default(), epoch: 1, epoch_acked: false } }
    /// Record a client ack. Unit: `net-delta`.
    ///
    /// Acks naming a frame never sent, or a frame from before the last reset, are ignored. The newest ack
    /// wins; an equal one merges its mask. Every ack settles the fate of the frames it covers: the newest
    /// and the masked ones were decoded, the others within 32 frames never will be (the decoder drops frames
    /// older than its newest).
    pub fn on_ack(&mut self, ack: SnapshotAck) {
        if ack.newest == 0 || ack.newest > self.newest_sent() || ack.newest < self.epoch { return; }
        if ack.newest > self.acked.newest { self.acked = ack; } else if ack.newest == self.acked.newest { self.acked.mask |= ack.mask; }
        self.epoch_acked = true;
        let fate = |seq: u32| -> Option<bool> {
            if seq == ack.newest { return Some(true); }
            if seq > ack.newest { return None; }
            let back = ack.newest - seq;
            if back > 32 { return None; }
            Some(ack.mask & (1u32 << (back - 1)) != 0)
        };
        for log in self.items.values_mut() { log.resolve(fate); }
    }
    /// Encode one frame of `snap`: items from `order` (highest priority first; the own vessel always fits)
    /// until `budget_bytes` is spent. Unit: `net-delta`.
    pub fn encode(&mut self, snap: &Snapshot, order: &[crate::delta::ItemKey], budget_bytes: usize) -> EncodedFrame {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        let tick = snap.tick;
        let base = self.history.iter().find(|f| f.seq == self.acked.newest).copied()
            .filter(|f| self.acked.newest >= self.epoch && seq.wrapping_sub(f.seq) < HISTORY as u32);
        let current = items_of(snap);

        // Header.
        let mut head = BitWriter::with_capacity(16);
        head.write_varint(seq as u64);
        head.write_varint(tick);
        head.write_bool(base.is_some());
        let with_epoch = !self.epoch_acked;
        head.write_bool(with_epoch);
        if let Some(b) = base { head.write_varint((seq - b.seq) as u64); }
        if with_epoch { head.write_varint(seq.wrapping_sub(self.epoch) as u64); }

        // Removals: everything the client might hold that the viewer may no longer see.
        let mut removed = BitWriter::new();
        let mut n_removed = 0u64;
        let mut gone = Vec::new();
        for (key, log) in &self.items {
            if current.contains_key(key) { continue; }
            if log.might_hold() { put_key(key, &mut removed); n_removed += 1; gone.push(*key); } else { gone.push(*key); }
        }
        for key in gone {
            let log = self.items.get_mut(&key).expect("key from the map");
            if log.might_hold() { log.push(Touch { seq, val: None, tick }); } else { self.items.remove(&key); }
        }
        let mut removed_all = BitWriter::new();
        removed_all.write_varint(n_removed);
        removed_all.append(&removed);

        // Items, in priority order, inside the budget.
        let budget_bits = budget_bytes.saturating_mul(8);
        let fixed_bits = head.bit_len() + removed_all.bit_len() + 16;
        let mut body = BitWriter::with_capacity(budget_bytes.min(64 * 1024));
        let mut sent = Vec::with_capacity(order.len());
        let mut done = std::collections::BTreeSet::new();
        let mut deferred = 0u32;
        let mut n_items = 0u64;
        let mut cat_bits = [0usize; 5]; // vessels, bodies (and decoys), tracks, sections, removal/header
        let mut vessel_items = 0u32;
        let (mut body_items, mut track_items) = (0u32, 0u32);
        let mut sec_bits = [0usize; 6];
        for key in order {
            let Some(cur) = current.get(key) else { continue };
            if !done.insert(*key) { continue; }
            let own = matches!(cur, ItemVal::Vessel(v) if v.relevance == Relevance::Own);
            let log = self.items.get(key);
            if !matches!(cur, ItemVal::Vessel(_)) && log.is_some_and(|l| l.surely_equal(cur)) {
                sent.push(*key);
                continue;
            }
            let (base_val, base_tick) = match (base, log) {
                (Some(b), Some(l)) => match l.at(b.seq) { Known::Value(v, t) => (Some(v), t), _ => (None, tick) },
                _ => (None, tick),
            };
            // Predict over the time since the value the client holds was sampled, not since the baseline
            // frame: an item not written every frame is older than the frame that carries it forward.
            let dt = tick.saturating_sub(base_tick);
            let mut payload = BitWriter::new();
            cur.put(base_val, dt, &mut payload);
            let mut item = BitWriter::with_capacity(payload.bytes.len() + 8);
            let kind = Kind::of(key);
            item.write_bits(kind.tag(), KIND_BITS);
            if let Kind::Section(s) = kind { item.write_bits(s as u64, 3); }
            if base.is_some() { item.write_bool(base_val.is_some()); }
            if base_val.is_some() {
                match key {
                    ItemKey::Vessel(e) | ItemKey::Body(e) | ItemKey::Decoy(e) => { item.write_varint(e.index as u64); item.write_varint(e.generation as u64); }
                    ItemKey::Track(t) => item.write_varint(t.0 as u64),
                    ItemKey::Section(_) => {}
                }
            }
            item.append(&payload);
            if !own && fixed_bits + body.bit_len() + item.bit_len() > budget_bits {
                deferred += 1;
                continue;
            }
            // Store exactly what the client will reconstruct: decode our own payload.
            let stored = match ItemVal::get(kind, base_val, dt, &mut BitReader::new(&payload.bytes)) {
                Ok(v) => v,
                Err(_) => { deferred += 1; continue; }
            };
            body.append(&item);
            n_items += 1;
            let cat = match kind { Kind::Vessel => 0, Kind::Body | Kind::Decoy => 1, Kind::Track => 2, Kind::Section(_) => 3 };
            cat_bits[cat] += item.bit_len();
            match kind {
                Kind::Vessel => vessel_items += 1,
                Kind::Body | Kind::Decoy => body_items += 1,
                Kind::Track => track_items += 1,
                Kind::Section(s) => sec_bits[s as usize] += item.bit_len(),
            }
            self.items.entry(*key).or_default().push(Touch { seq, val: Some(stored), tick });
            sent.push(*key);
        }

        let mut frame = head;
        frame.append(&removed_all);
        frame.write_varint(n_items);
        frame.append(&body);
        let bytes = frame.finish();
        let total = bytes.len() as u32;
        let to_bytes = |b: usize| (b / 8) as u32;
        let (v, b, t, s) = (to_bytes(cat_bits[0]), to_bytes(cat_bits[1]), to_bytes(cat_bits[2]), to_bytes(cat_bits[3]));
        let section_bytes = sec_bits.map(to_bytes);
        let removals = to_bytes(removed_all.bit_len());
        let stats = FrameBytes { header: total.saturating_sub(v + b + t + s), vessels: v, vessel_items, bodies: b, tracks: t, sections: s, body_items, track_items, section_bytes, removals };

        self.history.push_back(FrameRec { seq, tick });
        while self.history.len() > HISTORY { self.history.pop_front(); }
        EncodedFrame { seq, bytes, sent, deferred, stats }
    }
    /// Newest sequence sent.
    pub fn newest_sent(&self) -> u32 { self.next_seq.saturating_sub(1) }
    /// Snapshots the client is behind the newest one sent (L1033). Real code.
    pub fn frames_behind(&self) -> u32 { self.newest_sent().saturating_sub(self.acked.newest) }
    /// Drop all baselines (new sortie, rejoin): the next frame is full. Real code.
    pub fn reset(&mut self) {
        self.history.clear();
        // Nothing before the epoch can serve as a baseline; counting it as acked keeps `frames_behind`
        // meaningful across the reset instead of jumping to the whole session's frame count.
        self.acked = SnapshotAck { newest: self.newest_sent(), mask: 0 };
        self.items.clear();
        self.epoch = self.next_seq;
        self.epoch_acked = false;
    }
}
impl Default for SnapshotEncoder { fn default() -> Self { Self::new() } }

/// A decoded frame: the client's full reconstructed snapshot after this frame, and which items it updated.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedFrame { pub seq: u32, pub tick: u64, pub snapshot: Snapshot, pub fresh: Vec<ItemKey> }

/// The client's reconstructed state after one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClientState {
    pub(crate) tick: u64,
    pub(crate) items: BTreeMap<ItemKey, ItemVal>,
    /// Tick of the frame that last wrote each item (the prediction base time, see [`Touch::tick`]).
    pub(crate) ticks: BTreeMap<ItemKey, u64>,
}

impl ClientState {
    fn snapshot(&self) -> Snapshot {
        let mut s = Snapshot { tick: self.tick, ..Default::default() };
        for v in self.items.values() {
            match v {
                ItemVal::Vessel(w) => s.vessels.push(w.to_view()),
                ItemVal::Body(w) => s.bodies.push(w.to_view()),
                ItemVal::Decoy(w) => s.decoys.push(w.to_view()),
                ItemVal::Track(t) => s.tracks.push(t.clone()),
                ItemVal::Ship(x) => s.ship = x.clone(),
                ItemVal::ShipSlow(_) => {}
                ItemVal::Allies(x) => s.allies = x.clone(),
                ItemVal::Alerts(x) => s.alerts = x.clone(),
                ItemVal::Objectives(x) => s.objectives = x.clone(),
                ItemVal::Pacing(x) => s.pacing = x.clone(),
            }
        }
        // The own status is whole or absent: until its slow half has arrived it is not shown at all, so no
        // reader ever sees empty part lists or an empty Bridge Board.
        match (s.ship.as_mut(), self.items.get(&ItemKey::Section(Section::ShipSlow))) {
            (Some(ship), Some(ItemVal::ShipSlow(Some(slow)))) => slow.merge_into(ship),
            (Some(_), _) => s.ship = None,
            _ => {}
        }
        // Own-side vessels first, then the others, each ascending (the order `Sim::snapshot` uses).
        s.vessels.sort_by_key(|v| (!matches!(v.relevance, Relevance::Own | Relevance::OwnSide), v.entity));
        s
    }
}

/// Client side.
pub struct SnapshotDecoder {
    pub(crate) newest: u32,
    pub(crate) received_mask: u32,
    pub(crate) history: VecDeque<(u32, Box<ClientState>)>,
}

impl SnapshotDecoder {
    pub fn new() -> Self { Self { newest: 0, received_mask: 0, history: Default::default() } }
    /// Decode a frame. `Ok(None)` for a frame older than the newest decoded or whose baseline is gone
    /// (it is simply dropped; the server re-sends from an acked baseline). Unit: `net-delta`.
    ///
    /// A malformed frame is an error and leaves the decoder unchanged.
    pub fn decode(&mut self, bytes: &[u8]) -> Result<Option<DecodedFrame>, NetError> {
        let mut r = BitReader::new(bytes);
        let seq = r.read_varint_u32()?;
        let tick = r.read_varint()?;
        let has_base = r.read_bool()?;
        let has_epoch = r.read_bool()?;
        let base_seq = if has_base {
            let d = r.read_varint_u32()?;
            if d == 0 || d > seq { return Err(NetError::Decode("bad baseline distance".into())); }
            Some(seq - d)
        } else { None };
        let epoch = if has_epoch { Some(seq.checked_sub(r.read_varint_u32()?).ok_or_else(|| NetError::Decode("bad epoch".into()))?) } else { None };
        if seq == 0 || seq <= self.newest { return Ok(None); }
        let base: Option<&ClientState> = match base_seq {
            Some(b) => match self.history.iter().find(|(s, _)| *s == b) { Some((_, st)) => Some(st), None => return Ok(None) },
            None => None,
        };
        let fresh_epoch = epoch.is_some_and(|e| self.newest < e);
        let mut state = match (fresh_epoch, self.history.back()) {
            (false, Some((_, st))) => (**st).clone(),
            _ => ClientState::default(),
        };
        state.tick = tick;

        let n_removed = r.read_count(8)?;
        for _ in 0..n_removed {
            let key = get_key(&mut r)?;
            if matches!(key, ItemKey::Section(_)) { return Err(NetError::Decode("section removal".into())); }
            state.items.remove(&key);
            state.ticks.remove(&key);
        }
        let n_items = r.read_count(4)?;
        let mut fresh = Vec::with_capacity(n_items);
        for _ in 0..n_items {
            let kind = get_kind(&mut r)?;
            let delta = has_base && r.read_bool()?;
            let (key, val) = if delta {
                let key = get_id(kind, &mut r)?;
                let b = base.and_then(|b| b.items.get(&key)).ok_or_else(|| NetError::Decode("delta against an item the baseline lacks".into()))?;
                let sampled = base.and_then(|b| b.ticks.get(&key)).copied().unwrap_or(tick);
                (key, ItemVal::get(kind, Some(b), tick.saturating_sub(sampled), &mut r)?)
            } else {
                let v = ItemVal::get(kind, None, 0, &mut r)?;
                (v.key(), v)
            };
            state.items.insert(key, val);
            state.ticks.insert(key, tick);
            fresh.push(key);
        }

        // Commit.
        let shift = seq - self.newest;
        self.received_mask = if self.newest == 0 || shift > 32 { 0 } else { (((self.received_mask as u64) << shift) | (1u64 << (shift - 1))) as u32 };
        self.newest = seq;
        let snapshot = state.snapshot();
        self.history.push_back((seq, Box::new(state)));
        while self.history.len() > HISTORY { self.history.pop_front(); }
        Ok(Some(DecodedFrame { seq, tick, snapshot, fresh }))
    }
    /// The ack to put in the next input packet. Unit: `net-delta`.
    pub fn ack(&self) -> SnapshotAck { SnapshotAck { newest: self.newest, mask: self.received_mask } }
    /// Forget everything (new sortie). Real code.
    pub fn reset(&mut self) { self.newest = 0; self.received_mask = 0; self.history.clear(); }
}
impl Default for SnapshotDecoder { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::priority::{PriorityAccumulator, RateTuning};
    use glam::DVec3;
    use sim_core::math::{Kinematics, Pose};
    use sim_systems::protocol::snapshot::{TurretView, UncertaintyShape, VesselView};

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); ((self.0 >> 11) as f64) / ((1u64 << 53) as f64) }
    }

    /// A hull under way in a seaway: heading drifts, pitch and roll oscillate, heave bobs. Velocities are the
    /// time derivatives of the pose in the world frame (what the rigid-body solver reports). Test fixture
    /// only: the encoder sees the quantised result, so std trig here cannot affect cross-platform decoding.
    #[allow(clippy::disallowed_methods)]
    fn hull_pose(i: u32, t: f64) -> Pose {
        let speed = 4.0 + (i % 5) as f64 * 2.0;
        let h0 = i as f64 * 0.7;
        let turn = 0.01;
        let heading = h0 + t * turn;
        // Arc of a constant-rate turn: the integral of speed * (sin h, -cos h).
        let x = i as f64 * 900.0 + speed / turn * (h0.cos() - heading.cos());
        let z = -(i as f64) * 700.0 - speed / turn * (heading.sin() - h0.sin());
        let pitch = 0.03 * (t * 0.9 + i as f64).sin();
        let roll = 0.05 * (t * 0.7 + i as f64 * 1.3).sin();
        let rot = Pose::from_heading(DVec3::ZERO, heading).rot * DQuat::from_rotation_z(pitch) * DQuat::from_rotation_x(roll);
        Pose { pos: DVec3::new(x, 0.4 * (t * 1.1 + i as f64).sin(), z), rot }
    }
    fn hull(i: u32, tick: u64, own: bool) -> VesselView {
        let t = tick as f64 / 60.0;
        let h = 1e-4;
        let (a, b) = (hull_pose(i, t - h), hull_pose(i, t + h));
        let lin_vel = (b.pos - a.pos) / (2.0 * h);
        // World-frame angular velocity: 2 * vec(dq/dt * q^-1).
        let mut qb = b.rot;
        if qb.dot(a.rot) < 0.0 { qb = -qb; }
        let dq = (qb - a.rot) * (1.0 / (2.0 * h));
        let w = dq * hull_pose(i, t).rot.inverse();
        let ang_vel = DVec3::new(w.x, w.y, w.z) * 2.0;
        VesselView {
            entity: EntityId { index: i, generation: 1 },
            kin: Kinematics { pose: hull_pose(i, t), lin_vel, ang_vel },
            status: Default::default(),
            flags: Default::default(),
            turrets: vec![TurretView { slot: content::ids::PartSlot(3), train_rad: 0.5, elevation_rad: 0.1 }, TurretView { slot: content::ids::PartSlot(8), train_rad: -1.0 + (tick / 120) as f32 * 0.01, elevation_rad: 0.0 }],
            rudder: 0.1,
            power: 0.7,
            track: TrackId(if own { 0 } else { i + 100 }),
            relevance: if own { Relevance::Own } else if i < 10 { Relevance::Near } else { Relevance::Distant },
        }
    }

    fn track(id: u32, tick: u64) -> TrackView {
        let upd = tick / 6 * 6; // 10 Hz
        TrackView {
            id: TrackId(id), domain: Default::default(), stance: Default::default(), stance_reason: Default::default(), rung: Default::default(), rung_reason: Default::default(),
            grade: Default::default(), pos: DVec3::new(id as f64 * 1000.0 + upd as f64 * 0.1, 0.0, 5.0), vel: DVec3::new(6.0, 0.0, 0.0),
            shape: UncertaintyShape::Circle { radius_m: 50.0 }, last_update_tick: upd, last_known: false, class: None, candidates: 3, sources: Default::default(),
            trend: Default::default(), inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: if id.is_multiple_of(2) { Relevance::Near } else { Relevance::Distant }, struck: false,
        }
    }

    fn world(tick: u64, hulls: u32) -> Snapshot {
        let mut s = Snapshot { tick, ..Default::default() };
        s.vessels = (0..hulls).filter(|i| !(tick > 300 && tick < 900 && *i == 7)).map(|i| hull(i, tick, i == 0)).collect();
        s.tracks = (1..=12).map(|id| track(id, tick)).collect();
        s.ship = Some(ShipStatus { vessel: EntityId { index: 0, generation: 1 }, speed_kn: 12.0 + (tick / 30) as f32 * 0.1, heading_rad: 0.7, ..Default::default() });
        s.pacing.host_tick_ms = (tick / 120) as f32;
        s
    }

    /// The quantised truth: what a lossless full frame would decode to.
    fn truth(snap: &Snapshot) -> Snapshot {
        let mut e = SnapshotEncoder::new();
        let order: Vec<ItemKey> = items_of(snap).keys().copied().collect();
        let f = e.encode(snap, &order, usize::MAX / 16);
        SnapshotDecoder::new().decode(&f.bytes).unwrap().unwrap().snapshot
    }

    fn run(loss: f64, delay: u64, budget: usize, frames: u64, hulls: u32, seed: u64) -> (SnapshotEncoder, SnapshotDecoder, Vec<(u32, u32)>) {
        let mut rng = Lcg(seed);
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        let mut acc = PriorityAccumulator::new(RateTuning::default());
        let mut wire: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut acks: Vec<(u64, SnapshotAck)> = Vec::new();
        let mut last_x: BTreeMap<ItemKey, (f64, u64)> = BTreeMap::new();
        let mut vessel_bytes = Vec::new();
        for f in 0..frames {
            let tick = f * 2;
            let snap = world(tick, hulls);
            let order = acc.order(&snap);
            let out = enc.encode(&snap, &order, budget);
            acc.sent(&out.sent, tick);
            assert!(out.sent.contains(&ItemKey::Vessel(EntityId { index: 0, generation: 1 })));
            if budget < 100_000 { assert!(out.bytes.len() <= budget.max(40) + 2, "{} > {budget}", out.bytes.len()); }
            vessel_bytes.push((out.stats.vessels, out.stats.vessel_items));
            if rng.next() >= loss {
                let jitter = (rng.next() * 3.0) as u64;
                wire.push((f + delay + jitter, out.bytes));
            }
            let (now, later): (Vec<_>, Vec<_>) = wire.drain(..).partition(|(at, _)| *at <= f);
            wire = later;
            for (_, bytes) in now {
                if let Some(d) = dec.decode(&bytes).expect("frames always decode") {
                    for v in &d.snapshot.vessels {
                        // The second turret only ever trains one way: an older value would show a regression.
                        let key = ItemKey::Vessel(v.entity);
                        let train = v.turrets[1].train_rad as f64;
                        if let Some((x, _)) = last_x.get(&key) { assert!(train >= x - 1e-4, "hull {} regressed", v.entity.index); }
                        last_x.insert(key, (train, 0));
                    }
                    if let Some(ship) = &d.snapshot.ship {
                        let key = ItemKey::Section(Section::Ship);
                        if let Some((x, _)) = last_x.get(&key) { assert!(ship.speed_kn as f64 >= *x, "ship status regressed"); }
                        last_x.insert(key, (ship.speed_kn as f64, 0));
                    }
                    for t in &d.snapshot.tracks {
                        let key = ItemKey::Track(t.id);
                        if let Some((_, u)) = last_x.get(&key) { assert!(t.last_update_tick >= *u, "track {} regressed", t.id.0); }
                        last_x.insert(key, (0.0, t.last_update_tick));
                    }
                    if rng.next() >= loss { acks.push((f + delay, dec.ack())); }
                }
            }
            let (now, later): (Vec<_>, Vec<_>) = acks.drain(..).partition(|(at, _)| *at <= f);
            acks = later;
            for (_, a) in now { enc.on_ack(a); }
        }
        (enc, dec, vessel_bytes)
    }

    #[test]
    fn lossless_round_trip_is_the_quantised_truth() {
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        for f in 0..40u64 {
            let snap = world(f * 2, 6);
            let order: Vec<ItemKey> = items_of(&snap).keys().copied().collect();
            let out = enc.encode(&snap, &order, 1 << 20);
            let d = dec.decode(&out.bytes).unwrap().unwrap();
            assert_eq!(d.seq, out.seq);
            assert_eq!(d.snapshot, truth(&snap));
            enc.on_ack(dec.ack());
            // A replayed or older frame is ignored.
            assert!(dec.decode(&out.bytes).unwrap().is_none());
        }
        assert_eq!(enc.frames_behind(), 0);
    }

    #[test]
    fn loss_lag_and_deferral_never_corrupt_or_regress_and_converge() {
        let (mut enc, mut dec, _) = run(0.05, 6, 700, 1200, 20, 11);
        // Then a quiet, lossless spell with room to spare: the client converges to the exact truth.
        let mut acc = PriorityAccumulator::new(RateTuning::default());
        let snap = world(5000, 20);
        let mut last = None;
        for f in 0..120 {
            let order = acc.order(&snap);
            let out = enc.encode(&snap, &order, 1 << 16);
            acc.sent(&out.sent, 5000);
            if let Some(d) = dec.decode(&out.bytes).unwrap() { last = Some(d.snapshot); }
            if f % 3 == 0 { enc.on_ack(dec.ack()); }
        }
        assert_eq!(last.unwrap(), truth(&snap));
        // The removed and re-added hull 7 is back.
        assert!(dec.history.back().unwrap().1.items.contains_key(&ItemKey::Vessel(EntityId { index: 7, generation: 1 })));
    }

    #[test]
    fn a_change_in_a_lost_frame_still_arrives() {
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        let mut snap = world(0, 3);
        let keys: Vec<ItemKey> = items_of(&snap).keys().copied().collect();
        let f1 = enc.encode(&snap, &keys, 1 << 16);
        dec.decode(&f1.bytes).unwrap().unwrap();
        enc.on_ack(dec.ack());
        // Track 5 changes once; that frame is lost.
        snap.tracks[4].stance = content::enums::Stance::Hostile;
        let lost = enc.encode(&snap, &keys, 1 << 16);
        drop(lost);
        // Next frames do not list track 5 as due: the client still holds the old value...
        let others: Vec<ItemKey> = keys.iter().copied().filter(|k| *k != ItemKey::Track(TrackId(5))).collect();
        let f3 = enc.encode(&snap, &others, 1 << 16);
        let d3 = dec.decode(&f3.bytes).unwrap().unwrap();
        assert_ne!(d3.snapshot.tracks[4].stance, content::enums::Stance::Hostile);
        enc.on_ack(dec.ack());
        // ...and at its next due time it is written again, because the lost frame never got acked.
        let f4 = enc.encode(&snap, &keys, 1 << 16);
        assert!(f4.sent.contains(&ItemKey::Track(TrackId(5))));
        let d4 = dec.decode(&f4.bytes).unwrap().unwrap();
        assert!(d4.fresh.contains(&ItemKey::Track(TrackId(5))));
        assert_eq!(d4.snapshot.tracks[4].stance, content::enums::Stance::Hostile);
        // Unchanged and acked: due again but costs nothing.
        enc.on_ack(dec.ack());
        let f5 = enc.encode(&snap, &[ItemKey::Track(TrackId(5))], 1 << 16);
        assert_eq!(f5.sent, vec![ItemKey::Track(TrackId(5))]);
        assert!(dec.decode(&f5.bytes).unwrap().unwrap().fresh.is_empty());
    }

    #[test]
    fn reset_starts_a_clean_epoch() {
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        let snap = world(0, 5);
        let keys: Vec<ItemKey> = items_of(&snap).keys().copied().collect();
        for _ in 0..3 { let f = enc.encode(&snap, &keys, 1 << 16); dec.decode(&f.bytes).unwrap(); enc.on_ack(dec.ack()); }
        enc.reset();
        assert_eq!(enc.frames_behind(), 0);
        // New sortie with a single hull: the first post-reset frame is lost, the second still clears.
        let small = world(10, 1);
        let k2: Vec<ItemKey> = items_of(&small).keys().copied().collect();
        let _lost = enc.encode(&small, &k2, 1 << 16);
        let f = enc.encode(&small, &k2, 1 << 16);
        let d = dec.decode(&f.bytes).unwrap().unwrap();
        assert_eq!(d.snapshot, truth(&small));
    }

    #[test]
    fn vessel_updates_average_20_bytes_or_less() {
        // 41 hulls at 30 Hz with frames and acks each 5 to 7 frames in flight (a round trip of about 400 ms, so
        // baselines are old), no budget pressure (L930).
        let (_, _, bytes) = run(0.0, 5, 1 << 20, 300, 41, 3);
        let (b, n) = bytes[60..].iter().fold((0u64, 0u64), |a, &(b, n)| (a.0 + b as u64, a.1 + n as u64));
        let avg = b as f64 / n as f64;
        assert!(avg <= 20.0, "{avg:.1} bytes per vessel update");
    }

    /// A live track as the snapshot builds it: extrapolated to the frame's tick (`Track::predict`), with a
    /// filter update every 6 ticks that moves it by up to `noise_m` and its velocity by up to a tenth of that.
    fn live_track(id: u32, tick: u64, noise_m: f64) -> TrackView {
        let upd = tick / 6 * 6;
        let mut rng = Lcg(upd.wrapping_mul(31).wrapping_add(id as u64));
        let (dx, dz, dv) = ((rng.next() - 0.5) * 2.0 * noise_m, (rng.next() - 0.5) * 2.0 * noise_m, (rng.next() - 0.5) * 0.2 * noise_m);
        let vel = DVec3::new(7.5 + dv, 0.0, -3.75);
        let at_upd = DVec3::new(id as f64 * 2000.0 + upd as f64 * 0.125 + dx, 0.0, -(upd as f64) * 0.0625 + dz);
        TrackView {
            pos: at_upd + vel * ((tick - upd) as f64 / 60.0), vel, last_update_tick: upd,
            shape: UncertaintyShape::Ellipse { semi_major_m: 40.0 + (tick - upd) as f32 * 0.2, semi_minor_m: 15.0 + (tick - upd) as f32 * 0.1, bearing_rad: 0.6 },
            relevance: Relevance::Near, ..track(id, 0)
        }
    }

    #[test]
    fn prediction_runs_from_when_the_client_sampled_the_item() {
        // A track on the quantisation grid moving one unit per tick, written every third frame (frames 2 ticks
        // apart) and acked every frame, so the baseline is a frame that did not carry it. Predicting over the
        // time since the client's sample makes the write a single "unchanged" bit; predicting from the
        // baseline frame's tick would leave a 4-tick residual on every write.
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        let key = ItemKey::Track(TrackId(1));
        let at = |tick: u64| {
            let mut t = track(1, 0);
            t.pos = DVec3::new(100.0 + tick as f64 * 0.125, 0.0, 0.0);
            t.vel = DVec3::new(7.5, 0.0, 0.0);
            t.relevance = Relevance::Near;
            Snapshot { tick, tracks: vec![t], ..Default::default() }
        };
        for f in 0..30u64 {
            let snap = at(f * 2);
            let order = if f % 3 == 0 { vec![key] } else { vec![] };
            let out = enc.encode(&snap, &order, 1 << 16);
            let d = dec.decode(&out.bytes).unwrap().unwrap();
            if f % 3 == 0 {
                assert_eq!(d.snapshot.tracks[0].pos, snap.tracks[0].pos, "frame {f}");
                if f >= 6 { assert!(out.stats.tracks <= 2, "frame {f}: {} bytes for an exactly predicted track", out.stats.tracks); }
            }
            enc.on_ack(dec.ack());
        }
    }

    #[test]
    fn track_updates_stay_small() {
        // The byte budget of the dominant snapshot item in the reference soak (DESIGN.md L932): 40 live tracks
        // at 10 Hz in 30 Hz frames, metres of filter noise per update, acks 6 frames late. Before tracks were
        // predicted and quantised for what an estimate carries, the soak measured about 16.7 bytes per track
        // update; it measures about 8.4 now.
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        let mut acc = PriorityAccumulator::new(RateTuning::default());
        let mut acks: VecDeque<SnapshotAck> = VecDeque::new();
        let (mut bytes, mut items) = (0u64, 0u64);
        for f in 0..600u64 {
            let tick = f * 2;
            let snap = Snapshot { tick, tracks: (1..=40).map(|id| live_track(id, tick, 4.0)).collect(), ..Default::default() };
            let order = acc.order(&snap);
            let out = enc.encode(&snap, &order, 1 << 20);
            acc.sent(&out.sent, tick);
            let d = dec.decode(&out.bytes).unwrap().unwrap();
            for (got, want) in d.snapshot.tracks.iter().zip(&snap.tracks) {
                if d.fresh.contains(&ItemKey::Track(got.id)) { assert!((got.pos - want.pos).length() <= 0.11, "track {} off by {:.3} m", got.id.0, (got.pos - want.pos).length()); }
            }
            acks.push_back(dec.ack());
            if acks.len() > 6 { enc.on_ack(acks.pop_front().unwrap()); }
            if f >= 60 { bytes += out.stats.tracks as u64; items += out.stats.track_items as u64; }
        }
        let avg = bytes as f64 / items as f64;
        assert!(avg <= 9.0, "{avg:.2} bytes per track update");
    }

    #[test]
    fn ship_status_arrives_whole_from_its_two_sections() {
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        let mut snap = world(0, 2);
        let ship = snap.ship.as_mut().unwrap();
        ship.compartments = vec![sim_systems::protocol::snapshot::CompartmentView { id: Default::default(), fill: 9, fire: 0, breached: true, valve_open: false, sealed: false }];
        ship.board.esm_warning = true;
        // Only the fast half fits: the client shows no status rather than one with empty lists.
        let fast: Vec<ItemKey> = items_of(&snap).keys().copied().filter(|k| *k != ItemKey::Section(Section::ShipSlow)).collect();
        let d = dec.decode(&enc.encode(&snap, &fast, 1 << 16).bytes).unwrap().unwrap();
        assert_eq!(d.snapshot.ship, None);
        enc.on_ack(dec.ack());
        let d = dec.decode(&enc.encode(&snap, &[ItemKey::Section(Section::ShipSlow)], 1 << 16).bytes).unwrap().unwrap();
        assert_eq!(d.snapshot.ship, truth(&snap).ship);
        assert!(d.snapshot.ship.as_ref().unwrap().board.esm_warning);
    }

    #[test]
    fn a_new_alert_reaches_the_client_within_one_slow_section_period() {
        use sim_systems::alerts::{AlertChange, AlertManager, Condition};
        use sim_systems::protocol::alert::AlertKind;
        // A long battle's alert list (five inbound weapons a second for ten minutes, none acknowledged), then
        // 20 hulls and 12 tracks in frames held to the 256 kbps start cap with acks 6 frames late. A newly
        // raised alert must be on the client within one 2 Hz slow-section period (15 frames).
        let own = EntityId { index: 0, generation: 1 };
        let mut m = AlertManager::default();
        let mut out: Vec<AlertChange> = Vec::new();
        let hz = sim_core::TICK_HZ as u64;
        for sec in 0..600u64 {
            for sub in 0..hz {
                let conds: Vec<Condition> = (sec.saturating_sub(3) * 5..=sec * 5 + 4).map(|k| Condition { vessel: own, kind: AlertKind::WeaponInbound { track: TrackId(k as u32) } }).collect();
                m.update(&conds, sec * hz + sub, &mut out);
            }
        }
        let quiet_from = 600 * hz;
        let mut enc = SnapshotEncoder::new();
        let mut dec = SnapshotDecoder::new();
        let mut acc = PriorityAccumulator::new(RateTuning::fallback_20hz());
        let mut acks: VecDeque<SnapshotAck> = VecDeque::new();
        let budget = 256_000 / 8 / 30;
        let mut raised_at: Option<u64> = None;
        let mut seen_at: Option<u64> = None;
        for f in 0..300u64 {
            let tick = quiet_from + f * 2;
            let conds: Vec<Condition> = if f >= 150 { vec![Condition { vessel: own, kind: AlertKind::SteeringLost }] } else { vec![] };
            m.update(&conds, tick, &mut out);
            if f == 150 { raised_at = Some(f); }
            let mut snap = world(tick, 20);
            snap.alerts = m.for_vessel(own);
            let order = acc.order(&snap);
            let frame = enc.encode(&snap, &order, budget);
            acc.sent(&frame.sent, tick);
            let d = dec.decode(&frame.bytes).unwrap().unwrap();
            if seen_at.is_none() && d.snapshot.alerts.iter().any(|a| matches!(a.kind, AlertKind::SteeringLost)) { seen_at = Some(f); }
            acks.push_back(dec.ack());
            if acks.len() > 6 { enc.on_ack(acks.pop_front().unwrap()); }
        }
        let (r, s) = (raised_at.unwrap(), seen_at.expect("the new alert reached the client"));
        assert!(s - r <= 15, "new alert took {} frames", s - r);
    }

    #[test]
    fn hostile_frames_are_errors_not_panics() {
        let mut enc = SnapshotEncoder::new();
        let snap = world(0, 4);
        let keys: Vec<ItemKey> = items_of(&snap).keys().copied().collect();
        let mut dec = SnapshotDecoder::new();
        let f0 = enc.encode(&snap, &keys, 1 << 16);
        dec.decode(&f0.bytes).unwrap();
        enc.on_ack(dec.ack());
        let f1 = enc.encode(&world(2, 4), &keys, 1 << 16);
        let mut rng = Lcg(5);
        for i in 0..3000 {
            let mut d = SnapshotDecoder::new();
            d.decode(&f0.bytes).unwrap();
            let mut b = f1.bytes.clone();
            let bit = (rng.next() * (b.len() * 8) as f64) as usize;
            b[bit / 8] ^= 1 << (bit % 8);
            let cut = if i % 4 == 0 { (rng.next() * b.len() as f64) as usize } else { b.len() };
            let before = d.ack();
            if d.decode(&b[..cut]).is_err() { assert_eq!(d.ack(), before); }
        }
        // Acks for frames never sent are ignored.
        enc.on_ack(SnapshotAck { newest: 1000, mask: u32::MAX });
        assert_eq!(enc.acked.newest, 1);
    }
}
