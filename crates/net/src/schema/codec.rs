//! Unit `net-schema`. Message framing. DESIGN.md L1003 (version mismatch names both versions), L926 (compact
//! projectile events), L939 (the recorded output stream decodes identically on another OS).
//!
//! Every message except snapshot frames is bincode 1 (fixed-int little-endian, the crate default) behind a
//! size limit. The hello is the exception: [`crate::msg::HELLO_MAGIC`], protocol version as u32 LE, then
//! bincode, so a build of any version can read the first eight bytes. Event batches may use compact forms
//! per `SimEvent` variant (projectile spawns with quantised origin and velocity that still keep
//! client-integrated shells within 0.5 m of the server at impact); that choice is internal to this file.
//!
//! Event batch layout (bit packed): event count, then per event the tick as a signed delta from the previous
//! event's tick, a 2-bit form tag and the body. Forms: 0 any `SimEvent` as a compact bincode blob; 1
//! `ProjectileSpawned`; 2 `Splash`; 3 `ShellImpact`. The three projectile forms delta-code ids and positions
//! against the previous projectile event of the batch, and a spawn repeats the previous spawn's calibre,
//! band, shooter, mount, merge count and point-defence flag with one bit, so a salvo from one mount costs
//! about a dozen bytes a round. Spawn origins are quantised to 1/512 m and velocities to 1/4096 m/s: a
//! velocity error of at most 1/8192 m/s per axis is 7 mm after 60 s of flight, far inside the 0.5 m gate.
//! Splash and impact points are quantised to 1/512 m (they place effects only).
use super::bits::{BitReader, BitWriter};
use super::replicate::{get_blob, put_blob};
use crate::error::NetError;
use crate::msg::{EventBatch, Hello, WireEvent, HELLO_MAGIC};
use bincode::Options;
use glam::DVec3;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sim_core::events::PhysEvent;
use sim_core::projectiles::{ProjectileId, ProjectileSpawn};
use sim_core::EntityId;
use sim_systems::protocol::event::SimEvent;

/// Largest message accepted on `Session` or an event channel (bulk chunks are 16 KiB plus framing).
pub const MAX_MESSAGE_BYTES: u64 = 1 << 20;

/// Spawn velocity units per m/s.
pub(crate) const SPAWN_VEL_UNITS: f64 = 4096.0;
const POS_UNITS: f64 = super::POS_UNITS_PER_M;

/// Result of reading a hello.
#[derive(Clone, Debug, PartialEq)]
pub enum HelloRead {
    Ok(Hello),
    /// Header parsed but the protocol differs: the host refuses naming both (L1003).
    VersionMismatch { client: u32 },
    Malformed,
}

fn options() -> impl Options { bincode::DefaultOptions::new().with_fixint_encoding().with_little_endian().allow_trailing_bytes() }

/// Unit: `net-schema`.
pub fn encode_hello(hello: &Hello) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&HELLO_MAGIC);
    out.extend_from_slice(&hello.protocol.to_le_bytes());
    out.extend_from_slice(&encode(hello));
    out
}
/// Unit: `net-schema`.
pub fn decode_hello(bytes: &[u8], ours: u32) -> HelloRead {
    // The header is checked before the body: another version's body need not parse.
    if bytes.len() < 8 || bytes[..4] != HELLO_MAGIC { return HelloRead::Malformed; }
    let protocol = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if protocol != ours { return HelloRead::VersionMismatch { client: protocol }; }
    match decode::<Hello>(&bytes[8..]) {
        Ok(h) if h.protocol == protocol => HelloRead::Ok(h),
        _ => HelloRead::Malformed,
    }
}
/// bincode with the crate's options. Unit: `net-schema`.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    // Serialising to memory fails only for types serde cannot express (maps with non-string keys are fine in
    // bincode); every message type here is plain data, so an error is a programming bug surfaced as empty.
    options().serialize(msg).unwrap_or_default()
}
/// bincode with [`MAX_MESSAGE_BYTES`] as the limit. Unit: `net-schema`.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, NetError> {
    if bytes.len() as u64 > MAX_MESSAGE_BYTES { return Err(NetError::Decode(format!("message of {} bytes", bytes.len()))); }
    options().with_limit(MAX_MESSAGE_BYTES).deserialize(bytes).map_err(|e| NetError::Decode(e.to_string()))
}

const FORM_ANY: u64 = 0;
const FORM_SPAWN: u64 = 1;
const FORM_SPLASH: u64 = 2;
const FORM_IMPACT: u64 = 3;

/// Running context of the projectile forms within one batch.
#[derive(Default)]
struct ProjCtx { id: u32, pos: [i32; 3], vel: [i32; 3], spawn: Option<ProjectileSpawn> }

