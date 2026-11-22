//! Unit `gd-hud-damage`. Damage control and the three networks on `BpHud`.
//! DESIGN.md L392-396 (when things break: buffers, local control, jury rigs), L622-680 (flooding, stability,
//! fire), L680-727 (structure and crew), L281-299 (power, fuel and data networks), L1651 (damage states are
//! hatching, never hue), L640 (compartment water at 2 Hz).
//!
//! Sources: static facts from the own design (`ShipDesign::prefab`: grid, compartments, conduits, parts), live
//! facts from the newest snapshot's `ShipStatus`, and the facts only events carry, which [`DcBook`] keeps
//! (the core's event walk feeds it): which conduits are cut or jury-rigged and which magazines were flooded.
//!
//! # Frames
//! Every length is metres in the vessel-local grid frame with the grid's minimum corner at the origin: +x
//! towards the bow (so the bow is to the right), plan `y` = vessel z (starboard positive, so port is up when
//! drawn with y down), profile `y` = height above the grid bottom (up positive; the schematic flips it).
//! `rect` and `side_rect` of `CompartmentStaticDict` and the outlines of `hull_outline` share this frame.
//!
//! Decisions where the contract is silent:
//! - A compartment's `deck` counts the deck planes (`Axis::Y` internal planes) at or below its lowest cell:
//!   0 is the lowest level. Its name is "Compartment N" (N = id + 1, the number log lines use).
//! - `below_waterline` is true when any cell of the compartment lies below the design waterline: the
//!   compartment has a flood valve (L648) and its breaches count first on the repair list (L716).
//! - A conduit is cut from its `ConduitCut` event until a `ConduitJuryRigged` job completes; a live
//!   `ConduitCut` alert (unacknowledged, acknowledged or shelved) also marks it cut, which covers joining
//!   mid-sortie. Later events win over earlier ones by tick, so a kill-cam clip's repeated old events never
//!   undo a newer jury rig. The protocol does not report conduits jury-rigged before the sortie (carried
//!   theatre damage, `HullDamage::jury_rigged`) or before a rejoin, nor magazines flooded before a rejoin:
//!   those show intact until an event or a live alert says otherwise.
//! - `consumers_dead` and `consumers_on_buffer` count the snapshot's parts by their link on that network:
//!   Dead, and Buffer or LocalControl (the fallback steps of L394).
//! - `flooding` in the summary counts compartments holding any water; `fires` burning ones; `pumps_running`
//!   pumps whose readout says running.
//! - `damage_controls` lists pumps, then doors, then a flood valve per below-waterline compartment (its
//!   `slot` is `Bp.NONE`; `Orders.set_flood_valve` takes the compartment), then a flood order per magazine.
//!   A pump or door is usable while it is not destroyed and has power (any link but Dead); a valve while its
//!   compartment exists; a magazine flood until the magazine is flooded or destroyed (`on` is true once
//!   flooded: it cannot be undone in a sortie).
use super::{part_name_msg, BpHud, Out, Section};
use crate::convert::{d, msg_dict};
use crate::enums::GdEnum;
use content::cell::CELL_M;
use content::enums::{Axis, DamageLevel, NetworkKind};
use content::graph::RepairPriority;
use content::ids::{CompartmentId, PartSlot};
use content::part::PartFunction;
use godot::prelude::*;
use sim_core::events::PhysEvent;
use sim_core::networks::{ConduitState, LinkState};
use sim_core::parts::TeamOrder;
use sim_core::EntityId;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{AlertKind, AlertState, Msg, MsgArg, PartReadout, PartView};
use std::collections::BTreeMap;

/// Text keys this unit emits (rows in `game/i18n/log.csv`).
pub(crate) const TEXT_KEYS: &[&str] = &[
    "hud.dc.compartment", "hud.dc.valve", "hud.dc.magazine_flood",
    "hud.dc.team.idle", "hud.dc.team.fire", "hud.dc.team.patch", "hud.dc.team.jury_rig", "hud.dc.team.repair",
];

