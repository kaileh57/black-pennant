//! Unit `sys-ident`. Identification rungs and candidate lists; `stance.rs` (same unit) holds the stance rules.
//! DESIGN.md L470-477 (identification), L496-503 (stance).
//!
//! Each track holds a candidate list: every entry in the theatre order of battle, the civilian traffic list
//! and the own-side roster that is still consistent with the evidence. The rung is the finest level on which
//! all remaining candidates agree. Evidence:
//! - Domain: a detection that measures altitude or depth, or an emitter or acoustic match.
//! - Type: ESM emitter match (every Combine radar model is in the emitter library from the first operation);
//!   RCS and speed consistent with one type for 30 s; passive acoustic signature match.
//! - Class: ESM match to an emitter fitted to only one class; visual or camera sighting inside 50% of R_vis.
//! - Exact: visual or camera sighting inside 25% of R_vis; a completed Photograph action; a briefing naming
//!   the hull.
//!
//! Implementation notes:
//! - An empty `CandidateSet` means "not yet narrowed" (a fresh track): the whole pool. Evidence that would
//!   leave no candidate is ignored (it contradicts the pool, for example a design outside the briefing).
//! - The pool holds classes and munition types, not hulls, so [`rung_of`] stops at Class. Exact comes from
//!   explicit evidence and is remembered in the track history, which is also what restores the rung when an
//!   override is reverted.
//! - A detection carries what the sensor measured but not what the eye or the ear recognised. Visual and
//!   acoustic evidence therefore need the pool entry of the observed entity: [`apply_evidence_with`] takes it
//!   through a resolver ([`truth_candidate`] builds one from the `World`). [`apply_evidence`] without a
//!   resolver still raises the rung on a sighting (Class at 50% of R_vis, Exact at 25%) but cannot narrow
//!   the list by it.
//! - `Track::consistent_ticks` holds the tick (plus one) at which the RCS-and-speed rule first found the
//!   track consistent with a single type, 0 when it is not: the track filter does not keep the time of the
//!   previous update, so a start tick is the only exact way to measure the 30 s.
pub mod stance;

use crate::prelude::*;
use crate::protocol::config::SortieIntel;
use crate::protocol::snapshot::SourceMask;
use crate::sensors::radar::AIR_HEIGHT_M;
use crate::sensors::{Detection, Evidence, Measurement};
use crate::tracks::{RungStep, Track};
use content::campaign::{CampaignData, ClassRole};
use content::part::weapons::MunitionKind;
use content::part::{EngineKind, PartFunction, SensorKind};

/// One thing a track could be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub class: Option<ClassId>,
    /// Munition or small-body type for air and weapon tracks.
    pub munition: Option<MunitionIdx>,
    pub domain: Domain,
    /// "Type" level grouping (L473).
    pub role: Option<ClassRole>,
    pub civilian: bool,
    pub own_side: bool,
    /// Emitter library indices this candidate can radiate.
    pub emitters: Vec<u16>,
    /// Radar cross-section band in m^2 and top speed in m/s for the RCS-and-speed rule.
    pub rcs_band_m2: [f32; 2],
    pub max_speed_mps: f32,
    /// Acoustic signature key (propulsion type).
    pub acoustic: u8,
}

