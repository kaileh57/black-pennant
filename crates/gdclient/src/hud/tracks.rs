//! Unit `gd-hud-tracks`. The side picture on `BpHud`: track list, track table, selection, history.
//! DESIGN.md L458-477 (tracks and identification), L496-503 (stance), L1660-1663 (HUD rules 4 to 7),
//! L1670 (rule 14: declutter rank), L1723 (track table for screen readers, sortable by range and threat).
//! Source: `protocol::TrackView` list of the newest snapshot. Track numbers are never reused.
//!
//! Decisions where the contract is silent:
//! - Threat order (the docked list's default) puts each track in the first of these that fits: inbound
//!   weapon; Hostile inside the own hull's longest weapon envelope (by closest approach); Hostile outside
//!   (by range); Suspect identified at least to Domain (by range); Unknown rung (by range); Neutral; Friend;
//!   then last-known marks (by age). Ties break on range, then on the track number, so the list never
//!   reshuffles between refreshes. Without an own hull every range is unknown and the number decides.
//! - `threat` is a float that sorts the same way (higher first): 1,000 per band above the lowest, minus a
//!   term under 1,000 that grows with the distance key.
//! - `rank` follows rule 14 with the selected track at 0; an inbound weapon 1; Hostile in range 2; Hostile
//!   outside 3; Suspect 4; Unknown rung 5; Neutral and Friend 6; last-known marks 7.
//! - Closest approach is to the own hull at both current velocities, never in the past: a receding track's
//!   closest approach is now (`cpa_s` 0). Without an own hull `cpa_m` and `cpa_s` are -1.
//! - The accessible name is "Track 0412, Hostile, bearing 047" (with "last known" for marks); it is
//!   translated here because the track table reads it as is.
//! - The legend lists the uncertainty shapes, the stance frames, the climb and dive glyphs and the
//!   last-known mark that the current picture uses, in that order, plus the selection bracket while a track
//!   is selected.
use super::{BpHud, Section};
use crate::convert::{d, empty_msg, msg_dict};
use crate::enums::GdEnum;
use content::enums::{Domain, IdRung, Stance};
use glam::{DVec2, DVec3};
use godot::prelude::*;
use sim_systems::ids::TrackId;
use sim_systems::protocol::{Msg, TrackView, UncertaintyShape, VerticalTrend};

pub(crate) const TEXT_KEYS: &[&str] = &[
    "hud.track.name", "hud.track.name_no_bearing", "hud.track.name_last_known",
    "hud.legend.wedge", "hud.legend.ellipse", "hud.legend.circle", "hud.legend.hostile", "hud.legend.suspect", "hud.legend.neutral",
    "hud.legend.friend", "hud.legend.climbing", "hud.legend.diving", "hud.legend.last_known", "hud.legend.selected",
];

/// Threat bands, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Band { Inbound, HostileInRange, HostileOutside, Suspect, Unknown, Neutral, Friend, LastKnown }
impl Band {
    const COUNT: f64 = 8.0;
    fn index(self) -> u8 { self as u8 }
}

/// The own hull as the picture is measured from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Own { pub pos: DVec2, pub vel: DVec2, pub envelope_m: f64 }

/// Range, bearing and closest approach of a track from the own hull.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry { pub range_m: f64, pub bearing_deg: f64, pub cpa_m: f64, pub cpa_s: f64 }
impl Geometry {
    const NONE: Geometry = Geometry { range_m: -1.0, bearing_deg: -1.0, cpa_m: -1.0, cpa_s: -1.0 };
}

fn flat(v: DVec3) -> DVec2 { DVec2::new(v.x, v.z) }
fn finite2(v: DVec2) -> bool { v.x.is_finite() && v.y.is_finite() }

/// Geometry of one track; `None` own hull gives the "no value" sentinels.
pub(crate) fn geometry(t: &TrackView, own: Option<&Own>) -> Geometry {
    let Some(own) = own else { return Geometry::NONE };
    let (p, v) = (flat(t.pos), flat(t.vel));
    if !finite2(p) || !finite2(own.pos) { return Geometry::NONE; }
    let r = p - own.pos;
    let range = r.length();
    let bearing = crate::convert::course_deg(DVec3::new(r.x, 0.0, r.y));
    let rel = if finite2(v) && finite2(own.vel) { v - own.vel } else { DVec2::ZERO };
    let speed2 = rel.length_squared();
    let t_cpa = if speed2 > 1e-6 { (-r.dot(rel) / speed2).max(0.0) } else { 0.0 };
    // Beyond an hour the prediction means nothing; clamp so the number stays readable.
    let t_cpa = t_cpa.min(3_600.0);
    let cpa = (r + rel * t_cpa).length();
    Geometry { range_m: range, bearing_deg: if range > 0.0 { bearing } else { 0.0 }, cpa_m: cpa, cpa_s: t_cpa }
}

