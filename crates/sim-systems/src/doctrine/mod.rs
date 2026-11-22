//! Unit `sys-doctrine`. Engagement doctrine tables, their inheritance and the default effective ranges.
//! DESIGN.md L479-494.
//!
//! One entry per (weapon, target category); defaults set by the editor; inheritance fleet, then ship, then
//! mount. Defaults:
//! - anti-ship missiles: Hostile, ID Type or better, grade Coarse or better, 2 per salvo; hold while a Neutral
//!   track lies inside the seeker search area around the predicted target position;
//! - guns: Hostile, Fine or better, inside effective range; Suspect only inside self-defence range;
//! - SAM: Hostile air tracks, Fine or better, inside effective range, 1 per target, a second after a miss;
//! - point defence (guns and missiles, grade Lock): inbound weapons regardless of launcher;
//! - chaff and flares: when an inbound missile is inside 6 km, 1 salvo per missile, 10 s reload; towed decoys
//!   stream while a torpedo or anti-ship missile track is inbound;
//! - never auto-engage Neutral or Friend.
//!
//! Default effective range = the Punch gauge's: the range where predicted single-round Pk against a 40 m
//! reference hull under fire-control lock in Beaufort 4 is at least 10%, clamped to the class caps (autocannon
//! 3 km, 57-76 mm 8 km, 100-127 mm 12 km, heavy 18 km, anti-ship missile 80% of motor range, SAM 80% of
//! kinematic range, point-defence guns 2 km, point-defence missiles 8 km). Self-defence range: one value per
//! ship, default 4 km; a mount with a shorter effective range uses its own.
//!
//! The predicted Pk is `guidance::gun::single_round_pk`: the errors the officers' solver really has under a
//! steady fire-control lock (k2). Inside a default entry's range the Gunnery officer also holds a gun until the
//! same criterion holds against the contact itself (`crate::engage::hit_chance`).
use crate::prelude::*;
use crate::tracks::Track;
use content::graph::{DoctrineEntry, MountLimits, OfficerSettings, TargetCategory, WeaponClass};

/// Static facts about one weapon mount, launcher or decoy launcher, derived once per design.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MountInfo {
    pub slot: PartSlot,
    pub part: PartIdx,
    pub weapon: WeaponClass,
    pub munition: Option<MunitionIdx>,
    /// Pivot or launch point in vessel-local metres.
    pub pos_local: DVec3,
    /// Yaw of the mount's zero bearing relative to the bow, radians (part rotation).
    pub zero_rad: f64,
    /// Limits in force: hand-set from `OfficerSettings::mount_limits`, else auto-set by ray sweep (L205).
    pub limits: MountLimits,
    /// Solver-derived effective range before doctrine overrides, metres.
    pub effective_range_m: f64,
    /// Fire-control radar or director serving the mount through its kit, if any.
    pub director: Option<PartSlot>,
    pub kit: Option<KitSlot>,
    /// Weapon group for takeover (mounts of one kit type share a group), 0-based.
    pub group: u8,
    pub muzzle_mps: f64,
    pub point_defence: bool,
}

/// A doctrine entry after inheritance, with its effective range filled in.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedEntry { pub entry: DoctrineEntry, pub effective_range_m: f64, /// Which level supplied it: 0 fleet, 1 ship, 2 mount.
    pub level: u8 }

/// The full doctrine table per mount (expert view at L407).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DoctrineTable {
    /// Keyed (mount slot, target category).
    pub entries: BTreeMap<(PartSlot, TargetCategory), ResolvedEntry>,
    pub self_defence_range_m: f64,
}
impl DoctrineTable { pub fn get(&self, slot: PartSlot, cat: TargetCategory) -> Option<&ResolvedEntry> { self.entries.get(&(slot, cat)) } }

