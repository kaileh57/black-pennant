//! Unit `sys-interest` (hard: this is the anti-wallhack boundary). Which true poses and events each side may
//! receive. DESIGN.md L529 (the server sends each client only its own side's tracks plus the true pose of
//! objects inside an own-side unit's visual range; a modified client cannot reveal undetected objects),
//! L924 (interest management), L945 (exterior-only blueprints and damage for the other team in PvP),
//! L1049 (PvP kill cam limits).
//!
//! # Drawable sets
//! Own-side and allied entities are always drawable, from the tick they first appear in the world. Any other
//! entity is drawable while an own-side or allied Visual or Camera sensor that is active has it inside visual
//! range with line of sight (`sensors::optical::in_visual_range`). Visual checks run at 10 Hz; on the other
//! ticks only spawns of own-side entities and despawns are folded in.
//!
//! Hide delay: an entity that drops out of visual range stays drawable for [`HIDE_DELAY_TICKS`] (1 s) so a
//! hull sitting on the range edge does not flicker, but only while some own-side eye still has it inside
//! weather visibility V and the geometric horizon, the hard caps of the visual law. The delay therefore never
//! draws anything that no visual sensor could see in principle.
//!
//! # Exterior index (PvP)
//! [`ExteriorIndex::cells`] is the set of cells that are, or have been, exterior: it starts as the cells of
//! the exterior-only render blueprint and grows as damage opens the hull. When an exterior cell is destroyed
//! its face-adjacent design cells become exterior (through already-destroyed interior cells too, which are
//! air), and each newly exposed live cell is reported once with its current bucket (0 when undamaged), so the
//! other team's client learns about interior structure exactly when it becomes visible. A client that
//! receives a change for a cell absent from its exterior blueprint treats it as newly exposed structure.
use crate::prelude::*;
use crate::sensors::{SenseEnv, SensorScheduler};
use crate::signature::SignatureTable;
use content::cell::{CellChange, BUCKET_DESTROYED};
use content::part::SensorKind;
use sim_core::bits::BitSet;
use sim_core::vessel::VesselPrefab;

/// Rate of the visual drawable-set recompute (spec: 10 Hz is enough).
pub const INTEREST_HZ: u32 = 10;
/// Anti-flicker hold after an entity leaves visual range: 1 s.
pub const HIDE_DELAY_TICKS: u64 = 60;
/// Kill cam window before the loss, in ticks (10 s).
pub const KILL_CAM_TICKS: u64 = 600;

/// One own-side visual eye as of the last visual recompute: used to decide whether a point event (a splash,
/// an air burst) is visible to a side, and for the hide-delay hard caps.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Eye { pub side: SideId, pub pos: DVec3, pub height_m: f64, pub camera: bool }

/// Per-side drawable sets.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InterestState {
    /// Indexed by side id: entities whose true pose the side may draw now, ascending. Own-side and allied
    /// entities are always in; others only while inside the visual range of an own-side visual or camera
    /// sensor with line of sight (`sensors::optical::in_visual_range`).
    pub drawable: Vec<Vec<EntityId>>,
    /// Entities each side has ever been introduced to (so `EntityRevealed` carries damage only once).
    pub introduced: Vec<Vec<EntityId>>,
    /// Allied mask per side (always including the side itself), copied from the last `update`.
    #[serde(default)]
    pub(crate) allied: Vec<SideMask>,
    /// Per side: tick each non-own entity was last confirmed inside visual range.
    #[serde(default)]
    pub(crate) last_seen: Vec<BTreeMap<EntityId, u64>>,
    /// Per side: entities an `EntityRevealed` has been emitted for and no `EntityHidden` since, ascending.
    #[serde(default)]
    pub(crate) announced: Vec<Vec<EntityId>>,
    /// Per side: entities hidden by the latest announcement; events about them in the same tick still route.
    #[serde(default)]
    pub(crate) just_hidden: Vec<Vec<EntityId>>,
    #[serde(default)]
    pub(crate) eyes: Vec<Eye>,
    #[serde(default)]
    pub(crate) night: bool,
    #[serde(default)]
    pub(crate) last_update_tick: Option<u64>,
    /// Sides that received each live projectile's spawn; they also receive its closing impact or splash.
    #[serde(default)]
    pub(crate) projectile_audience: BTreeMap<u32, SideMask>,
    /// PvP exterior state per vessel and wreck (rebuilt lazily after a keyframe restore).
    #[serde(skip)]
    pub(crate) exterior: BTreeMap<EntityId, ExteriorIndex>,
    /// Pristine exterior index per design hash.
    #[serde(skip)]
    pub(crate) exterior_designs: BTreeMap<ContentHash, ExteriorIndex>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterestChange { pub side: SideId, pub entity: EntityId, pub revealed: bool }

