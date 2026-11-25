//! Part of unit `gd-plot`. The static chart under the plot: coasts, the shallows line and map markers, from a
//! `MapDict` (`show_static`) or traced from the sortie's `MapDef` (the land layer during a sortie). Polygons
//! are cleaned and triangulated once here, so drawing never asks the engine to triangulate bad data.
// Client presentation, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
use content::campaign::{MapDef, MarkerKind};
use glam::DVec2;
use godot::prelude::*;

/// Most points kept per polygon and polygons per layer (a `MapDict` comes from script).
const MAX_POINTS: usize = 4_096;
const MAX_POLYS: usize = 512;
const MAX_MARKERS: usize = 256;
/// Map positions beyond this are rejected as nonsense (L1133: sortie maps are at most 100 km across).
const MAX_COORD_M: f64 = 1.0e6;

/// A filled polygon in map metres with its triangulation (indices into `pts`; empty = outline only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Poly { pub pts: Vec<DVec2>, pub tris: Vec<u32> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerGlyph { Objective, Spawn, Site, Other }

#[derive(Clone, Debug, PartialEq)]
pub struct ChartMarker { pub pos: DVec2, pub name: String, pub label_key: String, pub glyph: MarkerGlyph }

/// What the chart layer draws.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chart { pub size_m: DVec2, pub land: Vec<Poly>, pub shallows: Vec<Poly>, pub markers: Vec<ChartMarker> }

impl Chart {
    pub fn is_empty(&self) -> bool { self.land.is_empty() && self.shallows.is_empty() && self.markers.is_empty() && self.size_m == DVec2::ZERO }

    /// The chart of a sortie map: coasts and the shallows line traced from its terrain (as `map_dict`
    /// does), markers without enemy spawns. Plain Rust: runs on a helper thread.
    pub fn from_def(map: &MapDef) -> Chart {
        let terrain = sim_core::terrain::Terrain::new(map.clone());
        let (land, shallows) = crate::convert::map_contours(map, &|x, z| terrain.height_at(x, z));
        let polys = |v: Vec<Vec<[f64; 2]>>| v.into_iter().filter_map(|p| poly(p.into_iter().map(|q| DVec2::new(q[0], q[1])).collect())).take(MAX_POLYS).collect();
        let markers = map.markers.iter().filter(|m| m.kind != MarkerKind::EnemySpawn).take(MAX_MARKERS)
            .filter_map(|m| {
                let pos = DVec2::new(m.pos_m[0], m.pos_m[1]);
                sane(pos).then(|| ChartMarker { pos, name: m.name.clone(), label_key: marker_key(m.kind).to_string(), glyph: glyph_of(marker_key(m.kind)) })
            })
            .collect();
        let size = DVec2::new(map.size_m[0], map.size_m[1]);
        Chart { size_m: if size.is_finite() && size.x > 0.0 && size.y > 0.0 { size } else { DVec2::ZERO }, land: polys(land), shallows: polys(shallows), markers }
    }

    /// A chart from a `MapDict`. Anything malformed is skipped, never trusted.
    pub fn from_dict(map: &VarDictionary) -> Chart {
        let size = map.get("size_m").and_then(|v| v.try_to::<Vector2>().ok()).map(|v| DVec2::new(v.x as f64, v.y as f64)).filter(|v| v.is_finite() && v.x > 0.0 && v.y > 0.0 && v.x < MAX_COORD_M && v.y < MAX_COORD_M).unwrap_or(DVec2::ZERO);
        let layer = |key: &str| -> Vec<Poly> {
            let Some(arr) = map.get(key).and_then(|v| v.try_to::<VarArray>().ok()) else { return Vec::new() };
            arr.iter_shared().take(MAX_POLYS).filter_map(|v| v.try_to::<PackedVector2Array>().ok()).filter_map(|p| poly(p.as_slice().iter().take(MAX_POINTS).map(|q| DVec2::new(q.x as f64, q.y as f64)).collect())).collect()
        };
        let markers = map.get("markers").and_then(|v| v.try_to::<VarArray>().ok()).map(|arr| {
            arr.iter_shared().take(MAX_MARKERS).filter_map(|v| v.try_to::<VarDictionary>().ok()).filter_map(|m| {
                let pos = m.get("pos").and_then(|v| v.try_to::<Vector2>().ok()).map(|p| DVec2::new(p.x as f64, p.y as f64)).filter(|p| sane(*p))?;
                let name = m.get("name").map(|v| v.to_string()).filter(|s| s != "<null>").unwrap_or_default();
                let label_key = m.get("label").map(|v| v.to_string()).filter(|s| s != "<null>").unwrap_or_default();
                Some(ChartMarker { pos, glyph: glyph_of(&label_key), name, label_key })
            }).collect()
        }).unwrap_or_default();
        Chart { size_m: size, land: layer("land"), shallows: layer("shallows"), markers }
    }
}