/// The candidate pool and emitter library of one sortie, built once from the briefing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IdentLibrary {
    pub pool: Vec<Candidate>,
    /// Emitter model keys (`SensorDef::emitter`), ascending; index = emitter id.
    pub emitters: Vec<String>,
    /// Emitter ids that count as "Combine radar" for the Hostile rule (L501).
    pub combine_emitters: Vec<u16>,
    /// "Type" key per pool entry: the role for classes, the munition kind for small bodies, the class itself
    /// for a class without a role. Rebuilt from `pool` when absent.
    #[serde(default)]
    pub(crate) type_keys: Vec<u32>,
}
impl IdentLibrary {
    /// Build the pool: order of battle classes, civilian list, own-side roster, plus every munition and
    /// small-body type in the build. Unit: `sys-ident`.
    ///
    /// Class facts come from each class's stock blueprint (emitters fitted, engines, tonnage); a class whose
    /// blueprint does not load keeps its role and domain, matches any RCS, and matches no emitter.
    /// "Combine radar" emitters are the search and fire-control radars fitted to any campaign class, less
    /// every emitter also fitted to a civilian class: a navigation radar, or a set civilian traffic carries,
    /// never makes a track Hostile on its own.
    pub fn build(intel: &SortieIntel, own_classes: &[ClassId], campaign: &CampaignData, catalogue: &Catalogue) -> IdentLibrary {
        let mut facts: BTreeMap<ClassId, ClassFacts> = BTreeMap::new();
        for id in campaign.classes.keys().chain(intel.order_of_battle.iter()).chain(intel.civilian.iter()).chain(own_classes.iter()) {
            if !facts.contains_key(id) { facts.insert(id.clone(), class_facts(id, campaign, catalogue)); }
        }
        build_core(intel, own_classes, &facts, catalogue)
    }
    pub fn emitter_index(&self, key: &str) -> u16 { self.emitters.binary_search_by(|e| e.as_str().cmp(key)).map(|i| i as u16).unwrap_or(u16::MAX) }

    fn type_key(&self, i: usize) -> u32 {
        if let Some(&k) = self.type_keys.get(i) { if self.type_keys.len() == self.pool.len() { return k; } }
        match self.pool.get(i) {
            Some(c) => type_key_of(c, i, &self.pool),
            None => u32::MAX,
        }
    }
    /// Pool entry of a class (the first one; own-side and order-of-battle entries of one class agree on every
    /// key the evidence compares).
    pub fn class_entry(&self, class: &ClassId) -> Option<u16> { self.pool.iter().position(|c| c.class.as_ref() == Some(class)).map(|i| i as u16) }
    /// Pool entry of a munition or small-body type.
    pub fn munition_entry(&self, m: MunitionIdx) -> Option<u16> { self.pool.iter().position(|c| c.munition == Some(m)).map(|i| i as u16) }
}

/// Fallback "Type" key of one candidate (see [`IdentLibrary::type_keys`]).
fn type_key_of(c: &Candidate, i: usize, pool: &[Candidate]) -> u32 {
    if let Some(r) = c.role { return 1 + r as u32; }
    if c.munition.is_some() { return 1_000 + c.munition.map(|m| m.0 as u32).unwrap_or(0); }
    let first = pool.iter().position(|p| p.class.is_some() && p.class == c.class).unwrap_or(i);
    100_000 + first as u32
}

/// What the pool needs to know about one class.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClassFacts {
    pub role: Option<ClassRole>,
    pub submersible: bool,
    /// (emitter key, sensor kind) of every emitting sensor fitted.
    pub emitters: Vec<(String, SensorKind)>,
    pub tonnage_t: Option<f64>,
    pub acoustic: u8,
}

/// Facts of one class from its campaign entry and stock blueprint.
fn class_facts(id: &ClassId, campaign: &CampaignData, catalogue: &Catalogue) -> ClassFacts {
    let Some(def) = campaign.classes.get(id) else { return ClassFacts::default() };
    let mut f = ClassFacts { role: Some(def.role), submersible: def.submersible, ..Default::default() };
    let Ok(bp) = content::campaign::load::stock_blueprint(&def.blueprint) else { return f };
    let mut engines: BTreeMap<u8, f64> = BTreeMap::new();
    for pp in &bp.parts {
        let Some(part) = catalogue.part(&pp.part) else { continue };
        match &part.function {
            PartFunction::Sensor(s) if !s.emitter.is_empty() => f.emitters.push((s.emitter.clone(), s.kind)),
            PartFunction::Engine(e) => { *engines.entry(engine_key(e.kind)).or_insert(0.0) += if e.shaft_kw.is_finite() { e.shaft_kw.max(0.0) } else { 0.0 }; }
            _ => {}
        }
    }
    f.emitters.sort();
    f.emitters.dedup();
    f.acoustic = engines.iter().fold((0u8, -1.0f64), |b, (&k, &kw)| if kw > b.1 { (k, kw) } else { b }).0;
    let t = content::stats::compute_stats(&bp, catalogue).tonnage_t;
    if t.is_finite() && t > 0.0 { f.tonnage_t = Some(t); }
    f
}

