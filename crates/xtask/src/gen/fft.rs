//! Unit `xt-gen-fft` (hard: spectral synthesis that must loop exactly). The baked ocean detail.
//! DESIGN.md L1903: a Rust-baked Tessendorf FFT looping every 24 s (quantised dispersion): 64 frames of 256^2
//! normals plus turbulence, sampled at 41 m and 11.3 m, no displacement, no live FFT in v1. L1907 (whitecaps
//! multiply the Gerstner Jacobian by the baked turbulence from sea state 3). L1905 (water normals smooth).
//!
//! Output (`hull_mesh::gen_files`): `OCEAN_DETAIL_NORMALS` as a `TexBlob` Array in `TexFormat::RgtcRg` (BC5; x, z
//! slopes) and `OCEAN_DETAIL_TURBULENCE` as an Array in `TexFormat::RgtcR` (BC4; Jacobian-derived turbulence),
//! each `OCEAN_DETAIL_FRAMES` layers of `OCEAN_DETAIL_SIZE`^2, tileable in space and looping in time (dispersion
//! omega quantised to multiples of 2 pi / 24 s so frame 64 equals frame 0), full mip chains, BC blocks encoded
//! in Rust (deterministic). A Phillips or JONSWAP spectrum, fixed wind and seed; the in-crate radix-2 FFT is f64 with
//! libm trig. About 8 MB in all (L1903). Unit: `xt-gen-fft`.
//!
//! # Spectrum
//! Phillips spectrum on a 41 m patch (the larger sampling scale) for a fixed 9 m/s wind along +x, with:
//! a high-pass at 8 m wavelength, because the Gerstner set already draws 8 to 400 m (L1885) and the detail must
//! not double it; a 0.2 m small-wave cut-off (`exp(-k^2 l^2)`, so ripples under about 1.3 m fade out) that keeps
//! the normals smooth (L1905) and free of aliasing at the 0.16 m texel; directional spreading
//! `0.1 + 0.9 cos^4` with waves running against the wind at half strength, so the detail still reads when the
//! shader rotates it to the live wind. Amplitudes are Gaussian from a seeded SplitMix hash with Box-Muller
//! (libm), so the bake is the same on every platform. The Nyquist row and column and the mean are zero, which
//! keeps every field exactly Hermitian (real).
//!
//! # Loop
//! Deep-water dispersion `omega = sqrt(g k)` is rounded to a whole multiple m >= 1 of `2 pi / 24 s`. Frame f of F
//! is at `t = 24 f / F` s, so `omega t = 2 pi m f / F` and the phase factor is the exact table entry
//! `(m f) mod F` of the F-th roots of unity: frame F is frame 0 bit for bit, and any t + 24 s equals t.
//! The time term is `h0(k) e^(-i omega t) + conj(h0(-k)) e^(i omega t)`, so waves travel along their k.
//!
//! # Fields and encoding
//! Per frame three complex inverse FFTs give the slopes (dh/dx, dh/dz) and the horizontal-displacement
//! derivatives, two real fields per transform. Amplitude is scaled once (from frame 0) so the slope variance
//! of both axes together is 0.012 (RMS slope about 0.11, about 0.077 per axis), the `DETAIL_SLOPE_VARIANCE`
//! the water shader turns into roughness once a tiling is too fine to resolve.
//! - Normals (BC5): R = 0.5 + 0.5 n.x, G = 0.5 + 0.5 n.z of the unit normal `n = normalize(-dh/dx, 1, -dh/dz)`
//!   in world axes (+x, +z of the patch). Reconstruct `n.y = sqrt(1 - n.x^2 - n.z^2)`. For small slopes
//!   `n.x ~ -dh/dx`. Mips average the slopes, then re-normalise, so distant water is smoother (L1905).
//! - Turbulence (BC4): with Gerstner-style choppy displacement (lambda 1), the Jacobian
//!   `J = (1 - Dxx)(1 - Dzz) - Dxz^2` drops below 1 where water converges at crests. The stored value is
//!   `0.5 + 0.5 (q - mean q) / (3 sigma q)` with `q = 1 - J`, clamped to 0..1: 0.5 on average with a
//!   standard deviation near 0.17, 1 on the sharpest crests, lower in troughs. Whitecaps multiply the Gerstner
//!   foam term by it (L1907). Mips average.
//!
//! Layout follows `TexBlob`: per layer (frame) its full mip chain to 1x1; levels under 4x4 still take one
//! block, edge texels repeated. At 256^2 x 64 frames that is 5.6 MB of BC5 plus 2.8 MB of BC4.
use super::GenFile;
use hull_mesh::gen_files::{self as gf, TexBlob, TexFormat, TexKind};
use std::path::Path;