/// The spec's default entries for a preset (the list above; `aggressive` lowers grade and ID requirements one
/// step and raises salvo size, `silent running` holds everything but point defence; exact preset deltas are
/// defined here and documented in the L0 preset table of `crate::officers::tuning`). Unit: `sys-doctrine`.
///
/// | Weapon | Target | Cautious (spec list) | Aggressive | Silent running |
/// | anti-ship missile | Surface | Hostile, Type, Coarse, 2 | Hostile, Domain, Bearing, 3 | held (salvo 0) |
/// | gun | Surface, Air | Hostile, Unknown, Fine (Suspect inside self-defence range) | grade Coarse | held |
/// | SAM | Air | Hostile, Unknown, Fine, 1 (a second after a miss) | grade Coarse, 2 | held |
/// | PD gun, PD missile | InboundWeapon | Suspect, Unknown, Lock, 1 | unchanged | unchanged |
/// | PD gun, PD missile | Air | Hostile, Unknown, Lock, 1 | unchanged | unchanged |
/// | torpedo | Surface | Hostile, Type, Fine, 1 | Domain, Coarse, 2 | held |
/// | torpedo | Subsurface | Hostile, Domain, Coarse, 1 | Unknown, Bearing, 2 | held |
/// | depth charge | Subsurface | Hostile, Domain, Coarse, 2 | Unknown, Bearing, 3 | held |
///
/// A salvo of 0 is the hold encoding: [`check`] answers `Holding: PermissionWithheld` for it, so a held
/// entry still shadows fleet entries through inheritance. Point defence is never weakened or held by a preset.
/// Inbound weapons need only Suspect because a weapon track keeps the default stance until something
/// identifies its launcher ("whatever launched them", L486); Neutral and Friend weapon tracks are never
/// engaged (L487). Decoy launchers have no entries: decoy control fires them (`crate::engage`).
pub fn default_entries(preset: DoctrinePreset) -> Vec<DoctrineEntry> {
    use TargetCategory as T;
    use WeaponClass as W;
    let e = |weapon, target, min_stance, min_id, min_grade, salvo| DoctrineEntry { weapon, target, min_stance, min_id, min_grade, salvo, effective_range_m: None };
    let cautious = [
        e(W::AntiShipMissile, T::Surface, Stance::Hostile, IdRung::Type, TrackGrade::Coarse, 2),
        e(W::Gun, T::Surface, Stance::Hostile, IdRung::Unknown, TrackGrade::Fine, 1),
        e(W::Gun, T::Air, Stance::Hostile, IdRung::Unknown, TrackGrade::Fine, 1),
        e(W::Sam, T::Air, Stance::Hostile, IdRung::Unknown, TrackGrade::Fine, 1),
        e(W::PointDefenceGun, T::InboundWeapon, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, 1),
        e(W::PointDefenceGun, T::Air, Stance::Hostile, IdRung::Unknown, TrackGrade::Lock, 1),
        e(W::PointDefenceMissile, T::InboundWeapon, Stance::Suspect, IdRung::Unknown, TrackGrade::Lock, 1),
        e(W::PointDefenceMissile, T::Air, Stance::Hostile, IdRung::Unknown, TrackGrade::Lock, 1),
        e(W::Torpedo, T::Surface, Stance::Hostile, IdRung::Type, TrackGrade::Fine, 1),
        e(W::Torpedo, T::Subsurface, Stance::Hostile, IdRung::Domain, TrackGrade::Coarse, 1),
        e(W::DepthCharge, T::Subsurface, Stance::Hostile, IdRung::Domain, TrackGrade::Coarse, 2),
    ];
    let point_defence = |w: WeaponClass| matches!(w, W::PointDefenceGun | W::PointDefenceMissile);
    cautious.iter().map(|&c| {
        if point_defence(c.weapon) { return c; }
        match preset {
            DoctrinePreset::Cautious => c,
            DoctrinePreset::Aggressive => DoctrineEntry {
                min_id: rung_down(c.min_id),
                min_grade: grade_down(c.min_grade),
                // Guns fire continuously while engaging; the salvo raise applies to guided weapons.
                salvo: if c.weapon == W::Gun { c.salvo } else { c.salvo.saturating_add(1) },
                ..c
            },
            DoctrinePreset::SilentRunning => DoctrineEntry { salvo: 0, ..c },
        }
    }).collect()
}

fn rung_down(r: IdRung) -> IdRung { match r { IdRung::Exact => IdRung::Class, IdRung::Class => IdRung::Type, IdRung::Type => IdRung::Domain, IdRung::Domain | IdRung::Unknown => IdRung::Unknown } }
fn grade_down(g: TrackGrade) -> TrackGrade { match g { TrackGrade::Lock => TrackGrade::Fine, TrackGrade::Fine => TrackGrade::Coarse, TrackGrade::Coarse | TrackGrade::Bearing => TrackGrade::Bearing } }

/// Length and height of the reference hull the effective range is solved against (L489). The height is
/// the hull's freeboard plus superstructure seen broadside; the beam is `guidance::gun::REFERENCE_BEAM_FRACTION`
/// of the length. Shared with the Punch gauge.
pub const REFERENCE_HULL_LENGTH_M: f64 = 40.0;
pub const REFERENCE_HULL_HEIGHT_M: f64 = 6.0;
/// Largest salvo any doctrine entry may ask for; player and network entries are clamped to it.
pub const MAX_SALVO: u8 = 16;

