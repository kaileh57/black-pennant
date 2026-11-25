//! Chart geometry for `theatre_graph()`, derived because the theatre data carries none: region outlines
//! and the chart's land and shallows, all in chart km (origin north-west, +x east, +y south).
//!
//! DECISION (region outlines): each node owns the part of the chart nearer to it than to any other node
//! (its Voronoi cell, clipped to the chart rectangle); a region's outline is the boundary of the union of
//! its nodes' cells. That gives straight, chart-like borders that sit midway between rival nodes, and every
//! point of the chart belongs to exactly one region. When a region's cells fall in more than one piece the
//! largest piece is its outline.
//!
//! DECISION (land): the theatre data has no chart-scale coastline, and a sortie map's islands (a few km
//! across) vanish at chart scale, so the chart draws a generalised archipelago derived from the graph: land
//! wherever the sea is far from every lane and node, its coast roughened by fixed value noise, with a band of
//! shallows around it. Lanes and nodes always lie in open water, so every route reads as a strait between
//! islands, and the same theatre always draws the same coast. The coast keeps at least
//! [`MIN_CLEAR_KM`] from every node and lane, outside the waters a sortie map covers near its node.
use content::campaign::TheatreDef;

type P = [f64; 2];

/// Points closer than this (km) are the same point when cells are merged.
const EPS_KM: f64 = 1e-6;

/// Sutherland-Hodgman clip of a convex polygon to the half-plane `dot(p, n) <= c`.
fn clip(poly: &[P], n: P, c: f64) -> Vec<P> {
    let side = |p: &P| p[0] * n[0] + p[1] * n[1] - c;
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (sa, sb) = (side(&a), side(&b));
        if sa <= 0.0 { out.push(a); }
        if (sa <= 0.0) != (sb <= 0.0) {
            let t = sa / (sa - sb);
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    out
}

/// Voronoi cells of `sites` inside `[0, w] x [0, h]`, counter-clockwise in a y-up sense (the order the
/// rectangle starts in, which clipping keeps). A site at the same place as an earlier site has an empty
/// cell; a non-finite site has none.
pub(crate) fn voronoi_cells(sites: &[P], w: f64, h: f64) -> Vec<Vec<P>> {
    let rect = vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];
    let finite = |p: &P| p[0].is_finite() && p[1].is_finite();
    (0..sites.len())
        .map(|i| {
            let si = sites[i];
            if !finite(&si) { return Vec::new(); }
            let mut cell = rect.clone();
            for (j, sj) in sites.iter().enumerate() {
                if j == i || !finite(sj) { continue; }
                let n = [sj[0] - si[0], sj[1] - si[1]];
                if n[0].abs() < EPS_KM && n[1].abs() < EPS_KM {
                    if j < i { return Vec::new(); }
                    continue;
                }
                let m = [(si[0] + sj[0]) * 0.5, (si[1] + sj[1]) * 0.5];
                cell = clip(&cell, n, n[0] * m[0] + n[1] * m[1]);
                if cell.len() < 3 { return Vec::new(); }
            }
            cell
        })
        .collect()
}

fn same(a: P, b: P) -> bool { (a[0] - b[0]).abs() < EPS_KM && (a[1] - b[1]).abs() < EPS_KM }
fn area(poly: &[P]) -> f64 { 0.5 * (0..poly.len()).map(|i| { let (a, b) = (poly[i], poly[(i + 1) % poly.len()]); a[0] * b[1] - b[0] * a[1] }).sum::<f64>() }

