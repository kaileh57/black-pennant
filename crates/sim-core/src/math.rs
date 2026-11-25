//! Contract-owned (no work unit). Small math types shared across sim-core and sim-systems.
//! Simulation math is f64 with glam `DVec3` / `DQuat` (the same glam rapier3d-f64 uses, so no conversion).
//! World frame: +Y up, sea level y = 0, +X east, +Z south. Compass heading 0 = north = -Z, clockwise.
//! Vessel-local frame: +X bow, +Y up, +Z starboard; local origin is the grid's minimum corner.
//! Transcendentals go through libm only (DESIGN.md L441).
pub use glam::{DMat3, DQuat, DVec2, DVec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pose { pub pos: DVec3, pub rot: DQuat }
impl Default for Pose { fn default() -> Self { Self { pos: DVec3::ZERO, rot: DQuat::IDENTITY } } }
impl Pose {
    pub fn new(pos: DVec3, rot: DQuat) -> Self { Self { pos, rot } }
    /// Local point (metres) to world.
    pub fn to_world(&self, local: DVec3) -> DVec3 { self.pos + self.rot * local }
    /// World point to local metres.
    pub fn to_local(&self, world: DVec3) -> DVec3 { self.rot.inverse() * (world - self.pos) }
    /// Pose facing compass `heading_rad` at a position on the sea surface.
    pub fn from_heading(pos: DVec3, heading_rad: f64) -> Self {
        // Local +X (bow) must point along (sin h, 0, -cos h): a rotation of (pi/2 - h) about +Y maps +X to (sin h, 0, -cos h).
        let a = core::f64::consts::FRAC_PI_2 - heading_rad;
        let (s, c) = (libm::sin(a * 0.5), libm::cos(a * 0.5));
        Self { pos, rot: DQuat::from_xyzw(0.0, s, 0.0, c) }
    }
    /// Compass heading of the bow in radians, 0..2 pi.
    pub fn heading_rad(&self) -> f64 {
        let f = self.rot * DVec3::X;
        let h = libm::atan2(f.x, -f.z);
        if h < 0.0 { h + core::f64::consts::TAU } else { h }
    }
}

/// Pose plus velocities of a rigid body in the world frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Kinematics { pub pose: Pose, pub lin_vel: DVec3, pub ang_vel: DVec3 }

/// Axis-aligned box in world metres.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Aabb { pub min: DVec3, pub max: DVec3 }