fn is_point_defence(w: WeaponClass) -> bool { matches!(w, WeaponClass::PointDefenceGun | WeaponClass::PointDefenceMissile) }
fn finite_pos(v: f64) -> Option<f64> { (v.is_finite() && v >= 0.0).then_some(v) }

/// Resolve inheritance: fleet entries (AI groups, doctrine cards), then ship settings, then per-mount
/// overrides and per-mount presets. Unit: `sys-doctrine`.
///
/// Order per mount, later wins for the same target category: the ship preset's [`default_entries`] (reported
/// as level 1), fleet entries (level 0), the ship's own entries `OfficerSettings::doctrine` (level 1), the
/// mount's preset `mount_presets` (level 2), then the mount's entries `mount_doctrine` (level 2). Fleet and ship
/// entries apply to a mount when their weapon class matches the mount's; mount entries are already addressed to
/// one mount and match on target category alone. Within one list the last entry for a pair wins.
///
/// An entry's effective range is its override when it is finite and not negative, else the mount's
/// solver-derived `MountInfo::effective_range_m`; a non-finite mount value falls back to the point-defence
/// caps for point-defence mounts and 0 otherwise (the mount then reports "out of range" rather than firing
/// blind). Salvo sizes are clamped to [`MAX_SALVO`]. Decoy launchers get no entries. Self-defence range is
/// `OfficerSettings::self_defence_range_m` when finite and positive, else `DoctrineTuning::self_defence_range_m`.
pub fn resolve(fleet: &[DoctrineEntry], settings: &OfficerSettings, mounts: &[MountInfo], tuning: &TuningTable) -> DoctrineTable {
    let d = &tuning.doctrine;
    let self_defence_range_m = settings.self_defence_range_m.filter(|v| v.is_finite() && *v > 0.0)
        .or_else(|| finite_pos(d.self_defence_range_m)).unwrap_or(4_000.0);
    let ship_defaults = default_entries(settings.preset);
    let mut entries = BTreeMap::new();
    for m in mounts {
        if m.weapon == WeaponClass::Decoy { continue; }
        let mut chosen: BTreeMap<TargetCategory, (DoctrineEntry, u8)> = BTreeMap::new();
        let mut apply = |list: &[DoctrineEntry], level: u8, by_weapon: bool| {
            for e in list.iter().filter(|e| !by_weapon || e.weapon == m.weapon) {
                chosen.insert(e.target, (DoctrineEntry { weapon: m.weapon, ..*e }, level));
            }
        };
        apply(&ship_defaults, 1, true);
        apply(fleet, 0, true);
        apply(&settings.doctrine, 1, true);
        if let Some(p) = settings.mount_presets.get(&m.slot) { apply(&default_entries(*p), 2, true); }
        if let Some(list) = settings.mount_doctrine.get(&m.slot) { apply(list, 2, false); }
        let fallback = finite_pos(m.effective_range_m).unwrap_or(match m.weapon {
            WeaponClass::PointDefenceGun => finite_pos(d.pd_gun_cap_m).unwrap_or(0.0),
            WeaponClass::PointDefenceMissile => finite_pos(d.pd_missile_cap_m).unwrap_or(0.0),
            _ => 0.0,
        });
        for (cat, (mut entry, level)) in chosen {
            entry.salvo = entry.salvo.min(MAX_SALVO);
            entry.effective_range_m = entry.effective_range_m.and_then(finite_pos);
            let effective_range_m = entry.effective_range_m.unwrap_or(fallback);
            entries.insert((m.slot, cat), ResolvedEntry { entry, effective_range_m, level });
        }
    }
    DoctrineTable { entries, self_defence_range_m }
}

/// Target category of a track for doctrine lookup: Weapon domain or inbound = InboundWeapon, else by domain.
pub fn target_category(track: &Track) -> TargetCategory {
    if track.domain == Domain::Weapon || track.inbound && track.domain == Domain::Air && track.rung < IdRung::Type { return TargetCategory::InboundWeapon; }
    match track.domain { Domain::Surface => TargetCategory::Surface, Domain::Subsurface => TargetCategory::Subsurface, Domain::Air => TargetCategory::Air, Domain::Weapon => TargetCategory::InboundWeapon }
}