/// Outer boundary of a union of cells that tile without overlap: edges two cells share cancel, the rest
/// chain into loops, and the largest loop is returned with collinear points removed.
pub(crate) fn union_outline(cells: &[&Vec<P>]) -> Vec<P> {
    // Split every edge at the vertices of the others' edges so shared stretches match exactly even when one
    // cell's edge spans two of its neighbours' edges.
    let verts: Vec<P> = cells.iter().flat_map(|c| c.iter().copied()).collect();
    let mut edges: Vec<(P, P)> = Vec::new();
    for c in cells {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            if same(a, b) { continue; }
            let d = [b[0] - a[0], b[1] - a[1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            let mut cuts: Vec<f64> = verts
                .iter()
                .filter_map(|v| {
                    let t = ((v[0] - a[0]) * d[0] + (v[1] - a[1]) * d[1]) / len2;
                    let q = [a[0] + d[0] * t, a[1] + d[1] * t];
                    (t > 1e-9 && t < 1.0 - 1e-9 && same(q, *v)).then_some(t)
                })
                .collect();
            cuts.sort_by(f64::total_cmp);
            let mut prev = a;
            for t in cuts {
                let q = [a[0] + d[0] * t, a[1] + d[1] * t];
                if !same(prev, q) { edges.push((prev, q)); prev = q; }
            }
            if !same(prev, b) { edges.push((prev, b)); }
        }
    }
    let mut alive = vec![true; edges.len()];
    for i in 0..edges.len() {
        if !alive[i] { continue; }
        if let Some(j) = (i + 1..edges.len()).find(|&j| alive[j] && same(edges[i].0, edges[j].1) && same(edges[i].1, edges[j].0)) {
            alive[i] = false;
            alive[j] = false;
        }
    }
    let mut loops: Vec<Vec<P>> = Vec::new();
    loop {
        let Some(start) = alive.iter().position(|a| *a) else { break };
        alive[start] = false;
        let mut poly = vec![edges[start].0];
        let mut at = edges[start].1;
        let mut guard = edges.len();
        while !same(at, poly[0]) && guard > 0 {
            guard -= 1;
            poly.push(at);
            match (0..edges.len()).find(|&k| alive[k] && same(edges[k].0, at)) {
                Some(k) => { alive[k] = false; at = edges[k].1; }
                None => break,
            }
        }
        if poly.len() >= 3 { loops.push(poly); }
    }
    let best = loops.into_iter().max_by(|a, b| area(a).abs().total_cmp(&area(b).abs())).unwrap_or_default();
    drop_collinear(best)
}

fn drop_collinear(poly: Vec<P>) -> Vec<P> {
    if poly.len() < 4 { return poly; }
    let n = poly.len();
    let keep: Vec<P> = (0..n)
        .filter(|&i| {
            let (a, b, c) = (poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]);
            let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
            cross.abs() > 1e-9
        })
        .map(|i| poly[i])
        .collect();
    if keep.len() >= 3 { keep } else { poly }
}

/// Outline of every region in data order: `(region index, polygon)`; a region without nodes has none.
pub(crate) fn region_outlines(theatre: &TheatreDef) -> Vec<Vec<P>> {
    let [w, h] = chart_size(theatre);
    let sites: Vec<P> = theatre.nodes.iter().map(|n| n.pos_km).collect();
    let cells = voronoi_cells(&sites, w, h);
    theatre
        .regions
        .iter()
        .map(|r| {
            let mine: Vec<&Vec<P>> = theatre.nodes.iter().zip(&cells).filter(|(n, c)| n.region == r.id && c.len() >= 3).map(|(_, c)| c).collect();
            if mine.is_empty() { Vec::new() } else { union_outline(&mine) }
        })
        .collect()
}

/// Chart size in km; the spec's 600 x 400 km when the data gives nothing usable.
pub(crate) fn chart_size(theatre: &TheatreDef) -> [f64; 2] {
    let ok = |v: f64| v.is_finite() && v > 0.0;
    if ok(theatre.size_km[0]) && ok(theatre.size_km[1]) { theatre.size_km } else { [600.0, 400.0] }
}