fn sorted_insert(v: &mut Vec<EntityId>, e: EntityId) { if let Err(i) = v.binary_search(&e) { v.insert(i, e); } }
fn has(v: &[EntityId], e: EntityId) -> bool { v.binary_search(&e).is_ok() }

impl InterestState {
    /// Recompute drawable sets (10 Hz is enough; entities entering are revealed before their first pose is
    /// sent; a short hide delay against flicker at the range edge is allowed if it never extends a reveal beyond
    /// what the visual law permits at reveal time). Returns the changes
    /// in ascending (side, entity). Unit: `sys-interest`.
    pub fn update(&mut self, world: &World, sensors: &SensorScheduler, sigs: &SignatureTable, env: &SenseEnv<'_>, allied: &[SideMask]) -> Vec<InterestChange> {
        let tick = world.tick().0;
        let n = allied.len().min(8);
        if self.last_update_tick == Some(tick) && self.allied.len() == n { return Vec::new(); }
        self.last_update_tick = Some(tick);
        self.allied = (0..n).map(|i| allied[i].with(SideId(i as u8))).collect();
        self.night = env.night;
        for v in [&mut self.drawable, &mut self.introduced, &mut self.announced, &mut self.just_hidden] { v.resize(n, Vec::new()); }
        self.last_seen.resize(n, BTreeMap::new());

        // Every live entity with its side and position, ascending slot.
        let mut universe: Vec<(EntityId, DVec3, SideId)> = world.entities().iter().filter_map(|id| world.locate(id).map(|(p, _, s)| (id, p, s))).collect();
        universe.sort_by_key(|u| u.0);
        let due = Tick(tick).is_due(INTEREST_HZ, 0) || self.eyes.is_empty();
        let visibility = env.ctx.weather.visibility_m.max(0.0);
        let eye_sensors: Vec<usize> = sensors.sensors.iter().enumerate().filter(|(_, s)| matches!(s.kind, SensorKind::Visual | SensorKind::Camera) && s.active).map(|(i, _)| i).collect();
        if due {
            self.eyes = eye_sensors.iter().map(|&i| { let s = &sensors.sensors[i]; Eye { side: s.side, pos: s.pos, height_m: s.height_m, camera: s.kind == SensorKind::Camera } }).collect();
        }

        let mut changes = Vec::new();
        for si in 0..n {
            let side = SideId(si as u8);
            let mask = self.allied[si];
            let old = core::mem::take(&mut self.drawable[si]);
            let mut new = Vec::with_capacity(old.len());
            for &(id, pos, owner) in &universe {
                if mask.has(owner) { new.push(id); continue; }
                if !due {
                    if has(&old, id) { new.push(id); }
                    continue;
                }
                let sig = sigs.get(id);
                let seen = sig.map(|sig| eye_sensors.iter().any(|&i| {
                    let s = &sensors.sensors[i];
                    mask.has(s.side) && s.pos.distance(pos) <= visibility + 1.0 && crate::sensors::optical::in_visual_range(s, sig, env)
                })).unwrap_or(false);
                if seen { self.last_seen[si].insert(id, tick); }
                let keep = match self.last_seen[si].get(&id) {
                    Some(&t) if t == tick => true,
                    Some(&t) if tick.saturating_sub(t) <= HIDE_DELAY_TICKS => {
                        let top = sig.map(|s| s.top_m).unwrap_or(0.0);
                        self.eyes.iter().any(|e| mask.has(e.side) && {
                            let d = e.pos.distance(pos);
                            d <= visibility && d <= crate::sensors::radar::horizon_m(e.height_m, top)
                        })
                    }
                    _ => false,
                };
                if keep { new.push(id); }
            }
            if due {
                let live: BTreeSet<EntityId> = new.iter().copied().collect();
                self.last_seen[si].retain(|id, t| live.contains(id) || tick.saturating_sub(*t) <= HIDE_DELAY_TICKS && world.locate(*id).is_some());
            }
            diff_into(side, &old, &new, &mut changes);
            for &e in &new { sorted_insert(&mut self.introduced[si], e); }
            self.drawable[si] = new;
        }
        changes
    }
    pub fn can_draw(&self, side: SideId, entity: EntityId) -> bool { self.drawable.get(side.index()).map(|v| v.binary_search(&entity).is_ok()).unwrap_or(false) }
    /// Sides that may draw an entity now. Real code.
    pub fn sides_seeing(&self, entity: EntityId) -> SideMask { let mut m = SideMask::NONE; for (i, v) in self.drawable.iter().enumerate() { if v.binary_search(&entity).is_ok() { m = m.with(SideId(i as u8)); } } m }

