//! Contract-owned types; the four files are unit `net-schema`. The replication schema (L920-922): every wire
//! item is declared once here with its quantisation, and `replicate.rs` generates its delta coding from that
//! one declaration, so no field can replicate by hand-written code that drifts (the Stormworks and From the
//! Depths failure pattern the spec cites).
//!
//! - `bits.rs`: [`BitWriter`] and [`BitReader`] (bit packing, zig-zag varints).
//! - `quant.rs`: pose and angle quantisation, view to wire item and back.
//! - `replicate.rs`: the [`Replicate`] trait's impls (a declarative macro over each item's field list) and the
//!   per-field change mask.
//! - `codec.rs`: hello header, message framing (bincode with size limits), compact event encoding.
//!
//! Quantisation (L920): positions to 1/512 m (about 2 mm) as i32 in a world at most 100 km across;
//! rotations as smallest-three quaternions with 15 bits per component (about 3 mm of bow error on a 150 m
//! hull); velocities to 1/256 m/s and 1/1024 rad/s as i32; turret angles 16 bits over a full turn. Decoded
//! poses must match the server within quantisation on every platform (cross-platform check, L939), so
//! dequantisation uses only integer maths and one f64 multiply per component, no transcendentals.
pub mod bits;
pub mod codec;
pub mod quant;
pub mod replicate;

pub use bits::{BitReader, BitWriter};

use crate::error::NetError;
use content::enums::DamageLevel;
use content::ids::{MunitionIdx, PartSlot, SideId};
use serde::{Deserialize, Serialize};
use sim_core::munitions::BodyPhase;
use sim_core::vessel::VesselStatus;
use sim_systems::protocol::snapshot::{Relevance, SeekerPhase, VesselFlags};

/// Position units per metre (1/512 m, about 2 mm).
pub const POS_UNITS_PER_M: f64 = 512.0;
/// Bits per smallest-three component (plus 2 bits for the dropped index).
pub const QUAT_COMPONENT_BITS: u32 = 15;
/// Linear velocity units per metre per second.
pub const LIN_VEL_UNITS_PER_MPS: f64 = 256.0;
/// Angular velocity units per radian per second.
pub const ANG_VEL_UNITS_PER_RADPS: f64 = 1024.0;
/// Turret angle bits over one full turn.
pub const ANGLE_BITS: u32 = 16;

/// A quantised pose. `rot` packs the smallest-three quaternion: bits 0..2 the index of the dropped largest
/// component, then three 15-bit components in index order skipping the dropped one; the dropped component is
/// made positive before packing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuantPose { pub pos: [i32; 3], pub rot: u64 }

/// One trainable part's angles, quantised.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTurret { pub slot: PartSlot, pub train: u16, pub elevation: u16 }

/// Wire form of `sim_systems::protocol::snapshot::VesselView`. Rudder is -127..=127, power 0..=255.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WireVessel {
    pub entity: u64,
    pub pose: QuantPose,
    pub lin_vel: [i32; 3],
    pub ang_vel: [i32; 3],
    pub status: VesselStatus,
    pub flags: VesselFlags,
    pub turrets: Vec<WireTurret>,
    pub rudder: i8,
    pub power: u8,
    pub track: u32,
    pub relevance: Relevance,
}

/// Wire form of `BodyView` (guided bodies at 5 Hz, L926).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WireBody {
    pub entity: u64,
    pub kind: MunitionIdx,
    pub side: SideId,
    pub pos: [i32; 3],
    pub vel: [i32; 3],
    pub phase: BodyPhase,
    pub seeker: SeekerPhase,
    pub track: u32,
    pub relevance: Relevance,
}

/// Wire form of `DecoyView`; age in 1/10 s.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireDecoy { pub entity: u64, pub kind: content::part::DecoyKind, pub owner: u64, pub pos: [i32; 3], pub age_ds: u16 }

/// The slow lists of the own `ShipStatus`, replicated as their own section at the slow-section rate
/// (`delta::Section::ShipSlow`): part states and readouts, compartments (2 Hz, L640), crew, the Bridge Board
/// (L1664), offboard units and actions. The rest of the status (speed, heading, attitude, orders, route,
/// power, mounts, held roles) stays in `Section::Ship` at the ship-status rate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WireShipSlow {
    pub parts: Vec<sim_systems::protocol::snapshot::PartView>,
    pub compartments: Vec<sim_systems::protocol::snapshot::CompartmentView>,
    pub crew: sim_systems::protocol::snapshot::CrewView,
    pub board: sim_systems::protocol::snapshot::BridgeBoard,
    pub offboard: Vec<sim_systems::protocol::snapshot::OffboardView>,
    pub actions: Vec<sim_systems::protocol::snapshot::ActionView>,
}
impl WireShipSlow {
    /// Take the slow lists out of a status, leaving them empty in it. Unit: `net-schema`.
    pub fn split(s: &mut sim_systems::protocol::snapshot::ShipStatus) -> Self {
        WireShipSlow {
            parts: std::mem::take(&mut s.parts),
            compartments: std::mem::take(&mut s.compartments),
            crew: std::mem::take(&mut s.crew),
            board: std::mem::take(&mut s.board),
            offboard: std::mem::take(&mut s.offboard),
            actions: std::mem::take(&mut s.actions),
        }
    }
    /// Put the slow lists back into a status. Unit: `net-schema`.
    pub fn merge_into(&self, s: &mut sim_systems::protocol::snapshot::ShipStatus) {
        s.parts = self.parts.clone();
        s.compartments = self.compartments.clone();
        s.crew = self.crew.clone();
        s.board = self.board.clone();
        s.offboard = self.offboard.clone();
        s.actions = self.actions.clone();
    }
}

/// Delta coding generated from one field list per item (see `replicate.rs`). `base` is the receiver's copy
/// of the same item from the acknowledged baseline frame, or `None` for a full write.
pub trait Replicate: Sized {
    /// Write `self`, as a change mask plus only the changed fields when `base` is given.
    fn write(&self, base: Option<&Self>, w: &mut BitWriter);
    /// Read what `write` wrote with the same `base`.
    fn read(base: Option<&Self>, r: &mut BitReader<'_>) -> Result<Self, NetError>;
}
