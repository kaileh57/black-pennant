//! Part of unit `gd-session`. Filling `Services::designs`, the store the 3D renderer resolves a revealed
//! entity's design hash from.
//!
//! Decision: designs for drawing are built on the sim pool (`pool.spawn`), not on the background worker.
//! The worker's results are drained by `BpEditor` once per frame, so a `JobResult::BuiltReceived` there would
//! never reach this session; and the pool is idle exactly when most builds happen (while the clock thread
//! builds the world during loading). Each build is the same prefab and `ShipDesign` the simulation makes, so
//! the hull the renderer draws is cell for cell the hull the simulation damages.
//!
//! Sources, tried in this order for a hash the store lacks: the session's own configs at sortie start (the
//! player's design, every stock class the config names), the link's blueprint store
//! (`ClientLink::blueprint`, full or exterior-only form as the reveal says), and the stock designs by hash.
//! A hash that resolves nowhere is remembered and not asked for again this session.
use crate::worker::ReceivedDesign;
use content::blueprint::{Blueprint, Shareable};
use content::ids::ContentHash;
use content::ruleset::Ruleset;
use content::share::PackedBlueprint;
use sim_systems::ship::ShipDesign;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};

/// What a design is built from.
pub(crate) enum DesignSource {
    Blueprint(Arc<Blueprint>),
    /// A stock path (`content::campaign::load::stock_blueprint`).
    Stock(String),
    /// A stock design known only by its hash.
    StockHash(ContentHash),
    /// Bytes from the link's blueprint store.
    Packed { packed: PackedBlueprint, exterior_only: bool },
}

struct Built { generation: u64, asked: Option<ContentHash>, result: Result<(ContentHash, ReceivedDesign), String> }

/// Builds in flight and their results.
pub(crate) struct DesignBuilds {
    tx: Sender<Built>,
    rx: Receiver<Built>,
    /// Hashes asked for and not yet back.
    pending: BTreeSet<ContentHash>,
    /// Builds whose hash is only known once built (a stock path).
    unkeyed: usize,
    /// Hashes that resolved nowhere this session.
    failed: BTreeSet<ContentHash>,
    /// Bumped by `clear`; results of an older generation are dropped.
    generation: u64,
    /// Builds started and finished since the last `clear`, for the loading progress.
    pub(crate) started: usize,
    pub(crate) finished: usize,
}

impl Default for DesignBuilds {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx, pending: BTreeSet::new(), unkeyed: 0, failed: BTreeSet::new(), generation: 0, started: 0, finished: 0 }
    }
}

impl DesignBuilds {
    /// True when `hash` is worth asking for: not empty, not stored, not in flight, not failed.
    pub(crate) fn wants(&self, hash: &ContentHash, stored: &BTreeMap<ContentHash, ReceivedDesign>) -> bool {
        !hash.is_empty() && !stored.contains_key(hash) && !self.pending.contains(hash) && !self.failed.contains(hash)
    }
    /// Builds not yet back.
    pub(crate) fn outstanding(&self) -> usize { self.pending.len() + self.unkeyed }

    /// Start one build on `pool`. `hash` is the hash the caller expects (`None` for a stock path).
    pub(crate) fn request(&mut self, pool: &rayon::ThreadPool, hash: Option<ContentHash>, source: DesignSource, ruleset: Ruleset) {
        match hash {
            Some(h) => { if !self.pending.insert(h) { return; } }
            None => self.unkeyed += 1,
        }
        self.started += 1;
        let tx = self.tx.clone();
        let generation = self.generation;
        pool.spawn(move || {
            let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(source, &ruleset))) {
                Ok(r) => r,
                Err(_) => Err("the design build panicked".to_string()),
            };
            let _ = tx.send(Built { generation, asked: hash, result });
        });
    }

    /// Move finished builds into `stored`. Returns how many arrived.
    pub(crate) fn poll(&mut self, stored: &mut BTreeMap<ContentHash, ReceivedDesign>) -> usize {
        let mut n = 0;
        while let Ok(built) = self.rx.try_recv() {
            if built.generation != self.generation { continue; }
            match built.asked { Some(h) => { self.pending.remove(&h); } None => self.unkeyed = self.unkeyed.saturating_sub(1) }
            self.finished += 1;
            match built.result {
                Ok((hash, design)) => {
                    if let Some(asked) = built.asked { if asked != hash { self.failed.insert(asked); } }
                    stored.entry(hash).or_insert(design);
                    n += 1;
                }
                Err(_) => { if let Some(h) = built.asked { self.failed.insert(h); } }
            }
        }
        n
    }

    /// Forget everything (session over); builds still running finish into nothing.
    pub(crate) fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
        self.unkeyed = 0;
        self.failed.clear();
        self.started = 0;
        self.finished = 0;
    }
}

/// The hash a vessel built from `bp` carries (`sim_core::vessel` uses the stored hash, else computes it).
pub(crate) fn blueprint_hash(bp: &Blueprint) -> ContentHash { if bp.header.hash.is_empty() { content::blueprint::io::compute_hash(bp) } else { bp.header.hash } }

fn full(bp: Arc<Blueprint>, ruleset: &Ruleset) -> Result<(ContentHash, ReceivedDesign), String> {
    let catalogue = content::catalogue::catalogue();
    let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, catalogue));
    let design = Arc::new(ShipDesign::build(&bp, prefab, catalogue, ruleset));
    Ok((design.hash, ReceivedDesign::Full { design, blueprint: bp }))
}

/// Every stock blueprint by its hash, read once per process (on a pool thread).
fn stock_index() -> &'static BTreeMap<ContentHash, String> {
    static INDEX: OnceLock<BTreeMap<ContentHash, String>> = OnceLock::new();
    INDEX.get_or_init(|| {
        content::data::stock_blueprint_files()
            .filter_map(|(file, _)| content::campaign::load::stock_blueprint(file).ok().map(|bp| (blueprint_hash(&bp), file.to_string())))
            .collect()
    })
}

fn build(source: DesignSource, ruleset: &Ruleset) -> Result<(ContentHash, ReceivedDesign), String> {
    match source {
        DesignSource::Blueprint(bp) => full(bp, ruleset),
        DesignSource::Stock(path) => {
            let bp = content::campaign::load::stock_blueprint(&path).map_err(|e| e.to_string())?;
            full(Arc::new(bp), ruleset)
        }
        DesignSource::StockHash(hash) => {
            let path = stock_index().get(&hash).ok_or_else(|| "not a stock design".to_string())?;
            let bp = content::campaign::load::stock_blueprint(path).map_err(|e| e.to_string())?;
            full(Arc::new(bp), ruleset)
        }
        DesignSource::Packed { packed, exterior_only: true } => {
            let ext = net::blueprints::unpack_exterior(&packed).map_err(|e| format!("{e:?}"))?;
            Ok((packed.hash, ReceivedDesign::Exterior(Arc::new(ext))))
        }
        DesignSource::Packed { packed, exterior_only: false } => match content::share::unpack(&packed).map_err(|e| e.to_string())? {
            Shareable::Ship(bp) => full(Arc::new(*bp), ruleset),
            Shareable::Kit(_) => Err("a kit is not a ship".to_string()),
        },
    }
}