    /// Number of sides the state covers.
    pub(crate) fn side_count(&self) -> usize { self.allied.len().min(8) }
    /// Allied mask of a side (empty for an unknown side).
    pub(crate) fn allied_of(&self, side: SideId) -> SideMask { self.allied.get(side.index()).copied().unwrap_or(SideMask::NONE) }
    /// Drawable now, or hidden by this tick's announcement (events of the hiding tick still reach the side).
    pub(crate) fn routable(&self, side: SideId, entity: EntityId) -> bool {
        self.can_draw(side, entity) || self.just_hidden.get(side.index()).map(|v| has(v, entity)).unwrap_or(false)
    }

    /// Diff the drawable sets against what has been announced and record the announcement: returns
    /// `(reveals, hides)` in ascending (side, entity). Safe to call any number of times per tick.
    pub(crate) fn announce(&mut self) -> (Vec<InterestChange>, Vec<InterestChange>) {
        let n = self.drawable.len();
        self.announced.resize(n, Vec::new());
        self.just_hidden.resize(n, Vec::new());
        let (mut reveals, mut hides) = (Vec::new(), Vec::new());
        for si in 0..n {
            let mut all = Vec::new();
            diff_into(SideId(si as u8), &self.announced[si], &self.drawable[si], &mut all);
            if !all.is_empty() {
                self.just_hidden[si] = all.iter().filter(|c| !c.revealed).map(|c| c.entity).collect();
            }
            for c in all { if c.revealed { reveals.push(c) } else { hides.push(c) } }
            self.announced[si] = self.drawable[si].clone();
        }
        (reveals, hides)
    }

    /// Forget last tick's hides once their events have been routed.
    pub(crate) fn clear_just_hidden(&mut self) { for v in &mut self.just_hidden { v.clear(); } }

    /// True when a point event at `pos` (water column or burst of roughly `area_m2` projected area, top
    /// `top_m` above the sea) is inside the visual range of an eye of `side` or its allies (L436).
    pub(crate) fn point_visible(&self, side: SideId, pos: DVec3, area_m2: f64, top_m: f64, weather: &WeatherState, tuning: &content::tuning::SensorTuning) -> bool {
        let mask = self.allied_of(side);
        self.eyes.iter().any(|e| {
            if !mask.has(e.side) { return false; }
            let d = e.pos.distance(pos);
            if d > weather.visibility_m.max(0.0) { return false; }
            let horizon = crate::sensors::radar::horizon_m(e.height_m, top_m);
            d <= crate::sensors::optical::visual_range_m(area_m2, e.camera, self.night, false, weather.visibility_m, horizon, tuning)
        })
    }

    /// Forget a despawned entity everywhere except `introduced` (kept for kill cams and replay cuts).
    pub(crate) fn forget(&mut self, entity: EntityId) {
        for v in &mut self.last_seen { v.remove(&entity); }
        self.exterior.remove(&entity);
    }
}

fn diff_into(side: SideId, old: &[EntityId], new: &[EntityId], out: &mut Vec<InterestChange>) {
    let (mut i, mut j) = (0, 0);
    loop {
        match (old.get(i), new.get(j)) {
            (Some(&a), Some(&b)) if a == b => { i += 1; j += 1; }
            (Some(&a), Some(&b)) if a < b => { out.push(InterestChange { side, entity: a, revealed: false }); i += 1; }
            (Some(_), Some(&b)) => { out.push(InterestChange { side, entity: b, revealed: true }); j += 1; }
            (Some(&a), None) => { out.push(InterestChange { side, entity: a, revealed: false }); i += 1; }
            (None, Some(&b)) => { out.push(InterestChange { side, entity: b, revealed: true }); j += 1; }
            (None, None) => break,
        }
    }
}