/// The band of a track (first match in the order of the module docs).
pub(crate) fn band(t: &TrackView, g: &Geometry, own: Option<&Own>) -> Band {
    if t.last_known { return Band::LastKnown; }
    // Own-side weapons are Friend and never "inbound" to the player.
    if t.inbound && t.stance != Stance::Friend { return Band::Inbound; }
    match t.stance {
        Stance::Hostile => {
            let envelope = own.map_or(0.0, |o| o.envelope_m);
            if g.range_m >= 0.0 && envelope > 0.0 && g.range_m <= envelope { Band::HostileInRange } else { Band::HostileOutside }
        }
        Stance::Suspect if t.rung != IdRung::Unknown => Band::Suspect,
        Stance::Suspect => Band::Unknown,
        Stance::Neutral => Band::Neutral,
        Stance::Friend => Band::Friend,
    }
}

/// Distance key inside a band: closest approach for Hostile in range, age for last-known marks, else range.
fn band_key(b: Band, t: &TrackView, g: &Geometry, tick: u64) -> f64 {
    let k = match b {
        Band::HostileInRange | Band::Inbound => if g.cpa_m >= 0.0 { g.cpa_m } else { g.range_m },
        Band::LastKnown => tick.saturating_sub(t.last_update_tick) as f64,
        _ => g.range_m,
    };
    if k.is_finite() && k >= 0.0 { k } else { f64::MAX }
}

/// Sort value, higher first: consistent with [`threat_order`].
pub(crate) fn threat_value(b: Band, key: f64) -> f64 {
    let base = (Band::COUNT - 1.0 - b.index() as f64) * 1_000.0;
    let k = if key.is_finite() { key.max(0.0) } else { f64::MAX };
    base + 999.0 * (1.0 - k / (k + 10_000.0)).clamp(0.0, 1.0)
}

/// Declutter rank (rule 14): 0 hides last.
pub(crate) fn rank(b: Band, selected: bool) -> i64 {
    if selected { return 0; }
    match b { Band::Inbound => 1, Band::HostileInRange => 2, Band::HostileOutside => 3, Band::Suspect => 4, Band::Unknown => 5, Band::Neutral | Band::Friend => 6, Band::LastKnown => 7 }
}

/// One track measured for sorting.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Row { pub index: usize, pub id: TrackId, pub band: Band, pub key: f64, pub g: Geometry }

pub(crate) fn rows(tracks: &[TrackView], own: Option<&Own>, tick: u64) -> Vec<Row> {
    tracks.iter().enumerate().map(|(index, t)| {
        let g = geometry(t, own);
        let b = band(t, &g, own);
        Row { index, id: t.id, band: b, key: band_key(b, t, &g, tick), g }
    }).collect()
}

/// Sort rows by `sort` (`threat`, `range`, `bearing` or `number`); stable with the track number as the last
/// tie-break, so the order never flips between refreshes. Unknown ranges and bearings sort last.
pub(crate) fn order(rows: &mut [Row], sort: &str, descending: bool) {
    let last = |v: f64| if v >= 0.0 && v.is_finite() { v } else { f64::MAX };
    match sort {
        "range" => rows.sort_by(|a, b| last(a.g.range_m).total_cmp(&last(b.g.range_m)).then(a.id.cmp(&b.id))),
        "bearing" => rows.sort_by(|a, b| last(a.g.bearing_deg).total_cmp(&last(b.g.bearing_deg)).then(a.id.cmp(&b.id))),
        "number" => rows.sort_by_key(|r| r.id),
        _ => rows.sort_by(|a, b| a.band.cmp(&b.band).then(a.key.total_cmp(&b.key)).then(last(a.g.range_m).total_cmp(&last(b.g.range_m))).then(a.id.cmp(&b.id))),
    }
    if descending { rows.reverse(); }
}