// ---------------------------------------------------------------------------------------------------------
// What only events tell (fed by the core's event walk)
// ---------------------------------------------------------------------------------------------------------

/// A conduit's state as the events told it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConduitMark { Cut, JuryRigged }

/// Damage facts of the own hull that reach the client only as events. Plain data; `Default` is empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DcBook {
    /// The hull these facts are about.
    vessel: Option<EntityId>,
    /// Conduit index (`Networks::conduits`) to its newest mark and the tick that set it.
    pub(crate) conduits: BTreeMap<u16, (ConduitMark, u64)>,
    /// Magazines flooded from the fire main, with the tick.
    pub(crate) flooded_magazines: BTreeMap<PartSlot, u64>,
}

impl DcBook {
    fn mark(&mut self, conduit: u16, mark: ConduitMark, tick: u64) -> bool {
        match self.conduits.get(&conduit) {
            Some((m, t)) if *t > tick || (*t == tick && *m == mark) => false,
            _ => { self.conduits.insert(conduit, (mark, tick)); true }
        }
    }

    /// One walked event. Called by `HudCore::event` for every event, fresh or repeated.
    pub(crate) fn observe(&mut self, own: Option<EntityId>, tick: u64, event: &SimEvent, out: &mut Vec<Out>) {
        let Some(own) = own else { return };
        if self.vessel != Some(own) { *self = DcBook { vessel: Some(own), ..DcBook::default() }; }
        let SimEvent::Phys(p) = event else { return };
        let changed = match p {
            PhysEvent::ConduitCut { vessel, conduit } if *vessel == own => self.mark(*conduit, ConduitMark::Cut, tick),
            PhysEvent::JobDone { vessel, job: sim_core::crew::JobDone::ConduitJuryRigged(c) } if *vessel == own => self.mark(*c, ConduitMark::JuryRigged, tick),
            PhysEvent::MagazineFlooded { vessel, magazine } if *vessel == own => self.flooded_magazines.insert(*magazine, tick).is_none(),
            _ => false,
        };
        if changed { out.push(Out::Bump(Section::Networks)); out.push(Out::Bump(Section::Compartments)); }
    }
}