/// Class cap in metres for a weapon (table at L489). Unit: `sys-doctrine`.
///
/// Guns: their calibre band's `cap_range_m` (3, 8, 12, 18 km). Point-defence guns: the smaller of the band cap
/// and `pd_gun_cap_m` (2 km). Anti-ship missiles, SAMs, torpedoes and depth charges: `missile_range_fraction`
/// (80%) of the munition's kinematic range. Point-defence missiles: that, capped at `pd_missile_cap_m` (8 km).
/// Decoy launchers: the decoy trigger range (6 km). Missing or invalid catalogue data gives 0, never a panic.
pub fn range_cap_m(mount: &MountInfo, catalogue: &Catalogue, tuning: &TuningTable) -> f64 {
    let d = &tuning.doctrine;
    let fraction = finite_pos(d.missile_range_fraction).unwrap_or(0.8);
    let kinematic = || mount.munition.and_then(|i| catalogue.munitions.get_index(i.index())).map(|(_, m)| finite_pos(m.range_m).unwrap_or(0.0) * fraction).unwrap_or(0.0);
    let band_cap = || gun_def(mount, catalogue).and_then(|g| band_def(catalogue, g.band)).map(|b| finite_pos(b.cap_range_m).unwrap_or(0.0)).unwrap_or(0.0);
    match mount.weapon {
        WeaponClass::Gun => band_cap(),
        WeaponClass::PointDefenceGun => {
            let pd = finite_pos(d.pd_gun_cap_m).unwrap_or(0.0);
            match gun_def(mount, catalogue).and_then(|g| band_def(catalogue, g.band)) { Some(b) => pd.min(finite_pos(b.cap_range_m).unwrap_or(0.0)), None => pd }
        }
        WeaponClass::AntiShipMissile | WeaponClass::Sam | WeaponClass::Torpedo | WeaponClass::DepthCharge => kinematic(),
        WeaponClass::PointDefenceMissile => {
            let pd = finite_pos(d.pd_missile_cap_m).unwrap_or(0.0);
            if mount.munition.is_some() { kinematic().min(pd) } else { pd }
        }
        WeaponClass::Decoy => finite_pos(d.decoy_trigger_m).unwrap_or(0.0),
    }
}

fn gun_def<'c>(mount: &MountInfo, catalogue: &'c Catalogue) -> Option<&'c content::part::GunDef> {
    match catalogue.parts.get_index(mount.part.index()).map(|(_, p)| &p.function) { Some(content::part::PartFunction::Gun(g)) => Some(g), _ => None }
}
fn band_def(catalogue: &Catalogue, band: content::part::weapons::CalibreBand) -> Option<&content::part::weapons::BandDef> { catalogue.bands.iter().find(|b| b.band == band) }

/// Largest range in `[0, cap]` at which the single-round Pk from `table` is at least `pk` against the reference
/// hull. Pk never rises with range, so the answer is the last tabulated row that passes, refined by bisection
/// up to the first row that fails. 0 when even point-blank fire misses the threshold.
pub(crate) fn gun_range_from_table(table: &[crate::guidance::gun::RangeRow], dispersion_mrad: f64, pk: f64, cap_m: f64) -> f64 {
    use crate::guidance::gun::single_round_pk;
    let Some(last) = table.last() else { return 0.0 };
    let cap = cap_m.min(last.range_m).max(0.0);
    let pk = if pk.is_finite() { pk.clamp(0.0, 1.0) } else { 0.10 };
    let ok = |r: f64| single_round_pk(table, r, dispersion_mrad, REFERENCE_HULL_LENGTH_M, REFERENCE_HULL_HEIGHT_M) >= pk;
    if ok(cap) { return cap; }
    // Bracket: lo passes (or is 0), hi fails.
    let mut lo = 0.0;
    let mut hi = cap;
    for row in table.iter().take_while(|r| r.range_m < cap) {
        if ok(row.range_m) { lo = row.range_m; } else { hi = row.range_m; break; }
    }
    if lo == 0.0 && !ok(0.0) { return 0.0; }
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if ok(mid) { lo = mid; } else { hi = mid; }
        if hi - lo < 1.0 { break; }
    }
    lo
}