fn engine_key(k: EngineKind) -> u8 { match k { EngineKind::Diesel => 1, EngineKind::GasTurbine => 2, EngineKind::ElectricMotor => 3 } }

/// Acoustic keys of small bodies (classes use 1..=3, their dominant engine).
fn munition_acoustic(kind: MunitionKind) -> u8 {
    match kind { MunitionKind::Torpedo => 20, MunitionKind::Rhib => 21, MunitionKind::Helicopter => 22, _ => 0 }
}

/// Top speed per role in m/s, generous so that a hull at full power is never excluded.
fn role_top_speed(role: Option<ClassRole>) -> f32 {
    match role {
        Some(ClassRole::Submarine) => 11.0,
        Some(ClassRole::PatrolBoat) => 19.0,
        Some(ClassRole::FastAttack) => 24.0,
        Some(ClassRole::Corvette | ClassRole::Frigate | ClassRole::Capital) => 17.0,
        Some(ClassRole::Destroyer) => 19.0,
        Some(ClassRole::Picket) => 16.0,
        Some(ClassRole::Minesweeper | ClassRole::Freighter | ClassRole::Tanker) => 9.0,
        Some(ClassRole::Tender) => 10.0,
        Some(ClassRole::Civilian) => 14.0,
        Some(ClassRole::Drone) => 40.0,
        None => 30.0,
    }
}

/// Nominal hull RCS from tonnage: about 150 m^2 at 150 t and 7,000 m^2 at 1,800 t (L432), a power law
/// through both anchors.
pub(crate) fn hull_rcs_m2(tonnage_t: f64) -> f64 {
    let k = libm::log(7_000.0 / 150.0) / libm::log(1_800.0 / 150.0);
    150.0 * libm::pow((tonnage_t / 150.0).max(1e-3), k)
}
/// RCS band around a nominal value: aspect (bow-on to beam) and clutter spread the apparent RCS widely.
const HULL_RCS_BAND: [f64; 2] = [0.1, 3.0];
const BODY_RCS_BAND: [f64; 2] = [0.3, 3.0];

fn class_candidate(id: &ClassId, f: &ClassFacts, emitter_keys: &[String], civilian: bool, own_side: bool) -> Candidate {
    let mut emitters: Vec<u16> = f.emitters.iter().filter_map(|(k, _)| emitter_keys.binary_search(k).ok().map(|i| i as u16)).collect();
    emitters.sort_unstable();
    emitters.dedup();
    let rcs_band_m2 = match f.tonnage_t {
        Some(t) => { let n = hull_rcs_m2(t); [(n * HULL_RCS_BAND[0]) as f32, (n * HULL_RCS_BAND[1]) as f32] }
        None => [0.0, f32::MAX],
    };
    Candidate {
        class: Some(id.clone()), munition: None, domain: if f.submersible { Domain::Subsurface } else { Domain::Surface },
        role: f.role, civilian, own_side, emitters, rcs_band_m2, max_speed_mps: role_top_speed(f.role), acoustic: f.acoustic,
    }
}