/// Exterior cells of a design: those in its `content::blueprint::ExteriorBlueprint`. Built once per design
/// that an opposing PvP team can see.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExteriorIndex {
    pub hash: ContentHash,
    pub exterior_hash: ContentHash,
    pub cells: sim_core::bits::BitSet,
    /// Design grid extent (cells are `CellIndex` in that grid).
    pub(crate) size: [u16; 3],
    /// Non-empty cells of the design grid.
    pub(crate) solid: BitSet,
    /// Cells destroyed or removed (split off, shed as debris): air from now on.
    pub(crate) gone: BitSet,
    /// Interior cells damaged while hidden: last bucket, reported when they become exterior.
    pub(crate) hidden: BTreeMap<u32, u8>,
    /// Slots of the weather-deck parts in the exterior blueprint, ascending.
    pub(crate) parts: Vec<PartSlot>,
}

impl ExteriorIndex {
    fn neighbours(&self, i: usize) -> [Option<usize>; 6] {
        let (sx, sy, sz) = (self.size[0] as usize, self.size[1] as usize, self.size[2] as usize);
        if sx == 0 || sy == 0 || sz == 0 || i >= sx * sy * sz { return [None; 6]; }
        let (x, y, z) = (i % sx, i / (sx * sz), (i / sx) % sz);
        [
            (x > 0).then(|| i - 1), (x + 1 < sx).then(|| i + 1),
            (y > 0).then(|| i - sx * sz), (y + 1 < sy).then(|| i + sx * sz),
            (z > 0).then(|| i - sx), (z + 1 < sz).then(|| i + sx),
        ]
    }

    /// Mark cells reachable from `start` through destroyed cells as exterior; report newly exposed live cells.
    fn expose_from(&mut self, start: usize, out: &mut Vec<CellChange>) {
        let mut stack = vec![start];
        while let Some(c) = stack.pop() {
            for nb in self.neighbours(c).into_iter().flatten() {
                if !self.solid.get(nb) || self.cells.get(nb) { continue; }
                self.cells.set(nb, true);
                if self.gone.get(nb) {
                    self.hidden.remove(&(nb as u32));
                    stack.push(nb);
                } else {
                    let bucket = self.hidden.remove(&(nb as u32)).unwrap_or(0);
                    out.push(CellChange { cell: CellIndex(nb as u32), bucket });
                }
            }
        }
    }

    /// Apply a damage list in order and return what the other team sees (the stateful form of
    /// [`exterior_changes`]): changes to exterior cells, then newly exposed cells as they open.
    pub(crate) fn absorb(&mut self, changes: &[CellChange]) -> Vec<CellChange> {
        let mut out = Vec::new();
        for ch in changes {
            let i = ch.cell.index();
            if !self.solid.get(i) { continue; }
            let destroyed = ch.bucket >= BUCKET_DESTROYED;
            if self.cells.get(i) {
                out.push(*ch);
            } else if destroyed {
                self.hidden.remove(&(i as u32));
            } else {
                self.hidden.insert(i as u32, ch.bucket);
            }
            if destroyed {
                self.gone.set(i, true);
                if self.cells.get(i) { self.expose_from(i, &mut out); }
            }
        }
        out
    }

    /// Cells left the hull without damage events (split off or shed): mark them air and open what they
    /// covered. Returns the newly exposed cells.
    pub(crate) fn absorb_removed(&mut self, cells: &[CellIndex]) -> Vec<CellChange> {
        let mut out = Vec::new();
        for c in cells {
            let i = c.index();
            if !self.solid.get(i) { continue; }
            self.gone.set(i, true);
            self.hidden.remove(&(i as u32));
            if self.cells.get(i) { self.expose_from(i, &mut out); }
        }
        out
    }

    /// The index of a split-off piece: every cell not in `live` is gone, and cut faces reachable from outside
    /// become exterior.
    pub(crate) fn restricted(&self, live: &dyn Fn(usize) -> bool) -> ExteriorIndex {
        let mut ix = self.clone();
        let solid: Vec<usize> = ix.solid.iter_ones().collect();
        for &i in &solid { if !live(i) { ix.gone.set(i, true); ix.hidden.remove(&(i as u32)); } }
        let mut sink = Vec::new();
        let starts: Vec<usize> = solid.iter().copied().filter(|&i| ix.gone.get(i) && ix.cells.get(i)).collect();
        for i in starts { ix.expose_from(i, &mut sink); }
        ix
    }

    /// True when the other team knows this cell (exterior now or earlier).
    pub(crate) fn knows(&self, cell: CellIndex) -> bool { self.cells.get(cell.index()) }
}

