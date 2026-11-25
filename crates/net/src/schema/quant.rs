//! Unit `net-schema`. Quantisation between protocol views and wire items. DESIGN.md L920 (smallest-three
//! quaternion, about 2 mm positions), L939 (decoded poses match within quantisation across platforms).
//! Deterministic integer rounding only (round half away from zero via `f64::round`, which is exact IEEE and
//! needs no libm); no transcendentals.
//!
//! Non-finite input never panics: float-to-int casts saturate and NaN becomes 0; a zero or non-finite
//! quaternion encodes as identity.
use super::{QuantPose, WireBody, WireDecoy, WireTurret, WireVessel, ANG_VEL_UNITS_PER_RADPS, LIN_VEL_UNITS_PER_MPS, POS_UNITS_PER_M, QUAT_COMPONENT_BITS};
use glam::{DQuat, DVec3};
use sim_core::math::{Kinematics, Pose};
use sim_core::EntityId;
use sim_systems::ids::TrackId;
use sim_systems::protocol::snapshot::{BodyView, DecoyView, TurretView, VesselView};
use std::f64::consts::{FRAC_1_SQRT_2, PI, SQRT_2, TAU};

/// Steps across [-1/sqrt 2, 1/sqrt 2]. An even count (the top code is never produced) puts a code exactly on
/// zero, so the identity and pure-yaw rotations, the commonest of all, round-trip exactly.
const QUAT_STEPS: f64 = ((1u64 << QUAT_COMPONENT_BITS) - 2) as f64;
const QUAT_MASK: u64 = (1u64 << QUAT_COMPONENT_BITS) - 1;
const ANGLE_STEPS: f64 = 65_536.0;

#[inline]
fn q_i32(v: f64, units: f64) -> i32 { (v * units).round() as i32 }

