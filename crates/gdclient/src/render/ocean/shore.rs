//! Part of unit `gd-render-ocean`. The camera-centred shore heightfield the water shader reads for shore
//! foam and the shallow tint (L1907): R16, 2048 squared at 4 m, 8 MB.
//!
//! Heights are `sim_core::terrain::Terrain::sample_patch` over the sortie map, sampled on a short-lived
//! thread (`bp-shore`) because 4.2 million samples do not fit a frame. The window is rebuilt only when the
//! camera has moved [`RECENTRE_M`] from its centre, and only while some island's shelf reaches into it: in
//! open water there is no texture and the shader's shore branch is off. Nothing here touches Godot; `gpu.rs`
//! uploads the bytes a finished job returns, once per re-centre.
#![allow(clippy::disallowed_methods)]
use content::campaign::MapDef;
use sim_core::terrain::Terrain;
use std::sync::mpsc;
use std::sync::Arc;

/// Texels per edge and metres per texel (L1907).
pub const SHORE_TEXELS: u32 = 2048;
pub const SHORE_TEXEL_M: f64 = 4.0;
/// Edge of the window in metres.
pub const SHORE_EXTENT_M: f64 = SHORE_TEXELS as f64 * SHORE_TEXEL_M;
/// The window is re-centred when the camera is this far from its centre on either axis, which still leaves
/// 3 km of heightfield in every direction.
pub const RECENTRE_M: f64 = 1_024.0;
/// Window centres snap to this grid, a whole number of texels, so a re-centred window lines up with the old.
const SNAP_M: f64 = 256.0;
/// Encoding: ground height in metres = `R * SHORE_RANGE_M + SHORE_MIN_M` (the shader's `shore_decode`),
/// 4 mm per step. Deeper water reads as 128 m, where neither the tint nor the foam shows.
pub const SHORE_RANGE_M: f32 = 256.0;
pub const SHORE_MIN_M: f32 = -128.0;

/// A finished window: its centre on the map and `SHORE_TEXELS` squared little-endian u16 texels, row-major
/// with +X along a row and +Z down the rows.
pub(crate) struct ShoreUpload { pub centre: [f64; 2], pub bytes: Vec<u8> }

struct Done { generation: u32, centre: [f64; 2], bytes: Vec<u8> }

pub(crate) struct Shore {
    terrain: Option<Arc<Terrain>>,
    generation: u32,
    /// Centre of the window the shader has, if any.
    current: Option<[f64; 2]>,
    pending: Option<[f64; 2]>,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
}

/// Texels of a height patch. Non-finite heights read as deep water.
pub(crate) fn encode(heights: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(heights.len() * 2);
    for &h in heights {
        let unit = if h.is_finite() { ((h - SHORE_MIN_M) / SHORE_RANGE_M).clamp(0.0, 1.0) } else { 0.0 };
        out.extend_from_slice(&((unit * 65_535.0).round() as u16).to_le_bytes());
    }
    out
}

/// True when some island's land or shelf can reach into the window centred on `centre`.
pub(crate) fn land_in_window(map: &MapDef, centre: [f64; 2]) -> bool {
    let half = 0.5 * SHORE_EXTENT_M;
    map.islands.iter().any(|i| {
        if !(i.centre_m.iter().all(|c| c.is_finite()) && i.radius_m.is_finite() && i.radius_m > 0.0 && i.height_m.is_finite()) { return false; }
        // The shoreline stays within 1.16 radii and the shelf adds max(half a radius, 300 m) (sim-core terrain).
        let reach = i.radius_m * 1.16 + (0.5 * i.radius_m).max(300.0);
        (i.centre_m[0] - centre[0]).abs() <= half + reach && (i.centre_m[1] - centre[1]).abs() <= half + reach
    })
}

/// The window's (0, 0) texel corner on the map.
pub(crate) fn corner(centre: [f64; 2]) -> [f64; 2] { [centre[0] - 0.5 * SHORE_EXTENT_M, centre[1] - 0.5 * SHORE_EXTENT_M] }

/// Heights of the window centred on `centre`, texel centres at `corner + (i + 0.5) * 4 m`.
pub(crate) fn sample(terrain: &Terrain, centre: [f64; 2], texels: u32) -> Vec<f32> {
    // `sample_patch` puts its outer samples on the patch edges, so the patch spans the texel centres.
    terrain.sample_patch(centre, SHORE_TEXEL_M * (texels.max(1) - 1) as f64, texels)
}