/// World-free build over prepared class facts.
pub(crate) fn build_core(intel: &SortieIntel, own_classes: &[ClassId], facts: &BTreeMap<ClassId, ClassFacts>, catalogue: &Catalogue) -> IdentLibrary {
    let emitters = crate::signature::emitter_keys(catalogue);
    let blank = ClassFacts::default();
    let sorted = |v: &[ClassId]| { let mut v = v.to_vec(); v.sort(); v.dedup(); v };
    let mut pool: Vec<Candidate> = Vec::new();
    for id in sorted(&intel.order_of_battle) { pool.push(class_candidate(&id, facts.get(&id).unwrap_or(&blank), &emitters, false, false)); }
    for id in sorted(&intel.civilian) { pool.push(class_candidate(&id, facts.get(&id).unwrap_or(&blank), &emitters, true, false)); }
    for id in sorted(own_classes) { pool.push(class_candidate(&id, facts.get(&id).unwrap_or(&blank), &emitters, false, true)); }
    for (i, def) in catalogue.munitions.values().enumerate() {
        let mut em: Vec<u16> = def.sensors.iter().filter(|s| !s.emitter.is_empty()).filter_map(|s| emitters.binary_search(&s.emitter).ok().map(|i| i as u16)).collect();
        em.sort_unstable();
        em.dedup();
        let rcs = if def.rcs_m2.is_finite() && def.rcs_m2 > 0.0 { def.rcs_m2 } else { 0.5 };
        let speed = if def.speed_mps.is_finite() && def.speed_mps > 0.0 { def.speed_mps * 1.25 } else { 0.5 };
        pool.push(Candidate {
            class: None, munition: Some(MunitionIdx(i as u16)), domain: def.domain,
            role: if def.kind == MunitionKind::Drone { Some(ClassRole::Drone) } else { None }, civilian: false, own_side: false, emitters: em,
            rcs_band_m2: [(rcs * BODY_RCS_BAND[0]) as f32, (rcs * BODY_RCS_BAND[1]) as f32], max_speed_mps: speed as f32, acoustic: munition_acoustic(def.kind),
        });
    }
    pool.truncate(u16::MAX as usize - 1);
    let type_keys = (0..pool.len()).map(|i| match &pool[i] {
        c if c.munition.is_some() => 1_000 + catalogue.munitions.get_index(c.munition.map(|m| m.index()).unwrap_or(0)).map(|(_, d)| d.kind as u32).unwrap_or(0),
        c => type_key_of(c, i, &pool),
    }).collect();

    // Combine radars: search and fire-control emitters on campaign classes, less any a civilian class fits.
    let civilian_classes: BTreeSet<&ClassId> = intel.civilian.iter().collect();
    let mut military: BTreeSet<u16> = BTreeSet::new();
    let mut civil: BTreeSet<u16> = BTreeSet::new();
    for (id, f) in facts {
        let is_civil = civilian_classes.contains(id) || f.role == Some(ClassRole::Civilian);
        if own_classes.contains(id) && f.role.is_none() { continue; }
        for (k, kind) in &f.emitters {
            let Ok(i) = emitters.binary_search(k) else { continue };
            if is_civil { civil.insert(i as u16); }
            else if f.role.is_some() && matches!(kind, SensorKind::SearchRadar | SensorKind::FireControlRadar) { military.insert(i as u16); }
        }
    }
    let combine_emitters = military.difference(&civil).copied().collect();
    IdentLibrary { pool, emitters, combine_emitters, type_keys }
}

/// Indices into [`IdentLibrary::pool`] still consistent with a track's evidence, ascending.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSet { pub remaining: Vec<u16> }

// ------------------------------------------------------------------------------------------------------------
// Evidence
// ------------------------------------------------------------------------------------------------------------

/// The RCS-and-speed rule needs 30 s of consistency (L473).
const RCS_CONSISTENT_S: f64 = 30.0;
/// Weight of a new radar return in the apparent-RCS running mean.
const RCS_EMA: f64 = 0.25;
/// Speed allowance over a candidate's top speed.
const SPEED_MARGIN: f64 = 1.15;
/// Depth below which a measured height is underwater, metres.
const UNDERWATER_M: f64 = -2.0;
/// History rows kept per track.
const HISTORY_CAP: usize = 16;

fn is_datalink_friend(t: &Track) -> bool { t.stance == Stance::Friend && t.stance_reason == StanceReason::Datalink && t.sources.contains(SourceMask::DATALINK) }