/// A conduit's state now: the newest event mark, else a live cut alert, else the design's routed state.
pub(crate) fn conduit_state(base: ConduitState, mark: Option<(ConduitMark, u64)>, alert_tick: Option<u64>) -> ConduitState {
    match (mark, alert_tick) {
        (Some((ConduitMark::JuryRigged, t)), Some(a)) if a > t => ConduitState::Cut,
        (Some((ConduitMark::JuryRigged, _)), _) => ConduitState::JuryRigged,
        (Some((ConduitMark::Cut, _)), _) => ConduitState::Cut,
        (None, Some(_)) => ConduitState::Cut,
        (None, None) => base,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Pure helpers (unit-tested without the engine)
// ---------------------------------------------------------------------------------------------------------

/// Outline polygons of a cell grid: (plan, profile) in metres. Plan runs along the port edge from stern to
/// bow and back along the starboard edge; profile along the keel and back along the top. Columns without a
/// cell are skipped; the steps follow the cell grid (the hull is cell-built).
pub(crate) fn outlines(size: [u16; 3], occupied: &dyn Fn(usize, usize, usize) -> bool) -> (Vec<[f32; 2]>, Vec<[f32; 2]>) {
    let (sx, sy, sz) = (size[0] as usize, size[1] as usize, size[2] as usize);
    let mut plan_cols: Vec<(usize, usize, usize)> = Vec::new();
    let mut side_cols: Vec<(usize, usize, usize)> = Vec::new();
    for x in 0..sx {
        let (mut z0, mut z1, mut y0, mut y1) = (usize::MAX, 0usize, usize::MAX, 0usize);
        for y in 0..sy {
            for z in 0..sz {
                if occupied(x, y, z) { z0 = z0.min(z); z1 = z1.max(z); y0 = y0.min(y); y1 = y1.max(y); }
            }
        }
        if z0 != usize::MAX { plan_cols.push((x, z0, z1)); side_cols.push((x, y0, y1)); }
    }
    (ring(&plan_cols), ring(&side_cols))
}

/// A closed stepped ring from per-column (x, low, high) extents, collinear points removed.
fn ring(cols: &[(usize, usize, usize)]) -> Vec<[f32; 2]> {
    let m = |v: usize| (v as f64 * CELL_M) as f32;
    let mut pts: Vec<[f32; 2]> = Vec::with_capacity(cols.len() * 4);
    for &(x, lo, _) in cols { pts.push([m(x), m(lo)]); pts.push([m(x + 1), m(lo)]); }
    for &(x, _, hi) in cols.iter().rev() { pts.push([m(x + 1), m(hi + 1)]); pts.push([m(x), m(hi + 1)]); }
    simplify(pts)
}

/// Drop repeated and collinear points of a closed axis-aligned ring.
fn simplify(pts: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    let mut v: Vec<[f32; 2]> = Vec::with_capacity(pts.len());
    for p in pts { if v.last() != Some(&p) { v.push(p); } }
    if v.len() > 1 && v.first() == v.last() { v.pop(); }
    if v.len() < 3 { return v; }
    let n = v.len();
    let keep: Vec<bool> = (0..n).map(|i| {
        let (a, b, c) = (v[(i + n - 1) % n], v[i], v[(i + 1) % n]);
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        cross.abs() > 1e-6
    }).collect();
    v.into_iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| p).collect()
}

/// Deck level of a compartment whose lowest cell is at `min_y`: the deck planes at or below it.
pub(crate) fn deck_of(min_y: i16, deck_offsets: &[i16]) -> i64 { deck_offsets.iter().filter(|o| **o <= min_y).count() as i64 }

/// Link of a part on one network.
fn link_of(p: &PartView, kind: NetworkKind) -> LinkState { match kind { NetworkKind::Power => p.power, NetworkKind::Fuel => p.fuel, NetworkKind::Data => p.data } }

/// (dead, on buffer or local control) parts of one network.
pub(crate) fn consumer_counts(parts: &[PartView], kind: NetworkKind) -> (i64, i64) {
    let dead = parts.iter().filter(|p| link_of(p, kind) == LinkState::Dead).count() as i64;
    let fallback = parts.iter().filter(|p| matches!(link_of(p, kind), LinkState::Buffer | LinkState::LocalControl)).count() as i64;
    (dead, fallback)
}

/// `Bp.TEAM_*`, target and the sentence of one team order.
pub(crate) fn team_parts(order: &TeamOrder, name_part: &dyn Fn(PartSlot) -> Msg) -> (i64, i64, Msg) {
    match order {
        TeamOrder::Idle => (0, -1, Msg::new("hud.dc.team.idle")),
        TeamOrder::FightFire(c) => (1, c.0 as i64, Msg::new("hud.dc.team.fire").arg(MsgArg::Compartment(*c))),
        TeamOrder::Patch { compartment } => (2, compartment.0 as i64, Msg::new("hud.dc.team.patch").arg(MsgArg::Compartment(*compartment))),
        TeamOrder::JuryRig { conduit } => (3, *conduit as i64, Msg::new("hud.dc.team.jury_rig").arg(MsgArg::Int(*conduit as i64 + 1))),
        TeamOrder::Repair(s) => (4, s.0 as i64, Msg::new("hud.dc.team.repair").arg(MsgArg::Name(crate::text::msg_text(&crate::text::EngineLookup::new(), &name_part(*s))))),
    }
}

fn frac_u8(v: u8) -> f64 { v as f64 / 255.0 }
fn fin(v: f32) -> f64 { if v.is_finite() { v as f64 } else { 0.0 } }
fn m(v: i16) -> f32 { (v as f64 * CELL_M) as f32 }
fn mspan(lo: i16, hi: i16) -> f32 { ((hi as i32 - lo as i32 + 1).max(0) as f64 * CELL_M) as f32 }

impl BpHud {
    /// Conduit index to the tick of a live `ConduitCut` alert of the own hull.
    fn cut_alerts(&self) -> BTreeMap<u16, u64> {
        let mut out = BTreeMap::new();
        for a in self.alert_views() {
            if let AlertKind::ConduitCut { conduit } = a.kind {
                if matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged | AlertState::Shelved) { out.insert(conduit, a.raised_tick); }
            }
        }
        out
    }

    /// Compartment ids below the design waterline, ascending.
    fn valve_compartments(&self) -> Vec<CompartmentId> {
        let Some(design) = self.design() else { return Vec::new() };
        let p = &design.prefab;
        let wl_cell = (p.waterline_y / CELL_M).floor();
        p.compartments.compartments.iter().filter(|c| !c.is_absorbed() && (c.below_waterline || (c.min[1] as f64) < wl_cell)).map(|c| c.id).collect()
    }
}