fn q3(v: DVec3, units: f64) -> [i32; 3] { [(v.x * units).round() as i32, (v.y * units).round() as i32, (v.z * units).round() as i32] }
fn dq3(q: [i32; 3], units: f64) -> DVec3 { let s = 1.0 / units; DVec3::new(q[0] as f64 * s, q[1] as f64 * s, q[2] as f64 * s) }
fn put3(v: [i32; 3], prev: [i32; 3], w: &mut BitWriter) { for i in 0..3 { w.write_svarint(v[i] as i64 - prev[i] as i64); } }
fn get3(prev: [i32; 3], r: &mut BitReader<'_>) -> Result<[i32; 3], NetError> {
    let mut out = [0; 3];
    for i in 0..3 { out[i] = i32::try_from(prev[i] as i64 + r.read_svarint()?).map_err(|_| NetError::Decode("coordinate out of range".into()))?; }
    Ok(out)
}
fn put_id(id: u32, ctx: &mut ProjCtx, w: &mut BitWriter) { w.write_svarint(id as i64 - ctx.id as i64); ctx.id = id; }
fn get_id(ctx: &mut ProjCtx, r: &mut BitReader<'_>) -> Result<u32, NetError> {
    let id = u32::try_from(ctx.id as i64 + r.read_svarint()?).map_err(|_| NetError::Decode("projectile id".into()))?;
    ctx.id = id;
    Ok(id)
}
fn same_meta(a: &ProjectileSpawn, b: &ProjectileSpawn) -> bool {
    a.calibre_mm == b.calibre_mm && a.band == b.band && a.shooter == b.shooter && a.mount == b.mount && a.rounds == b.rounds && a.point_defence == b.point_defence
}