fn pool_len(lib: &IdentLibrary) -> usize { lib.pool.len().min(u16::MAX as usize) }

/// Initialise an unnarrowed set to the whole pool and drop bad indices.
fn prepare(set: &mut CandidateSet, lib: &IdentLibrary) {
    let n = pool_len(lib);
    if set.remaining.is_empty() { set.remaining = (0..n as u16).collect(); return; }
    set.remaining.retain(|&i| (i as usize) < n);
    set.remaining.sort_unstable();
    set.remaining.dedup();
    if set.remaining.is_empty() { set.remaining = (0..n as u16).collect(); }
}

/// Keep candidates passing `keep`; refuses (returns false) when none would remain.
fn narrow(set: &mut CandidateSet, lib: &IdentLibrary, keep: impl Fn(usize, &Candidate) -> bool) -> bool {
    let kept: Vec<u16> = set.remaining.iter().copied().filter(|&i| lib.pool.get(i as usize).is_some_and(|c| keep(i as usize, c))).collect();
    if kept.is_empty() { return false; }
    let changed = kept.len() != set.remaining.len();
    set.remaining = kept;
    changed
}

fn class_key(c: &Candidate) -> (Option<&ClassId>, Option<MunitionIdx>) { (c.class.as_ref(), c.munition) }

/// The rung on which all remaining candidates agree. Unit: `sys-ident`.
pub fn rung_of(set: &CandidateSet, lib: &IdentLibrary) -> IdRung {
    let n = pool_len(lib);
    let idx: Vec<usize> = if set.remaining.is_empty() { (0..n).collect() } else { set.remaining.iter().map(|&i| i as usize).filter(|&i| i < n).collect() };
    let Some((&first, rest)) = idx.split_first() else { return IdRung::Unknown };
    let f = &lib.pool[first];
    let (fk, ft) = (class_key(f), lib.type_key(first));
    let (mut typ, mut cls) = (true, true);
    for &i in rest {
        let c = &lib.pool[i];
        if c.domain != f.domain { return IdRung::Unknown; }
        if typ && lib.type_key(i) != ft { typ = false; }
        if cls && class_key(c) != fk { cls = false; }
    }
    if !typ { IdRung::Domain } else if !cls { IdRung::Type } else { IdRung::Class }
}

/// The class all remaining candidates agree on, if any.
fn agreed_class(set: &CandidateSet, lib: &IdentLibrary) -> Option<ClassId> {
    let mut it = set.remaining.iter().filter_map(|&i| lib.pool.get(i as usize));
    let first = it.next()?.class.clone()?;
    if it.all(|c| c.class.as_ref() == Some(&first)) { Some(first) } else { None }
}

/// Measured height of a detection, metres (positive altitude, negative depth).
fn measured_height(d: &Detection) -> Option<f64> {
    match d.measurement { Measurement::RangeBearing { height_m: Some(h), .. } if h.is_finite() => Some(h), _ => None }
}

fn is_radar(k: SensorKind) -> bool { matches!(k, SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar | SensorKind::Seeker) }

/// Whether a candidate can be at a measured height band.
fn height_fits(c: &Candidate, h: Option<f64>, depth_flag: bool) -> bool {
    let underwater = depth_flag || h.is_some_and(|h| h < UNDERWATER_M);
    if underwater { return matches!(c.domain, Domain::Subsurface | Domain::Weapon); }
    match h {
        Some(h) if h >= AIR_HEIGHT_M => matches!(c.domain, Domain::Air | Domain::Weapon),
        _ => c.domain != Domain::Subsurface,
    }
}

/// The evidence rung a track holds apart from overrides: the set's agreement or the best evidence step in its
/// history, with the reason of that step.
fn evidence_rung(track: &Track, lib: &IdentLibrary) -> (IdRung, RungReason) {
    let from_set = rung_of(&track.candidates, lib);
    let best = track.history.iter().filter(|s| s.reason != RungReason::PlayerOverride).fold(None::<&RungStep>, |b, s| match b { Some(b) if b.rung >= s.rung => Some(b), _ => Some(s) });
    match best {
        Some(s) if s.rung >= from_set => (s.rung, s.reason),
        _ => (from_set, if from_set == IdRung::Unknown { RungReason::None } else { best.map(|s| s.reason).unwrap_or(RungReason::None) }),
    }
}