/// The next selection through `ids` (threat order) by `step`, wrapping. A selection not in the list starts
/// from before the first entry going forward, after the last going back.
pub(crate) fn step_selection(ids: &[TrackId], current: TrackId, step: i64) -> TrackId {
    if ids.is_empty() { return TrackId::NONE; }
    let n = ids.len() as i64;
    match ids.iter().position(|t| *t == current) {
        Some(i) => ids[(i as i64 + step.rem_euclid(n)).rem_euclid(n) as usize],
        None if step > 0 => ids[(step - 1).rem_euclid(n) as usize],
        None if step < 0 => ids[(n + step).rem_euclid(n) as usize],
        None => TrackId::NONE,
    }
}

fn shape_dict(s: &UncertaintyShape) -> VarDictionary {
    let f = |v: f32| if v.is_finite() { v.max(0.0) as f64 } else { 0.0 };
    let deg = |r: f32| if r.is_finite() { crate::convert::heading_deg(r as f64) } else { 0.0 };
    match *s {
        UncertaintyShape::Circle { radius_m } => d! { "kind" => 0i64, "radius_m" => f(radius_m) },
        UncertaintyShape::Ellipse { semi_major_m, semi_minor_m, bearing_rad } => d! { "kind" => 1i64, "semi_major_m" => f(semi_major_m), "semi_minor_m" => f(semi_minor_m), "bearing_deg" => deg(bearing_rad) },
        UncertaintyShape::Wedge { apex, bearing_rad, half_angle_rad, near_m, far_m } => d! {
            "kind" => 2i64, "apex" => crate::convert::map_from_dvec2(if finite2(apex) { apex } else { DVec2::ZERO }), "bearing_deg" => deg(bearing_rad),
            "half_angle_deg" => if half_angle_rad.is_finite() { (half_angle_rad.abs() as f64).to_degrees() } else { 0.0 }, "near_m" => f(near_m), "far_m" => f(far_m),
        },
    }
}

impl BpHud {
    fn own_geometry(&self) -> Option<Own> {
        let ship = self.ship()?;
        let v = self.own_view()?;
        Some(Own { pos: flat(v.kin.pose.pos), vel: flat(v.kin.lin_vel), envelope_m: if ship.longest_envelope_m.is_finite() { ship.longest_envelope_m.max(0.0) as f64 } else { 0.0 } })
    }

    /// Rows of the picture in threat order.
    fn threat_rows(&self) -> Vec<Row> {
        let Some(s) = self.snap() else { return Vec::new() };
        let mut r = rows(&s.tracks, self.own_geometry().as_ref(), s.tick);
        order(&mut r, "threat", false);
        r
    }

    fn track_dict(&self, t: &TrackView, row: &Row, lk: &dyn crate::text::Lookup) -> VarDictionary {
        let s = self.snap();
        let tick = s.map_or(0, |s| s.tick);
        let designated = self.ship().is_some_and(|x| x.designated == t.id);
        let selected = self.selected == t.id;
        let vessel = s.and_then(|s| s.vessels.iter().find(|v| v.track == t.id)).map_or(-1, |v| crate::convert::entity_to_gd(v.entity));
        let class = t.class.as_ref().filter(|_| matches!(t.rung, IdRung::Class | IdRung::Exact));
        let class_name = class.and_then(|c| crate::services::services().campaign().classes.get(c).map(|d| msg_dict(&Msg::new(d.name.as_str())))).unwrap_or_else(empty_msg);
        let g = row.g;
        let speed = flat(t.vel);
        let speed_kn = if finite2(speed) { crate::convert::mps_to_kn(speed.length()) } else { 0.0 };
        let label = crate::text::track_label_text(crate::convert::track_to_gd(t.id));
        let stance_word = crate::text::word_of(lk, "stance", t.stance);
        let name = if t.last_known {
            crate::text::fill(&lk.tr("hud.track.name_last_known"), &[lk.digits(&label), stance_word])
        } else if g.bearing_deg >= 0.0 {
            crate::text::fill(&lk.tr("hud.track.name"), &[lk.digits(&label), stance_word, lk.digits(&crate::text::bearing_text(g.bearing_deg))])
        } else {
            crate::text::fill(&lk.tr("hud.track.name_no_bearing"), &[lk.digits(&label), stance_word])
        };
        d! {
            "id" => crate::convert::track_to_gd(t.id),
            "label" => GString::from(label.as_str()),
            "domain" => t.domain.to_gd(),
            "stance" => t.stance.to_gd(),
            "stance_reason" => msg_dict(&sim_systems::text::stance_reason(t.stance_reason)),
            "rung" => t.rung.to_gd(),
            "rung_reason" => msg_dict(&sim_systems::text::rung_reason(t.rung_reason)),
            "grade" => t.grade.to_gd(),
            "pos" => crate::convert::map_from_world(if t.pos.is_finite() { t.pos } else { DVec3::ZERO }),
            "alt_m" => if t.pos.y.is_finite() { t.pos.y } else { 0.0 },
            "course_deg" => if finite2(speed) && speed.length_squared() > 1e-6 { crate::convert::course_deg(t.vel) } else { 0.0 },
            "speed_kn" => speed_kn,
            "bearing_deg" => g.bearing_deg,
            "range_m" => g.range_m,
            "cpa_m" => g.cpa_m,
            "cpa_s" => g.cpa_s,
            "shape" => shape_dict(&t.shape),
            "age_s" => super::tick_s(tick.saturating_sub(t.last_update_tick)),
            "last_known" => t.last_known,
            "class_id" => StringName::from(class.map_or("", |c| c.as_str())),
            "class_name" => class_name,
            "candidates" => t.candidates as i64,
            "sources" => t.sources.bits() as i64,
            "trend" => t.trend.to_gd(),
            "inbound" => t.inbound,
            "engaged_by" => t.engaged_by as i64,
            "envelope_m" => if t.envelope_m.is_finite() { t.envelope_m.max(0.0) as f64 } else { 0.0 },
            "emitting" => t.emitting,
            "struck" => t.struck,
            "designated" => designated,
            "selected" => selected,
            "rank" => rank(row.band, selected),
            "threat" => threat_value(row.band, row.key),
            "vessel" => vessel,
            "name" => GString::from(name.as_str()),
        }
    }

