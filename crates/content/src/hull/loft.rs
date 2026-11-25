//! Lofted shell geometry: validation, station interpolation, cross-sections, the shell frame and the SDF.
//!
//! # Design frame
//! The loft is built in a design frame: `x` from the aft extreme (0) to the forward extreme (`length_m`), `y` up
//! from the design waterline (draft datum), `z` to starboard from the centreline. Vessel-local metres (the
//! voxelized grid's frame, origin at its minimum corner) are the design frame shifted by whole cells: see
//! [`ShellLayout`].
//!
//! # Cross-section
//! At a position `t` along the hull the half-section (|z|, y) is the intersection of three half-planes:
//! below the deck (`y <= sheer`), inside the topside (a line through the deck edge leaning out by `flare`), and
//! above the bottom (a line from the keel rising at `deadrise`). That is a hard-chine section; with a large
//! deadrise or flare it degenerates gracefully to a triangle. Every station value is interpolated along `t` with
//! a monotone cubic (PCHIP), so the loft never overshoots its stations and the analytic bounds hold.
//!
//! # Ends
//! The stem and the transom are inclined planes (`bow_rake_deg`, `transom_rake_deg`). At a height `y` the hull
//! runs from `x_aft(y)` to `x_bow(y)` and `t` is measured between them, so the stations stretch with the rakes.
//! Over the aft run up to the second station the beam narrows to `transom_width` times the aftmost station's
//! beam at the transom (0 gives a double-ended stern).
use super::{HullShell, ParamRange, ShellError, Station};
use super::{
    BOW_RAKE_DEG, DEADRISE_DEG, DRAFT_M, FLARE_DEG, MAX_BEAM_FRACTION, MAX_STATIONS, MIN_INTERIOR_BEAM_M, MIN_MAX_BEAM_M, MIN_RUN_FRACTION, MIN_STATIONS,
    SHEER_M, SHELL_CELLS, STATION_BEAM_M, STATION_T, TRANSOM_RAKE_DEG, TRANSOM_WIDTH,
};
use crate::cell::{CELL_M, MAX_ENEMY_HULL_M, MAX_PLAYER_HULL_M, MIN_PLAYER_HULL_M};

const DEG: f64 = core::f64::consts::PI / 180.0;
/// Half-breadth of a column that holds no hull at all.
pub(crate) const NONE: f64 = f64::NEG_INFINITY;

/// Where the voxelized shell sits: the grid size and the whole-cell offsets between the design frame and the
/// vessel-local cell frame. Computed from the shell alone, so the editor, the voxelizer and [`super::shell_sdf`]
/// agree. Every boundary slab of the grid holds at least one solid cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShellLayout {
    /// Grid extent in cells. All zero when the shell has no solid cell.
    pub size: [u16; 3],
    /// Cell row whose bottom face is the design waterline (draft datum, y = 0 in the design frame). New designs
    /// set [`super::Hull::design_waterline_y`] to this. Negative or past the top only for shells wholly above or
    /// below the datum.
    pub waterline_y: i16,
    /// Cells from the design frame's x = 0 (aft extreme) to the grid's x = 0.
    pub x_offset: i16,
    /// Cells per side of the centreline; the centreline is the lattice plane `z = half_width_cells` (cells), so
    /// `size[2] == 2 * half_width_cells` and port and starboard mirror exactly.
    pub half_width_cells: u16,
}

impl ShellLayout {
    /// Design-frame metres of a vessel-local point.
    pub fn to_design(&self, p: [f64; 3]) -> [f64; 3] {
        [
            p[0] + self.x_offset as f64 * CELL_M,
            p[1] - self.waterline_y as f64 * CELL_M,
            p[2] - self.half_width_cells as f64 * CELL_M,
        ]
    }
    /// Vessel-local metres of a design-frame point.
    pub fn to_vessel(&self, d: [f64; 3]) -> [f64; 3] {
        [
            d[0] - self.x_offset as f64 * CELL_M,
            d[1] + self.waterline_y as f64 * CELL_M,
            d[2] + self.half_width_cells as f64 * CELL_M,
        ]
    }
}