/// Lanes with both ends known, each pair once, with straight-line length in km (as the theatre's
/// logistics net measures them).
pub(crate) fn lanes(theatre: &TheatreDef) -> Vec<(usize, usize, f64)> {
    let index = |id: &content::ids::TheatreNodeId| theatre.nodes.iter().position(|n| &n.id == id);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for l in &theatre.lanes {
        let (Some(a), Some(b)) = (index(&l.a), index(&l.b)) else { continue };
        if a == b || !seen.insert((a.min(b), a.max(b))) { continue; }
        let (pa, pb) = (theatre.nodes[a].pos_km, theatre.nodes[b].pos_km);
        let km = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2)).sqrt();
        out.push((a, b, if km.is_finite() { km } else { 0.0 }));
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// The chart-scale archipelago.

/// Grid step of the land field, km.
const LAND_STEP_KM: f64 = 2.0;
/// Open water kept around every lane and node, km, before the noise.
const LAND_CLEAR_KM: f64 = 16.0;
/// How far the noise moves the coast either way, km.
const LAND_NOISE_KM: f64 = 6.0;
/// The coast never comes closer than this to a node or lane, km.
pub(crate) const MIN_CLEAR_KM: f64 = LAND_CLEAR_KM - LAND_NOISE_KM;
/// Width of the shallows band around the land, km.
const SHALLOWS_KM: f64 = 4.0;
/// Open water along the chart's edge, km.
const EDGE_SEA_KM: f64 = 6.0;
/// Douglas-Peucker tolerance for the traced coast, km.
const SIMPLIFY_KM: f64 = 0.35;

fn hash2(x: i64, y: i64, salt: u64) -> f64 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ salt.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth value noise in [-1, 1] at `p` km with a feature size of `cell` km.
fn value_noise(p: P, cell: f64, salt: u64) -> f64 {
    let (x, y) = (p[0] / cell, p[1] / cell);
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (ix, iy) = (ix as i64, iy as i64);
    let a = hash2(ix, iy, salt);
    let b = hash2(ix + 1, iy, salt);
    let c = hash2(ix, iy + 1, salt);
    let d = hash2(ix + 1, iy + 1, salt);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    (top + (bottom - top) * sy) * 2.0 - 1.0
}

/// Three octaves of value noise, in [-1, 1].
fn coast_noise(p: P) -> f64 {
    (value_noise(p, 70.0, 1) + 0.5 * value_noise(p, 28.0, 2) + 0.25 * value_noise(p, 11.0, 3)) / 1.75
}