/// Solver-derived effective range of a gun: the largest range at which `crate::guidance::gun::single_round_pk`
/// against the 40 m reference hull under lock in Beaufort 4 is at least `DoctrineTuning::effective_range_pk`,
/// clamped to [`range_cap_m`]. Missiles and torpedoes use `missile_range_fraction` of kinematic range.
/// This is also the Punch gauge's number (L162). Unit: `sys-doctrine`.
///
/// Guns build their table with `guidance::gun::range_table` from the mount's muzzle velocity (the gun's own
/// when the mount carries none) and the band's drag, and solve the 10% range with the gun's dispersion. A
/// mount whose part is not a gun in the catalogue gets 0. Never reads `mount.effective_range_m`, which it
/// computes.
pub fn effective_range_m(mount: &MountInfo, catalogue: &Catalogue, tuning: &TuningTable) -> f64 {
    let cap = range_cap_m(mount, catalogue, tuning);
    match mount.weapon {
        WeaponClass::Gun | WeaponClass::PointDefenceGun => {
            let (Some(gun), true) = (gun_def(mount, catalogue), cap > 0.0) else { return 0.0 };
            let Some(band) = band_def(catalogue, gun.band) else { return 0.0 };
            let muzzle = if mount.muzzle_mps.is_finite() && mount.muzzle_mps > 0.0 { mount.muzzle_mps } else { gun.muzzle_mps };
            if !(muzzle.is_finite() && muzzle > 0.0) { return 0.0; }
            let table = crate::guidance::gun::range_table(muzzle, band);
            gun_range_from_table(&table, gun.dispersion_mrad, tuning.doctrine.effective_range_pk, cap)
        }
        _ => cap,
    }
}

/// Check one track against one resolved entry; `Ok` to engage, or the first failing condition in the order
/// stance, ID, grade, range (L407 "Holding: reason"). Unit: `sys-doctrine`.
///
/// - A salvo of 0 is a held entry (silent running): `PermissionWithheld`.
/// - Stance: Neutral and Friend never pass, whatever the entry asks (L487). A Suspect track against a gun entry
///   that needs Hostile passes only inside the self-defence range (the smaller of the ship's value and the
///   mount's effective range, L484-489) and fires as `SelfDefence`; outside it holds with
///   `SuspectOutsideSelfDefence`.
/// - ID rung, then grade, then range against the entry's effective range.
///
/// Point-defence weapons and inbound-weapon targets fire as `PointDefence`, everything else as `Doctrine`.
pub fn check(entry: &ResolvedEntry, track: &Track, range_m: f64, self_defence_range_m: f64) -> Result<FireReason, HoldReason> {
    check_facts(entry, track.stance, track.rung, track.grade, range_m, self_defence_range_m, false)
}

/// [`check`] on the track facts alone. `designated`: the player designated this track, which lifts a Suspect
/// track to Hostile for the stance test and a held (salvo 0) entry to firing; Neutral and Friend still never
/// pass. Fires as `Designated` (point defence keeps `PointDefence`).
pub(crate) fn check_facts(entry: &ResolvedEntry, stance: Stance, rung: IdRung, grade: TrackGrade, range_m: f64, self_defence_range_m: f64, designated: bool) -> Result<FireReason, HoldReason> {
    let e = &entry.entry;
    let pd = is_point_defence(e.weapon) || e.target == TargetCategory::InboundWeapon;
    if e.salvo == 0 && !designated { return Err(HoldReason::PermissionWithheld); }
    let range = if range_m.is_finite() { range_m.max(0.0) } else { f64::MAX };
    let limit = finite_pos(entry.effective_range_m).unwrap_or(0.0);
    let need = e.min_stance.max(Stance::Suspect);
    let mut reason = if pd { FireReason::PointDefence } else if designated { FireReason::Designated } else { FireReason::Doctrine };
    let mut range_limit = limit;
    if stance < Stance::Suspect { return Err(HoldReason::Stance { have: stance, need }); }
    let effective_stance = if designated { Stance::Hostile } else { stance };
    if effective_stance < need {
        if e.weapon == WeaponClass::Gun && stance == Stance::Suspect && need == Stance::Hostile {
            let sd = finite_pos(self_defence_range_m).unwrap_or(0.0).min(limit);
            if range > sd { return Err(HoldReason::SuspectOutsideSelfDefence { range_m: range, limit_m: sd }); }
            reason = FireReason::SelfDefence;
            range_limit = sd;
        } else {
            return Err(HoldReason::Stance { have: stance, need });
        }
    }
    if rung < e.min_id { return Err(HoldReason::Id { have: rung, need: e.min_id }); }
    if grade < e.min_grade { return Err(HoldReason::Grade { have: grade, need: e.min_grade }); }
    if range > range_limit { return Err(HoldReason::OutOfRange { range_m: range, limit_m: range_limit }); }
    Ok(reason)
}

#[cfg(test)]
pub(crate) mod tests;
