//! Part of unit `gd-plot`. `BpPlotCheck`: a test-only class that gives a `BpPlotView` a scripted picture in
//! place of the session, so the plot can be captured and probed with real drawing, fonts and theme
//! (`res://test/capture/plot_capture.gd`). The scripts are deterministic. Not referenced by shipped scenes.
//!
//! - `"plot_40_tracks"`: golden scene 2 (L1312): the own hull with a route and its weapon envelope, forty
//!   tracks of every stance and domain, uncertainty wedges, ellipses and fire-control circles, inbound
//!   weapons, emitters, envelopes, last-known marks, an allied hull, an objective, history and an island.
//! - `"calm"`: the own hull alone on open water.
//! - `"crowded"`: forty tracks packed into one convoy, for the declutter order.
#![allow(clippy::disallowed_methods)]
use super::chart::Chart;
use super::picture::SymKind;
use super::{BpPlotView, Injected};
use crate::interp::{TrackPose, VesselPose, ViewFrame};
use content::enums::{Domain, IdRung, Stance};
use glam::{DVec2, DVec3};
use godot::prelude::*;
use sim_core::EntityId;
use sim_systems::ids::{ObjectiveId, TrackId};
use sim_systems::protocol::{Msg, ObjectiveState, ObjectiveView, Relevance, ShipStatus, Snapshot, TrackView, UncertaintyShape, VerticalTrend, VesselView};
use std::collections::BTreeMap;
use std::sync::Arc;

const SCRIPTS: [&str; 3] = ["plot_40_tracks", "calm", "crowded"];
const OWN: u64 = 1;
const ALLY: u64 = 2;
const OWN_SPEED: f64 = 8.0;
const OWN_HEADING: f64 = 0.785;

/// Test-only scripted pictures for `BpPlotView`. Instantiate with `BpPlotCheck.new()`.
#[derive(GodotClass)]
#[class(base = RefCounted)]
pub struct BpPlotCheck { script: String, tick: u64, base: Base<RefCounted> }

#[godot_api]
impl IRefCounted for BpPlotCheck {
    fn init(base: Base<RefCounted>) -> Self { Self { script: String::new(), tick: 0, base } }
}

