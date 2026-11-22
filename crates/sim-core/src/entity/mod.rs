//! Unit `sim-core-entity`. The hand-rolled entity table: generational slots, dense columns, ascending
//! iteration. DESIGN.md L1106-1112.
//!
//! Every object that can be sensed, targeted or owned has an [`EntityId`] from the one table: vessels, wreck
//! pieces, missiles, torpedoes, mines, depth charges, drones, aircraft, helicopters, RHIBs, submarines and
//! decoys. Ballistic projectiles do not; they are spawn-ordered rows in `projectiles`.
//! Freed slots are reused LIFO. Every system iterates ascending slot index; snapshots and replays serialise
//! in that order. Iterating std `HashMap` / `HashSet` is banned in this crate (clippy `disallowed_types`).
//!
//! Generations start at 1, so `EntityId::default()` (index 0, generation 0) never names a live entity. A slot
//! whose generation would wrap is retired instead of reused, so a stale id can never alias a new one.
//! Every lookup tolerates inconsistent tables (they arrive through serde) and never panics.
use crate::bits::BitSet;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// Most slots one table (and one column) will ever hold. Spawns beyond it return [`EntityId::NONE`] and
/// column inserts beyond it are dropped, so a bogus id from the network cannot grow a column to gigabytes.
pub const MAX_ENTITY_SLOTS: u32 = 1 << 22;
/// Generation of a slot the first time it is used.
const FIRST_GENERATION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntityId { pub index: u32, pub generation: u32 }
impl EntityId {
    pub const NONE: EntityId = EntityId { index: u32::MAX, generation: 0 };
    pub fn is_none(self) -> bool { self.index == u32::MAX }
    /// Packed form for RNG keys and wire ids.
    pub fn to_u64(self) -> u64 { ((self.generation as u64) << 32) | self.index as u64 }
    pub fn from_u64(v: u64) -> Self { Self { index: v as u32, generation: (v >> 32) as u32 } }
}

/// What an entity is. Stored as a column so queries need not probe every store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EntityKind { Vessel, Wreck, Body, Decoy }

/// Slot allocator.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EntityTable {
    pub(crate) generations: Vec<u32>,
    pub(crate) alive: BitSet,
    pub(crate) free: Vec<u32>,
    pub(crate) kinds: Vec<Option<EntityKind>>,
}
impl EntityTable {
    pub fn new() -> Self { Self::default() }
    /// Allocate a slot (most recently freed first) and bump nothing; the generation was bumped at despawn.
    /// Unit: `sim-core-entity`.
    pub fn spawn(&mut self, kind: EntityKind) -> EntityId {
        while let Some(index) = self.free.pop() {
            let i = index as usize;
            // Skip corrupt free-list entries: out of range, still alive or retired.
            if i < self.generations.len() && !self.alive.get(i) && self.generations[i] != 0 {
                self.alive.set(i, true);
                self.set_kind(i, Some(kind));
                return EntityId { index, generation: self.generations[i] };
            }
        }
        let index = self.generations.len();
        if index >= MAX_ENTITY_SLOTS as usize { return EntityId::NONE; }
        self.generations.push(FIRST_GENERATION);
        self.alive.set(index, true);
        self.set_kind(index, Some(kind));
        EntityId { index: index as u32, generation: FIRST_GENERATION }
    }
    /// Free a slot and bump its generation. Returns false if the id was stale. Unit: `sim-core-entity`.
    pub fn despawn(&mut self, id: EntityId) -> bool {
        if !self.is_alive(id) { return false; }
        let i = id.index as usize;
        self.alive.set(i, false);
        self.set_kind(i, None);
        let next = self.generations[i].wrapping_add(1);
        if next == 0 {
            // Retired: generation 0 marks a slot that is never handed out again.
            self.generations[i] = 0;
        } else {
            self.generations[i] = next;
            self.free.push(id.index);
        }
        true
    }
    pub fn is_alive(&self, id: EntityId) -> bool {
        let i = id.index as usize;
        !id.is_none() && self.alive.get(i) && self.generations.get(i) == Some(&id.generation) && id.generation != 0
    }
    pub fn kind(&self, id: EntityId) -> Option<EntityKind> {
        if !self.is_alive(id) { return None; }
        self.kinds.get(id.index as usize).copied().flatten()
    }
    /// Live ids in ascending slot index. Unit: `sim-core-entity`.
    pub fn iter(&self) -> EntityIter<'_> { EntityIter { table: self, next: 0, kind: None } }
    /// Live ids of one kind in ascending slot index. Unit: `sim-core-entity`.
    pub fn iter_kind(&self, kind: EntityKind) -> EntityIter<'_> { EntityIter { table: self, next: 0, kind: Some(kind) } }
    /// Number of slots ever allocated (column length).
    pub fn capacity(&self) -> usize { self.generations.len() }
    pub fn len(&self) -> usize { self.alive.count_ones() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    /// The live id occupying a slot index, if any. Unit: `sim-core-entity`.
    pub fn id_at(&self, index: u32) -> Option<EntityId> {
        let i = index as usize;
        match self.generations.get(i) {
            Some(&generation) if generation != 0 && self.alive.get(i) => Some(EntityId { index, generation }),
            _ => None,
        }
    }

    fn set_kind(&mut self, i: usize, kind: Option<EntityKind>) {
        if self.kinds.len() <= i { self.kinds.resize(i + 1, None); }
        self.kinds[i] = kind;
    }
}

/// Ascending-slot iterator over live entities.
pub struct EntityIter<'a> { pub(crate) table: &'a EntityTable, pub(crate) next: u32, pub(crate) kind: Option<EntityKind> }
impl Iterator for EntityIter<'_> {
    type Item = EntityId;
    fn next(&mut self) -> Option<EntityId> {
        let t = self.table;
        let end = t.generations.len().min(t.alive.len);
        while (self.next as usize) < end {
            let i = self.next as usize;
            let word = t.alive.words.get(i / 64).copied().unwrap_or(0) >> (i % 64);
            if word == 0 {
                // Nothing alive in the rest of this word.
                self.next = ((i / 64 + 1) * 64) as u32;
                continue;
            }
            let j = i + word.trailing_zeros() as usize;
            if j >= end { break; }
            self.next = j as u32 + 1;
            let Some(id) = t.id_at(j as u32) else { continue };
            match self.kind {
                Some(k) if t.kinds.get(j).copied().flatten() != Some(k) => continue,
                _ => return Some(id),
            }
        }
        self.next = end as u32;
        None
    }
}