const GRAVITY: f64 = 9.81;
const WIND_MS: f64 = 9.0;
const WIND_DIR: [f64; 2] = [1.0, 0.0];
const SEED: u64 = 0xB1AC_0CEA_FF70_0003;
/// Shortest Gerstner wavelength (L1885); longer waves are filtered out of the detail.
const HIGH_PASS_M: f64 = 8.0;
const SMALL_WAVE_M: f64 = 0.2;
/// Target slope variance of the detail normals, both axes together: the mean of (dh/dx)^2 + (dh/dz)^2.
/// `water.gdshader` adds this much (its `DETAIL_SLOPE_VARIANCE`) to the roughness once a tiling is too small
/// to resolve, so the bake must carry the same amount or distant water would change brightness.
const SLOPE_VARIANCE: f64 = 0.012;
/// Turbulence spans this many standard deviations of q from mid-grey to white, which gives the standard
/// deviation of about 0.17 that the water shader's foam thresholds assume.
const TURB_SIGMAS: f64 = 3.0;

/// Parameters of one bake.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bake { pub size: usize, pub frames: usize, pub loop_s: f64, pub patch_m: f64, pub wind_ms: f64, pub seed: u64 }

impl Bake {
    /// The shipped bake (L1903).
    pub(crate) fn shipped() -> Bake {
        Bake { size: gf::OCEAN_DETAIL_SIZE as usize, frames: gf::OCEAN_DETAIL_FRAMES as usize, loop_s: gf::OCEAN_DETAIL_LOOP_S, patch_m: gf::OCEAN_DETAIL_SCALES_M[0], wind_ms: WIND_MS, seed: SEED }
    }
    fn validate(&self) -> Result<(), String> {
        if !(self.size >= 4 && self.size.is_power_of_two() && self.size <= 4096) { return Err(format!("detail size {} must be a power of two in 4..=4096", self.size)); }
        if !(1..=1024).contains(&self.frames) { return Err(format!("{} frames outside 1..=1024", self.frames)); }
        if !(self.loop_s.is_finite() && self.loop_s > 0.0 && self.patch_m.is_finite() && self.patch_m > 0.0 && self.wind_ms.is_finite() && self.wind_ms > 0.0) { return Err("loop, patch and wind must be positive".into()); }
        Ok(())
    }
    fn mip_levels(&self) -> u32 { self.size.trailing_zeros() + 1 }
}

// ---------------------------------------------------------------------------------------------------------
// Complex numbers and the radix-2 FFT
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct C { pub re: f64, pub im: f64 }
impl C {
    const ZERO: C = C { re: 0.0, im: 0.0 };
    fn new(re: f64, im: f64) -> C { C { re, im } }
    fn add(self, o: C) -> C { C::new(self.re + o.re, self.im + o.im) }
    fn sub(self, o: C) -> C { C::new(self.re - o.re, self.im - o.im) }
    fn mul(self, o: C) -> C { C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re) }
    fn scale(self, s: f64) -> C { C::new(self.re * s, self.im * s) }
    fn conj(self) -> C { C::new(self.re, -self.im) }
    /// Multiply by i.
    fn i(self) -> C { C::new(-self.im, self.re) }
    fn cis(theta: f64) -> C { C::new(libm::cos(theta), libm::sin(theta)) }
}