fn h(i: u64) -> f64 {
    let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn base_track(id: u32, stance: Stance, domain: Domain, pos: DVec2, vel: DVec2) -> TrackView {
    TrackView {
        id: TrackId(id), domain, stance, stance_reason: Default::default(), rung: IdRung::Type, rung_reason: Default::default(), grade: Default::default(),
        pos: DVec3::new(pos.x, if domain == Domain::Air { 300.0 } else if domain == Domain::Subsurface { -60.0 } else { 0.0 }, pos.y),
        vel: DVec3::new(vel.x, 0.0, vel.y), shape: UncertaintyShape::Circle { radius_m: 40.0 }, last_update_tick: 0, last_known: false, class: None,
        candidates: 2, sources: Default::default(), trend: VerticalTrend::Steady, inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Relevance::Near, struck: false,
    }
}

fn polar(brg_deg: f64, range: f64) -> DVec2 { let b = brg_deg.to_radians(); DVec2::new(b.sin(), -b.cos()) * range }

/// The scripted side picture at `t` seconds, around an own hull at `own`.
fn tracks(script: &str, own: DVec2, t: f64) -> Vec<TrackView> {
    let mut out = Vec::new();
    match script {
        "plot_40_tracks" => {
            let mut id = 101u32;
            // Ships move with the clock; weapons stay at their scripted range so they never fly off the plot.
            let mut push = |mut tr: TrackView, out: &mut Vec<TrackView>| { tr.id = TrackId(id); id += 7; if tr.domain != Domain::Weapon { tr.pos += DVec3::new(tr.vel.x * t, 0.0, tr.vel.z * t); } out.push(tr); };
            // A hostile surface group to the north-east with ellipses, envelopes and emitters.
            for k in 0..6u64 {
                let p = own + polar(30.0 + 14.0 * k as f64, 5_500.0 + 1_400.0 * h(k));
                let mut tr = base_track(0, Stance::Hostile, Domain::Surface, p, polar(220.0, 6.0));
                tr.shape = UncertaintyShape::Ellipse { semi_major_m: 300.0 + 500.0 * h(k + 50) as f32, semi_minor_m: 120.0 + 100.0 * h(k + 60) as f32, bearing_rad: (1.2 * h(k + 70)) as f32 };
                if k < 2 { tr.envelope_m = 4_000.0 + 2_000.0 * k as f32; tr.emitting = true; tr.rung = IdRung::Class; }
                push(tr, &mut out);
            }
            // Inbound anti-ship missiles under fire-control lock.
            for k in 0..4u64 {
                let p = own + polar(48.0 + 4.0 * k as f64, 2_400.0 + 500.0 * k as f64);
                let mut tr = base_track(0, Stance::Hostile, Domain::Weapon, p, polar(228.0 + 4.0 * k as f64, 270.0));
                tr.inbound = true;
                tr.shape = UncertaintyShape::Circle { radius_m: 30.0 };
                push(tr, &mut out);
            }
            // Aircraft climbing and diving.
            for k in 0..3u64 {
                let p = own + polar(110.0 + 25.0 * k as f64, 7_000.0);
                let mut tr = base_track(0, if k == 0 { Stance::Hostile } else { Stance::Suspect }, Domain::Air, p, polar(300.0, 120.0));
                tr.trend = if k == 1 { VerticalTrend::Diving } else { VerticalTrend::Climbing };
                tr.shape = UncertaintyShape::Circle { radius_m: 400.0 };
                push(tr, &mut out);
            }
            // Bearing-only contacts: wedges from the own hull.
            for k in 0..5u64 {
                let brg = 200.0 + 18.0 * k as f64;
                let p = own + polar(brg, 6_000.0);
                let mut tr = base_track(0, Stance::Suspect, Domain::Surface, p, DVec2::ZERO);
                tr.rung = if k < 2 { IdRung::Unknown } else { IdRung::Domain };
                tr.shape = UncertaintyShape::Wedge { apex: own, bearing_rad: brg.to_radians() as f32, half_angle_rad: 0.035 + 0.02 * k as f32, near_m: 2_500.0, far_m: 11_000.0 };
                push(tr, &mut out);
            }
            // Submarines.
            for k in 0..2u64 {
                let p = own + polar(300.0 + 30.0 * k as f64, 4_500.0);
                let mut tr = base_track(0, Stance::Suspect, Domain::Subsurface, p, polar(90.0, 3.0));
                tr.shape = UncertaintyShape::Ellipse { semi_major_m: 1_200.0, semi_minor_m: 700.0, bearing_rad: 0.4 };
                push(tr, &mut out);
            }
            // A merchant lane to the south.
            for k in 0..12u64 {
                let p = own + DVec2::new(-9_000.0 + 1_600.0 * k as f64 + 300.0 * h(k + 9), 3_500.0 + 500.0 * h(k + 19));
                let mut tr = base_track(0, Stance::Neutral, Domain::Surface, p, DVec2::new(if k % 2 == 0 { 6.0 } else { -6.0 }, 0.0));
                tr.shape = UncertaintyShape::Circle { radius_m: 60.0 + 80.0 * h(k + 29) as f32 };
                push(tr, &mut out);
            }
            // Friendly traffic and last-known marks.
            for k in 0..4u64 {
                let p = own + polar(140.0 + 50.0 * k as f64, 2_000.0 + 400.0 * k as f64);
                push(base_track(0, Stance::Friend, Domain::Surface, p, polar(45.0, 7.0)), &mut out);
            }
            for k in 0..4u64 {
                let p = own + polar(250.0 + 30.0 * k as f64, 8_500.0);
                let mut tr = base_track(0, if k % 2 == 0 { Stance::Hostile } else { Stance::Suspect }, Domain::Surface, p, DVec2::ZERO);
                tr.last_known = true;
                push(tr, &mut out);
            }
        }
        "crowded" => {
            for k in 0..40u32 {
                let p = own + DVec2::new(1_800.0 + 30.0 * (k % 8) as f64, -300.0 + 40.0 * (k / 8) as f64);
                let st = [Stance::Neutral, Stance::Suspect, Stance::Friend, Stance::Hostile][k as usize % 4];
                let mut tr = base_track(0, st, Domain::Surface, p, DVec2::new(5.0, 0.0));
                tr.id = TrackId(200 + k);
                if k == 17 { tr.domain = Domain::Weapon; tr.inbound = true; tr.stance = Stance::Hostile; }
                out.push(tr);
            }
        }
        _ => {}
    }
    out.sort_by_key(|t| t.id);
    out
}

fn picture(script: &str, tick: u64) -> Injected {
    let t = tick as f64 / sim_core::TICK_HZ as f64;
    let own = polar(OWN_HEADING.to_degrees(), OWN_SPEED * t) + DVec2::new(1_200.0, -800.0);
    let ally = own + polar(170.0, 900.0);
    let ts = tracks(script, own, t);
    let pose = |p: DVec2, heading: f64| sim_core::math::Pose::from_heading(DVec3::new(p.x, 0.0, p.y), heading);
    let vel = DVec3::new(OWN_HEADING.sin() * OWN_SPEED, 0.0, -OWN_HEADING.cos() * OWN_SPEED);
    let full = script == "plot_40_tracks";
    let mut vessels = vec![VesselView {
        entity: EntityId::from_u64(OWN), kin: sim_core::math::Kinematics { pose: pose(own, OWN_HEADING), lin_vel: vel, ang_vel: DVec3::ZERO },
        status: Default::default(), flags: Default::default(), turrets: Vec::new(), rudder: 0.0, power: 0.6, track: TrackId::NONE, relevance: Relevance::Own,
    }];
    if full {
        vessels.push(VesselView { entity: EntityId::from_u64(ALLY), kin: sim_core::math::Kinematics { pose: pose(ally, OWN_HEADING), lin_vel: vel, ang_vel: DVec3::ZERO }, relevance: Relevance::OwnSide, ..vessels[0].clone() });
    }
    let designated = ts.iter().find(|t| t.stance == Stance::Hostile && t.domain == Domain::Surface).map_or(TrackId::NONE, |t| t.id);
    let route = if full { vec![own + polar(45.0, 4_000.0), own + polar(70.0, 9_000.0), own + polar(40.0, 14_000.0)] } else { Vec::new() };
    let objectives = if full { vec![ObjectiveView { id: ObjectiveId(1), ghost: false, state: ObjectiveState::Active, text: Msg::new("objective.destroy_site"), progress: 0.0, marker: Some(own + polar(75.0, 8_200.0)) }] } else { Vec::new() };
    let snap = Snapshot {
        tick, vessels: vessels.clone(), tracks: ts.clone(), objectives,
        ship: Some(ShipStatus { vessel: EntityId::from_u64(OWN), heading_rad: OWN_HEADING as f32, speed_kn: 15.5, longest_envelope_m: 4_500.0, route, designated, ..Default::default() }),
        ..Default::default()
    };
    let to_pose = |v: &VesselView| {
        let q = v.kin.pose.rot;
        VesselPose { entity: v.entity, pos: [v.kin.pose.pos.x as f32, 0.0, v.kin.pose.pos.z as f32], rot: [q.x as f32, q.y as f32, q.z as f32, q.w as f32], vel: [v.kin.lin_vel.x as f32, 0.0, v.kin.lin_vel.z as f32], status: v.status, flags: v.flags, track: v.track, rudder: 0.0, power: 0.6, turrets: 0..0, extrapolated: false, own: v.relevance == Relevance::Own }
    };
    let view = ViewFrame {
        rev: tick, render_tick: tick as f64, sim_time_s: t, origin: Default::default(), origin_shift: None,
        vessels: vessels.iter().map(to_pose).collect(), turrets: Vec::new(), bodies: Vec::new(), decoys: Vec::new(), shells: Vec::new(),
        tracks: ts.iter().map(|t| TrackPose { id: t.id, pos: [t.pos.x as f32, t.pos.y as f32, t.pos.z as f32], world_xz: [t.pos.x, t.pos.z] }).collect(),
        own: Some(0), snapshot: Some(Arc::new(snap)),
    };
    let mut history = BTreeMap::new();
    if full {
        for tr in ts.iter().filter(|t| !t.last_known).take(12) {
            let v = DVec2::new(tr.vel.x, tr.vel.z);
            let p = DVec2::new(tr.pos.x, tr.pos.z);
            history.insert(tr.id, (0..120).rev().map(|s| { let q = p - v * s as f64 - DVec2::new((s as f64 * 0.05).sin(), 0.0) * 40.0; [q.x as f32, q.y as f32] }).collect());
        }
    }
    let chart = full.then(|| Chart::from_def(&content::campaign::MapDef {
        size_m: [40_000.0, 40_000.0], depth_m: 200.0,
        islands: vec![
            content::campaign::Island { centre_m: [own.x - 4_200.0, own.y - 3_000.0], radius_m: 900.0, height_m: 120.0, seed: 7 },
            content::campaign::Island { centre_m: [own.x + 9_000.0, own.y + 6_500.0], radius_m: 1_400.0, height_m: 200.0, seed: 11 },
        ],
        markers: Vec::new(), seed: 4,
    }));
    Injected { view, selected: TrackId::NONE, history, rev: tick + 1, chart }
}

#[godot_api]
impl BpPlotCheck {
    #[func]
    fn scripts(&self) -> PackedStringArray { SCRIPTS.iter().map(|s| GString::from(*s)).collect() }
    /// Give `plot` the scripted picture `script` in place of the session. False for an unknown script.
    #[func]
    fn install(&mut self, mut plot: Gd<BpPlotView>, script: GString) -> bool {
        let s = script.to_string();
        if !SCRIPTS.contains(&s.as_str()) { return false; }
        self.script = s;
        self.tick = 1_800;
        let pic = picture(&self.script, self.tick);
        let mut p = plot.bind_mut();
        let chart = pic.chart.clone();
        p.st.injected = Some(pic);
        if let Some(i) = p.st.injected.as_mut() { i.chart = chart; }
        p.st.dirty = true;
        true
    }
    /// Move the script on by `ticks` simulation ticks (the selection is kept).
    #[func]
    fn advance(&mut self, mut plot: Gd<BpPlotView>, ticks: i64) {
        if self.script.is_empty() { return; }
        self.tick = self.tick.saturating_add(ticks.clamp(0, 1_000_000) as u64);
        let mut next = picture(&self.script, self.tick);
        let mut p = plot.bind_mut();
        if let Some(old) = p.st.injected.as_ref() { next.selected = old.selected; }
        p.st.injected = Some(next);
        p.st.dirty = true;
    }
    /// Select a track as the HUD would. `Bp.NONE` clears.
    #[func]
    fn select(&mut self, mut plot: Gd<BpPlotView>, track: i64) {
        let mut p = plot.bind_mut();
        if let Some(i) = p.st.injected.as_mut() { i.selected = u32::try_from(track).ok().map_or(TrackId::NONE, TrackId); }
        p.st.dirty = true;
    }
    /// Track numbers in the scripted picture, ascending.
    #[func]
    fn track_ids(&self, plot: Gd<BpPlotView>) -> PackedInt32Array {
        let p = plot.bind();
        let ids: Vec<i32> = p.st.injected.as_ref().and_then(|i| i.view.snapshot.as_ref()).map(|s| s.tracks.iter().map(|t| t.id.0 as i32).collect()).unwrap_or_default();
        PackedInt32Array::from(ids.as_slice())
    }
    /// A real briefing: the map of the first operation whose chart has land and markers, as a `MapDict`,
    /// a route from its start marker to its objective, and belief samples along that route:
    /// `{ "operation": String, "map": MapDict, "route": PackedVector2Array, "belief": PackedVector3Array }`.
    #[func]
    fn briefing(&self) -> VarDictionary {
        use content::campaign::MarkerKind;
        let campaign = crate::services::services().campaign();
        let found = campaign.operations.keys().find_map(|id| {
            let map = crate::world::picture::sortie_map(&campaign, content::enums::GameMode::Campaign, 0, id.as_str(), 1);
            let start = map.markers.iter().find(|m| m.kind == MarkerKind::PlayerSpawn)?.pos_m;
            let goal = map.markers.iter().find(|m| m.kind != MarkerKind::PlayerSpawn && m.kind != MarkerKind::EnemySpawn)?.pos_m;
            (!map.islands.is_empty()).then(|| (id.as_str().to_string(), map, start, goal))
        });
        let Some((op, map, start, goal)) = found else { return VarDictionary::new() };
        let (a, b) = (DVec2::new(start[0], start[1]), DVec2::new(goal[0], goal[1]));
        let side = (b - a).perp().normalize_or_zero() * (b - a).length() * 0.18;
        let route: PackedVector2Array = [a, a + (b - a) * 0.4 + side, a + (b - a) * 0.75 - side * 0.5, b].iter().map(|p| Vector2::new(p.x as f32, p.y as f32)).collect();
        let belief: PackedVector3Array = (0..160u64).map(|i| {
            let t = h(i);
            let p = a + (b - a) * t + DVec2::new(h(i + 1_000) - 0.5, h(i + 2_000) - 0.5) * (b - a).length() * 0.3;
            Vector3::new(p.x as f32, p.y as f32, (1.0 - (t - 0.6).abs()) as f32)
        }).collect();
        crate::convert::d! { "operation" => GString::from(op.as_str()), "map" => crate::convert::map_dict(&map), "route" => route, "belief" => belief }
    }
    /// Return the plot to the session's picture.
    #[func]
    fn clear(&mut self, mut plot: Gd<BpPlotView>) {
        self.script.clear();
        let mut p = plot.bind_mut();
        p.st.injected = None;
        p.st.cursor_shown = false;
        p.st.dirty = true;
    }
    /// What the last redraw showed: `{ "symbols": int, "tracks": int, "labels": int, "hidden": int,
    /// "shapes": int, "rings": int, "legend": int, "overlaps": int, "message": String,
    /// "first_track_local": Vector2, "draw_us": int, "access_items": int }` (`access_items`: screen-reader
    /// sub-elements alive, 0 unless a screen reader is connected; `top_covers`: labels covering a symbol of
    /// rank 3 or better, which must be 0) (`overlaps` counts pairs of declutter-placed labels that overlap; it
    /// must be 0; hover and readout labels are shown whatever they overlap).
    #[func]
    fn facts(&self, plot: Gd<BpPlotView>) -> VarDictionary {
        let p = plot.bind();
        let pic = &p.st.picture;
        let fixed: Vec<_> = pic.labels.iter().filter(|l| !l.forced).collect();
        let overlaps = fixed.iter().enumerate().map(|(i, a)| fixed[i + 1..].iter().filter(|b| a.rect.overlaps(&b.rect)).count()).sum::<usize>();
        let first = pic.symbols.iter().find(|s| s.kind == SymKind::Track).map_or(Vector2::new(-1.0, -1.0), |s| Vector2::new(s.pos.x, s.pos.y));
        crate::convert::d! {
            "symbols" => pic.symbols.len() as i64,
            "tracks" => pic.symbols.iter().filter(|s| s.kind == SymKind::Track).count() as i64,
            "labels" => pic.labels.len() as i64,
            "hidden" => pic.hidden.len() as i64,
            "shapes" => pic.shapes.len() as i64,
            "rings" => pic.rings.len() as i64,
            "legend" => pic.legend.len() as i64,
            "overlaps" => overlaps as i64,
            "message" => GString::from(pic.message.as_deref().unwrap_or("")),
            "first_track_local" => first,
            "draw_us" => p.st.draw_us as i64,
            "access_items" => p.st.access.count() as i64,
            "top_covers" => pic.top_covers(super::core::symbol_half(p.st.symbol_scale())) as i64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_deterministic_and_full() {
        let a = picture("plot_40_tracks", 1_800);
        let b = picture("plot_40_tracks", 1_800);
        let ta = &a.view.snapshot.as_ref().unwrap().tracks;
        assert_eq!(ta.len(), 40);
        assert_eq!(*ta, b.view.snapshot.as_ref().unwrap().tracks);
        assert!(ta.windows(2).all(|w| w[0].id < w[1].id));
        assert!(a.chart.as_ref().is_some_and(|c| c.land.len() == 2));
        assert_eq!(picture("crowded", 0).view.snapshot.as_ref().unwrap().tracks.len(), 40);
        assert!(picture("calm", 0).view.snapshot.as_ref().unwrap().tracks.is_empty());
    }

    #[test]
    fn the_golden_scene_builds_a_full_uncluttered_picture() {
        let inj = picture("plot_40_tracks", 1_800);
        let prev = BTreeMap::new();
        let env = super::super::picture::tests::env();
        let own = super::super::picture::own_map_pos(&inj.view).unwrap();
        let inputs = super::super::picture::Inputs {
            mode: super::super::PlotMode::Sortie, layers: super::super::picture::layer::DEFAULT,
            frame: super::super::core::Frame::new(own, 25.0, glam::Vec2::new(1920.0, 1080.0)), view: Some(&inj.view),
            selected: TrackId::NONE, hovered: TrackId::NONE, history: Some(&inj.history), boxed: &[], chart: inj.chart.as_ref(), route: &[], belief: &[],
            prev_sides: &prev, overlay: Default::default(), symbol_size: 24.0, clear: [0.0; 4], blocked: &[],
        };
        let t0 = std::time::Instant::now();
        let pic = super::super::picture::build(&inputs, &env);
        let built = t0.elapsed();
        assert!(pic.land);
        assert!(pic.symbols.iter().filter(|s| s.kind == SymKind::Track).count() >= 30);
        assert!(pic.symbols.iter().any(|s| s.kind == SymKind::Ally));
        assert!(pic.shapes.len() >= 10);
        for (i, a) in pic.labels.iter().enumerate() { for b in &pic.labels[i + 1..] { assert!(!a.rect.overlaps(&b.rect)); } }
        // Four inbound weapons inside 60 px: labels never cover one another's symbols, so most keep a label
        // and any that hides is listed for hover.
        let snap = inj.view.snapshot.as_ref().unwrap();
        let inbound: Vec<TrackId> = snap.tracks.iter().filter(|t| t.inbound).map(|t| t.id).collect();
        let labelled = inbound.iter().filter(|id| pic.labels.iter().any(|l| l.track == **id)).count();
        assert!(labelled >= 3, "{labelled} of {}", inbound.len());
        assert!(inbound.iter().all(|id| pic.labels.iter().any(|l| l.track == *id) || pic.hidden.iter().any(|(t, _)| t == id)));
        assert_eq!(pic.top_covers(12.0), 0);
        // A loose bound only: wall-clock asserts flake on loaded CI runners; the real budget is measured in
        // the engine (`facts().draw_us`).
        assert!(built.as_secs_f64() < 2.0, "{built:?}");
    }
}