/// One component kind: a dense `Vec` indexed by slot plus an occupancy bitset (L1108).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Column<T> { pub(crate) slots: Vec<Option<T>>, pub(crate) occupied: BitSet }
impl<T> Default for Column<T> { fn default() -> Self { Self { slots: Vec::new(), occupied: BitSet::default() } } }
impl<T> Column<T> {
    pub fn new() -> Self { Self::default() }
    /// Insert or replace the component of a live entity. Unit: `sim-core-entity`.
    ///
    /// A `NONE` id or an index at or beyond [`MAX_ENTITY_SLOTS`] is ignored and the value dropped.
    pub fn insert(&mut self, id: EntityId, value: T) -> Option<T> {
        if id.is_none() || id.index >= MAX_ENTITY_SLOTS { return None; }
        let i = id.index as usize;
        if self.slots.len() <= i { self.slots.resize_with(i + 1, || None); }
        self.occupied.set(i, true);
        self.slots[i].replace(value)
    }
    pub fn remove(&mut self, id: EntityId) -> Option<T> {
        let i = id.index as usize;
        let old = self.slots.get_mut(i).and_then(Option::take);
        if i < self.occupied.len { self.occupied.set(i, false); }
        old
    }
    /// Access by slot index only; callers hold ids validated against the table this tick.
    pub fn get(&self, id: EntityId) -> Option<&T> { self.slots.get(id.index as usize).and_then(Option::as_ref) }
    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut T> { self.slots.get_mut(id.index as usize).and_then(Option::as_mut) }
    pub fn contains(&self, id: EntityId) -> bool { self.occupied.get(id.index as usize) }
    /// (slot index, component) in ascending slot index. Unit: `sim-core-entity`.
    pub fn iter(&self) -> ColumnIter<'_, T> { ColumnIter { col: self, next: 0 } }
    /// Mutable slice of occupied slots in ascending order, for rayon `par_iter_mut` over vessels; results are
    /// merged serially in this same order (L1114). Unit: `sim-core-entity`.
    pub fn values_mut(&mut self) -> Vec<(u32, &mut T)> {
        self.slots.iter_mut().enumerate().filter_map(|(i, s)| s.as_mut().map(|v| (i as u32, v))).collect()
    }
    pub fn len(&self) -> usize { self.occupied.count_ones() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}
pub struct ColumnIter<'a, T> { pub(crate) col: &'a Column<T>, pub(crate) next: usize }
impl<'a, T> Iterator for ColumnIter<'a, T> {
    type Item = (u32, &'a T);
    fn next(&mut self) -> Option<(u32, &'a T)> {
        let slots = &self.col.slots;
        while self.next < slots.len() {
            let i = self.next;
            self.next += 1;
            if let Some(v) = slots[i].as_ref() { return Some((i as u32, v)); }
        }
        None
    }
}

/// Hash of the iteration order after a scripted spawn and despawn sequence; the unit test asserts it (L1112).
/// Unit: `sim-core-entity`.
///
/// Folds (packed id, kind) of every live entity in iteration order through the frozen RNG absorption round,
/// then the count; a fixed integer hash, independent of std hashers.
pub fn iteration_order_hash(table: &EntityTable) -> u64 {
    let mut h = crate::rng::mix64(0x454E_5449_5459_5F31);
    let mut n = 0u64;
    for id in table.iter() {
        let kind = match table.kind(id) { Some(EntityKind::Vessel) => 1u64, Some(EntityKind::Wreck) => 2, Some(EntityKind::Body) => 3, Some(EntityKind::Decoy) => 4, None => 0 };
        h = crate::rng::absorb(h, id.to_u64());
        h = crate::rng::absorb(h, kind);
        n += 1;
    }
    crate::rng::mix64(crate::rng::absorb(h, n))
}