fn segment_distance(p: P, a: P, b: P) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let len2 = d[0] * d[0] + d[1] * d[1];
    let t = if len2 > 0.0 { (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let q = [a[0] + d[0] * t - p[0], a[1] + d[1] * t - p[1]];
    (q[0] * q[0] + q[1] * q[1]).sqrt()
}

/// Land field at `p`: positive on land, km beyond the coast. Land needs both distance from every lane and
/// node and a high island noise, so the gaps between routes hold separate islands with sea between them.
fn land_field(p: P, nodes: &[P], lanes: &[(P, P)], w: f64, h: f64) -> f64 {
    let mut d = f64::INFINITY;
    for n in nodes { d = d.min({ let (dx, dy) = (p[0] - n[0], p[1] - n[1]); (dx * dx + dy * dy).sqrt() }); }
    for (a, b) in lanes { d = d.min(segment_distance(p, *a, *b)); }
    // The theatre's edge counts as a route, so the outer coasts stay off it with the same rough shore.
    let edge = p[0].min(w - p[0]).min(p[1]).min(h - p[1]);
    let d = d.min(edge + LAND_CLEAR_KM - EDGE_SEA_KM);
    let clear = d - LAND_CLEAR_KM + LAND_NOISE_KM * coast_noise(p);
    let islands = ISLAND_KM * (island_noise(p) - ISLAND_CUT);
    clear.min(islands).min(edge - 1.0)
}

/// Scale of the island mask, km per unit of noise above the cut.
const ISLAND_KM: f64 = 40.0;
/// Island noise needed for land: higher leaves more open sea between islands.
const ISLAND_CUT: f64 = 0.12;

/// Noise that decides where islands stand, in [-1, 1], with features about 40 km across.
fn island_noise(p: P) -> f64 {
    (value_noise(p, 46.0, 7) + 0.45 * value_noise(p, 17.0, 8)) / 1.45
}

/// Closed contour loops of `field > level` on a grid padded with one ring of open water, so every loop closes.
/// Loops are in km; holes (water inside land) are dropped, since the chart fills each loop as one area.
pub(crate) fn contour_loops(field: &[f64], nx: usize, ny: usize, step: f64, level: f64) -> Vec<Vec<P>> {
    // Padded grid: indices -1..=nx, -1..=ny, water outside.
    let at = |i: i64, j: i64| -> f64 {
        if i < 0 || j < 0 || i >= nx as i64 || j >= ny as i64 { level - 1.0 } else { field[j as usize * nx + i as usize] }
    };
    let pos = |i: i64, j: i64| -> P { [i as f64 * step, j as f64 * step] };
    // Edge ids: horizontal edge from (i, j) to (i+1, j) and vertical edge from (i, j) to (i, j+1), padded.
    let w = nx as i64 + 2;
    let hid = |i: i64, j: i64| -> u64 { (((j + 1) * w + (i + 1)) * 2) as u64 };
    let vid = |i: i64, j: i64| -> u64 { (((j + 1) * w + (i + 1)) * 2 + 1) as u64 };
    let cross = |pa: P, va: f64, pb: P, vb: f64| -> P {
        let t = if (vb - va).abs() > 1e-12 { ((level - va) / (vb - va)).clamp(0.0, 1.0) } else { 0.5 };
        [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t]
    };
    let mut points: std::collections::BTreeMap<u64, P> = std::collections::BTreeMap::new();
    let mut links: std::collections::BTreeMap<u64, Vec<u64>> = std::collections::BTreeMap::new();
    for j in -1..ny as i64 {
        for i in -1..nx as i64 {
            let (v0, v1, v2, v3) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            let case = (v0 > level) as u8 | ((v1 > level) as u8) << 1 | ((v2 > level) as u8) << 2 | ((v3 > level) as u8) << 3;
            if case == 0 || case == 15 { continue; }
            // Edges: 0 top (v0-v1), 1 right (v1-v2), 2 bottom (v3-v2), 3 left (v0-v3).
            let edge = |e: u8| -> (u64, P) {
                match e {
                    0 => (hid(i, j), cross(pos(i, j), v0, pos(i + 1, j), v1)),
                    1 => (vid(i + 1, j), cross(pos(i + 1, j), v1, pos(i + 1, j + 1), v2)),
                    2 => (hid(i, j + 1), cross(pos(i, j + 1), v3, pos(i + 1, j + 1), v2)),
                    _ => (vid(i, j), cross(pos(i, j), v0, pos(i, j + 1), v3)),
                }
            };
            let centre = (v0 + v1 + v2 + v3) * 0.25 > level;
            let pairs: &[(u8, u8)] = match case {
                1 | 14 => &[(3, 0)],
                2 | 13 => &[(0, 1)],
                3 | 12 => &[(3, 1)],
                4 | 11 => &[(1, 2)],
                6 | 9 => &[(0, 2)],
                7 | 8 => &[(3, 2)],
                5 => if centre { &[(3, 2), (0, 1)] } else { &[(3, 0), (1, 2)] },
                _ => if centre { &[(3, 0), (1, 2)] } else { &[(3, 2), (0, 1)] },
            };
            for &(a, b) in pairs {
                let (ka, pa) = edge(a);
                let (kb, pb) = edge(b);
                points.insert(ka, pa);
                points.insert(kb, pb);
                links.entry(ka).or_default().push(kb);
                links.entry(kb).or_default().push(ka);
            }
        }
    }
    let mut loops: Vec<Vec<P>> = Vec::new();
    let mut used: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    for (&start, _) in &links {
        if used.contains(&start) { continue; }
        let mut ring = vec![points[&start]];
        used.insert(start);
        let mut prev = start;
        let mut at_key = start;
        loop {
            let next = links[&at_key].iter().copied().find(|k| *k != prev && !used.contains(k));
            match next {
                Some(k) => { used.insert(k); ring.push(points[&k]); prev = at_key; at_key = k; }
                None => break,
            }
        }
        if ring.len() >= 3 { loops.push(ring); }
    }
    // Drop holes: a loop inside a larger loop is water inside land (or land inside a pond, too small to show).
    let areas: Vec<f64> = loops.iter().map(|l| area(l).abs()).collect();
    let keep: Vec<bool> = (0..loops.len())
        .map(|i| !(0..loops.len()).any(|k| k != i && areas[k] > areas[i] && point_in(&loops[k], loops[i][0])))
        .collect();
    loops.into_iter().zip(keep).filter(|(_, k)| *k).map(|(l, _)| simplify_loop(&l, SIMPLIFY_KM)).filter(|l| l.len() >= 3).collect()
}

fn point_in(poly: &[P], p: P) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] { inside = !inside; }
        j = i;
    }
    inside
}