fn sane(p: DVec2) -> bool { p.is_finite() && p.x.abs() < MAX_COORD_M && p.y.abs() < MAX_COORD_M }

fn glyph_of(label_key: &str) -> MarkerGlyph {
    match label_key {
        "units.marker.objective" => MarkerGlyph::Objective,
        "units.marker.player_spawn" => MarkerGlyph::Spawn,
        "units.marker.radar_site" | "units.marker.depot_site" => MarkerGlyph::Site,
        _ => MarkerGlyph::Other,
    }
}

/// Text key of a marker kind (the keys `convert::map_dict` writes).
fn marker_key(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::PlayerSpawn => "units.marker.player_spawn",
        MarkerKind::EnemySpawn => "units.marker.enemy_spawn",
        MarkerKind::Objective => "units.marker.objective",
        MarkerKind::ShoreLanding => "units.marker.shore_landing",
        MarkerKind::Survivors => "units.marker.survivors",
        MarkerKind::Berth => "units.marker.berth",
        MarkerKind::ConvoyEntry => "units.marker.convoy_entry",
        MarkerKind::ConvoyExit => "units.marker.convoy_exit",
        MarkerKind::Exit => "units.marker.exit",
        MarkerKind::RadarSite => "units.marker.radar_site",
        MarkerKind::DepotSite => "units.marker.depot_site",
    }
}

/// A cleaned polygon with its triangulation, or `None` when nothing drawable is left.
pub fn poly(pts: Vec<DVec2>) -> Option<Poly> {
    let pts = clean(pts)?;
    let tris = triangulate(&pts);
    Some(Poly { pts, tris })
}

/// Drops non-finite and repeated points and a closing point equal to the first; `None` under 3 points or
/// with no area.
pub fn clean(pts: Vec<DVec2>) -> Option<Vec<DVec2>> {
    let mut out: Vec<DVec2> = Vec::with_capacity(pts.len());
    for p in pts.into_iter().filter(|p| sane(*p)) {
        if out.last().is_none_or(|q| q.distance_squared(p) > 1e-6) { out.push(p); }
    }
    while out.len() > 1 && out[0].distance_squared(*out.last().unwrap_or(&out[0])) <= 1e-6 { out.pop(); }
    if out.len() < 3 || signed_area(&out).abs() < 1e-3 { return None; }
    Some(out)
}

pub fn signed_area(p: &[DVec2]) -> f64 {
    let n = p.len();
    (0..n).map(|i| { let (a, b) = (p[i], p[(i + 1) % n]); a.x * b.y - b.x * a.y }).sum::<f64>() * 0.5
}

