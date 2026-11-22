//! Contract-owned (no work unit). A plain growable bitset, LSB first, 64 bits per word. Iteration is ascending.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitSet { pub words: Vec<u64>, pub len: usize }
impl BitSet {
    pub fn new(len: usize) -> Self { Self { words: vec![0; len.div_ceil(64)], len } }
    pub fn get(&self, i: usize) -> bool { i < self.len && (self.words[i / 64] >> (i % 64)) & 1 == 1 }
    pub fn set(&mut self, i: usize, v: bool) {
        if i >= self.len { self.len = i + 1; self.words.resize(self.len.div_ceil(64), 0); }
        if v { self.words[i / 64] |= 1 << (i % 64); } else { self.words[i / 64] &= !(1 << (i % 64)); }
    }
    pub fn clear(&mut self) { self.words.iter_mut().for_each(|w| *w = 0); }
    pub fn count_ones(&self) -> usize { self.words.iter().map(|w| w.count_ones() as usize).sum() }
    /// Ascending indices of set bits.
    pub fn iter_ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(wi, &w)| {
            let mut w = w;
            core::iter::from_fn(move || { if w == 0 { None } else { let b = w.trailing_zeros() as usize; w &= w - 1; Some(wi * 64 + b) } })
        })
    }
}