fn push_history(track: &mut Track, step: RungStep) {
    if track.history.last().is_some_and(|l| l.rung == step.rung && l.reason == step.reason) { return; }
    track.history.push(step);
    if track.history.len() > HISTORY_CAP { let over = track.history.len() - HISTORY_CAP; track.history.drain(0..over); }
}

/// Narrow a track's candidates with the evidence of the detections just fused into it and recompute its rung.
/// Returns the new rung and what moved it when it rose. Rungs never fall while the track lives, except
/// through a player or graph override being reverted. Unit: `sys-ident`.
///
/// Without a truth resolver: sightings raise the rung but cannot name what was seen; use
/// [`apply_evidence_with`] where the `World` is at hand.
pub fn apply_evidence(track: &mut Track, detections: &[Detection], lib: &IdentLibrary, ctx: &SimCtx<'_>) -> Option<(IdRung, RungReason)> {
    apply_evidence_with(track, detections, lib, ctx, &|_| None)
}

/// Pool entry of the entity a detection saw, for visual and acoustic recognition: the class of a vessel, the
/// munition type of a small body. `None` for unclassed hulls (player designs) and unknown entities.
pub fn truth_candidate(world: &World, entity: EntityId, lib: &IdentLibrary) -> Option<u16> {
    if let Some(v) = world.vessel(entity) { return v.class.as_ref().and_then(|c| lib.class_entry(c)); }
    let b = world.bodies();
    b.row_of(entity).and_then(|i| b.kind.get(i)).and_then(|&k| lib.munition_entry(k))
}