/// Ear-clipping triangulation of a simple polygon (either winding). Returns triangle indices, or an empty
/// list when the polygon is not simple (it is then drawn as an outline only).
pub fn triangulate(p: &[DVec2]) -> Vec<u32> {
    let n = p.len();
    if n < 3 { return Vec::new(); }
    let ccw = signed_area(p) > 0.0;
    let mut idx: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity((n - 2) * 3);
    let cross = |o: DVec2, a: DVec2, b: DVec2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut guard = 0usize;
    while idx.len() > 3 {
        guard += 1;
        if guard > n * n + 8 { return Vec::new(); }
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (p[ia], p[ib], p[ic]);
            let turn = cross(a, b, c);
            let convex = if ccw { turn > 1e-12 } else { turn < -1e-12 };
            if !convex { continue; }
            let inside = idx.iter().any(|&j| {
                if j == ia || j == ib || j == ic { return false; }
                let q = p[j];
                let (d1, d2, d3) = (cross(a, b, q), cross(b, c, q), cross(c, a, q));
                if ccw { d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0 } else { d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0 }
            });
            if inside { continue; }
            out.extend_from_slice(&[ia as u32, ib as u32, ic as u32]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped { return Vec::new(); }
    }
    out.extend(idx.iter().map(|&i| i as u32));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygons_are_cleaned_and_triangulated() {
        let square = vec![DVec2::new(0.0, 0.0), DVec2::new(10.0, 0.0), DVec2::new(10.0, 10.0), DVec2::new(0.0, 10.0), DVec2::new(0.0, 0.0)];
        let p = poly(square).unwrap();
        assert_eq!(p.pts.len(), 4);
        assert_eq!(p.tris.len(), 6);
        // A concave L, clockwise: every triangle inside, total area kept.
        let l = vec![DVec2::new(0.0, 0.0), DVec2::new(0.0, 20.0), DVec2::new(10.0, 20.0), DVec2::new(10.0, 10.0), DVec2::new(20.0, 10.0), DVec2::new(20.0, 0.0)];
        let t = triangulate(&l);
        assert_eq!(t.len(), 12);
        let area: f64 = t.chunks(3).map(|c| signed_area(&[l[c[0] as usize], l[c[1] as usize], l[c[2] as usize]]).abs()).sum();
        assert!((area - 300.0).abs() < 1e-9);
        // Junk: too few points, NaN, no area.
        assert!(poly(vec![DVec2::ZERO, DVec2::X]).is_none());
        assert_eq!(poly(vec![DVec2::ZERO, DVec2::new(f64::NAN, 1.0), DVec2::X, DVec2::Y]).map(|p| p.pts.len()), Some(3));
        assert!(poly(vec![DVec2::ZERO, DVec2::new(f64::NAN, 1.0), DVec2::X]).is_none());
        assert!(poly(vec![DVec2::ZERO, DVec2::X, DVec2::new(2.0, 0.0)]).is_none());
        // A bow tie is not simple: outline only, never a panic.
        let bow = vec![DVec2::new(0.0, 0.0), DVec2::new(10.0, 10.0), DVec2::new(10.0, 0.0), DVec2::new(0.0, 10.0)];
        let _ = triangulate(&bow);
    }

    #[test]
    fn sortie_maps_trace_coasts_and_hide_enemy_spawns() {
        let map = MapDef {
            size_m: [20_000.0, 20_000.0], depth_m: 200.0,
            islands: vec![content::campaign::Island { centre_m: [1_000.0, -500.0], radius_m: 600.0, height_m: 80.0, seed: 3 }],
            markers: vec![
                content::campaign::Marker { kind: MarkerKind::EnemySpawn, name: String::new(), pos_m: [5_000.0, 0.0], heading_deg: 0.0 },
                content::campaign::Marker { kind: MarkerKind::Objective, name: "Pier".into(), pos_m: [100.0, 0.0], heading_deg: 0.0 },
            ],
            seed: 1,
        };
        let c = Chart::from_def(&map);
        assert_eq!(c.size_m, DVec2::splat(20_000.0));
        assert_eq!(c.land.len(), 1);
        assert!(!c.land[0].tris.is_empty());
        assert_eq!(c.markers.len(), 1);
        assert_eq!(c.markers[0].glyph, MarkerGlyph::Objective);
        assert!(Chart::default().is_empty());
    }
}