/// Unnormalised inverse transform `x[j] = sum_k X[k] e^(+2 pi i j k / n)`, iterative radix 2.
pub(crate) struct Fft { n: usize, twiddle: Vec<C>, rev: Vec<usize> }
impl Fft {
    pub(crate) fn new(n: usize) -> Fft {
        let bits = n.trailing_zeros();
        let twiddle = (0..n / 2).map(|k| C::cis(2.0 * std::f64::consts::PI * k as f64 / n as f64)).collect();
        let rev = (0..n).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (usize::BITS - bits) }).collect();
        Fft { n, twiddle, rev }
    }
    pub(crate) fn inverse(&self, a: &mut [C]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if j > i { a.swap(i, j); }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let w = self.twiddle[k * step];
                    let u = a[start + k];
                    let v = a[start + k + len / 2].mul(w);
                    a[start + k] = u.add(v);
                    a[start + k + len / 2] = u.sub(v);
                }
            }
            len <<= 1;
        }
    }
    /// 2D inverse transform of a row-major n x n grid (x fastest).
    pub(crate) fn inverse_2d(&self, a: &mut [C]) {
        let n = self.n;
        for row in a.chunks_exact_mut(n) { self.inverse(row); }
        let mut col = vec![C::ZERO; n];
        for x in 0..n {
            for z in 0..n { col[z] = a[z * n + x]; }
            self.inverse(&mut col);
            for z in 0..n { a[z * n + x] = col[z]; }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Spectrum
// ---------------------------------------------------------------------------------------------------------

fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
/// Uniform in (0, 1], never 0 (safe for the log).
fn uniform(seed: u64, a: u64, b: u64) -> f64 { ((mix64(seed ^ mix64((a << 32) ^ b)) >> 11) as f64 + 1.0) / (1u64 << 53) as f64 }

/// The initial amplitudes, wavenumbers and quantised dispersion of one bake.
pub(crate) struct Spectrum {
    n: usize,
    /// h0(k) per bin, row-major (x index fastest), already scaled to the slope target.
    h0: Vec<C>,
    /// Signed wavenumbers per bin (rad/m).
    kx: Vec<f64>,
    kz: Vec<f64>,
    /// omega = m 2 pi / loop.
    m: Vec<u64>,
    omega0: f64,
    frames: usize,
}

fn signed(i: usize, n: usize) -> i64 { if i < n / 2 { i as i64 } else { i as i64 - n as i64 } }

fn phillips(b: &Bake, kx: f64, kz: f64) -> f64 {
    let k2 = kx * kx + kz * kz;
    if k2 <= 0.0 { return 0.0; }
    let k = libm::sqrt(k2);
    let lw = b.wind_ms * b.wind_ms / GRAVITY;
    let c = (kx * WIND_DIR[0] + kz * WIND_DIR[1]) / k;
    let spread = (0.1 + 0.9 * c * c * c * c) * if c < 0.0 { 0.5 } else { 1.0 };
    let k_hp = 2.0 * std::f64::consts::PI / HIGH_PASS_M;
    let high_pass = 1.0 - libm::exp(-libm::pow(k / k_hp, 4.0));
    libm::exp(-1.0 / (k2 * lw * lw)) / (k2 * k2) * spread * high_pass * libm::exp(-k2 * SMALL_WAVE_M * SMALL_WAVE_M)
}

impl Spectrum {
    pub(crate) fn new(b: &Bake) -> Spectrum {
        let n = b.size;
        let omega0 = 2.0 * std::f64::consts::PI / b.loop_s;
        let mut s = Spectrum { n, h0: vec![C::ZERO; n * n], kx: vec![0.0; n * n], kz: vec![0.0; n * n], m: vec![0; n * n], omega0, frames: b.frames };
        let dk = 2.0 * std::f64::consts::PI / b.patch_m;
        for iz in 0..n {
            for ix in 0..n {
                let i = iz * n + ix;
                let (kx, kz) = (signed(ix, n) as f64 * dk, signed(iz, n) as f64 * dk);
                s.kx[i] = kx;
                s.kz[i] = kz;
                if ix == n / 2 || iz == n / 2 || (ix == 0 && iz == 0) { continue; }
                let k = libm::sqrt(kx * kx + kz * kz);
                s.m[i] = (libm::round(libm::sqrt(GRAVITY * k) / omega0) as u64).max(1);
                let (u1, u2) = (uniform(b.seed, ix as u64, iz as u64), uniform(b.seed ^ 0x5EED, ix as u64, iz as u64));
                let r = libm::sqrt(-2.0 * libm::log(u1));
                let th = 2.0 * std::f64::consts::PI * u2;
                let amp = libm::sqrt(phillips(b, kx, kz) * 0.5);
                s.h0[i] = C::new(r * libm::cos(th) * amp, r * libm::sin(th) * amp);
            }
        }
        // Scale so frame 0's slope variance hits the target (fields are linear in h0).
        let fft = Fft::new(n);
        let (sx, sz) = s.slopes(&fft, &s.h_at_frame(0));
        let var: f64 = sx.iter().zip(sz.iter()).map(|(x, z)| x * x + z * z).sum::<f64>() / (n * n) as f64;
        if var > 0.0 {
            let k = libm::sqrt(SLOPE_VARIANCE / var);
            for h in &mut s.h0 { *h = h.scale(k); }
        }
        s
    }

    /// Index of bin -k for bin i.
    fn neg(&self, i: usize) -> usize {
        let n = self.n;
        let (ix, iz) = (i % n, i / n);
        ((n - iz) % n) * n + (n - ix) % n
    }

    /// H(k, t) from per-bin phase factors e^(-i omega t).
    fn h_with(&self, phase: impl Fn(usize) -> C) -> Vec<C> {
        (0..self.n * self.n).map(|i| {
            let p = phase(i);
            self.h0[i].mul(p).add(self.h0[self.neg(i)].conj().mul(p.conj()))
        }).collect()
    }

    /// H(k) at frame f from the exact table of F-th roots of unity.
    pub(crate) fn h_at_frame(&self, f: usize) -> Vec<C> {
        let frames = self.frames.max(1) as u64;
        let roots: Vec<C> = (0..frames).map(|j| C::cis(-2.0 * std::f64::consts::PI * j as f64 / frames as f64)).collect();
        self.h_with(|i| roots[((self.m[i] % frames) * (f as u64 % frames) % frames) as usize])
    }

    /// H(k) at any time t, straight from libm trig (the reference for the table path).
    pub(crate) fn h_at_time(&self, t: f64) -> Vec<C> { self.h_with(|i| C::cis(-(self.m[i] as f64) * self.omega0 * t)) }

    /// Slope fields (dh/dx, dh/dz) of a spectrum, packed as one complex transform.
    fn slopes(&self, fft: &Fft, h: &[C]) -> (Vec<f64>, Vec<f64>) {
        let mut a: Vec<C> = (0..h.len()).map(|i| h[i].scale(self.kx[i]).i().add(h[i].scale(self.kz[i]).i().i())).collect();
        fft.inverse_2d(&mut a);
        (a.iter().map(|c| c.re).collect(), a.iter().map(|c| c.im).collect())
    }

    /// Jacobian compression q = 1 - J of the Gerstner-style choppy displacement (lambda 1).
    fn compression(&self, fft: &Fft, h: &[C]) -> Vec<f64> {
        let k = |i: usize| { let k = libm::sqrt(self.kx[i] * self.kx[i] + self.kz[i] * self.kz[i]); if k > 0.0 { 1.0 / k } else { 0.0 } };
        let mut d2: Vec<C> = (0..h.len()).map(|i| h[i].scale(self.kx[i] * self.kx[i] * k(i)).add(h[i].scale(self.kz[i] * self.kz[i] * k(i)).i())).collect();
        let mut d3: Vec<C> = (0..h.len()).map(|i| h[i].scale(self.kx[i] * self.kz[i] * k(i))).collect();
        fft.inverse_2d(&mut d2);
        fft.inverse_2d(&mut d3);
        // A single crest h = A cos(kx) displaces by -A sin(kx), so Dxx = -k h: J = (1 + dxx)(1 + dzz) - dxz^2
        // with dxx = -(Re of the kx^2/k transform).
        (0..h.len()).map(|i| {
            let (dxx, dzz, dxz) = (-d2[i].re, -d2[i].im, -d3[i].re);
            1.0 - ((1.0 + dxx) * (1.0 + dzz) - dxz * dxz)
        }).collect()
    }
}

// ---------------------------------------------------------------------------------------------------------
// Mips and block compression
// ---------------------------------------------------------------------------------------------------------

/// Halve a square tileable field by 2x2 box averages.
fn downsample(v: &[f64], n: usize) -> Vec<f64> {
    let h = (n / 2).max(1);
    if n == 1 { return v.to_vec(); }
    let mut out = vec![0.0; h * h];
    for y in 0..h {
        for x in 0..h {
            let (x0, y0) = (2 * x, 2 * y);
            out[y * h + x] = 0.25 * (v[y0 * n + x0] + v[y0 * n + x0 + 1] + v[(y0 + 1) * n + x0] + v[(y0 + 1) * n + x0 + 1]);
        }
    }
    out
}

/// One BC4 block of 16 values on a 0..255 scale, row-major within the block: 8-value mode with the block's max
/// and min as endpoints (a flat block is all index 0), each texel at the nearest palette entry.
pub(crate) fn bc4_block(v: &[f64; 16]) -> [u8; 8] {
    let q = |x: f64| libm::round(if x.is_finite() { x.clamp(0.0, 255.0) } else { 0.0 });
    let hi = v.iter().map(|&x| q(x)).fold(0.0, f64::max);
    let lo = v.iter().map(|&x| q(x)).fold(255.0, f64::min);
    let (r0, r1) = (hi as u8, lo as u8);
    let mut out = [0u8; 8];
    out[0] = r0;
    out[1] = r1;
    if r0 == r1 { return out; }
    let (a, b) = (r0 as f64, r1 as f64);
    let mut palette = [0.0; 8];
    palette[0] = a;
    palette[1] = b;
    for i in 2..8 { palette[i] = ((8 - i) as f64 * a + (i - 1) as f64 * b) / 7.0; }
    let mut bits: u64 = 0;
    for (t, &x) in v.iter().enumerate() {
        let x = if x.is_finite() { x.clamp(0.0, 255.0) } else { 0.0 };
        let mut best = 0usize;
        for i in 1..8 { if (palette[i] - x).abs() < (palette[best] - x).abs() { best = i; } }
        bits |= (best as u64) << (3 * t);
    }
    out[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
    out
}

/// BC4 blocks of one `n` x `n` level (0..255 values), blocks row-major; texels past the edge repeat the edge.
fn bc4_level(v: &[f64], n: usize, out: &mut Vec<u8>) {
    let blocks = (n + 3) / 4;
    for by in 0..blocks {
        for bx in 0..blocks {
            let mut b = [0.0; 16];
            for t in 0..16 {
                let (x, y) = ((bx * 4 + t % 4).min(n - 1), (by * 4 + t / 4).min(n - 1));
                b[t] = v[y * n + x];
            }
            out.extend_from_slice(&bc4_block(&b));
        }
    }
}

/// BC5 blocks (red BC4 then green BC4 per block) of one level.
fn bc5_level(r: &[f64], g: &[f64], n: usize, out: &mut Vec<u8>) {
    let (mut rb, mut gb) = (Vec::new(), Vec::new());
    bc4_level(r, n, &mut rb);
    bc4_level(g, n, &mut gb);
    for (a, b) in rb.chunks_exact(8).zip(gb.chunks_exact(8)) { out.extend_from_slice(a); out.extend_from_slice(b); }
}

/// Encoded layer bytes (normals BC5, turbulence BC4) of one frame, each with its full mip chain.
fn encode_frame(n: usize, sx: Vec<f64>, sz: Vec<f64>, turb: Vec<f64>) -> (Vec<u8>, Vec<u8>) {
    let (mut normals, mut turbulence) = (Vec::new(), Vec::new());
    let (mut sx, mut sz, mut turb, mut size) = (sx, sz, turb, n);
    loop {
        let (mut r, mut g) = (vec![0.0; size * size], vec![0.0; size * size]);
        for i in 0..size * size {
            let inv = 1.0 / libm::sqrt(1.0 + sx[i] * sx[i] + sz[i] * sz[i]);
            r[i] = (0.5 - 0.5 * sx[i] * inv) * 255.0;
            g[i] = (0.5 - 0.5 * sz[i] * inv) * 255.0;
        }
        bc5_level(&r, &g, size, &mut normals);
        let t: Vec<f64> = turb.iter().map(|v| v * 255.0).collect();
        bc4_level(&t, size, &mut turbulence);
        if size == 1 { break; }
        sx = downsample(&sx, size);
        sz = downsample(&sz, size);
        turb = downsample(&turb, size);
        size /= 2;
    }
    (normals, turbulence)
}

/// Bake both arrays.
pub(crate) fn bake(b: &Bake) -> Result<(TexBlob, TexBlob), String> {
    b.validate()?;
    let n = b.size;
    let spec = Spectrum::new(b);
    let fft = Fft::new(n);
    // Turbulence statistics from frame 0 (serial sums in index order).
    let q0 = spec.compression(&fft, &spec.h_at_frame(0));
    let mean = q0.iter().sum::<f64>() / q0.len() as f64;
    let var = q0.iter().map(|q| (q - mean) * (q - mean)).sum::<f64>() / q0.len() as f64;
    let sigma = libm::sqrt(var).max(1e-12);
    let frames: Vec<usize> = (0..b.frames).collect();
    let layers = super::basic::map_in_order(&frames, super::basic::workers(), |&f| {
        let h = spec.h_at_frame(f);
        let (sx, sz) = spec.slopes(&fft, &h);
        let turb = spec.compression(&fft, &h).into_iter().map(|q| (0.5 + 0.5 * (q - mean) / (TURB_SIGMAS * sigma)).clamp(0.0, 1.0)).collect();
        encode_frame(n, sx, sz, turb)
    }).map_err(|f| format!("ocean detail frame {f} panicked"))?;
    let mips = b.mip_levels();
    let (mut nd, mut td) = (Vec::new(), Vec::new());
    for (a, t) in layers { nd.extend(a); td.extend(t); }
    let normals = TexBlob { kind: TexKind::Array, format: TexFormat::RgtcRg, width: n as u32, height: n as u32, layers: b.frames as u32, mip_levels: mips, data: nd };
    let turbulence = TexBlob { kind: TexKind::Array, format: TexFormat::RgtcR, width: n as u32, height: n as u32, layers: b.frames as u32, mip_levels: mips, data: td };
    for blob in [&normals, &turbulence] {
        if blob.data.len() != blob.expected_len() { return Err(format!("ocean detail: {} bytes, the header implies {}", blob.data.len(), blob.expected_len())); }
    }
    Ok((normals, turbulence))
}

/// Unit: `xt-gen-fft`.
pub fn ocean_detail(root: &Path) -> Result<Vec<GenFile>, String> {
    let (normals, turbulence) = bake(&Bake::shipped())?;
    Ok(vec![
        GenFile { path: gf::OCEAN_DETAIL_NORMALS.to_string(), bytes: normals.encode() },
        GenFile { path: gf::OCEAN_DETAIL_TURBULENCE.to_string(), bytes: turbulence.encode() },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> Bake { Bake { size: 32, frames: 8, loop_s: 24.0, patch_m: 41.0, wind_ms: WIND_MS, seed: SEED } }

    /// Decode a BC4 block per the format: index 0 = r0, 1 = r1, 2..7 interpolate (8-value mode when r0 > r1).
    fn bc4_decode(b: &[u8]) -> [f64; 16] {
        let (r0, r1) = (b[0] as f64, b[1] as f64);
        let mut bits = [0u8; 8];
        bits[..6].copy_from_slice(&b[2..8]);
        let bits = u64::from_le_bytes(bits);
        let mut out = [0.0; 16];
        for t in 0..16 {
            let i = ((bits >> (3 * t)) & 7) as usize;
            out[t] = match i {
                0 => r0,
                1 => r1,
                i if b[0] > b[1] => ((8 - i) as f64 * r0 + (i - 1) as f64 * r1) / 7.0,
                i if i < 6 => ((6 - i) as f64 * r0 + (i - 1) as f64 * r1) / 5.0,
                6 => 0.0,
                _ => 255.0,
            };
        }
        out
    }

    #[test]
    fn fft_matches_a_naive_dft() {
        let n = 8;
        let input: Vec<C> = (0..n * n).map(|i| C::new(uniform(1, i as u64, 0) - 0.5, uniform(2, i as u64, 0) - 0.5)).collect();
        let mut a = input.clone();
        Fft::new(n).inverse_2d(&mut a);
        for z in 0..n {
            for x in 0..n {
                let mut s = C::ZERO;
                for kz in 0..n {
                    for kx in 0..n {
                        let th = 2.0 * std::f64::consts::PI * ((kx * x + kz * z) as f64) / n as f64;
                        s = s.add(input[kz * n + kx].mul(C::cis(th)));
                    }
                }
                let d = a[z * n + x].sub(s);
                assert!(d.re.abs() < 1e-12 && d.im.abs() < 1e-12);
            }
        }
    }

    #[test]
    fn fields_are_real() {
        let s = Spectrum::new(&small());
        let fft = Fft::new(32);
        for f in [0, 3] {
            let mut h = s.h_at_frame(f);
            fft.inverse_2d(&mut h);
            let re = h.iter().map(|c| c.re.abs()).fold(0.0, f64::max);
            let im = h.iter().map(|c| c.im.abs()).fold(0.0, f64::max);
            assert!(re > 0.0 && im < 1e-12 * re.max(1.0), "imag residue {im} vs {re}");
        }
    }

    #[test]
    fn loops_exactly_every_24_seconds() {
        let b = small();
        let s = Spectrum::new(&b);
        let (a, z) = (s.h_at_time(0.0), s.h_at_time(24.0));
        let peak = a.iter().map(|c| c.re.abs().max(c.im.abs())).fold(0.0, f64::max);
        for (x, y) in a.iter().zip(&z) { assert!((x.re - y.re).abs() <= 1e-9 * peak && (x.im - y.im).abs() <= 1e-9 * peak); }
        // The frame table is the continuous spectrum at t = 24 f / F, and frame F is frame 0 bit for bit.
        for f in 0..=b.frames {
            let t = s.h_at_time(24.0 * f as f64 / b.frames as f64);
            for (x, y) in s.h_at_frame(f).iter().zip(&t) { assert!((x.re - y.re).abs() <= 1e-9 * peak && (x.im - y.im).abs() <= 1e-9 * peak); }
        }
        assert_eq!(s.h_at_frame(b.frames), s.h_at_frame(0));
        // Dispersion is quantised to whole multiples of 2 pi / 24 s, at least one, within half a step.
        for i in 0..s.m.len() {
            if s.h0[i] == C::ZERO { continue; }
            assert!(s.m[i] >= 1);
            let k = libm::sqrt(s.kx[i] * s.kx[i] + s.kz[i] * s.kz[i]);
            let w = libm::sqrt(GRAVITY * k);
            if w > s.omega0 { assert!((s.m[i] as f64 * s.omega0 - w).abs() <= 0.5 * s.omega0 + 1e-12); }
        }
    }

    #[test]
    fn long_waves_are_left_to_gerstner() {
        let b = Bake::shipped();
        let dk = 2.0 * std::f64::consts::PI / b.patch_m;
        // A 41 m wave carries a tiny fraction of the power of a 4 m wave along the wind.
        let long = phillips(&b, dk, 0.0);
        let short = phillips(&b, 10.0 * dk, 0.0);
        assert!(long * dk * dk < 0.1 * short * (10.0 * dk) * (10.0 * dk), "{long} vs {short}");
    }

    #[test]
    fn slopes_hit_the_target_variance() {
        let s = Spectrum::new(&small());
        let (sx, sz) = s.slopes(&Fft::new(32), &s.h_at_frame(0));
        let var = sx.iter().zip(&sz).map(|(x, z)| x * x + z * z).sum::<f64>() / (32.0 * 32.0);
        assert!((var - SLOPE_VARIANCE).abs() < 1e-12);
        // Wind along +x: slopes across x dominate slopes across z.
        let (mx, mz) = (sx.iter().map(|v| v * v).sum::<f64>(), sz.iter().map(|v| v * v).sum::<f64>());
        assert!(mx > mz);
    }

    #[test]
    fn turbulence_spread_matches_the_water_shader() {
        // water.gdshader's foam thresholds assume mid-grey turbulence with a standard deviation near 0.17.
        let b = Bake { size: 64, ..small() };
        let s = Spectrum::new(&b);
        let fft = Fft::new(64);
        let q = s.compression(&fft, &s.h_at_frame(0));
        let mean = q.iter().sum::<f64>() / q.len() as f64;
        let sigma = libm::sqrt(q.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / q.len() as f64);
        let t: Vec<f64> = q.iter().map(|v| (0.5 + 0.5 * (v - mean) / (TURB_SIGMAS * sigma)).clamp(0.0, 1.0)).collect();
        let tm = t.iter().sum::<f64>() / t.len() as f64;
        let ts = libm::sqrt(t.iter().map(|v| (v - tm) * (v - tm)).sum::<f64>() / t.len() as f64);
        assert!((tm - 0.5).abs() < 0.01 && (0.15..=0.18).contains(&ts), "mean {tm} std {ts}");
    }

    #[test]
    fn crests_read_as_turbulence() {
        // One wave along x: compression peaks at the crest (h max) and is negative in the trough.
        let n = 16;
        let mut s = Spectrum::new(&Bake { size: n, ..small() });
        for h in &mut s.h0 { *h = C::ZERO; }
        s.h0[1] = C::new(0.05, 0.0);
        let fft = Fft::new(n);
        let h = s.h_at_frame(0);
        let mut height = h.clone();
        fft.inverse_2d(&mut height);
        let q = s.compression(&fft, &h);
        let crest = (0..n).max_by(|&a, &b| height[a].re.total_cmp(&height[b].re)).unwrap();
        let trough = (0..n).min_by(|&a, &b| height[a].re.total_cmp(&height[b].re)).unwrap();
        assert!(q[crest] > 0.0 && q[trough] < 0.0, "crest {} trough {}", q[crest], q[trough]);
    }

    #[test]
    fn bc4_round_trips() {
        let mut ramp = [0.0; 16];
        for t in 0..16 { ramp[t] = 40.0 + 10.0 * t as f64; }
        let d = bc4_decode(&bc4_block(&ramp));
        for t in 0..16 { assert!((d[t] - ramp[t]).abs() <= 150.0 / 14.0 + 0.5, "texel {t}: {} vs {}", d[t], ramp[t]); }
        // Texel order: the brightest value is the last texel, the darkest the first.
        assert_eq!(d[0], 40.0);
        assert_eq!(d[15], 190.0);
        // Flat blocks decode exactly.
        let flat = bc4_decode(&bc4_block(&[77.0; 16]));
        assert!(flat.iter().all(|&v| v == 77.0));
        // Two-level block: exact.
        let mut two = [10.0; 16];
        two[5] = 250.0;
        assert_eq!(bc4_decode(&bc4_block(&two))[5], 250.0);
        assert_eq!(bc4_decode(&bc4_block(&two))[4], 10.0);
        // Garbage in, valid block out.
        let mut bad = [0.0; 16];
        bad[0] = f64::NAN;
        bad[1] = 1e9;
        let _ = bc4_decode(&bc4_block(&bad));
    }

    #[test]
    fn small_bake_layout_and_determinism() {
        let b = small();
        let (normals, turb) = bake(&b).unwrap();
        for blob in [&normals, &turb] {
            let d = TexBlob::decode(&blob.encode()).unwrap();
            assert_eq!((d.kind, d.width, d.height, d.layers, d.mip_levels), (TexKind::Array, 32, 32, 8, 6));
        }
        assert_eq!(normals.format, TexFormat::RgtcRg);
        assert_eq!(turb.format, TexFormat::RgtcR);
        // 32^2: 64 + 16 + 4 + 1 + 1 + 1 blocks per layer.
        assert_eq!(turb.layer_bytes(), 8 * 87);
        assert_eq!(normals.layer_bytes(), 16 * 87);
        let (n2, t2) = bake(&b).unwrap();
        assert!(normals.data == n2.data && turb.data == t2.data, "two bakes differ");
        // Turbulence averages near mid-grey: decode the 1x1 mips.
        let mips_r: f64 = (0..8).map(|l| turb.data[l * turb.layer_bytes() + turb.layer_bytes() - 8] as f64).sum::<f64>() / 8.0;
        assert!((mips_r - 127.5).abs() < 20.0, "mean turbulence {mips_r}");
        // Normals average to nearly flat: the 1x1 mip's red endpoint is near 128.
        let red = normals.data[normals.layer_bytes() - 16] as f64;
        assert!((red - 127.5).abs() < 6.0, "mean normal x {red}");
    }

    #[test]
    fn shipped_bake_fits_the_budget() {
        let b = Bake::shipped();
        assert_eq!((b.size, b.frames, b.mip_levels()), (256, 64, 9));
        let size = |f: TexFormat| (0..9).map(|l| f.level_bytes(256 >> l, 256 >> l)).sum::<usize>() * 64;
        let total = size(TexFormat::RgtcRg) + size(TexFormat::RgtcR);
        assert!((7_500_000..9_000_000).contains(&total), "{total} bytes; L1903 says about 8 MB");
    }

    #[test]
    fn bad_parameters_are_errors() {
        assert!(bake(&Bake { size: 30, ..small() }).is_err());
        assert!(bake(&Bake { frames: 0, ..small() }).is_err());
        assert!(bake(&Bake { loop_s: f64::NAN, ..small() }).is_err());
    }
}