#[godot_api(secondary)]
impl BpHud {
    /// Returns `Array[CompartmentStaticDict]`, ascending id. Changes only with `ship_static_changed`.
    #[func]
    fn compartments_static(&self) -> Array<VarDictionary> {
        self.cached("compartments_static", 0, &[Section::Static], |h| {
            let Some(design) = h.design() else { return Array::new() };
            let p = &design.prefab;
            let catalogue = crate::services::services().catalogue();
            let mut decks: Vec<i16> = design.blueprint().hull.planes.iter().filter(|pl| pl.axis == Axis::Y).map(|pl| pl.offset).collect();
            decks.sort_unstable();
            let wl_cell = (p.waterline_y / CELL_M).floor();
            let set = &p.compartments;
            set.compartments.iter().filter(|c| !c.is_absorbed()).map(|c| {
                let mut neighbours: Vec<i32> = set.adjacency.iter().filter_map(|a| if a.a == c.id { Some(a.b.0 as i32) } else if a.b == c.id { Some(a.a.0 as i32) } else { None }).collect();
                neighbours.sort_unstable();
                neighbours.dedup();
                let has_magazine = c.magazine.is_some() || c.parts.iter().any(|s| p.parts.get(s.index()).and_then(|pr| catalogue.parts.get_index(pr.def.index())).is_some_and(|(_, def)| matches!(def.function, PartFunction::Magazine { .. })));
                d! {
                    "id" => c.id.0 as i64,
                    "name" => msg_dict(&Msg::new("hud.dc.compartment").arg(MsgArg::Int(c.id.0 as i64 + 1))),
                    "deck" => deck_of(c.min[1], &decks),
                    "rect" => Rect2::new(Vector2::new(m(c.min[0]), m(c.min[2])), Vector2::new(mspan(c.min[0], c.max[0]), mspan(c.min[2], c.max[2]))),
                    "side_rect" => Rect2::new(Vector2::new(m(c.min[0]), m(c.min[1])), Vector2::new(mspan(c.min[0], c.max[0]), mspan(c.min[1], c.max[1]))),
                    "volume_m3" => if c.volume_m3.is_finite() { c.volume_m3 } else { 0.0 },
                    "has_magazine" => has_magazine,
                    "below_waterline" => c.below_waterline || (c.min[1] as f64) < wl_cell,
                    "neighbours" => PackedInt32Array::from(neighbours.as_slice()),
                }
            }).collect()
        })
    }
    /// Returns `Array[CompartmentDict]`, same order as `compartments_static()`. 2 Hz.
    #[func]
    fn compartments(&self) -> Array<VarDictionary> {
        self.cached("compartments", 0, &[Section::Compartments, Section::Static], |h| {
            let Some(design) = h.design() else { return Array::new() };
            let live = h.ship().map(|s| s.compartments.as_slice()).unwrap_or(&[]);
            design.prefab.compartments.compartments.iter().filter(|c| !c.is_absorbed()).map(|c| {
                let v = live.iter().find(|x| x.id == c.id);
                d! {
                    "id" => c.id.0 as i64,
                    "fill" => v.map_or(0.0, |v| frac_u8(v.fill)),
                    "fire" => v.map_or(0.0, |v| frac_u8(v.fire)),
                    "breached" => v.is_some_and(|v| v.breached),
                    "valve_open" => v.is_some_and(|v| v.valve_open),
                    "sealed" => v.is_none_or(|v| v.sealed),
                }
            }).collect()
        })
    }
    /// Returns `CrewDict`: berths, passengers, damage-control teams and the repair order.
    #[func]
    fn crew(&self) -> VarDictionary {
        self.cached("crew", 0, &[Section::Crew, Section::Static], |h| {
            let Some(ship) = h.ship() else { return VarDictionary::new() };
            let c = &ship.crew;
            let design = h.design();
            let name_part = |s: PartSlot| part_name_msg(design, s);
            let teams: Array<VarDictionary> = c.teams.iter().enumerate().map(|(i, (order, progress, pinned))| {
                let (kind, target, text) = team_parts(order, &name_part);
                d! { "index" => i as i64, "kind" => kind, "target" => target, "progress" => frac_u8(*progress), "pinned" => *pinned, "text" => msg_dict(&h.named_parts(&text)) }
            }).collect();
            let order: Vec<i32> = if c.repair_order.is_empty() { RepairPriority::DEFAULT_ORDER.to_vec() } else { c.repair_order.clone() }.iter().map(|r| r.to_gd() as i32).collect();
            d! {
                "original_berths" => c.original_berths as i64,
                "surviving_berths" => c.surviving_berths as i64,
                "passengers" => c.passengers as i64,
                "passengers_lost" => c.passengers_lost as i64,
                "teams" => teams,
                "repair_order" => PackedInt32Array::from(order.as_slice()),
            }
        })
    }
    /// Returns `Array[NetworkDict]` in `Bp.NETWORK_*` order. 2 Hz, `networks_changed`.
    #[func]
    fn networks(&self) -> Array<VarDictionary> {
        self.cached("networks", 0, &[Section::Networks, Section::Parts, Section::Alerts, Section::Static], |h| {
            let Some(design) = h.design() else { return Array::new() };
            let conduits = &design.prefab.networks.conduits;
            let book = &h.core().dc;
            let alerts = h.cut_alerts();
            let parts = h.ship().map(|s| s.parts.as_slice()).unwrap_or(&[]);
            [NetworkKind::Power, NetworkKind::Fuel, NetworkKind::Data].into_iter().map(|kind| {
                let list: Array<VarDictionary> = conduits.iter().enumerate().take(u16::MAX as usize).filter(|(_, c)| c.network == kind).map(|(i, c)| {
                    let i16_ = i as u16;
                    let state = conduit_state(c.state, book.conduits.get(&i16_).copied(), alerts.get(&i16_).copied());
                    d! {
                        "id" => i as i64,
                        "from" => c.from.0 as i64,
                        "to" => c.to.0 as i64,
                        "cut" => state == ConduitState::Cut,
                        "jury_rigged" => state == ConduitState::JuryRigged,
                        "redundant" => c.redundant,
                        "pinned" => c.pinned,
                    }
                }).collect();
                let (dead, fallback) = consumer_counts(parts, kind);
                d! { "kind" => kind.to_gd(), "conduits" => list, "consumers_dead" => dead, "consumers_on_buffer" => fallback }
            }).collect()
        })
    }
    /// Totals for the damage-control header: `{ "flooding": int, "fires": int, "breached": int,
    /// "parts_damaged": int, "parts_heavy": int, "parts_destroyed": int, "cells_lost": float,
    /// "heel_deg": float, "trim_deg": float, "teams_idle": int, "pumps_running": int }`.
    #[func]
    fn damage_summary(&self) -> VarDictionary {
        self.cached("damage_summary", 0, &[Section::Compartments, Section::Parts, Section::Crew, Section::Motion, Section::Static], |h| {
            let Some(s) = h.ship() else { return VarDictionary::new() };
            let level = |l: DamageLevel| s.parts.iter().filter(|p| p.level == l).count() as i64;
            let pumps: Vec<PartSlot> = h.design().map(|d| d.pumps.clone()).unwrap_or_default();
            let running = s.parts.iter().filter(|p| pumps.contains(&p.slot) && matches!(p.readout, PartReadout::Running(true))).count() as i64;
            d! {
                "flooding" => s.compartments.iter().filter(|c| c.fill > 0).count() as i64,
                "fires" => s.compartments.iter().filter(|c| c.fire > 0).count() as i64,
                "breached" => s.compartments.iter().filter(|c| c.breached).count() as i64,
                "parts_damaged" => level(DamageLevel::Damaged),
                "parts_heavy" => level(DamageLevel::Heavy),
                "parts_destroyed" => level(DamageLevel::Destroyed),
                "cells_lost" => fin(s.cells_lost).clamp(0.0, 1.0),
                "heel_deg" => fin(s.heel_deg),
                "trim_deg" => fin(s.trim_deg),
                "teams_idle" => s.crew.teams.iter().filter(|t| t.0 == TeamOrder::Idle).count() as i64,
                "pumps_running" => running,
            }
        })
    }
    /// Hull outline for the schematic, constant per design: `{ "plan": PackedVector2Array,
    /// "profile": PackedVector2Array, "waterline_m": float, "length_m": float, "beam_m": float,
    /// "decks": PackedFloat32Array }` (metres, bow to the right, same frame as `CompartmentStaticDict.rect`).
    #[func]
    fn hull_outline(&self) -> VarDictionary {
        self.cached("hull_outline", 0, &[Section::Static], |h| {
            let Some(design) = h.design() else { return VarDictionary::new() };
            let p = &design.prefab;
            let grid = &p.grid.design;
            let size = grid.size;
            let (sx, sz) = (size[0] as usize, size[2] as usize);
            let occupied = |x: usize, y: usize, z: usize| grid.cells.get(x + sx * (z + sz * y)).is_some_and(|c| !c.is_empty());
            let (plan, profile) = outlines(size, &occupied);
            let mut decks: Vec<f32> = design.blueprint().hull.planes.iter().filter(|pl| pl.axis == Axis::Y).map(|pl| m(pl.offset) + (CELL_M as f32)).collect();
            decks.sort_by(|a, b| a.total_cmp(b));
            decks.dedup();
            let dims = |i: usize| if p.dims[i].is_finite() && p.dims[i] > 0.0 { p.dims[i] } else { size[i] as f64 * CELL_M };
            d! {
                "plan" => plan.iter().map(|q| Vector2::new(q[0], q[1])).collect::<PackedVector2Array>(),
                "profile" => profile.iter().map(|q| Vector2::new(q[0], q[1])).collect::<PackedVector2Array>(),
                "waterline_m" => if p.waterline_y.is_finite() { p.waterline_y } else { 0.0 },
                "length_m" => dims(0),
                "beam_m" => dims(2),
                "decks" => PackedFloat32Array::from(decks.as_slice()),
            }
        })
    }
    /// Pumps, doors and valves the player can command: `Array[Dictionary]` with `slot: int`,
    /// `kind: StringName` (`&"pump"`, `&"door"`, `&"valve"`, `&"magazine_flood"`), `compartment: int`,
    /// `on: bool` (running, closed or open), `usable: bool`, `name: MsgDict`.
    #[func]
    fn damage_controls(&self) -> Array<VarDictionary> {
        self.cached("damage_controls", 0, &[Section::Parts, Section::Compartments, Section::Static], |h| {
            let Some(design) = h.design() else { return Array::new() };
            let p = &design.prefab;
            let parts = h.ship().map(|s| s.parts.as_slice()).unwrap_or(&[]);
            let live = |s: PartSlot| parts.iter().find(|x| x.slot == s);
            let comp_of = |s: PartSlot| p.parts.get(s.index()).and_then(|r| r.compartment).map_or(-1, |c| c.0 as i64);
            let works = |s: PartSlot| live(s).is_some_and(|v| v.level != DamageLevel::Destroyed && v.power != LinkState::Dead);
            let row = |slot: i64, kind: &str, compartment: i64, on: bool, usable: bool, name: Msg| d! {
                "slot" => slot, "kind" => StringName::from(kind), "compartment" => compartment, "on" => on, "usable" => usable, "name" => msg_dict(&name),
            };
            let mut out: Array<VarDictionary> = Array::new();
            let mut pumps = design.pumps.clone();
            pumps.sort_unstable();
            for s in pumps {
                let on = live(s).is_some_and(|v| matches!(v.readout, PartReadout::Running(true)));
                out.push(&row(s.0 as i64, "pump", comp_of(s), on, works(s), part_name_msg(Some(design), s)));
            }
            let mut doors = design.doors.clone();
            doors.sort_unstable();
            for s in doors {
                let on = live(s).is_some_and(|v| matches!(v.readout, PartReadout::Closed(true)));
                out.push(&row(s.0 as i64, "door", comp_of(s), on, works(s), part_name_msg(Some(design), s)));
            }
            let comps = h.ship().map(|s| s.compartments.as_slice()).unwrap_or(&[]);
            for c in h.valve_compartments() {
                let v = comps.iter().find(|x| x.id == c);
                out.push(&row(-1, "valve", c.0 as i64, v.is_some_and(|v| v.valve_open), h.ship().is_some(), Msg::new("hud.dc.valve").arg(MsgArg::Int(c.0 as i64 + 1))));
            }
            let catalogue = crate::services::services().catalogue();
            for r in &p.parts {
                let is_mag = catalogue.parts.get_index(r.def.index()).is_some_and(|(_, d)| matches!(d.function, PartFunction::Magazine { .. }));
                if !is_mag { continue; }
                let flooded = h.core().dc.flooded_magazines.contains_key(&r.slot);
                let alive = live(r.slot).is_some_and(|v| v.level != DamageLevel::Destroyed);
                let name = Msg::new("hud.dc.magazine_flood").arg(MsgArg::Name(crate::text::msg_text(&crate::text::EngineLookup::new(), &part_name_msg(Some(design), r.slot))));
                out.push(&row(r.slot.0 as i64, "magazine_flood", comp_of(r.slot), flooded, alive && !flooded, name));
            }
            out
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::crew::JobDone;

    fn own() -> EntityId { EntityId::from_u64(5) }
    fn cut(c: u16) -> SimEvent { SimEvent::Phys(PhysEvent::ConduitCut { vessel: own(), conduit: c }) }
    fn rigged(c: u16) -> SimEvent { SimEvent::Phys(PhysEvent::JobDone { vessel: own(), job: JobDone::ConduitJuryRigged(c) }) }

    #[test]
    fn conduit_marks_follow_the_newest_event() {
        let mut b = DcBook::default();
        let mut out = Vec::new();
        b.observe(Some(own()), 100, &cut(3), &mut out);
        assert!(out.contains(&Out::Bump(Section::Networks)));
        assert_eq!(b.conduits.get(&3), Some(&(ConduitMark::Cut, 100)));
        b.observe(Some(own()), 400, &rigged(3), &mut out);
        assert_eq!(b.conduits.get(&3).map(|m| m.0), Some(ConduitMark::JuryRigged));
        // A kill-cam clip repeats the old cut: the newer jury rig stands.
        out.clear();
        b.observe(Some(own()), 100, &cut(3), &mut out);
        assert_eq!(b.conduits.get(&3).map(|m| m.0), Some(ConduitMark::JuryRigged));
        assert!(out.is_empty());
        // Other hulls' events and an unknown hull are ignored.
        b.observe(Some(own()), 500, &SimEvent::Phys(PhysEvent::ConduitCut { vessel: EntityId::from_u64(9), conduit: 1 }), &mut out);
        b.observe(None, 500, &cut(1), &mut out);
        assert!(!b.conduits.contains_key(&1));
        // A new own hull starts a new book.
        b.observe(Some(EntityId::from_u64(6)), 600, &rigged(0), &mut out);
        assert!(b.conduits.is_empty());
    }

    #[test]
    fn conduit_state_combines_marks_and_alerts() {
        use ConduitState as S;
        assert_eq!(conduit_state(S::Intact, None, None), S::Intact);
        assert_eq!(conduit_state(S::JuryRigged, None, None), S::JuryRigged);
        assert_eq!(conduit_state(S::Intact, None, Some(10)), S::Cut);
        assert_eq!(conduit_state(S::Intact, Some((ConduitMark::Cut, 5)), None), S::Cut);
        assert_eq!(conduit_state(S::Intact, Some((ConduitMark::JuryRigged, 50)), Some(10)), S::JuryRigged);
        // Cut again after the rig (a new hit raises a new alert).
        assert_eq!(conduit_state(S::Intact, Some((ConduitMark::JuryRigged, 50)), Some(90)), S::Cut);
    }

    #[test]
    fn magazine_flood_is_booked_once() {
        let mut b = DcBook::default();
        let mut out = Vec::new();
        let ev = SimEvent::Phys(PhysEvent::MagazineFlooded { vessel: own(), magazine: PartSlot(4) });
        b.observe(Some(own()), 10, &ev, &mut out);
        assert_eq!(out.len(), 2);
        out.clear();
        b.observe(Some(own()), 12, &ev, &mut out);
        assert!(out.is_empty());
        assert!(b.flooded_magazines.contains_key(&PartSlot(4)));
    }

    #[test]
    fn outlines_step_with_the_cells() {
        // A 4 x 2 x 3 block with the bow column narrowed to the centre cell.
        let occ = |x: usize, _y: usize, z: usize| x < 3 || z == 1;
        let (plan, profile) = outlines([4, 2, 3], &occ);
        assert_eq!(plan, vec![[0.0, 0.0], [1.5, 0.0], [1.5, 0.5], [2.0, 0.5], [2.0, 1.0], [1.5, 1.0], [1.5, 1.5], [0.0, 1.5]]);
        assert_eq!(profile, vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [0.0, 1.0]]);
        let (p, q) = outlines([3, 3, 3], &|_, _, _| false);
        assert!(p.is_empty() && q.is_empty());
    }

    #[test]
    fn decks_and_team_text() {
        assert_eq!(deck_of(0, &[4, 8]), 0);
        assert_eq!(deck_of(4, &[4, 8]), 1);
        assert_eq!(deck_of(9, &[8, 4]), 2);
        let name = |_s: PartSlot| Msg::new("units.part.numbered").arg(MsgArg::Int(3));
        assert_eq!(team_parts(&TeamOrder::Idle, &name).0, 0);
        let (k, t, text) = team_parts(&TeamOrder::JuryRig { conduit: 6 }, &name);
        assert_eq!((k, t), (3, 6));
        assert_eq!(text.args, vec![MsgArg::Int(7)]);
        assert_eq!(team_parts(&TeamOrder::FightFire(CompartmentId(2)), &name).1, 2);
    }

    #[test]
    fn link_counts() {
        let pv = |power: LinkState, data: LinkState| PartView { slot: PartSlot(0), level: DamageLevel::Intact, health: 255, power, fuel: LinkState::Network, data, shed: false, priority: 1, readout: PartReadout::None };
        let parts = [pv(LinkState::Dead, LinkState::LocalControl), pv(LinkState::Buffer, LinkState::Network), pv(LinkState::Redundant, LinkState::Dead)];
        assert_eq!(consumer_counts(&parts, NetworkKind::Power), (1, 1));
        assert_eq!(consumer_counts(&parts, NetworkKind::Data), (1, 1));
        assert_eq!(consumer_counts(&parts, NetworkKind::Fuel), (0, 0));
    }
}