/// [`apply_evidence`] with a resolver from the observed entity to its pool entry (see [`truth_candidate`]),
/// so a sighting narrows the list to what was seen and a passive acoustic match to its propulsion type.
pub fn apply_evidence_with(track: &mut Track, detections: &[Detection], lib: &IdentLibrary, ctx: &SimCtx<'_>, truth: &dyn Fn(EntityId) -> Option<u16>) -> Option<(IdRung, RungReason)> {
    if is_datalink_friend(track) || lib.pool.is_empty() { return None; }
    let tick = ctx.tick.0;
    prepare(&mut track.candidates, lib);
    let overridden = track.id_override.is_some();
    // The track filter re-classified the domain (a fresh surface track turning out to be a sea-skimmer):
    // when no candidate of the new domain remains, re-seed from the pool under the emitter evidence.
    let dom = track.domain;
    if dom != Domain::Surface && !track.candidates.remaining.iter().any(|&i| lib.pool.get(i as usize).is_some_and(|c| c.domain == dom)) {
        let mut set = CandidateSet { remaining: (0..pool_len(lib) as u16).filter(|&i| lib.pool[i as usize].domain == dom).collect() };
        for e in &track.emitters { narrow(&mut set, lib, |_, c| c.emitters.binary_search(e).is_ok()); }
        if !set.remaining.is_empty() { track.candidates = set; track.consistent_ticks = 0; }
    }
    let (old, _) = if overridden { evidence_rung(track, lib) } else { (track.rung, track.rung_reason) };

    // Each applied step: the rung it implies and its reason.
    let mut steps: Vec<(IdRung, RungReason)> = Vec::new();
    let mut floor = IdRung::Unknown;
    let mut seen_class: Option<ClassId> = None;
    let rung_now = |t: &Track, floor: IdRung| rung_of(&t.candidates, lib).max(floor);

    // 1. Emitters heard (ESM, or a ping heard by sonar). Re-filter on every heard emitter: merges union them.
    let mut heard = false;
    for d in detections {
        if d.evidence.contains(Evidence::EMITTER) && (d.emitter as usize) < lib.emitters.len() {
            if let Err(i) = track.emitters.binary_search(&d.emitter) { track.emitters.insert(i, d.emitter); }
            heard = true;
        }
    }
    if !track.emitters.is_empty() {
        // One emitter at a time: one that fits no remaining candidate (a seeker heard on the launcher's
        // bearing) is skipped instead of blocking the rest.
        for e in track.emitters.clone() { narrow(&mut track.candidates, lib, |_, c| c.emitters.binary_search(&e).is_ok()); }
        if heard {
            let r = rung_now(track, floor);
            steps.push((r, if r >= IdRung::Class { RungReason::EmitterUniqueToClass } else { RungReason::EmitterMatch }));
        }
    }

    // 2. Measured altitude or depth: the domain, as the track filter classified it, within the height band.
    for d in detections {
        let h = measured_height(d);
        let depth = d.evidence.contains(Evidence::DEPTH);
        if !(depth || (d.evidence.contains(Evidence::ALTITUDE) && h.is_some())) { continue; }
        let dom = track.domain;
        if !narrow(&mut track.candidates, lib, |_, c| height_fits(c, h, depth) && c.domain == dom) {
            narrow(&mut track.candidates, lib, |_, c| height_fits(c, h, depth));
        }
        let underwater = depth || h.is_some_and(|h| h < UNDERWATER_M);
        steps.push((rung_now(track, floor), if underwater { RungReason::DepthMeasured } else { RungReason::AltitudeMeasured }));
    }

    // 3. Passive acoustic signature: the propulsion type of what was heard.
    for d in detections {
        if !d.evidence.contains(Evidence::ACOUSTIC) { continue; }
        if let Some(c) = truth(d.target).and_then(|i| lib.pool.get(i as usize)) {
            if c.acoustic != 0 {
                let key = c.acoustic;
                narrow(&mut track.candidates, lib, |_, c| c.acoustic == key);
            }
        }
        steps.push((rung_now(track, floor), RungReason::AcousticMatch));
    }

    // 4. Radar cross-section and speed consistent with one type for 30 s.
    for d in detections {
        if !is_radar(d.kind) || !(d.rcs_m2.is_finite() && d.rcs_m2 > 0.0) { continue; }
        let cur = track.rcs_m2 as f64;
        let next = if cur.is_finite() && cur > 0.0 { cur + RCS_EMA * (d.rcs_m2 as f64 - cur) } else { d.rcs_m2 as f64 };
        track.rcs_m2 = next.clamp(0.0, f32::MAX as f64) as f32;
    }
    if track.rcs_m2.is_finite() && track.rcs_m2 > 0.0 && track.bearing_only.is_none() {
        let rcs = track.rcs_m2;
        let v = DVec2::new(track.vel.x, track.vel.z).length();
        let sv = if track.sigma_v.is_finite() { track.sigma_v.abs() } else { 0.0 };
        let speed_lb = if v.is_finite() { (v - 2.0 * sv).max(0.0) } else { 0.0 };
        let fits = |c: &Candidate| rcs >= c.rcs_band_m2[0] && rcs <= c.rcs_band_m2[1] && (c.max_speed_mps as f64) * SPEED_MARGIN >= speed_lb;
        let consistent: Vec<u16> = track.candidates.remaining.iter().copied().filter(|&i| lib.pool.get(i as usize).is_some_and(fits)).collect();
        let one_type = consistent.first().is_some_and(|&f| { let k = lib.type_key(f as usize); consistent.iter().all(|&i| lib.type_key(i as usize) == k) });
        if one_type {
            if track.consistent_ticks == 0 { track.consistent_ticks = (tick.min(u32::MAX as u64 - 1) + 1) as u32; }
            let since = track.consistent_ticks as u64 - 1;
            let need = (RCS_CONSISTENT_S * sim_core::TICK_HZ as f64) as u64;
            if tick.saturating_sub(since) >= need && consistent.len() < track.candidates.remaining.len() {
                track.candidates.remaining = consistent;
                steps.push((rung_now(track, floor), RungReason::RcsAndSpeed));
            }
        } else {
            track.consistent_ticks = 0;
        }
    }

    // 5. Visual or camera sighting: Class inside 50% of R_vis, Exact inside 25%.
    for d in detections {
        if !matches!(d.kind, SensorKind::Visual | SensorKind::Camera) || !d.evidence.contains(Evidence::VISUAL_HALF) { continue; }
        let close = d.evidence.contains(Evidence::VISUAL_QUARTER);
        if let Some(i) = truth(d.target) {
            if let Some(seen) = lib.pool.get(i as usize) {
                let key = (seen.class.clone(), seen.munition);
                narrow(&mut track.candidates, lib, |_, c| c.class == key.0 && c.munition == key.1);
                if seen.class.is_some() { seen_class = seen.class.clone(); }
            }
        }
        floor = floor.max(if close { IdRung::Exact } else { IdRung::Class });
        steps.push((rung_now(track, floor), if close { RungReason::CloseVisualSighting } else { RungReason::VisualSighting }));
    }

    let new = steps.iter().map(|s| s.0).max().unwrap_or(IdRung::Unknown).max(rung_of(&track.candidates, lib));
    if !overridden && track.rung.max(new) >= IdRung::Class {
        if let Some(c) = agreed_class(&track.candidates, lib).or_else(|| seen_class.clone()) { track.class = Some(c); }
    }
    if new <= old { return None; }
    let reason = steps.iter().find(|s| s.0 == new).map(|s| s.1).or_else(|| steps.last().map(|s| s.1)).unwrap_or(RungReason::None);
    push_history(track, RungStep { rung: new, reason, tick });
    if overridden { return None; }
    track.rung = new;
    track.rung_reason = reason;
    Some((new, reason))
}