/// Build the exterior index of a design. Unit: `sys-interest`.
pub fn exterior_index(bp: &content::blueprint::Blueprint, prefab: &VesselPrefab, catalogue: &Catalogue) -> ExteriorIndex {
    let ext = content::blueprint::io::exterior_only(bp, catalogue);
    let exterior_hash = content::blueprint::io::exterior_hash(&ext);
    let design = &prefab.grid.design;
    let n = design.cells.len();
    let mut solid = BitSet::new(n);
    for (i, c) in design.cells.iter().enumerate() { if !c.is_empty() { solid.set(i, true); } }
    let mut cells = BitSet::new(n);
    for row in &ext.cells.0 {
        for run in &row.runs {
            for k in 0..run.len as i32 {
                let x = run.x as i32 + k;
                if x > i16::MAX as i32 { break; }
                if let Some(i) = design.index(content::cell::CellPos::new(x as i16, row.y, row.z)) {
                    if solid.get(i.index()) { cells.set(i.index(), true); }
                }
            }
        }
    }
    let mut parts: Vec<PartSlot> = ext.parts.iter().map(|(s, _)| *s).collect();
    parts.sort();
    parts.dedup();
    for slot in &parts {
        if let Some(p) = prefab.parts.get(slot.index()) {
            for c in &p.cells { if solid.get(c.index()) { cells.set(c.index(), true); } }
        }
    }
    ExteriorIndex { hash: prefab.hash, exterior_hash, cells, size: design.size, solid, gone: BitSet::new(n), hidden: BTreeMap::new(), parts }
}

/// Cut a damage list to exterior cells for the other team (L945). Interior damage that newly exposes cells
/// is reported when those cells become exterior. Unit: `sys-interest`.
pub fn exterior_changes(index: &ExteriorIndex, changes: &[CellChange]) -> Vec<CellChange> {
    if changes.is_empty() { return Vec::new(); }
    index.clone().absorb(changes)
}