    /// Every `TrackDict` by index into the snapshot's list (ascending track number), cached per revision.
    fn all_tracks(&self) -> Array<VarDictionary> {
        self.cached("tracks", 0, &[Section::Tracks], |h| {
            let Some(s) = h.snap() else { return Array::new() };
            let own = h.own_geometry();
            let rows = rows(&s.tracks, own.as_ref(), s.tick);
            let lk = crate::text::EngineLookup::new();
            s.tracks.iter().zip(rows.iter()).map(|(t, r)| h.track_dict(t, r, &lk)).collect()
        })
    }
}

#[godot_api(secondary)]
impl BpHud {
    /// Returns `Array[TrackDict]`, ascending track number, last-known marks included. 10 Hz, `tracks_changed`.
    #[func]
    fn tracks(&self) -> Array<VarDictionary> { self.all_tracks() }
    /// One `TrackDict`, or `null` when the number is not in the picture.
    #[func]
    fn track(&self, id: i64) -> Variant {
        let Some(s) = self.snap() else { return Variant::nil() };
        let Ok(id) = u32::try_from(id) else { return Variant::nil() };
        let Ok(i) = s.tracks.binary_search_by_key(&TrackId(id), |t| t.id) else { return Variant::nil() };
        let all = self.all_tracks();
        all.get(i).map_or(Variant::nil(), |d| d.to_variant())
    }
    /// The same tracks sorted for the track list and the accessible track table: `sort` is `&"threat"`
    /// (default order of the docked list: inbound weapons, Hostile inside own weapon range by closest
    /// approach, Hostile outside, Suspect, Unknown rung, Neutral and Friend), `&"range"`, `&"bearing"` or
    /// `&"number"`. Returns `Array[TrackDict]`.
    #[func]
    fn track_table(&self, sort: StringName, descending: bool) -> Array<VarDictionary> {
        let key = match sort.to_string().as_str() { "range" => "range", "bearing" => "bearing", "number" => "number", _ => "threat" };
        let arg = match key { "threat" => 0, "range" => 1, "bearing" => 2, _ => 3 } * 2 + descending as i64;
        self.cached("track_table", arg, &[Section::Tracks], |h| {
            let Some(s) = h.snap() else { return Array::new() };
            let mut r = rows(&s.tracks, h.own_geometry().as_ref(), s.tick);
            order(&mut r, key, descending);
            let all = h.all_tracks();
            r.iter().filter_map(|row| all.get(row.index)).collect()
        })
    }
    /// Counts for the list header: `{ "total": int, "hostile": int, "suspect": int, "neutral": int,
    /// "friend": int, "inbound": int, "last_known": int }`.
    #[func]
    fn track_counts(&self) -> VarDictionary {
        self.cached("track_counts", 0, &[Section::Tracks], |h| {
            let tracks = h.snap().map_or(&[][..], |s| &s.tracks[..]);
            let live = || tracks.iter().filter(|t| !t.last_known);
            let stance = |st: Stance| live().filter(|t| t.stance == st).count() as i64;
            d! {
                "total" => live().count() as i64,
                "hostile" => stance(Stance::Hostile),
                "suspect" => stance(Stance::Suspect),
                "neutral" => stance(Stance::Neutral),
                "friend" => stance(Stance::Friend),
                "inbound" => live().filter(|t| t.inbound).count() as i64,
                "last_known" => tracks.iter().filter(|t| t.last_known).count() as i64,
            }
        })
    }
    /// Select a track for the track card and the white selection bracket; `Bp.NONE` clears. Local only
    /// (selection is not a command). Emits `selection_changed`.
    #[func]
    fn select_track(&mut self, id: i64) {
        let want = match u32::try_from(id) {
            Ok(n) if n > 0 && self.snap().is_some_and(|s| s.tracks.binary_search_by_key(&TrackId(n), |t| t.id).is_ok()) => TrackId(n),
            _ => TrackId::NONE,
        };
        if want == self.selected { return; }
        self.selected = want;
        // The selected flag and rank live in every TrackDict.
        self.revs[Section::Tracks.index()] += 1;
        self.emit_selection();
        self.base_mut().emit_signal("tracks_changed", &[]);
    }
    /// Move the selection through `track_table(&"threat")` by `step` (+1 next, -1 previous), wrapping.
    /// Returns the new selection.
    #[func]
    fn select_step(&mut self, step: i64) -> i64 {
        // The own hull's own track (the side's datalink holds it) is no contact: the track list leaves it
        // out, so stepping skips it too.
        let own = self.own_view().map_or(TrackId::NONE, |v| v.track);
        let ids: Vec<TrackId> = self.threat_rows().iter().map(|r| r.id).filter(|id| own.is_none() || *id != own).collect();
        let next = step_selection(&ids, self.selected, step);
        self.select_track(crate::convert::track_to_gd(next));
        crate::convert::track_to_gd(self.selected)
    }
    #[func]
    fn selected_track(&self) -> i64 { crate::convert::track_to_gd(self.selected) }
    /// The track the officers were told to engage, or `Bp.NONE`.
    #[func]
    fn designated_track(&self) -> i64 { self.ship().map_or(-1, |s| crate::convert::track_to_gd(s.designated)) }
    /// Past positions of one track, oldest first, one point per second for up to 10 minutes (map positions).
    #[func]
    fn track_history(&self, id: i64) -> PackedVector2Array {
        let Ok(id) = u32::try_from(id) else { return PackedVector2Array::new() };
        self.history.get(&TrackId(id)).map_or_else(PackedVector2Array::new, |h| h.iter().map(|p| Vector2::new(p[0], p[1])).collect())
    }
    /// Legend rows for the symbols in use (rule 6: uncertainty shapes need a legend): `Array[Dictionary]`
    /// with `symbol: StringName` (icon name under `res://ui/icons/`), `text: StringName`.
    #[func]
    fn track_legend(&self) -> Array<VarDictionary> {
        let arg = !self.selected.is_none() as i64;
        self.cached("track_legend", arg, &[Section::Tracks], |h| {
            let tracks = h.snap().map_or(&[][..], |s| &s.tracks[..]);
            legend_rows(tracks, !h.selected.is_none()).into_iter().map(|(symbol, text)| d! { "symbol" => StringName::from(symbol), "text" => StringName::from(text) }).collect()
        })
    }
}