/// Douglas-Peucker on a closed loop, keeping its first point and the point farthest from it.
fn simplify_loop(ring: &[P], eps: f64) -> Vec<P> {
    if ring.len() < 8 { return ring.to_vec(); }
    let far = (1..ring.len()).max_by(|&a, &b| {
        let da = { let (x, y) = (ring[a][0] - ring[0][0], ring[a][1] - ring[0][1]); x * x + y * y };
        let db = { let (x, y) = (ring[b][0] - ring[0][0], ring[b][1] - ring[0][1]); x * x + y * y };
        da.total_cmp(&db)
    }).unwrap_or(ring.len() / 2);
    let mut first = simplify_open(&ring[..=far], eps);
    let mut second: Vec<P> = ring[far..].to_vec();
    second.push(ring[0]);
    let second = simplify_open(&second, eps);
    first.pop();
    first.extend_from_slice(&second[..second.len() - 1]);
    first
}

fn simplify_open(pts: &[P], eps: f64) -> Vec<P> {
    if pts.len() < 3 { return pts.to_vec(); }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let (mut worst, mut at) = (0.0, 0);
    for (i, p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let d = segment_distance(*p, a, b);
        if d > worst { worst = d; at = i; }
    }
    if worst <= eps { return vec![a, b]; }
    let mut left = simplify_open(&pts[..=at], eps);
    let right = simplify_open(&pts[at..], eps);
    left.pop();
    left.extend(right);
    left
}

/// The chart's land and shallows in chart km (see the module's land decision).
pub(crate) fn archipelago(theatre: &TheatreDef) -> (Vec<Vec<P>>, Vec<Vec<P>>) {
    let [w, h] = chart_size(theatre);
    let nodes: Vec<P> = theatre.nodes.iter().map(|n| n.pos_km).filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
    let lanes: Vec<(P, P)> = lanes(theatre).into_iter().map(|(a, b, _)| (theatre.nodes[a].pos_km, theatre.nodes[b].pos_km)).collect();
    if nodes.is_empty() { return (Vec::new(), Vec::new()); }
    let nx = (w / LAND_STEP_KM).floor() as usize + 1;
    let ny = (h / LAND_STEP_KM).floor() as usize + 1;
    let mut field = Vec::with_capacity(nx * ny);
    for j in 0..ny {
        for i in 0..nx {
            field.push(land_field([i as f64 * LAND_STEP_KM, j as f64 * LAND_STEP_KM], &nodes, &lanes, w, h));
        }
    }
    let land = contour_loops(&field, nx, ny, LAND_STEP_KM, 0.0);
    let shallows = contour_loops(&field, nx, ny, LAND_STEP_KM, -SHALLOWS_KM);
    (land, shallows)
}