/// Compact event batch encoding. Unit: `net-schema`.
pub fn encode_events(batch: &EventBatch) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(batch.events.len() * 16 + 4);
    w.write_varint(batch.events.len() as u64);
    let mut tick = 0u64;
    let mut ctx = ProjCtx::default();
    for e in &batch.events {
        w.write_svarint(e.tick.wrapping_sub(tick) as i64);
        tick = e.tick;
        match &e.event {
            SimEvent::Phys(PhysEvent::ProjectileSpawned(s)) => {
                w.write_bits(FORM_SPAWN, 2);
                put_id(s.id.0, &mut ctx, &mut w);
                w.write_svarint(s.tick.wrapping_sub(e.tick) as i64);
                let pos = q3(s.origin, POS_UNITS);
                let vel = q3(s.velocity, SPAWN_VEL_UNITS);
                put3(pos, ctx.pos, &mut w);
                put3(vel, ctx.vel, &mut w);
                ctx.pos = pos;
                ctx.vel = vel;
                let repeat = ctx.spawn.as_ref().is_some_and(|p| same_meta(p, s));
                w.write_bool(repeat);
                if !repeat {
                    w.write_varint(s.calibre_mm as u64);
                    put_blob(&s.band, &mut w);
                    w.write_varint(s.shooter.to_u64());
                    w.write_varint(s.mount.0 as u64);
                    w.write_bits(s.rounds as u64, 8);
                    w.write_bool(s.point_defence);
                }
                ctx.spawn = Some(*s);
            }
            SimEvent::Phys(PhysEvent::Splash { projectile, pos, calibre_mm, shooter }) => {
                w.write_bits(FORM_SPLASH, 2);
                put_id(projectile.0, &mut ctx, &mut w);
                let p = q3(*pos, POS_UNITS);
                put3(p, ctx.pos, &mut w);
                ctx.pos = p;
                w.write_varint(*calibre_mm as u64);
                w.write_varint(shooter.to_u64());
            }
            SimEvent::Phys(PhysEvent::ShellImpact { projectile, vessel, cell, outcome, world_point, shooter, burst }) => {
                w.write_bits(FORM_IMPACT, 2);
                put_id(projectile.0, &mut ctx, &mut w);
                let p = q3(*world_point, POS_UNITS);
                put3(p, ctx.pos, &mut w);
                ctx.pos = p;
                w.write_varint(vessel.to_u64());
                w.write_varint(cell.0 as u64);
                put_blob(outcome, &mut w);
                w.write_varint(shooter.to_u64());
                w.write_bool(*burst);
            }
            other => {
                w.write_bits(FORM_ANY, 2);
                put_blob(other, &mut w);
            }
        }
    }
    w.finish()
}
/// Unit: `net-schema`.
pub fn decode_events(bytes: &[u8]) -> Result<EventBatch, NetError> {
    if bytes.len() as u64 > MAX_MESSAGE_BYTES { return Err(NetError::Decode(format!("event batch of {} bytes", bytes.len()))); }
    let mut r = BitReader::new(bytes);
    // An event takes at least 8 (tick) + 2 (form) + 8 bits.
    let n = r.read_count(18)?;
    let mut events = Vec::with_capacity(n);
    let mut tick = 0u64;
    let mut ctx = ProjCtx::default();
    for _ in 0..n {
        tick = tick.wrapping_add(r.read_svarint()? as u64);
        let event = match r.read_bits(2)? {
            FORM_SPAWN => {
                let id = get_id(&mut ctx, &mut r)?;
                let s_tick = tick.wrapping_add(r.read_svarint()? as u64);
                let pos = get3(ctx.pos, &mut r)?;
                let vel = get3(ctx.vel, &mut r)?;
                ctx.pos = pos;
                ctx.vel = vel;
                let repeat = r.read_bool()?;
                let mut s = if repeat {
                    ctx.spawn.ok_or_else(|| NetError::Decode("repeated spawn without a previous one".into()))?
                } else {
                    let calibre_mm = u16::try_from(r.read_varint()?).map_err(|_| NetError::Decode("calibre".into()))?;
                    let band = get_blob(&mut r)?;
                    let shooter = EntityId::from_u64(r.read_varint()?);
                    let mount = content::ids::PartSlot(u16::try_from(r.read_varint()?).map_err(|_| NetError::Decode("mount".into()))?);
                    let rounds = r.read_bits(8)? as u8;
                    let point_defence = r.read_bool()?;
                    ProjectileSpawn { id: ProjectileId(0), tick: 0, origin: DVec3::ZERO, velocity: DVec3::ZERO, calibre_mm, band, shooter, mount, rounds, point_defence }
                };
                s.id = ProjectileId(id);
                s.tick = s_tick;
                s.origin = dq3(pos, POS_UNITS);
                s.velocity = dq3(vel, SPAWN_VEL_UNITS);
                ctx.spawn = Some(s);
                SimEvent::Phys(PhysEvent::ProjectileSpawned(s))
            }
            FORM_SPLASH => {
                let id = get_id(&mut ctx, &mut r)?;
                let p = get3(ctx.pos, &mut r)?;
                ctx.pos = p;
                let calibre_mm = u16::try_from(r.read_varint()?).map_err(|_| NetError::Decode("calibre".into()))?;
                let shooter = EntityId::from_u64(r.read_varint()?);
                SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(id), pos: dq3(p, POS_UNITS), calibre_mm, shooter })
            }
            FORM_IMPACT => {
                let id = get_id(&mut ctx, &mut r)?;
                let p = get3(ctx.pos, &mut r)?;
                ctx.pos = p;
                let vessel = EntityId::from_u64(r.read_varint()?);
                let cell = content::ids::CellIndex(r.read_varint_u32()?);
                let outcome = get_blob(&mut r)?;
                let shooter = EntityId::from_u64(r.read_varint()?);
                let burst = r.read_bool()?;
                SimEvent::Phys(PhysEvent::ShellImpact { projectile: ProjectileId(id), vessel, cell, outcome, world_point: dq3(p, POS_UNITS), shooter, burst })
            }
            _ => get_blob::<SimEvent>(&mut r)?,
        };
        events.push(WireEvent { tick, event });
    }
    Ok(EventBatch { events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{BuildKind, ClientMsg, InputPacket, SnapshotAck};
    use content::part::weapons::CalibreBand;
    use sim_core::armour::HitOutcome;

    fn hello(protocol: u32) -> Hello { Hello { protocol, game_version: "0.1.0".into(), build: BuildKind::GitHub, client_id: 0xABCD_EF01_2345_6789, name: "Skipper".into() } }

    #[test]
    fn hello_header_is_frozen_and_checked_first() {
        let bytes = encode_hello(&hello(7));
        assert_eq!(&bytes[..4], b"BPNH");
        assert_eq!(&bytes[4..8], &7u32.to_le_bytes());
        assert_eq!(decode_hello(&bytes, 7), HelloRead::Ok(hello(7)));
        assert_eq!(decode_hello(&bytes, 8), HelloRead::VersionMismatch { client: 7 });
        // A future build's body may not parse; the header alone must still name its version.
        let mut future = bytes[..8].to_vec();
        future[4..8].copy_from_slice(&99u32.to_le_bytes());
        future.extend_from_slice(&[0xff; 3]);
        assert_eq!(decode_hello(&future, 7), HelloRead::VersionMismatch { client: 99 });
        assert_eq!(decode_hello(&bytes[..6], 7), HelloRead::Malformed);
        assert_eq!(decode_hello(&bytes[..12], 7), HelloRead::Malformed);
        assert_eq!(decode_hello(b"XXXX\x07\0\0\0", 7), HelloRead::Malformed);
    }

    #[test]
    fn codec_is_plain_bincode_with_a_limit() {
        let msg = InputPacket { ack: SnapshotAck { newest: 5, mask: 3 }, inputs: vec![] };
        assert_eq!(encode(&msg), bincode::serialize(&msg).unwrap());
        assert_eq!(decode::<InputPacket>(&encode(&msg)).unwrap(), msg);
        let leave = ClientMsg::Leave;
        assert_eq!(decode::<ClientMsg>(&encode(&leave)).unwrap(), leave);
        // A length prefix claiming a huge vec must fail, not allocate.
        let mut evil = vec![0u8; 8];
        evil.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode::<InputPacket>(&evil).is_err());
        assert!(decode::<InputPacket>(&[1, 2]).is_err());
    }

    fn spawn(id: u32, x: f64, vx: f64) -> WireEvent {
        WireEvent { tick: 100, event: SimEvent::Phys(PhysEvent::ProjectileSpawned(ProjectileSpawn {
            id: ProjectileId(id), tick: 100, origin: DVec3::new(x, 12.3456, -88.1), velocity: DVec3::new(vx, 120.123456, -400.0001),
            calibre_mm: 127, band: CalibreBand::Large, shooter: EntityId { index: 3, generation: 1 }, mount: content::ids::PartSlot(4), rounds: 1, point_defence: false,
        })) }
    }

    #[test]
    fn events_round_trip_and_salvos_are_compact() {
        let mut events = vec![
            WireEvent { tick: 90, event: SimEvent::SortieStarted { seed: 42, tick_hz: 60 } },
            WireEvent { tick: 95, event: SimEvent::EntityHidden { entity: EntityId { index: 9, generation: 2 } } },
        ];
        for k in 0..8 { events.push(spawn(1000 + k, 100.0 + k as f64 * 0.01, 600.0 + k as f64 * 0.37)); }
        events.push(WireEvent { tick: 160, event: SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(1003), pos: DVec3::new(5000.2, 0.0, -10.0), calibre_mm: 127, shooter: EntityId { index: 3, generation: 1 } }) });
        events.push(WireEvent { tick: 161, event: SimEvent::Phys(PhysEvent::ShellImpact { projectile: ProjectileId(1004), vessel: EntityId { index: 8, generation: 1 }, cell: content::ids::CellIndex(777), outcome: HitOutcome::Penetrated, world_point: DVec3::new(5001.0, 2.0, -11.0), shooter: EntityId { index: 3, generation: 1 }, burst: true }) });
        let batch = EventBatch { events };
        let bytes = encode_events(&batch);
        let back = decode_events(&bytes).unwrap();
        assert_eq!(back.events.len(), batch.events.len());
        for (a, b) in batch.events.iter().zip(&back.events) {
            assert_eq!(a.tick, b.tick);
            match (&a.event, &b.event) {
                (SimEvent::Phys(PhysEvent::ProjectileSpawned(x)), SimEvent::Phys(PhysEvent::ProjectileSpawned(y))) => {
                    assert_eq!((x.id, x.tick, x.calibre_mm, x.band, x.shooter, x.mount, x.rounds), (y.id, y.tick, y.calibre_mm, y.band, y.shooter, y.mount, y.rounds));
                    assert!((x.origin - y.origin).abs().max_element() <= 0.5 / 512.0);
                    // Worst-case drift over a 60 s flight stays far under the 0.5 m gate.
                    assert!((x.velocity - y.velocity).abs().max_element() * 60.0 < 0.01);
                }
                (SimEvent::Phys(PhysEvent::Splash { pos: p, projectile: i, .. }), SimEvent::Phys(PhysEvent::Splash { pos: q, projectile: j, .. })) => { assert_eq!(i, j); assert!((*p - *q).length() < 0.01); }
                (SimEvent::Phys(PhysEvent::ShellImpact { world_point: p, cell: c, outcome: o, .. }), SimEvent::Phys(PhysEvent::ShellImpact { world_point: q, cell: d, outcome: u, .. })) => { assert_eq!((c, o), (d, u)); assert!((*p - *q).length() < 0.01); }
                (x, y) => assert_eq!(x, y),
            }
        }
        // The salvo of 8 spawns costs far less than its bincode form.
        let salvo: usize = encode_events(&EventBatch { events: batch.events[2..10].to_vec() }).len();
        assert!(salvo < 8 * 20, "{salvo} bytes for 8 rounds");
        // Truncations and bit flips are errors, never panics.
        for cut in 0..bytes.len() { let _ = decode_events(&bytes[..cut]); }
        for bit in 0..bytes.len() * 8 { let mut b = bytes.clone(); b[bit / 8] ^= 1 << (bit % 8); let _ = decode_events(&b); }
        assert!(decode_events(&[0xff, 0xff, 0xff, 0x0f]).is_err());
    }
}
