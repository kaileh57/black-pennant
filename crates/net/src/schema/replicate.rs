//! Unit `net-schema`. The generated delta coding (L920-922). One declarative macro, `replicate!`, takes an
//! item's field list with a coder per field (fixed bits, signed varint delta against the base, enum index,
//! small vec) and emits the [`Replicate`] impl: a change mask with one bit per field, then only the changed
//! fields, signed deltas against the base where the coder says so. `WireVessel`, `WireBody`, `WireDecoy` and
//! `QuantPose` are declared through it; nothing in `delta` writes fields by hand. Target: 20 bytes or less per
//! vessel update on average (L930) with ships near rest compressing to a few bytes.
//!
//! Coding with a base: one "any change" bit (a resting item costs one bit), then the per-field mask, then the
//! changed fields in declaration order. Without a base every field is written in full. The macro lists every
//! field of the struct literal it builds, so adding a field to a wire item without declaring its coder is a
//! compile error: no field can silently stop replicating.
//!
//! The same macro also declares the snapshot's non-pose items (tracks, the own ship status, the Bridge Board
//! and pacing), so `delta` sends their changed fields rather than whole values.
use super::bits::small_bits;
use super::quant::{dequantize_pos, dequantize_vec, pack_rot, quantize_pos, quantize_vec, unpack_rot};
use super::{BitReader, BitWriter, QuantPose, Replicate, WireBody, WireDecoy, WireTurret, WireVessel, LIN_VEL_UNITS_PER_MPS, QUAT_COMPONENT_BITS};
use crate::error::NetError;
use bincode::Options;
use content::ids::PartSlot;
use glam::DVec3;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sim_systems::protocol::snapshot::{BridgeBoard, Pacing, ShipStatus, TrackView};

/// Largest serde blob inside a frame or event batch.
pub(crate) const MAX_BLOB_BYTES: u64 = 1 << 20;
const QUAT_MAX: u32 = (1u32 << QUAT_COMPONENT_BITS) - 1;
const ROT_BITS: u32 = 2 + 3 * QUAT_COMPONENT_BITS;

/// Equality as the receiver sees it: two values that encode identically compare equal.
pub(crate) trait WireEq { fn wire_eq(&self, other: &Self) -> bool; }