/// Legend rows (icon, text key) for a picture, in the order of the module docs.
pub(crate) fn legend_rows(tracks: &[TrackView], selected: bool) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let live = || tracks.iter().filter(|t| !t.last_known);
    let any = |f: &dyn Fn(&TrackView) -> bool| live().any(f);
    if any(&|t| matches!(t.shape, UncertaintyShape::Wedge { .. })) { out.push(("track_bearing_wedge", "hud.legend.wedge")); }
    if any(&|t| matches!(t.shape, UncertaintyShape::Ellipse { .. })) { out.push(("track_samples", "hud.legend.ellipse")); }
    if any(&|t| matches!(t.shape, UncertaintyShape::Circle { .. })) { out.push(("track_fire_control", "hud.legend.circle")); }
    for (stance, icon, text) in [(Stance::Hostile, "stance_hostile_surface", "hud.legend.hostile"), (Stance::Suspect, "stance_suspect_surface", "hud.legend.suspect"), (Stance::Neutral, "stance_neutral_surface", "hud.legend.neutral"), (Stance::Friend, "stance_friend_surface", "hud.legend.friend")] {
        if any(&|t| t.stance == stance) { out.push((icon, text)); }
    }
    if any(&|t| t.trend == VerticalTrend::Climbing) { out.push(("trend_climbing", "hud.legend.climbing")); }
    if any(&|t| t.trend == VerticalTrend::Diving) { out.push(("trend_diving", "hud.legend.diving")); }
    if tracks.iter().any(|t| t.last_known) { out.push(("track_history", "hud.legend.last_known")); }
    if selected { out.push(("select_bracket", "hud.legend.selected")); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_systems::protocol::{RungReason, SourceMask, StanceReason};

    fn track(id: u32, stance: Stance, rung: IdRung, x: f64, z: f64) -> TrackView {
        TrackView {
            id: TrackId(id), domain: Domain::Surface, stance, stance_reason: StanceReason::Default, rung, rung_reason: RungReason::None,
            grade: content::enums::TrackGrade::Fine, pos: DVec3::new(x, 0.0, z), vel: DVec3::ZERO, shape: UncertaintyShape::Circle { radius_m: 50.0 },
            last_update_tick: 0, last_known: false, class: None, candidates: 3, sources: SourceMask::SEARCH_RADAR, trend: VerticalTrend::Steady,
            inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Default::default(), struck: false,
        }
    }
    const OWN: Own = Own { pos: DVec2::ZERO, vel: DVec2::ZERO, envelope_m: 8_000.0 };

    #[test]
    fn geometry_bearing_and_cpa() {
        // North is -z: a track 1 km north bears 000, one 1 km east bears 090.
        let g = geometry(&track(1, Stance::Hostile, IdRung::Type, 0.0, -1_000.0), Some(&OWN));
        assert!((g.range_m - 1_000.0).abs() < 1e-9 && g.bearing_deg.abs() < 1e-9);
        assert!((geometry(&track(1, Stance::Hostile, IdRung::Type, 1_000.0, 0.0), Some(&OWN)).bearing_deg - 90.0).abs() < 1e-9);
        // Crossing ahead at 10 m/s, 2 km east and 1 km north: closest approach 1 km, 200 s from now.
        let mut t = track(2, Stance::Hostile, IdRung::Type, 2_000.0, -1_000.0);
        t.vel = DVec3::new(-10.0, 0.0, 0.0);
        let g = geometry(&t, Some(&OWN));
        assert!((g.cpa_m - 1_000.0).abs() < 1e-6 && (g.cpa_s - 200.0).abs() < 1e-6);
        // Receding: closest approach is now.
        t.vel = DVec3::new(10.0, 0.0, 0.0);
        let g = geometry(&t, Some(&OWN));
        assert!(g.cpa_s == 0.0 && (g.cpa_m - g.range_m).abs() < 1e-9);
        assert_eq!(geometry(&t, None), Geometry::NONE);
        t.pos.x = f64::NAN;
        assert_eq!(geometry(&t, Some(&OWN)), Geometry::NONE);
    }

    #[test]
    fn threat_order_follows_rule_14() {
        let mut inbound = track(9, Stance::Hostile, IdRung::Type, 0.0, -9_000.0);
        inbound.domain = Domain::Weapon;
        inbound.inbound = true;
        let mut lk = track(1, Stance::Hostile, IdRung::Class, 0.0, -100.0);
        lk.last_known = true;
        let tracks = vec![
            lk,
            track(2, Stance::Friend, IdRung::Exact, 0.0, -50.0),
            track(3, Stance::Neutral, IdRung::Type, 0.0, -60.0),
            track(4, Stance::Suspect, IdRung::Unknown, 0.0, -70.0),
            track(5, Stance::Suspect, IdRung::Domain, 0.0, -20_000.0),
            track(6, Stance::Hostile, IdRung::Type, 0.0, -12_000.0),
            track(7, Stance::Hostile, IdRung::Type, 0.0, -5_000.0),
            track(8, Stance::Hostile, IdRung::Type, 0.0, -3_000.0),
            inbound,
        ];
        let mut r = rows(&tracks, Some(&OWN), 0);
        order(&mut r, "threat", false);
        let ids: Vec<u32> = r.iter().map(|r| r.id.0).collect();
        assert_eq!(ids, vec![9, 8, 7, 6, 5, 4, 3, 2, 1]);
        // `threat` sorts the same way.
        let values: Vec<f64> = r.iter().map(|r| threat_value(r.band, r.key)).collect();
        assert!(values.windows(2).all(|w| w[0] > w[1]), "{values:?}");
        // Ranks: selected first, last-known marks hide first.
        assert_eq!(rank(Band::Inbound, false), 1);
        assert_eq!(rank(Band::LastKnown, true), 0);
        assert_eq!(rank(Band::LastKnown, false), 7);
        // Range and number sorts, descending, and a stable tie on equal ranges.
        let mut r2 = rows(&tracks, Some(&OWN), 0);
        order(&mut r2, "range", false);
        assert_eq!(r2[0].id.0, 2);
        order(&mut r2, "number", true);
        assert_eq!(r2.iter().map(|r| r.id.0).collect::<Vec<_>>(), vec![9, 8, 7, 6, 5, 4, 3, 2, 1]);
        let twins = vec![track(12, Stance::Suspect, IdRung::Type, 0.0, -1_000.0), track(11, Stance::Suspect, IdRung::Type, 1_000.0, 0.0)];
        let mut r3 = rows(&twins, Some(&OWN), 0);
        order(&mut r3, "threat", false);
        assert_eq!(r3[0].id.0, 11);
        // Without an own hull nothing is "in range" and the number breaks ties.
        let mut r4 = rows(&tracks[5..8], None, 0);
        order(&mut r4, "threat", false);
        assert_eq!(r4.iter().map(|r| r.id.0).collect::<Vec<_>>(), vec![6, 7, 8]);
        assert!(r4.iter().all(|r| r.band == Band::HostileOutside));
    }

    #[test]
    fn select_step_wraps_and_survives_edges() {
        let ids = [TrackId(4), TrackId(2), TrackId(9)];
        assert_eq!(step_selection(&[], TrackId(4), 1), TrackId::NONE);
        assert_eq!(step_selection(&ids, TrackId::NONE, 1), TrackId(4));
        assert_eq!(step_selection(&ids, TrackId::NONE, -1), TrackId(9));
        assert_eq!(step_selection(&ids, TrackId(4), 1), TrackId(2));
        assert_eq!(step_selection(&ids, TrackId(9), 1), TrackId(4));
        assert_eq!(step_selection(&ids, TrackId(4), -1), TrackId(9));
        assert_eq!(step_selection(&ids, TrackId(77), -1), TrackId(9));
        assert_eq!(step_selection(&ids, TrackId(2), 0), TrackId(2));
        assert_eq!(step_selection(&ids, TrackId(2), 3), TrackId(2));
        assert_eq!(step_selection(&ids, TrackId(2), -4), TrackId(4));
        assert_eq!(step_selection(&ids, TrackId::NONE, 0), TrackId::NONE);
        for step in [i64::MIN, i64::MAX, -4, 4, 5] { let _ = step_selection(&ids, TrackId(2), step); }
        assert_eq!(step_selection(&ids, TrackId(2), i64::MAX), TrackId(9));
    }

    #[test]
    fn legend_lists_what_is_drawn() {
        let mut a = track(1, Stance::Hostile, IdRung::Type, 0.0, 0.0);
        a.shape = UncertaintyShape::Wedge { apex: DVec2::ZERO, bearing_rad: 0.0, half_angle_rad: 0.05, near_m: 100.0, far_m: 9_000.0 };
        let b = track(2, Stance::Neutral, IdRung::Type, 0.0, 0.0);
        let rows = legend_rows(&[a, b], true);
        assert_eq!(rows, vec![("track_bearing_wedge", "hud.legend.wedge"), ("track_fire_control", "hud.legend.circle"), ("stance_hostile_surface", "hud.legend.hostile"), ("stance_neutral_surface", "hud.legend.neutral"), ("select_bracket", "hud.legend.selected")]);
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/ui/icons");
        for (icon, _) in rows { assert!(std::path::Path::new(&format!("{dir}/{icon}.svg")).exists(), "{icon}"); }
        assert!(legend_rows(&[], false).is_empty());
    }
}