pub(crate) fn validate(shell: &HullShell, max_length_m: f64) -> Result<(), ShellError> {
    let max_m = if max_length_m.is_finite() { max_length_m.clamp(MIN_PLAYER_HULL_M, MAX_ENEMY_HULL_M) } else { MAX_PLAYER_HULL_M };
    let n = shell.stations.len();
    if !(MIN_STATIONS..=MAX_STATIONS).contains(&n) {
        return Err(ShellError::StationCount(n));
    }
    let l = shell.length_m;
    if !(l.is_finite() && (MIN_PLAYER_HULL_M..=max_m).contains(&l)) {
        return Err(ShellError::Length { length_m: l, min_m: MIN_PLAYER_HULL_M, max_m });
    }
    let checks: [(&'static str, ParamRange, fn(&Station) -> f64); 6] = [
        ("t", STATION_T, |s| s.t),
        ("beam_m", STATION_BEAM_M, |s| s.beam_m),
        ("draft_m", DRAFT_M, |s| s.draft_m),
        ("deadrise_deg", DEADRISE_DEG, |s| s.deadrise_deg),
        ("flare_deg", FLARE_DEG, |s| s.flare_deg),
        ("sheer_m", SHEER_M, |s| s.sheer_m),
    ];
    for s in &shell.stations {
        for (name, range, get) in &checks {
            if !range.contains(get(s)) {
                return Err(ShellError::Param(name));
            }
        }
    }
    if shell.stations.windows(2).any(|w| w[1].t <= w[0].t) {
        return Err(ShellError::StationOrder);
    }
    for (name, range, v) in [
        ("bow_rake_deg", BOW_RAKE_DEG, shell.bow_rake_deg),
        ("transom_rake_deg", TRANSOM_RAKE_DEG, shell.transom_rake_deg),
        ("transom_width", TRANSOM_WIDTH, shell.transom_width),
        ("shell_cells", SHELL_CELLS, shell.shell_cells as f64),
    ] {
        if !range.contains(v) {
            return Err(ShellError::Param(name));
        }
    }
    // A station narrower than two cells between the ends would pinch the hull into two pieces.
    if shell.stations[1..n - 1].iter().any(|s| s.beam_m < MIN_INTERIOR_BEAM_M) {
        return Err(ShellError::Param("beam_m"));
    }
    let max_beam = shell.stations.iter().map(|s| s.beam_m).fold(0.0, f64::max);
    if max_beam < MIN_MAX_BEAM_M || max_beam > MAX_BEAM_FRACTION * l {
        return Err(ShellError::Param("beam_m"));
    }
    // The stem and transom planes must leave a real run of hull at every height the grid can reach.
    let ends = Ends::new(shell);
    let (d_max, s_max) = extremes(&shell.stations);
    for y in [-d_max, s_max] {
        if ends.bow(y) - ends.aft(y) < MIN_RUN_FRACTION * l {
            // Blame whichever end eats more of the length at that height.
            let bow_loss = l - ends.bow(y);
            let aft_loss = ends.aft(y);
            return Err(ShellError::Param(if bow_loss >= aft_loss { "bow_rake_deg" } else { "transom_rake_deg" }));
        }
    }
    Ok(())
}

/// Deepest draft and highest sheer over the stations (PCHIP never exceeds them).
fn extremes(stations: &[Station]) -> (f64, f64) {
    stations.iter().fold((0.0f64, 0.0f64), |(d, s), st| (d.max(st.draft_m), s.max(st.sheer_m)))
}

/// The stem and transom planes in the design frame.
#[derive(Clone, Copy, Debug)]
struct Ends {
    /// x of the transom at y = `aft_y0`, and how far it moves aft per metre of height.
    aft_x0: f64,
    aft_y0: f64,
    aft_tan: f64,
    aft_cos: f64,
    /// x of the stem at y = `bow_y0`, and how far it moves forward per metre of height.
    bow_x0: f64,
    bow_y0: f64,
    bow_tan: f64,
    bow_cos: f64,
}

impl Ends {
    fn new(shell: &HullShell) -> Self {
        let first = shell.stations.first().copied().unwrap_or(ZERO_STATION);
        let last = shell.stations.last().copied().unwrap_or(ZERO_STATION);
        let l = shell.length_m;
        let aft_tan = libm::tan(shell.transom_rake_deg * DEG);
        let bow_tan = libm::tan(shell.bow_rake_deg * DEG);
        // The transom runs from the keel (y = -draft) to the deck (y = sheer) at the aftmost station; its aftmost
        // point sits on x = 0. The stem does the same at the foremost station with its foremost point on x = L.
        let aft_h = first.sheer_m + first.draft_m;
        let bow_h = last.sheer_m + last.draft_m;
        Ends {
            aft_x0: (aft_h * aft_tan).max(0.0),
            aft_y0: -first.draft_m,
            aft_tan,
            aft_cos: libm::cos(shell.transom_rake_deg * DEG),
            bow_x0: l - (-bow_h * bow_tan).max(0.0),
            bow_y0: last.sheer_m,
            bow_tan,
            bow_cos: libm::cos(shell.bow_rake_deg * DEG),
        }
    }
    /// Transom x at height `y` (raked aft going up).
    fn aft(&self, y: f64) -> f64 { self.aft_x0 - (y - self.aft_y0) * self.aft_tan }
    /// Stem x at height `y` (raked forward going up).
    fn bow(&self, y: f64) -> f64 { self.bow_x0 - (self.bow_y0 - y) * self.bow_tan }
}

const ZERO_STATION: Station = Station { t: 0.0, beam_m: 0.0, draft_m: 0.0, deadrise_deg: 0.0, flare_deg: 0.0, sheer_m: 0.0 };

/// Monotone cubic (Fritsch-Carlson PCHIP) through the station values of one parameter.
#[derive(Clone, Debug)]
struct Pchip {
    y: Vec<f64>,
    m: Vec<f64>,
}

fn pchip_slopes(t: &[f64], y: &[f64]) -> Vec<f64> {
    let n = t.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let h: Vec<f64> = t.windows(2).map(|w| w[1] - w[0]).collect();
    let d: Vec<f64> = (0..n - 1).map(|k| (y[k + 1] - y[k]) / h[k]).collect();
    let mut m = vec![0.0; n];
    if n == 2 {
        m[0] = d[0];
        m[1] = d[0];
        return m;
    }
    for k in 1..n - 1 {
        if d[k - 1] * d[k] > 0.0 {
            let w1 = 2.0 * h[k] + h[k - 1];
            let w2 = h[k] + 2.0 * h[k - 1];
            m[k] = (w1 + w2) / (w1 / d[k - 1] + w2 / d[k]);
        }
    }
    m[0] = end_slope(h[0], h[1], d[0], d[1]);
    m[n - 1] = end_slope(h[n - 2], h[n - 3], d[n - 2], d[n - 3]);
    m
}

/// Shape-preserving three-point end slope.
fn end_slope(h0: f64, h1: f64, d0: f64, d1: f64) -> f64 {
    let m = ((2.0 * h0 + h1) * d0 - h0 * d1) / (h0 + h1);
    if m * d0 <= 0.0 {
        0.0
    } else if d0 * d1 < 0.0 && m.abs() > 3.0 * d0.abs() {
        3.0 * d0
    } else {
        m
    }
}

/// Station parameters at one `t`, in the order beam, draft, deadrise, flare, sheer.
const PARAMS: usize = 5;

/// One cross-section, ready for half-breadth and distance queries.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Section {
    half_beam: f64,
    sheer: f64,
    draft: f64,
    tan_a: f64,
    tan_f: f64,
}