/// How one field is written. Implemented by zero-sized coder types for the field types they handle.
pub(crate) trait Coder<T> {
    fn same(cur: &T, base: &T) -> bool;
    fn put(cur: &T, base: Option<&T>, w: &mut BitWriter);
    fn get(base: Option<&T>, r: &mut BitReader<'_>) -> Result<T, NetError>;
}

/// Fixed-width raw bits.
pub(crate) struct Raw;
/// Unsigned varint, always the full value.
pub(crate) struct UVar;
/// Signed varint delta against the base (wrapping), full value without one.
pub(crate) struct SVar;
/// A residual against a predicted base: like [`SVar`], but the delta is a 3-bit-group varint, because a
/// prediction leaves residuals of a few quantisation steps (L930's 20-byte average depends on it).
pub(crate) struct Resid;
/// `f32` as its 32 raw bits; equality is bit equality.
pub(crate) struct F32;
/// Any serde type as a length-prefixed compact bincode blob (enum index, small structs).
pub(crate) struct Serde;
/// A nested `Replicate` item, delta coded against the base's copy.
pub(crate) struct Sub;
/// An optional nested item.
pub(crate) struct OptSub;
/// A list of serde elements, delta coded element by element against the base list.
pub(crate) struct Elems;
/// Turret angle list: per-turret wrapping deltas when the base has the same slots.
pub(crate) struct Turrets;
/// Smallest-three rotation bits: component deltas when the dropped index is unchanged.
pub(crate) struct Rot;
/// World position in metres, quantised to 1/512 m.
pub(crate) struct Pos;
/// Linear velocity in m/s, quantised to 1/256 m/s.
pub(crate) struct Vel;

fn bad(what: &str) -> NetError { NetError::Decode(format!("bad {what}")) }

macro_rules! raw_coder {
    ($t:ty, $bits:expr, $to:expr, $from:expr) => {
        impl Coder<$t> for Raw {
            fn same(cur: &$t, base: &$t) -> bool { cur == base }
            fn put(cur: &$t, _: Option<&$t>, w: &mut BitWriter) { w.write_bits(($to)(*cur), $bits) }
            fn get(_: Option<&$t>, r: &mut BitReader<'_>) -> Result<$t, NetError> { Ok(($from)(r.read_bits($bits)?)) }
        }
    };
}
raw_coder!(bool, 1, |v: bool| v as u64, |v: u64| v != 0);
raw_coder!(u8, 8, |v: u8| v as u64, |v: u64| v as u8);
raw_coder!(i8, 8, |v: i8| v as u8 as u64, |v: u64| v as u8 as i8);
raw_coder!(u16, 16, |v: u16| v as u64, |v: u64| v as u16);

macro_rules! uvar_coder {
    ($t:ty) => {
        impl Coder<$t> for UVar {
            fn same(cur: &$t, base: &$t) -> bool { cur == base }
            fn put(cur: &$t, _: Option<&$t>, w: &mut BitWriter) { w.write_varint(*cur as u64) }
            fn get(_: Option<&$t>, r: &mut BitReader<'_>) -> Result<$t, NetError> {
                <$t>::try_from(r.read_varint()?).map_err(|_| bad(stringify!($t)))
            }
        }
    };
}
uvar_coder!(u16);
uvar_coder!(u32);
uvar_coder!(u64);

impl Coder<i32> for SVar {
    fn same(cur: &i32, base: &i32) -> bool { cur == base }
    fn put(cur: &i32, base: Option<&i32>, w: &mut BitWriter) { w.write_svarint(*cur as i64 - base.copied().unwrap_or(0) as i64) }
    fn get(base: Option<&i32>, r: &mut BitReader<'_>) -> Result<i32, NetError> {
        let v = base.copied().unwrap_or(0) as i64 + r.read_svarint()?;
        i32::try_from(v).map_err(|_| bad("i32 delta"))
    }
}
impl Coder<[i32; 3]> for SVar {
    fn same(cur: &[i32; 3], base: &[i32; 3]) -> bool { cur == base }
    fn put(cur: &[i32; 3], base: Option<&[i32; 3]>, w: &mut BitWriter) {
        for i in 0..3 { <SVar as Coder<i32>>::put(&cur[i], base.map(|b| &b[i]), w); }
    }
    fn get(base: Option<&[i32; 3]>, r: &mut BitReader<'_>) -> Result<[i32; 3], NetError> {
        let mut out = [0; 3];
        for (i, o) in out.iter_mut().enumerate() { *o = <SVar as Coder<i32>>::get(base.map(|b| &b[i]), r)?; }
        Ok(out)
    }
}
impl Coder<[i32; 3]> for Resid {
    fn same(cur: &[i32; 3], base: &[i32; 3]) -> bool { cur == base }
    fn put(cur: &[i32; 3], base: Option<&[i32; 3]>, w: &mut BitWriter) {
        match base {
            Some(b) => for i in 0..3 { w.write_small(cur[i] as i64 - b[i] as i64) },
            None => <SVar as Coder<[i32; 3]>>::put(cur, None, w),
        }
    }
    fn get(base: Option<&[i32; 3]>, r: &mut BitReader<'_>) -> Result<[i32; 3], NetError> {
        let Some(b) = base else { return <SVar as Coder<[i32; 3]>>::get(None, r) };
        let mut out = [0; 3];
        for (i, o) in out.iter_mut().enumerate() {
            *o = i32::try_from(b[i] as i64 + r.read_small()?).map_err(|_| bad("i32 residual"))?;
        }
        Ok(out)
    }
}
impl Coder<u64> for SVar {
    fn same(cur: &u64, base: &u64) -> bool { cur == base }
    fn put(cur: &u64, base: Option<&u64>, w: &mut BitWriter) {
        match base { Some(b) => w.write_svarint(cur.wrapping_sub(*b) as i64), None => w.write_varint(*cur) }
    }
    fn get(base: Option<&u64>, r: &mut BitReader<'_>) -> Result<u64, NetError> {
        Ok(match base { Some(b) => b.wrapping_add(r.read_svarint()? as u64), None => r.read_varint()? })
    }
}

impl Coder<f32> for F32 {
    fn same(cur: &f32, base: &f32) -> bool { cur.to_bits() == base.to_bits() }
    fn put(cur: &f32, _: Option<&f32>, w: &mut BitWriter) { w.write_bits(cur.to_bits() as u64, 32) }
    fn get(_: Option<&f32>, r: &mut BitReader<'_>) -> Result<f32, NetError> { Ok(f32::from_bits(r.read_bits(32)? as u32)) }
}

/// Compact bincode (varint integers, enum index as a varint) used for blobs inside frames and event batches.
pub(crate) fn blob_options() -> impl Options {
    bincode::DefaultOptions::new().with_varint_encoding().with_little_endian().reject_trailing_bytes().with_limit(MAX_BLOB_BYTES)
}
pub(crate) fn put_blob<T: Serialize>(v: &T, w: &mut BitWriter) {
    // Serialising plain data to memory cannot fail except past the size limit, which no snapshot value
    // reaches; an empty blob then decodes as an error on the far side instead of desynchronising silently.
    let bytes = blob_options().serialize(v).unwrap_or_default();
    w.write_varint(bytes.len() as u64);
    put_bytes(&bytes, w);
}
pub(crate) fn get_blob<T: DeserializeOwned>(r: &mut BitReader<'_>) -> Result<T, NetError> {
    let bytes = get_bytes(r)?;
    blob_options().deserialize(&bytes).map_err(|e| NetError::Decode(format!("blob: {e}")))
}
pub(crate) fn put_bytes(bytes: &[u8], w: &mut BitWriter) {
    let tail = BitWriter { bytes: bytes.to_vec(), bit: 0 };
    w.append(&tail);
}
pub(crate) fn get_bytes(r: &mut BitReader<'_>) -> Result<Vec<u8>, NetError> {
    let n = r.read_count(8)?;
    if n as u64 > MAX_BLOB_BYTES { return Err(bad("blob length")); }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n { out.push(r.read_bits(8)? as u8); }
    Ok(out)
}

impl<T: Serialize + DeserializeOwned + PartialEq + Clone> Coder<T> for Serde {
    fn same(cur: &T, base: &T) -> bool { cur == base }
    fn put(cur: &T, _: Option<&T>, w: &mut BitWriter) { put_blob(cur, w) }
    fn get(_: Option<&T>, r: &mut BitReader<'_>) -> Result<T, NetError> { get_blob(r) }
}

impl<T: Replicate + WireEq + Clone> Coder<T> for Sub {
    fn same(cur: &T, base: &T) -> bool { cur.wire_eq(base) }
    fn put(cur: &T, base: Option<&T>, w: &mut BitWriter) { cur.write(base, w) }
    fn get(base: Option<&T>, r: &mut BitReader<'_>) -> Result<T, NetError> { T::read(base, r) }
}

impl<T: Replicate + WireEq + Clone> Coder<Option<T>> for OptSub {
    fn same(cur: &Option<T>, base: &Option<T>) -> bool {
        match (cur, base) { (Some(a), Some(b)) => a.wire_eq(b), (None, None) => true, _ => false }
    }
    fn put(cur: &Option<T>, base: Option<&Option<T>>, w: &mut BitWriter) {
        w.write_bool(cur.is_some());
        if let Some(c) = cur { c.write(base.and_then(|b| b.as_ref()), w); }
    }
    fn get(base: Option<&Option<T>>, r: &mut BitReader<'_>) -> Result<Option<T>, NetError> {
        if !r.read_bool()? { return Ok(None); }
        Ok(Some(T::read(base.and_then(|b| b.as_ref()), r)?))
    }
}

impl<T: Serialize + DeserializeOwned + PartialEq + Clone> Coder<Vec<T>> for Elems {
    fn same(cur: &Vec<T>, base: &Vec<T>) -> bool { cur == base }
    fn put(cur: &Vec<T>, base: Option<&Vec<T>>, w: &mut BitWriter) {
        w.write_varint(cur.len() as u64);
        let empty = Vec::new();
        let b = base.unwrap_or(&empty);
        for (i, e) in cur.iter().enumerate() {
            match b.get(i) {
                Some(be) => { let changed = e != be; w.write_bool(changed); if changed { put_blob(e, w); } }
                None => put_blob(e, w),
            }
        }
    }
    fn get(base: Option<&Vec<T>>, r: &mut BitReader<'_>) -> Result<Vec<T>, NetError> {
        let n = r.read_count(1)?;
        let empty = Vec::new();
        let b = base.unwrap_or(&empty);
        let mut out = Vec::with_capacity(n.min(4096));
        for i in 0..n {
            match b.get(i) {
                Some(be) => out.push(if r.read_bool()? { get_blob(r)? } else { be.clone() }),
                None => out.push(get_blob(r)?),
            }
        }
        Ok(out)
    }
}

impl Coder<Vec<WireTurret>> for Turrets {
    fn same(cur: &Vec<WireTurret>, base: &Vec<WireTurret>) -> bool { cur == base }
    fn put(cur: &Vec<WireTurret>, base: Option<&Vec<WireTurret>>, w: &mut BitWriter) {
        if let Some(b) = base {
            let same_slots = b.len() == cur.len() && b.iter().zip(cur).all(|(x, y)| x.slot == y.slot);
            w.write_bool(same_slots);
            if same_slots {
                for (t, bt) in cur.iter().zip(b) {
                    let changed = t != bt;
                    w.write_bool(changed);
                    if changed {
                        w.write_small(t.train.wrapping_sub(bt.train) as i16 as i64);
                        w.write_small(t.elevation.wrapping_sub(bt.elevation) as i16 as i64);
                    }
                }
                return;
            }
        }
        w.write_varint(cur.len() as u64);
        for t in cur {
            w.write_varint(t.slot.0 as u64);
            w.write_bits(t.train as u64, 16);
            w.write_bits(t.elevation as u64, 16);
        }
    }
    fn get(base: Option<&Vec<WireTurret>>, r: &mut BitReader<'_>) -> Result<Vec<WireTurret>, NetError> {
        if let Some(b) = base {
            if r.read_bool()? {
                let mut out = b.clone();
                for t in out.iter_mut() {
                    if r.read_bool()? {
                        let dt = i16::try_from(r.read_small()?).map_err(|_| bad("turret delta"))?;
                        let de = i16::try_from(r.read_small()?).map_err(|_| bad("turret delta"))?;
                        t.train = t.train.wrapping_add(dt as u16);
                        t.elevation = t.elevation.wrapping_add(de as u16);
                    }
                }
                return Ok(out);
            }
        }
        let n = r.read_count(40)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let slot = u16::try_from(r.read_varint()?).map_err(|_| bad("turret slot"))?;
            out.push(WireTurret { slot: PartSlot(slot), train: r.read_bits(16)? as u16, elevation: r.read_bits(16)? as u16 });
        }
        Ok(out)
    }
}

impl Coder<u64> for Rot {
    fn same(cur: &u64, base: &u64) -> bool { cur == base }
    fn put(cur: &u64, base: Option<&u64>, w: &mut BitWriter) {
        if let Some(b) = base {
            let (bi, bs) = unpack_rot(*b);
            let (ci, cs) = unpack_rot(*cur);
            let d = [cs[0] as i64 - bs[0] as i64, cs[1] as i64 - bs[1] as i64, cs[2] as i64 - bs[2] as i64];
            let delta_ok = bi == ci && d.iter().map(|&x| small_bits(x)).sum::<usize>() < ROT_BITS as usize;
            w.write_bool(delta_ok);
            if delta_ok {
                for x in d { w.write_small(x); }
                return;
            }
        }
        w.write_bits(*cur, ROT_BITS);
    }
    fn get(base: Option<&u64>, r: &mut BitReader<'_>) -> Result<u64, NetError> {
        if let Some(b) = base {
            if r.read_bool()? {
                let (bi, bs) = unpack_rot(*b);
                let mut s = [0u32; 3];
                for k in 0..3 {
                    let v = bs[k] as i64 + r.read_small()?;
                    if !(0..=QUAT_MAX as i64).contains(&v) { return Err(bad("rotation delta")); }
                    s[k] = v as u32;
                }
                return Ok(pack_rot(bi, s));
            }
        }
        r.read_bits(ROT_BITS)
    }
}

macro_rules! vec3_coder {
    ($name:ty, $q:expr, $dq:expr) => {
        impl Coder<DVec3> for $name {
            fn same(cur: &DVec3, base: &DVec3) -> bool { ($q)(*cur) == ($q)(*base) }
            fn put(cur: &DVec3, base: Option<&DVec3>, w: &mut BitWriter) {
                let b = base.map(|b| ($q)(*b));
                <SVar as Coder<[i32; 3]>>::put(&($q)(*cur), b.as_ref(), w)
            }
            fn get(base: Option<&DVec3>, r: &mut BitReader<'_>) -> Result<DVec3, NetError> {
                let b = base.map(|b| ($q)(*b));
                Ok(($dq)(<SVar as Coder<[i32; 3]>>::get(b.as_ref(), r)?))
            }
        }
    };
}
vec3_coder!(Pos, quantize_pos, dequantize_pos);
vec3_coder!(Vel, |v| quantize_vec(v, LIN_VEL_UNITS_PER_MPS), |q| dequantize_vec(q, LIN_VEL_UNITS_PER_MPS));

/// Declare one replicated item: `replicate!(Type { field: Coder, ... })` with every field of `Type`.
macro_rules! replicate {
    ($ty:ty { $($f:ident : $c:ty),+ $(,)? }) => {
        impl WireEq for $ty {
            fn wire_eq(&self, o: &Self) -> bool { true $(&& <$c as Coder<_>>::same(&self.$f, &o.$f))+ }
        }
        impl Replicate for $ty {
            fn write(&self, base: Option<&Self>, w: &mut BitWriter) {
                match base {
                    None => { $( <$c as Coder<_>>::put(&self.$f, None, w); )+ }
                    Some(b) => {
                        let any = !self.wire_eq(b);
                        w.write_bool(any);
                        if !any { return; }
                        $( w.write_bool(!<$c as Coder<_>>::same(&self.$f, &b.$f)); )+
                        $( if !<$c as Coder<_>>::same(&self.$f, &b.$f) { <$c as Coder<_>>::put(&self.$f, Some(&b.$f), w); } )+
                    }
                }
            }
            fn read(base: Option<&Self>, r: &mut BitReader<'_>) -> Result<Self, NetError> {
                match base {
                    None => Ok(Self { $( $f: <$c as Coder<_>>::get(None, r)?, )+ }),
                    Some(b) => {
                        if !r.read_bool()? { return Ok(b.clone()); }
                        $( let $f = r.read_bool()?; )+
                        Ok(Self { $( $f: if $f { <$c as Coder<_>>::get(Some(&b.$f), r)? } else { b.$f.clone() }, )+ })
                    }
                }
            }
        }
    };
}

replicate!(QuantPose { pos: Resid, rot: Rot });

replicate!(WireVessel {
    entity: UVar,
    pose: Sub,
    lin_vel: Resid,
    ang_vel: Resid,
    status: Serde,
    flags: Serde,
    turrets: Turrets,
    rudder: Raw,
    power: Raw,
    track: UVar,
    relevance: Serde,
});

replicate!(WireBody {
    entity: UVar,
    kind: Serde,
    side: Serde,
    pos: Resid,
    vel: Resid,
    phase: Serde,
    seeker: Serde,
    track: UVar,
    relevance: Serde,
});

replicate!(WireDecoy { entity: UVar, kind: Serde, owner: UVar, pos: Resid, age_ds: UVar });

replicate!(TrackView {
    id: Serde,
    domain: Serde,
    stance: Serde,
    stance_reason: Serde,
    rung: Serde,
    rung_reason: Serde,
    grade: Serde,
    pos: Pos,
    vel: Vel,
    shape: Serde,
    last_update_tick: SVar,
    last_known: Raw,
    class: Serde,
    candidates: UVar,
    sources: Serde,
    trend: Serde,
    inbound: Raw,
    engaged_by: Raw,
    envelope_m: F32,
    emitting: Raw,
    relevance: Serde,
    struck: Raw,
});

replicate!(BridgeBoard { officers: Elems, radiating: Serde, esm_warning: Raw, emcon: Serde, radio: Serde });

replicate!(ShipStatus {
    vessel: Serde,
    speed_kn: F32,
    heading_rad: F32,
    heel_deg: F32,
    trim_deg: F32,
    ordered_speed_kn: F32,
    ordered_heading_rad: F32,
    route: Serde,
    designated: Serde,
    power: Serde,
    parts: Elems,
    mounts: Elems,
    compartments: Elems,
    crew: Serde,
    board: Sub,
    held: Serde,
    offboard: Elems,
    actions: Elems,
    longest_envelope_m: F32,
    cells_lost: F32,
});

replicate!(Pacing {
    paused: Raw,
    requested: Serde,
    effective: Serde,
    blocked: Serde,
    time_scale: F32,
    rung: Raw,
    sub_step: Raw,
    host_tick_ms: F32,
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::quant::quantize_rot;
    use glam::DQuat;
    use sim_core::vessel::VesselStatus;
    use sim_systems::protocol::snapshot::{Relevance, VesselFlags};

    fn vessel() -> WireVessel {
        WireVessel {
            entity: (3u64 << 32) | 17,
            pose: QuantPose { pos: [512_000, 100, -77_000], rot: quantize_rot(DQuat::from_rotation_y(0.7)) },
            lin_vel: [1280, 10, -600],
            ang_vel: [5, -3, 20],
            status: VesselStatus::Active,
            flags: VesselFlags::LIGHTS,
            turrets: vec![WireTurret { slot: PartSlot(2), train: 1000, elevation: 200 }, WireTurret { slot: PartSlot(9), train: 65_000, elevation: 10 }],
            rudder: -20,
            power: 200,
            track: 0,
            relevance: Relevance::Own,
        }
    }

    fn round<T: Replicate + PartialEq + std::fmt::Debug>(v: &T, base: Option<&T>) -> usize {
        let mut w = BitWriter::new();
        v.write(base, &mut w);
        let bits = w.bit_len();
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(&T::read(base, &mut r).unwrap(), v);
        bits
    }

    #[test]
    fn vessel_full_and_delta_round_trip() {
        let a = vessel();
        round(&a, None);
        assert_eq!(round(&a, Some(&a)), 1, "an unchanged item is one bit");
        let mut b = a.clone();
        b.pose.pos[0] += 170;
        b.pose.pos[2] -= 3;
        b.pose.rot = quantize_rot(DQuat::from_rotation_y(0.701) * DQuat::from_rotation_x(0.01));
        b.lin_vel[1] += 4;
        b.turrets[1].train = 20; // wraps through zero
        b.rudder = 127;
        b.status = VesselStatus::Disabled;
        let bits = round(&b, Some(&a));
        assert!(bits < 20 * 8, "{bits} bits");
        // Different turret layout falls back to a full list.
        let mut c = b.clone();
        c.turrets.pop();
        round(&c, Some(&a));
    }

    #[test]
    fn bodies_decoys_tracks_round_trip() {
        let body = WireBody { entity: 99, pos: [1, 2, 3], vel: [76_800, 0, -5], track: 4, ..Default::default() };
        round(&body, None);
        let mut b2 = body;
        b2.pos[0] += 2560;
        round(&b2, Some(&body));
        let decoy = WireDecoy { entity: 5, kind: content::part::DecoyKind::Flare, owner: 17, pos: [9, 9, 9], age_ds: 12 };
        round(&decoy, None);
        let mut d2 = decoy;
        d2.age_ds = 13;
        round(&d2, Some(&decoy));
    }

    #[test]
    fn hostile_bytes_never_panic() {
        let a = vessel();
        let mut w = BitWriter::new();
        a.write(None, &mut w);
        let full = w.finish();
        let mut s = 1u64;
        for i in 0..4000 {
            let mut bytes = full.clone();
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            let flip = (s >> 33) as usize % (bytes.len() * 8);
            bytes[flip / 8] ^= 1 << (flip % 8);
            let cut = if i % 3 == 0 { (s >> 20) as usize % bytes.len() } else { bytes.len() };
            let _ = WireVessel::read(None, &mut BitReader::new(&bytes[..cut]));
            let _ = WireVessel::read(Some(&a), &mut BitReader::new(&bytes[..cut]));
            let _ = TrackView::read(None, &mut BitReader::new(&bytes[..cut]));
            let _ = ShipStatus::read(None, &mut BitReader::new(&bytes[..cut]));
        }
    }
}