/// Unit: `net-schema`.
pub fn quantize_pos(p: DVec3) -> [i32; 3] { quantize_vec(p, POS_UNITS_PER_M) }
/// Unit: `net-schema`.
pub fn dequantize_pos(q: [i32; 3]) -> DVec3 { dequantize_vec(q, POS_UNITS_PER_M) }
/// Smallest-three encode of a unit quaternion (normalised first; `q` and `-q` encode the same). Unit: `net-schema`.
pub fn quantize_rot(q: DQuat) -> u64 {
    let len2 = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
    if !len2.is_finite() || len2 < 1e-24 { return quantize_rot(DQuat::IDENTITY); }
    let inv = 1.0 / len2.sqrt();
    let c = [q.x * inv, q.y * inv, q.z * inv, q.w * inv];
    // Largest magnitude; ties go to the lowest index so encoding is a pure function of the input.
    let mut big = 0;
    for i in 1..4 { if c[i].abs() > c[big].abs() { big = i; } }
    let sign = if c[big] < 0.0 { -1.0 } else { 1.0 };
    let mut bits = big as u64;
    let mut shift = 2;
    for (i, &ci) in c.iter().enumerate() {
        if i == big { continue; }
        let v = ((ci * sign + FRAC_1_SQRT_2) / SQRT_2 * QUAT_STEPS).round().clamp(0.0, QUAT_STEPS) as u64;
        bits |= v << shift;
        shift += QUAT_COMPONENT_BITS;
    }
    bits
}
/// Decode and renormalise. Unit: `net-schema`.
pub fn dequantize_rot(bits: u64) -> DQuat {
    let (big, small) = unpack_rot(bits);
    let mut c = [0.0f64; 4];
    let mut sum = 0.0;
    let mut k = 0;
    for (i, ci) in c.iter_mut().enumerate() {
        if i == big { continue; }
        let v = small[k] as f64 / QUAT_STEPS * SQRT_2 - FRAC_1_SQRT_2;
        *ci = v;
        sum += v * v;
        k += 1;
    }
    c[big] = (1.0 - sum).max(0.0).sqrt();
    let len = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2] + c[3] * c[3]).sqrt();
    if len < 1e-12 { return DQuat::IDENTITY; }
    DQuat::from_xyzw(c[0] / len, c[1] / len, c[2] / len, c[3] / len)
}
/// Split packed smallest-three bits into the dropped index and the three stored components.
pub(crate) fn unpack_rot(bits: u64) -> (usize, [u32; 3]) {
    let big = (bits & 3) as usize;
    let mut s = [0u32; 3];
    for (k, sk) in s.iter_mut().enumerate() { *sk = ((bits >> (2 + k as u32 * QUAT_COMPONENT_BITS)) & QUAT_MASK) as u32; }
    (big, s)
}
pub(crate) fn pack_rot(big: usize, s: [u32; 3]) -> u64 {
    let mut bits = (big & 3) as u64;
    for (k, &sk) in s.iter().enumerate() { bits |= ((sk as u64) & QUAT_MASK) << (2 + k as u32 * QUAT_COMPONENT_BITS); }
    bits
}
pub fn quantize_pose(p: &Pose) -> QuantPose { QuantPose { pos: quantize_pos(p.pos), rot: quantize_rot(p.rot) } }
pub fn dequantize_pose(q: &QuantPose) -> Pose { Pose { pos: dequantize_pos(q.pos), rot: dequantize_rot(q.rot) } }
/// Velocity vector with a units-per-unit scale. Unit: `net-schema`.
pub fn quantize_vec(v: DVec3, units: f64) -> [i32; 3] { [q_i32(v.x, units), q_i32(v.y, units), q_i32(v.z, units)] }
/// Unit: `net-schema`.
pub fn dequantize_vec(q: [i32; 3], units: f64) -> DVec3 {
    let s = 1.0 / units;
    DVec3::new(q[0] as f64 * s, q[1] as f64 * s, q[2] as f64 * s)
}
/// Angle in radians to 16 bits over a full turn (wrapped to 0..TAU first). Unit: `net-schema`.
pub fn quantize_angle(rad: f32) -> u16 {
    if !rad.is_finite() { return 0; }
    let r = (rad as f64).rem_euclid(TAU);
    ((r / TAU * ANGLE_STEPS).round() as u64 % 65_536) as u16
}
/// Back to -PI..PI. Unit: `net-schema`.
pub fn dequantize_angle(q: u16) -> f32 {
    // Steps above half a turn map to negative angles, so 32768 is -PI and the result lies in [-PI, PI).
    let signed = if q >= 32_768 { q as f64 - ANGLE_STEPS } else { q as f64 };
    let a = signed * (TAU / ANGLE_STEPS);
    debug_assert!((-PI..PI).contains(&a));
    a as f32
}