impl Section {
    /// Half-breadth of the section at height `y`; [`NONE`] above the deck. Negative below the keel.
    #[inline]
    pub(crate) fn half_breadth(&self, y: f64) -> f64 {
        if y > self.sheer {
            return NONE;
        }
        let top = self.half_beam - (self.sheer - y) * self.tan_f;
        let bottom = if self.tan_a > 0.0 {
            (y + self.draft) / self.tan_a
        } else if y >= -self.draft {
            f64::INFINITY
        } else {
            NONE
        };
        top.min(bottom)
    }

    /// Unsigned distance in the section plane from `(|z|, y)` to the section outline (the folded half: keel,
    /// chine, deck edge, deck centre).
    fn outline_distance(&self, q: [f64; 2]) -> f64 {
        let (hb, s, d, ta, tf) = (self.half_beam, self.sheer, self.draft, self.tan_a, self.tan_f);
        let keel = [0.0, -d];
        let edge = [hb, s];
        let centre = [0.0, s];
        let denom = 1.0 - ta * tf;
        let zc = if denom > 0.0 { (hb - (s + d) * tf) / denom } else { 0.0 };
        let chain: [[f64; 2]; 4] = if zc <= 0.0 {
            // The topside reaches the centreline above the keel (or the section has no width at all).
            let y0 = if tf > 0.0 { (s - hb / tf).clamp(-d, s) } else { -d };
            [[0.0, y0], edge, centre, centre]
        } else {
            let yc = -d + zc * ta;
            if yc >= s && ta > 0.0 {
                // The bottom reaches the deck before the topside: a V section.
                [keel, [(s + d) / ta, s], centre, centre]
            } else {
                [keel, [zc, yc], edge, centre]
            }
        };
        chain.windows(2).map(|w| segment_distance(q, w[0], w[1])).fold(f64::INFINITY, f64::min)
    }
}

fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let h = if len2 > 0.0 { ((ap[0] * ab[0] + ap[1] * ab[1]) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let dx = ap[0] - ab[0] * h;
    let dy = ap[1] - ab[1] * h;
    libm::sqrt(dx * dx + dy * dy)
}

/// A validated shell prepared for repeated queries: interpolants, end planes and the grid layout.
/// Build it once per edit and query it as often as needed; [`super::shell_sdf`] builds one per call.
#[derive(Clone, Debug)]
pub struct ShellField {
    length: f64,
    t: Vec<f64>,
    params: [Pchip; PARAMS],
    ends: Ends,
    transom_width: f64,
    /// The aft taper runs from t = 0 to the second station.
    taper_run: f64,
    d_max: f64,
    s_max: f64,
    layout: ShellLayout,
    /// Half-breadth at every cell-centre column of the grid, `x + size_x * y`. Reused by the voxelizer.
    pub(crate) centre_hb: Vec<f64>,
}

impl ShellField {
    /// Prepare a shell. `None` when it fails [`super::validate_shell`] at the enemy-class limit (150 m).
    pub fn new(shell: &HullShell) -> Option<ShellField> {
        validate(shell, MAX_ENEMY_HULL_M).ok()?;
        let st = &shell.stations;
        let t: Vec<f64> = st.iter().map(|s| s.t).collect();
        let getters: [fn(&Station) -> f64; PARAMS] =
            [|s| s.beam_m, |s| s.draft_m, |s| s.deadrise_deg, |s| s.flare_deg, |s| s.sheer_m];
        let params = getters.map(|g| {
            let y: Vec<f64> = st.iter().map(g).collect();
            let m = pchip_slopes(&t, &y);
            Pchip { y, m }
        });
        let (d_max, s_max) = extremes(st);
        let mut field = ShellField {
            length: shell.length_m,
            taper_run: t[1].max(1e-6),
            t,
            params,
            ends: Ends::new(shell),
            transom_width: shell.transom_width,
            d_max,
            s_max,
            layout: ShellLayout::default(),
            centre_hb: Vec::new(),
        };
        field.lay_out();
        Some(field)
    }

    pub fn layout(&self) -> ShellLayout { self.layout }

    /// Station parameters at `t` (clamped to the end stations outside their range).
    fn params_at(&self, t: f64) -> [f64; PARAMS] {
        let n = self.t.len();
        if t <= self.t[0] {
            return self.params.each_ref().map(|p| p.y[0]);
        }
        if t >= self.t[n - 1] {
            return self.params.each_ref().map(|p| p.y[n - 1]);
        }
        let k = self.t.partition_point(|&s| s <= t).saturating_sub(1).min(n - 2);
        let h = self.t[k + 1] - self.t[k];
        let s = (t - self.t[k]) / h;
        let (s2, s3) = (s * s, s * s * s);
        // Cubic Hermite, written so equal neighbours with zero slopes reproduce the station value exactly (a
        // flat run of stations must stay on its lattice plane).
        let h10 = s3 - 2.0 * s2 + s;
        let h01 = -2.0 * s3 + 3.0 * s2;
        let h11 = s3 - s2;
        self.params.each_ref().map(|p| p.y[k] + (p.y[k + 1] - p.y[k]) * h01 + h * (h10 * p.m[k] + h11 * p.m[k + 1]))
    }

    /// Cross-section at `t` in 0..=1, with the aft taper applied.
    pub(crate) fn section(&self, t: f64) -> Section {
        let [beam, draft, deadrise, flare, sheer] = self.params_at(t);
        let w = if t < self.taper_run {
            let u = 1.0 - (t / self.taper_run).max(0.0);
            self.transom_width + (1.0 - self.transom_width) * (1.0 - u * u)
        } else {
            1.0
        };
        Section {
            half_beam: 0.5 * beam.max(0.0) * w,
            sheer,
            draft: draft.max(0.0),
            tan_a: libm::tan(deadrise.clamp(DEADRISE_DEG.min, DEADRISE_DEG.max) * DEG),
            tan_f: libm::tan(flare.clamp(FLARE_DEG.min, FLARE_DEG.max) * DEG),
        }
    }

    /// `t` at design-frame `(x, y)`, or `None` outside the stem and transom planes.
    fn t_at(&self, x: f64, y: f64) -> Option<f64> {
        let (a, b) = (self.ends.aft(y), self.ends.bow(y));
        if x < a || x > b || b <= a {
            return None;
        }
        Some(((x - a) / (b - a)).clamp(0.0, 1.0))
    }

    /// Half-breadth of the hull at design-frame `(x, y)`: the hull holds `(x, y, z)` exactly when
    /// `|z| <= half_breadth`. [`NONE`] where the column holds no hull.
    #[inline]
    pub(crate) fn half_breadth(&self, x: f64, y: f64) -> f64 {
        match self.t_at(x, y) {
            Some(t) => self.section(t).half_breadth(y),
            None => NONE,
        }
    }

    /// Signed distance at a design-frame point (negative inside). The sign is exact (it is the same test the
    /// voxelizer uses); the magnitude is the distance within the cross-section combined with the distance to the
    /// stem and transom planes, which is exact on most of the hull and an estimate near where they meet.
    pub fn sdf_design(&self, p: [f64; 3]) -> f64 {
        let [x, y, z] = p;
        let az = z.abs();
        let (a, b) = (self.ends.aft(y), self.ends.bow(y));
        let span = (b - a).max(1e-9);
        let t = ((x - a) / span).clamp(0.0, 1.0);
        let sec = self.section(t);
        let in_section = az <= sec.half_breadth(y);
        let d_sec = sec.outline_distance([az, y]);
        // Positive outside the end planes, negative inside them.
        let d_end = ((a - x) * self.ends.aft_cos).max((x - b) * self.ends.bow_cos);
        let inside = in_section && x >= a && x <= b;
        if inside {
            -d_sec.min((-d_end).max(0.0))
        } else {
            let o_s = if in_section { 0.0 } else { d_sec };
            let o_e = d_end.max(0.0);
            libm::sqrt(o_s * o_s + o_e * o_e)
        }
    }

    /// Signed distance at a vessel-local point (the voxelized grid's frame).
    pub fn sdf(&self, p: [f64; 3]) -> f64 { self.sdf_design(self.layout.to_design(p)) }

    /// Scan every cell-centre column of the analytic box and keep the tight box of columns holding a cell.
    fn lay_out(&mut self) {
        let inv = 1.0 / CELL_M;
        // Cells whose centre can lie inside: centre x in 0..=L, centre y in -d_max..=s_max.
        let nx = (self.length * inv + 0.5).floor() as i32;
        let jb = -((self.d_max * inv + 0.5).floor() as i32);
        let ja = (self.s_max * inv + 0.5).floor() as i32;
        let ny = (ja - jb).max(0);
        let mut hb = vec![NONE; (nx.max(0) * ny) as usize];
        let (mut i0, mut i1, mut j0, mut j1, mut half) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN, 0i32);
        for j in 0..ny {
            let y = (jb + j) as f64 * CELL_M + 0.5 * CELL_M;
            for i in 0..nx {
                let x = (i as f64 + 0.5) * CELL_M;
                let h = self.half_breadth(x, y);
                hb[(i + nx * j) as usize] = h;
                // A cell exists in this column when the innermost centre (|z| = half a cell) is inside.
                if h >= 0.5 * CELL_M {
                    i0 = i0.min(i);
                    i1 = i1.max(i);
                    j0 = j0.min(j);
                    j1 = j1.max(j);
                    half = half.max((h * inv + 0.5).floor().min(1000.0) as i32);
                }
            }
        }
        if i0 > i1 {
            self.layout = ShellLayout { waterline_y: (-jb).clamp(0, i16::MAX as i32) as i16, ..ShellLayout::default() };
            self.centre_hb.clear();
            return;
        }
        let (sx, sy) = (i1 - i0 + 1, j1 - j0 + 1);
        let mut centre = Vec::with_capacity((sx * sy) as usize);
        for j in j0..=j1 {
            for i in i0..=i1 {
                centre.push(hb[(i + nx * j) as usize]);
            }
        }
        self.centre_hb = centre;
        self.layout = ShellLayout {
            size: [sx as u16, sy as u16, (2 * half) as u16],
            waterline_y: (-(jb + j0)) as i16,
            x_offset: i0 as i16,
            half_width_cells: half as u16,
        };
    }
}
