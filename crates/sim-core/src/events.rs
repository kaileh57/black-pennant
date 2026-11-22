//! Contract-owned (no work unit). The physical event queue: everything that happened in a tick that another
//! layer must hear about (sim-systems for scoring, Alert, reasons and replication; the client for VFX and
//! audio). Events are appended in a deterministic order by `World::step` and drained once per tick.
//! DESIGN.md L557 (splash), L561 (damage stream), L537, L666, L682-686 (outcomes), L920-926 (wire content).
use crate::armour::HitOutcome;
use crate::crew::JobDone;
use crate::damage::DamageCause;
use crate::entity::EntityId;
use crate::networks::LinkState;
use crate::projectiles::{ProjectileId, ProjectileSpawn};
use crate::vessel::{LossCause, VesselStatus};
use content::cell::CellChange;
use content::enums::DamageLevel;
use content::ids::{CellIndex, CompartmentId, MunitionIdx, PartSlot, SideId};
use content::part::DecoyKind;
use content::weather::WeatherState;
use glam::DVec3;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PhysEvent {
    // Spawns and despawns.
    VesselSpawned { vessel: EntityId, side: SideId },
    VesselDespawned { vessel: EntityId },
    /// A gun fired (muzzle flash, audio, "fired in the last 10 s" for the night visual rule).
    MountFired { vessel: EntityId, slot: PartSlot, rounds: u8 },
    /// Replicated to clients, who integrate the shell themselves (L926).
    ProjectileSpawned(ProjectileSpawn),
    BodySpawned { body: EntityId, kind: MunitionIdx, side: SideId, launcher: EntityId, slot: PartSlot, pos: DVec3, vel: DVec3 },
    DecoyDeployed { decoy: EntityId, kind: DecoyKind, owner: EntityId, pos: DVec3 },
    DecoyExpired { decoy: EntityId },
    MineLaid { mine: EntityId, layer: EntityId, pos: DVec3 },
    // Hits.
    /// A shell reached the water (L557).
    Splash { projectile: ProjectileId, pos: DVec3, calibre_mm: u16, shooter: EntityId },
    /// Reliable impact event closing a replicated projectile (L926).
    ShellImpact { projectile: ProjectileId, vessel: EntityId, cell: CellIndex, outcome: HitOutcome, world_point: DVec3, shooter: EntityId, burst: bool },
    BodyImpact { body: EntityId, vessel: EntityId, cell: CellIndex, world_point: DVec3, cause: DamageCause },
    /// Air or underwater burst of a body (proximity, influence, depth, command).
    BodyBurst { body: EntityId, pos: DVec3, underwater: bool },
    BodyDestroyed { body: EntityId, pos: DVec3, shot_down: bool, by: EntityId },
    BodyRecovered { body: EntityId, vessel: EntityId },
    // Damage.
    /// The replicated damage stream for one vessel this tick: cell index + bucket, in application order (L561).
    CellsDamaged { vessel: EntityId, changes: Vec<CellChange>, cause: DamageCause, attacker: EntityId },
    PartDamaged { vessel: EntityId, slot: PartSlot, level: DamageLevel },
    Breach { vessel: EntityId, compartment: CompartmentId, below_waterline: bool },
    ConduitCut { vessel: EntityId, conduit: u16 },
    /// A part moved along its fallback chain or was shed or restored (L287, L394).
    LinkChanged { vessel: EntityId, slot: PartSlot, power: LinkState, fuel: LinkState, data: LinkState, shed: bool },
    // Fire and flooding.
    FireStarted { vessel: EntityId, compartment: CompartmentId, spread: bool },
    FireOut { vessel: EntityId, compartment: CompartmentId, flooded: bool },
    CookOff { vessel: EntityId, magazine: PartSlot, radius_cells: u8, pos: DVec3 },
    CompartmentHalfFlooded { vessel: EntityId, compartment: CompartmentId },
    MagazineFlooded { vessel: EntityId, magazine: PartSlot },
    PassengersLost { vessel: EntityId, count: u16 },
    // Crew.
    JobDone { vessel: EntityId, job: JobDone },
    // Structure.
    /// A piece split off as a new wreck body: its cells (indices in the shared design grid) and new id (L920).
    Split { vessel: EntityId, piece: EntityId, cells: Vec<CellIndex> },
    /// Cells shed as client-only debris (pieces under 64 cells, L686).
    Debris { vessel: EntityId, cells: Vec<CellIndex>, pos: DVec3 },
    StatusChanged { vessel: EntityId, status: VesselStatus },
    /// A vessel was lost; salvage is recorded from this (L686). `tonnage_t` is its tonnage at spawn.
    VesselLost { vessel: EntityId, side: SideId, cause: LossCause, by: EntityId, tonnage_t: f64 },
    WreckDespawned { wreck: EntityId },
    // Contacts.
    Collision { a: EntityId, b: EntityId, point: DVec3, impulse: f64 },
    Grounded { vessel: EntityId, point: DVec3 },
    // Environment.
    /// Weather or wave seed changed; sent reliably (L920).
    WeatherChanged { weather: WeatherState, wave_seed: u64 },
}