fn rudder_q(r: f32) -> i8 { if r.is_finite() { (r.clamp(-1.0, 1.0) * 127.0).round() as i8 } else { 0 } }
fn power_q(p: f32) -> u8 { if p.is_finite() { (p.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 } }
fn age_q(a: f32) -> u16 { if a.is_finite() { (a.max(0.0) * 10.0).round().min(65_535.0) as u16 } else { 0 } }

impl WireVessel {
    /// Unit: `net-schema`.
    pub fn from_view(v: &VesselView) -> Self {
        WireVessel {
            entity: v.entity.to_u64(),
            pose: quantize_pose(&v.kin.pose),
            lin_vel: quantize_vec(v.kin.lin_vel, LIN_VEL_UNITS_PER_MPS),
            ang_vel: quantize_vec(v.kin.ang_vel, ANG_VEL_UNITS_PER_RADPS),
            status: v.status,
            flags: v.flags,
            turrets: v.turrets.iter().map(|t| WireTurret { slot: t.slot, train: quantize_angle(t.train_rad), elevation: quantize_angle(t.elevation_rad) }).collect(),
            rudder: rudder_q(v.rudder),
            power: power_q(v.power),
            track: v.track.0,
            relevance: v.relevance,
        }
    }
    /// Unit: `net-schema`.
    pub fn to_view(&self) -> VesselView {
        VesselView {
            entity: EntityId::from_u64(self.entity),
            kin: Kinematics {
                pose: dequantize_pose(&self.pose),
                lin_vel: dequantize_vec(self.lin_vel, LIN_VEL_UNITS_PER_MPS),
                ang_vel: dequantize_vec(self.ang_vel, ANG_VEL_UNITS_PER_RADPS),
            },
            status: self.status,
            flags: self.flags,
            turrets: self.turrets.iter().map(|t| TurretView { slot: t.slot, train_rad: dequantize_angle(t.train), elevation_rad: dequantize_angle(t.elevation) }).collect(),
            rudder: self.rudder.max(-127) as f32 / 127.0,
            power: self.power as f32 / 255.0,
            track: TrackId(self.track),
            relevance: self.relevance,
        }
    }
}
impl WireBody {
    /// Unit: `net-schema`.
    pub fn from_view(v: &BodyView) -> Self {
        WireBody {
            entity: v.entity.to_u64(),
            kind: v.kind,
            side: v.side,
            pos: quantize_pos(v.pos),
            vel: quantize_vec(v.vel, LIN_VEL_UNITS_PER_MPS),
            phase: v.phase,
            seeker: v.seeker,
            track: v.track.0,
            relevance: v.relevance,
        }
    }
    /// Unit: `net-schema`.
    pub fn to_view(&self) -> BodyView {
        BodyView {
            entity: EntityId::from_u64(self.entity),
            kind: self.kind,
            side: self.side,
            pos: dequantize_pos(self.pos),
            vel: dequantize_vec(self.vel, LIN_VEL_UNITS_PER_MPS),
            phase: self.phase,
            seeker: self.seeker,
            track: TrackId(self.track),
            relevance: self.relevance,
        }
    }
}
impl WireDecoy {
    /// Unit: `net-schema`.
    pub fn from_view(v: &DecoyView) -> Self {
        WireDecoy { entity: v.entity.to_u64(), kind: v.kind, owner: v.owner.to_u64(), pos: quantize_pos(v.pos), age_ds: age_q(v.age_s) }
    }
    /// Unit: `net-schema`.
    pub fn to_view(&self) -> DecoyView {
        DecoyView { entity: EntityId::from_u64(self.entity), kind: self.kind, owner: EntityId::from_u64(self.owner), pos: dequantize_pos(self.pos), age_s: self.age_ds as f32 / 10.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::part::DecoyKind;
    use sim_core::vessel::VesselStatus;
    use sim_systems::protocol::snapshot::{Relevance, VesselFlags};

    fn rng(state: &mut u64) -> f64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*state >> 11) as f64) / ((1u64 << 53) as f64)
    }

    #[test]
    fn positions_within_quantisation_and_idempotent() {
        let mut s = 7u64;
        for _ in 0..2000 {
            let p = DVec3::new(rng(&mut s) * 1e5 - 5e4, rng(&mut s) * 40.0 - 20.0, rng(&mut s) * 1e5 - 5e4);
            let q = quantize_pos(p);
            let back = dequantize_pos(q);
            assert!((back - p).abs().max_element() <= 0.5 / 512.0 + 1e-9);
            assert_eq!(quantize_pos(back), q);
        }
        assert_eq!(quantize_pos(DVec3::new(f64::NAN, f64::INFINITY, -f64::INFINITY)), [0, i32::MAX, i32::MIN]);
    }

    #[test]
    fn rotation_error_is_millimetres_on_a_150m_hull() {
        let mut s = 99u64;
        let mut worst: f64 = 0.0;
        let mut sum = 0.0;
        for _ in 0..5000 {
            let q = DQuat::from_xyzw(rng(&mut s) - 0.5, rng(&mut s) - 0.5, rng(&mut s) - 0.5, rng(&mut s) - 0.5).normalize();
            let bits = quantize_rot(q);
            assert!(bits < 1 << 47);
            let back = dequantize_rot(bits);
            let bow = DVec3::new(75.0, 0.0, 0.0);
            let err = (q * bow - back * bow).length();
            worst = worst.max(err);
            sum += err;
            // q and -q encode the same.
            assert_eq!(quantize_rot(-q), bits);
            // Re-encoding the decoded value lands on the same or an adjacent code: never a jump.
            let again = dequantize_rot(quantize_rot(back));
            assert!((again * bow - back * bow).length() < 0.01);
        }
        // Half a step per stored component is 2.2e-5; the rebuilt largest component adds up to three times that
        // in the worst case, so the bound on a 75 m lever is about 1 cm and the typical error a few millimetres.
        let mean = sum / 5000.0;
        assert!(worst < 0.011, "worst bow error {worst} m");
        assert!(mean < 0.004, "mean bow error {mean} m");
        assert_eq!(dequantize_rot(quantize_rot(DQuat::from_xyzw(0.0, 0.0, 0.0, 0.0))), DQuat::IDENTITY);
        assert_eq!(dequantize_rot(quantize_rot(DQuat::from_xyzw(f64::NAN, 0.0, 0.0, 1.0))), DQuat::IDENTITY);
        // Hostile bits decode to a unit quaternion.
        let q = dequantize_rot(u64::MAX);
        assert!((q.length() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn angles_wrap_and_round_trip() {
        for &a in &[0.0f32, 1.0, -1.0, std::f32::consts::PI - 1e-5, 1e-5 - std::f32::consts::PI, 6.3, -12.0, 100.0] {
            let q = quantize_angle(a);
            let back = dequantize_angle(q);
            assert!((-std::f32::consts::PI..std::f32::consts::PI).contains(&back) || back == -std::f32::consts::PI);
            let d = (back as f64 - a as f64).rem_euclid(TAU);
            let d = d.min(TAU - d);
            assert!(d < TAU / 65_536.0, "{a} -> {back}");
            assert_eq!(quantize_angle(back), q);
        }
        assert_eq!(quantize_angle(f32::NAN), 0);
        assert_eq!(quantize_angle(TAU as f32 - 1e-7), 0);
    }

    #[test]
    fn wire_items_are_idempotent_through_views() {
        let v = VesselView {
            entity: EntityId { index: 41, generation: 3 },
            kin: Kinematics { pose: Pose::from_heading(DVec3::new(1234.567, 0.31, -9876.5), 1.2), lin_vel: DVec3::new(5.1, 0.2, -3.3), ang_vel: DVec3::new(0.01, -0.02, 0.003) },
            status: VesselStatus::Active,
            flags: VesselFlags::BURNING | VesselFlags::LIGHTS,
            turrets: vec![TurretView { slot: content::ids::PartSlot(4), train_rad: 3.1, elevation_rad: -0.2 }],
            rudder: -0.37,
            power: 0.8,
            track: TrackId(12),
            relevance: Relevance::Near,
        };
        let w = WireVessel::from_view(&v);
        assert_eq!(WireVessel::from_view(&w.to_view()), w);
        let b = BodyView { entity: EntityId { index: 7, generation: 1 }, kind: content::ids::MunitionIdx(3), side: content::ids::SideId(1), pos: DVec3::new(10.0, 50.0, 20.0), vel: DVec3::new(300.0, 0.0, -2.0), phase: Default::default(), seeker: Default::default(), track: TrackId(0), relevance: Relevance::Engaged };
        let wb = WireBody::from_view(&b);
        assert_eq!(WireBody::from_view(&wb.to_view()), wb);
        let d = DecoyView { entity: EntityId { index: 8, generation: 2 }, kind: DecoyKind::Chaff, owner: EntityId { index: 41, generation: 3 }, pos: DVec3::new(1.0, 30.0, 2.0), age_s: 4.27 };
        let wd = WireDecoy::from_view(&d);
        assert_eq!(WireDecoy::from_view(&wd.to_view()), wd);
        assert_eq!(rudder_q(f32::NAN), 0);
        assert_eq!(power_q(2.0), 255);
    }
}