impl Shore {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { terrain: None, generation: 0, current: None, pending: None, tx, rx }
    }

    /// The sortie's map; forgets any window of the previous one.
    pub fn set_map(&mut self, map: &MapDef) {
        self.terrain = Some(Arc::new(Terrain::new(map.clone())));
        self.generation = self.generation.wrapping_add(1);
        self.current = None;
        self.pending = None;
    }

    pub fn clear(&mut self) {
        self.terrain = None;
        self.generation = self.generation.wrapping_add(1);
        self.current = None;
        self.pending = None;
    }

    /// Centre of the window in use; `None` while the shore branch is off.
    pub fn window(&self) -> Option<[f64; 2]> { self.current }

    /// Ground height at a map position (picking); open water without a map.
    pub fn ground_height(&self, x: f64, z: f64) -> f64 {
        self.terrain.as_ref().map_or(-sim_core::terrain::DEFAULT_DEPTH_M, |t| t.height_at(x, z))
    }

    /// One frame: start a job when the camera has left the window, and hand back a finished one for upload.
    pub fn step(&mut self, camera: [f64; 2]) -> Option<ShoreUpload> {
        let mut upload = None;
        while let Ok(done) = self.rx.try_recv() {
            if done.generation != self.generation || self.pending != Some(done.centre) { continue; }
            self.pending = None;
            self.current = Some(done.centre);
            upload = Some(ShoreUpload { centre: done.centre, bytes: done.bytes });
        }
        let Some(terrain) = &self.terrain else { return upload };
        if !(camera[0].is_finite() && camera[1].is_finite()) { return upload; }
        let inside = |c: [f64; 2]| (camera[0] - c[0]).abs() <= RECENTRE_M && (camera[1] - c[1]).abs() <= RECENTRE_M;
        if self.current.is_some_and(inside) || self.pending.is_some_and(inside) { return upload; }
        let wanted = [(camera[0] / SNAP_M).round() * SNAP_M, (camera[1] / SNAP_M).round() * SNAP_M];
        if !land_in_window(&terrain.map, wanted) {
            // Open water: drop a window the camera has sailed out of.
            let out = |c: [f64; 2]| (camera[0] - c[0]).abs() > 0.5 * SHORE_EXTENT_M - RECENTRE_M || (camera[1] - c[1]).abs() > 0.5 * SHORE_EXTENT_M - RECENTRE_M;
            if self.current.is_some_and(out) { self.current = None; }
            self.pending = None;
            return upload;
        }
        let (terrain, tx, generation) = (terrain.clone(), self.tx.clone(), self.generation);
        let job = move || {
            let bytes = encode(&sample(&terrain, wanted, SHORE_TEXELS));
            let _ = tx.send(Done { generation, centre: wanted, bytes });
        };
        // A thread that cannot start leaves the shore branch as it is; the next re-centre tries again.
        if std::thread::Builder::new().name("bp-shore".into()).spawn(job).is_ok() { self.pending = Some(wanted); }
        upload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::campaign::Island;

    fn map() -> MapDef { MapDef { size_m: [40_000.0, 40_000.0], depth_m: 200.0, islands: vec![Island { centre_m: [1_000.0, -500.0], radius_m: 600.0, height_m: 80.0, seed: 4 }], markers: Vec::new(), seed: 9 } }

    #[test]
    fn texels_decode_to_the_terrain_under_their_centres() {
        let terrain = Terrain::new(map());
        let n = 64u32;
        let centre = [1_024.0, -512.0];
        let heights = sample(&terrain, centre, n);
        let bytes = encode(&heights);
        assert_eq!(bytes.len(), (n * n * 2) as usize);
        // Texel (i, j) is centred at centre - 2 (n - 1) + 4 (i, j) metres.
        let mut land = 0;
        for (i, j) in [(0u32, 0u32), (31, 31), (40, 20), (63, 63), (30, 33)] {
            let x = centre[0] - 2.0 * (n - 1) as f64 + 4.0 * i as f64;
            let z = centre[1] - 2.0 * (n - 1) as f64 + 4.0 * j as f64;
            let k = 2 * (i + n * j) as usize;
            let r = u16::from_le_bytes([bytes[k], bytes[k + 1]]) as f32 / 65_535.0;
            let decoded = r * SHORE_RANGE_M + SHORE_MIN_M;
            let truth = (terrain.height_at(x, z) as f32).clamp(SHORE_MIN_M, SHORE_MIN_M + SHORE_RANGE_M);
            assert!((decoded - truth).abs() < 0.01, "texel ({i}, {j}): {decoded} against {truth}");
            if truth > 0.0 { land += 1; }
        }
        assert!(land >= 3);
        // Clamping and bad values.
        assert_eq!(encode(&[f32::NAN, -500.0, 500.0]), [0, 0, 0, 0, 255, 255]);
    }

    #[test]
    fn the_window_exists_only_near_land_and_recentres_with_the_camera() {
        let m = map();
        assert!(land_in_window(&m, [0.0, 0.0]));
        assert!(!land_in_window(&m, [15_000.0, 15_000.0]));
        assert!(!land_in_window(&MapDef { islands: vec![Island { centre_m: [f64::NAN, 0.0], radius_m: 600.0, height_m: 80.0, seed: 0 }], ..m.clone() }, [0.0, 0.0]));
        assert_eq!(corner([4_096.0, 0.0]), [0.0, -4_096.0]);

        let mut s = Shore::new();
        // No map: nothing happens, and the ground reads as open water.
        assert!(s.step([0.0, 0.0]).is_none() && s.window().is_none());
        assert!(s.ground_height(1_000.0, -500.0) < -100.0);
        s.set_map(&m);
        assert!(s.ground_height(1_000.0, -500.0) > 40.0);
        // Far from land: no job.
        assert!(s.step([15_000.0, 15_000.0]).is_none());
        assert!(s.pending.is_none());
        // Near land: a job starts and its window arrives.
        let cam = [900.0, -300.0];
        assert!(s.step(cam).is_none());
        assert_eq!(s.pending, Some([1_024.0, -256.0]));
        let mut got = None;
        for _ in 0..2_000 {
            if let Some(u) = s.step(cam) { got = Some(u); break; }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let u = got.expect("the shore job did not finish");
        assert_eq!(u.centre, [1_024.0, -256.0]);
        assert_eq!(u.bytes.len(), (SHORE_TEXELS * SHORE_TEXELS * 2) as usize);
        assert_eq!(s.window(), Some(u.centre));
        // Small moves keep the window; a new map forgets it.
        assert!(s.step([1_500.0, -900.0]).is_none() && s.pending.is_none());
        s.set_map(&m);
        assert!(s.window().is_none());
    }
}