/// Raise a track straight to Exact: completed Photograph action or a briefing-named hull (L475).
/// Unit: `sys-ident`.
pub fn set_exact(track: &mut Track, class: Option<ClassId>, reason: RungReason, lib: &IdentLibrary, tick: u64) {
    if is_datalink_friend(track) { return; }
    prepare(&mut track.candidates, lib);
    let class = class.or_else(|| track.class.clone());
    if let Some(c) = &class { narrow(&mut track.candidates, lib, |_, p| p.class.as_ref() == Some(c)); }
    push_history(track, RungStep { rung: IdRung::Exact, reason, tick });
    if track.id_override.is_some() { return; }
    track.rung = IdRung::Exact;
    track.rung_reason = reason;
    if class.is_some() { track.class = class; }
}

/// Apply or clear a manual identification override (sink "manual stance and ID override per track", L347).
/// Unit: `sys-ident`.
///
/// A datalink Friend is never overridden (its identity is exact from the link). Clearing restores the rung
/// the evidence holds: the agreement of the candidate list or the best evidence step in the history.
pub fn set_override(track: &mut Track, value: Option<(IdRung, Option<ClassId>)>, lib: &IdentLibrary, tick: u64) -> (IdRung, RungReason) {
    if is_datalink_friend(track) { return (track.rung, track.rung_reason); }
    match value {
        Some((rung, class)) => {
            track.id_override = Some((rung, class.clone()));
            track.rung = rung;
            track.rung_reason = RungReason::PlayerOverride;
            track.class = if rung >= IdRung::Class { class.or_else(|| track.class.clone()) } else { None };
            push_history(track, RungStep { rung, reason: RungReason::PlayerOverride, tick });
        }
        None => {
            if track.id_override.take().is_none() { return (track.rung, track.rung_reason); }
            let (rung, reason) = evidence_rung(track, lib);
            track.rung = rung;
            track.rung_reason = reason;
            track.class = if rung >= IdRung::Class { agreed_class(&track.candidates, lib) } else { None };
            push_history(track, RungStep { rung, reason, tick });
        }
    }
    (track.rung, track.rung_reason)
}

#[cfg(test)]
mod tests;