/// PvP kill cam window for a lost player hull: only what that player's side detected, plus the exterior of
/// the vessel that fired the killing shot (L1049). Returns the entities the replay cut may show.
/// Unit: `sys-interest`.
pub fn kill_cam_entities(state: &InterestState, victim_side: SideId, killer: Option<EntityId>) -> Vec<EntityId> {
    let mut out: Vec<EntityId> = Vec::new();
    if let Some(v) = state.introduced.get(victim_side.index()) { out.extend_from_slice(v); }
    if let Some(v) = state.drawable.get(victim_side.index()) { out.extend_from_slice(v); }
    if let Some(k) = killer { if !k.is_none() { out.push(k); } }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }

    /// A solid box of `sx * sy * sz` cells whose outer shell is the exterior.
    fn box_index(sx: u16, sy: u16, sz: u16) -> ExteriorIndex {
        let n = sx as usize * sy as usize * sz as usize;
        let mut solid = BitSet::new(n);
        let mut cells = BitSet::new(n);
        for y in 0..sy as usize { for z in 0..sz as usize { for x in 0..sx as usize {
            let i = x + sx as usize * (z + sz as usize * y);
            solid.set(i, true);
            if x == 0 || y == 0 || z == 0 || x + 1 == sx as usize || y + 1 == sy as usize || z + 1 == sz as usize { cells.set(i, true); }
        } } }
        ExteriorIndex { hash: ContentHash::EMPTY, exterior_hash: ContentHash::EMPTY, cells, size: [sx, sy, sz], solid, gone: BitSet::new(n), hidden: BTreeMap::new(), parts: vec![] }
    }
    fn idx(x: u32, y: u32, z: u32, s: [u16; 3]) -> CellIndex { CellIndex(x + s[0] as u32 * (z + s[2] as u32 * y)) }

    #[test]
    fn interior_damage_is_cut_then_reported_on_exposure() {
        let s = [5, 5, 5];
        let mut ix = box_index(5, 5, 5);
        let inner = idx(1, 2, 2, s);
        let outer = idx(0, 2, 2, s);
        assert!(exterior_changes(&ix, &[CellChange { cell: inner, bucket: 3 }]).is_empty());
        assert!(ix.absorb(&[CellChange { cell: inner, bucket: 3 }]).is_empty());
        assert_eq!(ix.absorb(&[CellChange { cell: outer, bucket: 5 }]), vec![CellChange { cell: outer, bucket: 5 }]);
        // Destroying the outer cell exposes the damaged interior one with its bucket.
        let out = ix.absorb(&[CellChange { cell: outer, bucket: 8 }]);
        assert_eq!(out[0], CellChange { cell: outer, bucket: 8 });
        assert!(out.contains(&CellChange { cell: inner, bucket: 3 }));
        assert!(ix.knows(inner));
        assert_eq!(ix.absorb(&[CellChange { cell: inner, bucket: 6 }]), vec![CellChange { cell: inner, bucket: 6 }]);
    }

    #[test]
    fn exposure_cascades_through_destroyed_interior_cells() {
        let s = [7, 7, 7];
        let mut ix = box_index(7, 7, 7);
        let a = idx(1, 3, 3, s);
        let b = idx(2, 3, 3, s);
        assert!(ix.absorb(&[CellChange { cell: a, bucket: 8 }, CellChange { cell: b, bucket: 8 }]).is_empty());
        let out = ix.absorb(&[CellChange { cell: idx(0, 3, 3, s), bucket: 8 }]);
        assert!(out.iter().any(|c| c.cell == idx(3, 3, 3, s) && c.bucket == 0));
        assert!(!out.iter().any(|c| c.cell == a || c.cell == b));
        assert!(ix.knows(idx(3, 3, 3, s)));
    }

    #[test]
    fn pure_cut_matches_absorb_and_ignores_bad_cells() {
        let ix = box_index(4, 4, 4);
        let ch = [CellChange { cell: CellIndex(0), bucket: 8 }, CellChange { cell: CellIndex(9_999_999), bucket: 8 }, CellChange { cell: idx(1, 1, 1, [4, 4, 4]), bucket: 2 }];
        let pure = exterior_changes(&ix, &ch);
        let mut m = ix.clone();
        assert_eq!(pure, m.absorb(&ch));
        assert_eq!(pure[0], ch[0]);
        assert!(!pure.iter().any(|c| c.cell.0 == 9_999_999));
    }

    #[test]
    fn piece_cut_faces_become_exterior() {
        let s = [6, 3, 3];
        let ix = box_index(6, 3, 3);
        assert!(!ix.cells.get(idx(3, 1, 1, s).index()));
        let piece = ix.restricted(&|i| i % 6 >= 3);
        assert!(piece.cells.get(idx(3, 1, 1, s).index()));
        assert!(!piece.cells.get(idx(4, 1, 1, s).index()));
    }

    #[test]
    fn kill_cam_is_victim_knowledge_plus_killer() {
        let st = InterestState { drawable: vec![vec![e(1), e(4)], vec![e(2)]], introduced: vec![vec![e(1), e(3), e(4)], vec![e(2)]], ..Default::default() };
        assert_eq!(kill_cam_entities(&st, SideId(0), Some(e(9))), vec![e(1), e(3), e(4), e(9)]);
        assert_eq!(kill_cam_entities(&st, SideId(1), None), vec![e(2)]);
        assert_eq!(kill_cam_entities(&st, SideId(7), Some(EntityId::NONE)), Vec::<EntityId>::new());
    }

    #[test]
    fn announce_diffs_and_tracks_just_hidden() {
        let mut st = InterestState { drawable: vec![vec![e(1), e(2)]], allied: vec![SideMask::only(SideId(0))], ..Default::default() };
        let (r, h) = st.announce();
        assert_eq!(r.len(), 2);
        assert!(h.is_empty());
        let (r, h) = st.announce();
        assert!(r.is_empty() && h.is_empty());
        st.drawable[0] = vec![e(2), e(5)];
        let (r, h) = st.announce();
        assert_eq!(r, vec![InterestChange { side: SideId(0), entity: e(5), revealed: true }]);
        assert_eq!(h, vec![InterestChange { side: SideId(0), entity: e(1), revealed: false }]);
        assert!(st.routable(SideId(0), e(1)));
        assert!(!st.can_draw(SideId(0), e(1)));
        st.clear_just_hidden();
        assert!(!st.routable(SideId(0), e(1)));
    }

    #[test]
    fn diff_is_ascending() {
        let mut out = Vec::new();
        diff_into(SideId(2), &[e(1), e(3), e(5)], &[e(2), e(3), e(6)], &mut out);
        let got: Vec<(u32, bool)> = out.iter().map(|c| (c.entity.index, c.revealed)).collect();
        assert_eq!(got, vec![(1, false), (2, true), (5, false), (6, true)]);
    }
}
