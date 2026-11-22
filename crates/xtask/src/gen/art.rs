//! Unit `xt-gen-art`. LUTs, VFX ramps and noise textures generated from the world tokens.
//! DESIGN.md L1788 (LUTs, ramps and noise come from the tokens via Rust build steps, never a grading tool or a
//! model), L1846-1848 (AgX plus one 17^3 LUT per preset, 33^3 on High; presets vary exposure, light, fog and LUT;
//! no teal-orange grades, L1824), L1953 (256x1 ramps posterised to 4 to 6 bands; peak in the first 25% of life),
//! L1905 (cloud shadows: one noise fetch), L1940 (rust bleed: 1:8 vertical noise), L1868 (presets: 6 weathers x
//! dusk, day and night).
//!
//! Inputs come from `crate::art::tokens` (the Rust token table is the source of truth; the committed JSON is its
//! export). Outputs are `hull_mesh::gen_files::TexBlob` files at the paths `gen_files` names:
//! - `lut_path(weather, time, high)` for every `WeatherKind` x `TimeOfDay`, both edges: Volume RGB8, identity
//!   plus the preset's grade (lift, gamma, gain, saturation within the token chroma limits);
//! - `ramp_path(RampId::name())` for every `hull_mesh::vfx::RampId`: Texture2D RGBA8 256x1;
//! - `NOISE_CLOUD_SHADOW` R8 512^2 and `NOISE_WEATHERING` R8 256x32 with mips, tileable value noise from
//!   fixed seeds.
//!
//! The token table is validated before anything is generated; an invalid table is an error, not a texture.
//!
//! # LUT layout
//! Godot applies `adjustment_color_correction` after tonemapping, in display-encoded space, and samples it as
//! `textureLod(source_color_correction, clamp(colour, 0, 1), 0.0)` with linear filtering and no half-texel
//! scale (checked in the 4.7.2 binary's tonemap shaders, RD and GLES3). Colour c therefore lands at texel
//! coordinate `c * N - 0.5`, and texel i is read exactly at colour `(i + 0.5) / N`. So texel (r, g, b) of an
//! N^3 LUT holds `grade((r + 0.5) / N, (g + 0.5) / N, (b + 0.5) / N)`: between the outer texel centres the
//! sampled result is the trilinear interpolation of exact grades, the same on the 17^3 and 33^3 LUTs. The
//! outer half texel (colours under 0.5 / N or over 1 - 0.5 / N) reads the end texel, a slight lift of pure
//! black and pull of pure white; the ambient floor keeps lit scenes out of that band. The common
//! `i / (N - 1)` layout would add about 6% contrast at 17^3 (3% at 33^3), more than the grades themselves.
//! r runs fastest, then g, and b is the depth slice (one `TexBlob` layer per b), RGB8, no mipmaps.
//!
//! # Ramp texel encoding
//! The 256 texels are posterised: texel i falls in band `b = i * bands / 256`, and every texel of band b holds
//! the ramp at `t = b / (bands - 1)`, so band 0 is the ramp's start (its peak) and the last band its end. Sample
//! with nearest filtering at `age / lifetime`.
//! - RGB: the stop colour, linear in the tokens, written sRGB-encoded (sample with `source_color`, which
//!   decodes to linear).
//! - A: one 0..1 envelope. On a ramp that emits (flash, fire, hit puff, debris, flak) it is emission divided
//!   by the ramp's peak emission, so the shader multiplies it by the kind's ladder emission (`vfx::emission`,
//!   or `custom.w` where the instance carries it). On a ramp that does not emit (foam, smoke, defence smoke,
//!   trail) it is opacity, used by the blended kinds (missile trail, muzzle haze) and ignored by opaque ones.
//!
//! # Noise
//! Value noise on an integer lattice that wraps at the texture edge, quintic-interpolated, summed over octaves
//! (fBm) and stretched to the full 0..255 range. Seeds are fixed. Cloud shadow: 512^2, 4x4 base cells, 6
//! octaves, isotropic. Weathering: 256 x 32 with lattice cells 8 times taller than wide (4 x 32, 2 x 16, 1 x 8
//! texels), so the field is made of vertical streaks in texel space; map u and v with the same world scale
//! per texel (for example 256 texels across 16 m, 32 across 2 m) and the streaks stay 1:8 in the world.
//! The weathering texture carries a full box-filtered mip chain (9 levels, 256 x 32 down to 1 x 1, the level
//! sizes Godot expects), because the hull shader samples it with `filter_linear_mipmap`; the cloud shadow is
//! read with `textureLod(.., 0.0)` and has one level.
use super::GenFile;
use crate::art::tokens::{self, grade, ramp_eval, ramp_strength, srgb_encode, to_u8};
use crate::art::{Ramp, WorldTokens};
use hull_mesh::gen_files::{self as gf, TexBlob, TexFormat, TexKind};
use hull_mesh::vfx::RampId;
use std::path::Path;

/// Texels across one ramp (L1953).
pub(crate) const RAMP_TEXELS: u32 = 256;
const CLOUD_SHADOW_SIZE: u32 = 512;
const WEATHERING_SIZE: (u32, u32) = (256, 32);
const CLOUD_SHADOW_SEED: u64 = 0xB1AC_C10D_5AAD_0001;
const WEATHERING_SEED: u64 = 0xB1AC_2057_B1EE_0002;

fn checked_tokens() -> Result<WorldTokens, String> {
    let t = tokens::world_tokens();
    tokens::validate(&t).map_err(|e| format!("world tokens fail validation:\n{e}"))?;
    Ok(t)
}

fn file(path: String, blob: &TexBlob) -> Result<GenFile, String> {
    if blob.data.len() != blob.expected_len() { return Err(format!("{path}: {} bytes, the header implies {}", blob.data.len(), blob.expected_len())); }
    Ok(GenFile { path, bytes: blob.encode() })
}

/// The display colour Godot's unscaled `textureLod(lut, colour)` reads at the centre of texel `i` of `n`.
pub(crate) fn lut_texel_centre(i: u32, n: u32) -> f64 { (i as f64 + 0.5) / n.max(1) as f64 }

/// One preset's LUT of edge `n` (see "LUT layout").
pub(crate) fn lut_blob(p: &crate::art::Preset, n: u32) -> TexBlob {
    let n = n.max(2);
    let mut data = Vec::with_capacity((n * n * n * 3) as usize);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let out = grade(p, [lut_texel_centre(r, n), lut_texel_centre(g, n), lut_texel_centre(b, n)]);
                data.extend(out.map(to_u8));
            }
        }
    }
    TexBlob { kind: TexKind::Volume, format: TexFormat::Rgb8, width: n, height: n, layers: n, mip_levels: 1, data }
}

/// Unit: `xt-gen-art`.
pub fn luts(root: &Path) -> Result<Vec<GenFile>, String> {
    let t = checked_tokens()?;
    let mut out = Vec::new();
    for time in tokens::TIMES {
        for w in tokens::WEATHERS {
            let p = t.presets.iter().find(|p| p.weather == w && p.time == time)
                .ok_or_else(|| format!("no preset for {}/{}", tokens::time_name(time), tokens::weather_name(w)))?;
            for high in [false, true] {
                let edge = if high { gf::LUT_EDGE_HIGH } else { gf::LUT_EDGE };
                out.push(file(gf::lut_path(w, time, high), &lut_blob(p, edge))?);
            }
        }
    }
    Ok(out)
}

/// RGBA8 texels of one ramp (see "Ramp texel encoding").
pub(crate) fn ramp_texels(r: &Ramp) -> Vec<u8> {
    let bands = r.bands.clamp(2, 255) as u32;
    let mut data = Vec::with_capacity(RAMP_TEXELS as usize * 4);
    for i in 0..RAMP_TEXELS {
        let b = (i * bands / RAMP_TEXELS).min(bands - 1);
        let (rgba, e) = ramp_eval(r, b as f64 / (bands - 1) as f64);
        data.extend([to_u8(srgb_encode(rgba[0])), to_u8(srgb_encode(rgba[1])), to_u8(srgb_encode(rgba[2])), to_u8(ramp_strength(r, rgba, e))]);
    }
    data
}

/// Unit: `xt-gen-art`.
pub fn ramps(root: &Path) -> Result<Vec<GenFile>, String> {
    let t = checked_tokens()?;
    let mut out = Vec::new();
    for id in RampId::ALL {
        let r = t.ramps.iter().find(|r| r.id == id).ok_or_else(|| format!("no ramp for {}", id.name()))?;
        let blob = TexBlob { kind: TexKind::Texture2D, format: TexFormat::Rgba8, width: RAMP_TEXELS, height: 1, layers: 1, mip_levels: 1, data: ramp_texels(r) };
        out.push(file(gf::ramp_path(id.name()), &blob)?);
    }
    Ok(out)
}

/// SplitMix64 finaliser: a well-mixed 64-bit hash of `x`.
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Lattice value 0..1 for (seed, octave, cell).
fn lattice(seed: u64, octave: u32, ix: u32, iy: u32) -> f64 {
    let h = mix64(seed ^ mix64(((octave as u64) << 48) ^ ((ix as u64) << 24) ^ iy as u64));
    (h >> 11) as f64 / (1u64 << 53) as f64
}

fn quintic(t: f64) -> f64 { t * t * t * (t * (t * 6.0 - 15.0) + 10.0) }

/// Tileable fBm value noise over a `w` x `h` texture, stretched to 0..255. `octaves` lists (cells across,
/// cells down) per octave; each octave's lattice wraps at the texture edge, so the result tiles. Amplitude
/// halves per octave.
pub(crate) fn value_noise(w: u32, h: u32, octaves: &[(u32, u32)], seed: u64) -> Vec<u8> {
    value_noise_field(w, h, octaves, seed).into_iter().map(to_u8).collect()
}

/// Halve a tileable `w` x `h` field by box averages (2 x 2, or 2 x 1 once a side is down to one texel).
fn halve(v: &[f64], w: u32, h: u32) -> (Vec<f64>, u32, u32) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let (sx, sy) = (if w > 1 { 2 } else { 1 }, if h > 1 { 2 } else { 1 });
    let mut out = Vec::with_capacity((nw * nh) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let mut s = 0.0;
            for dy in 0..sy {
                for dx in 0..sx { s += v[((y * sy + dy) * w + x * sx + dx) as usize]; }
            }
            out.push(s / (sx * sy) as f64);
        }
    }
    (out, nw, nh)
}

/// A 0..1 field quantised to R8 with its full mip chain down to 1 x 1 (Godot's order and level sizes; sides
/// must be powers of two), and the number of levels.
fn r8_with_mips(field: Vec<f64>, w: u32, h: u32) -> (Vec<u8>, u32) {
    let (mut v, mut w, mut h) = (field, w.max(1), h.max(1));
    let mut data: Vec<u8> = Vec::with_capacity(v.len() * 4 / 3 + 16);
    let mut levels = 1;
    loop {
        data.extend(v.iter().map(|&x| to_u8(x)));
        if w == 1 && h == 1 { break; }
        (v, w, h) = halve(&v, w, h);
        levels += 1;
    }
    (data, levels)
}

/// The noise field behind [`value_noise`], normalised to 0..1.
fn value_noise_field(w: u32, h: u32, octaves: &[(u32, u32)], seed: u64) -> Vec<f64> {
    let (w, h) = (w.max(1), h.max(1));
    let mut field = vec![0.0f64; (w * h) as usize];
    let mut amp = 1.0;
    for (o, &(cx, cy)) in octaves.iter().enumerate() {
        let (cx, cy) = (cx.max(1), cy.max(1));
        for y in 0..h {
            let fy = (y as f64 + 0.5) * cy as f64 / h as f64;
            let iy = fy.floor() as u32;
            let ty = quintic(fy - iy as f64);
            let (y0, y1) = (iy % cy, (iy + 1) % cy);
            for x in 0..w {
                let fx = (x as f64 + 0.5) * cx as f64 / w as f64;
                let ix = fx.floor() as u32;
                let tx = quintic(fx - ix as f64);
                let (x0, x1) = (ix % cx, (ix + 1) % cx);
                let l = |a, b| lattice(seed, o as u32, a, b);
                let top = l(x0, y0) + (l(x1, y0) - l(x0, y0)) * tx;
                let bot = l(x0, y1) + (l(x1, y1) - l(x0, y1)) * tx;
                field[(y * w + x) as usize] += amp * (top + (bot - top) * ty);
            }
        }
        amp *= 0.5;
    }
    let (lo, hi) = field.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
    let span = if hi - lo > 1e-12 { hi - lo } else { 1.0 };
    field.iter().map(|&v| (v - lo) / span).collect()
}

/// Unit: `xt-gen-art`.
pub fn noise(root: &Path) -> Result<Vec<GenFile>, String> {
    let s = CLOUD_SHADOW_SIZE;
    let cloud_octaves: Vec<(u32, u32)> = (0..6).map(|o| (4 << o, 4 << o)).collect();
    let cloud = TexBlob { kind: TexKind::Texture2D, format: TexFormat::R8, width: s, height: s, layers: 1, mip_levels: 1, data: value_noise(s, s, &cloud_octaves, CLOUD_SHADOW_SEED) };
    let (ww, wh) = WEATHERING_SIZE;
    // Cells 1:8 (wide:tall): 4 x 32, 2 x 16 and 1 x 8 texels.
    let streak_octaves = [(ww / 4, wh / 32), (ww / 2, wh / 16), (ww, wh / 8)];
    // The hull shader samples it with mipmaps at up to sensor range; the box-filtered chain keeps distant
    // streaks from shimmering.
    let (data, mip_levels) = r8_with_mips(value_noise_field(ww, wh, &streak_octaves, WEATHERING_SEED), ww, wh);
    let weathering = TexBlob { kind: TexKind::Texture2D, format: TexFormat::R8, width: ww, height: wh, layers: 1, mip_levels, data };
    Ok(vec![file(gf::NOISE_CLOUD_SHADOW.to_string(), &cloud)?, file(gf::NOISE_WEATHERING.to_string(), &weathering)?])
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::enums::TimeOfDay;
    use content::weather::WeatherKind;

    #[test]
    fn luts_cover_every_preset_at_both_edges() {
        let files = luts(Path::new(".")).unwrap();
        assert_eq!(files.len(), 36);
        let mut paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), 36);
        assert!(files.iter().any(|f| f.path == "art/gen/lut_dusk_clear_17.bin"));
        for f in &files {
            let b = TexBlob::decode(&f.bytes).unwrap();
            assert_eq!((b.kind, b.format, b.mip_levels), (TexKind::Volume, TexFormat::Rgb8, 1));
            assert!(b.width == 17 || b.width == 33);
            assert_eq!((b.height, b.layers), (b.width, b.width));
        }
    }

    #[test]
    fn identity_grade_gives_an_identity_lut_in_the_documented_order() {
        let mut p = tokens::world_tokens().presets[0].clone();
        p.lift = [0.0; 3];
        p.gamma = [1.0; 3];
        p.gain = [1.0; 3];
        p.saturation = 1.0;
        let b = lut_blob(&p, 17);
        let at = |r: usize, g: usize, bl: usize| { let i = 3 * ((bl * 17 + g) * 17 + r); [b.data[i], b.data[i + 1], b.data[i + 2]] };
        // Texel i holds the colour Godot reads at its centre, (i + 0.5) / 17.
        let c = |i: u32| to_u8(lut_texel_centre(i, 17));
        assert_eq!((c(0), c(8), c(16)), (8, 128, 248));
        assert_eq!(at(0, 0, 0), [c(0); 3]);
        assert_eq!(at(16, 16, 16), [c(16); 3]);
        assert_eq!(at(16, 0, 0), [c(16), c(0), c(0)]);
        assert_eq!(at(0, 16, 0), [c(0), c(16), c(0)]);
        assert_eq!(at(0, 0, 16), [c(0), c(0), c(16)]);
        assert_eq!(at(8, 4, 12), [c(8), c(4), c(12)]);
    }

    /// Godot's read of a LUT: `textureLod(lut, colour, 0.0)` with linear filtering, clamp to edge and no
    /// half-texel scale, so colour c sits at texel coordinate `c * N - 0.5`.
    fn godot_sample(b: &TexBlob, c: [f64; 3]) -> [f64; 3] {
        let n = b.width as usize;
        let axis = |v: f64| {
            let u = (v.clamp(0.0, 1.0) * n as f64 - 0.5).clamp(0.0, (n - 1) as f64);
            let i0 = (u.floor() as usize).min(n - 1);
            (i0, (i0 + 1).min(n - 1), u - i0 as f64)
        };
        let (ax, ay, az) = (axis(c[0]), axis(c[1]), axis(c[2]));
        let mut out = [0.0; 3];
        for (zi, wz) in [(az.0, 1.0 - az.2), (az.1, az.2)] {
            for (yi, wy) in [(ay.0, 1.0 - ay.2), (ay.1, ay.2)] {
                for (xi, wx) in [(ax.0, 1.0 - ax.2), (ax.1, ax.2)] {
                    let i = 3 * ((zi * n + yi) * n + xi);
                    for ch in 0..3 { out[ch] += wx * wy * wz * b.data[i + ch] as f64 / 255.0; }
                }
            }
        }
        out
    }

    #[test]
    fn godot_reads_back_the_grade_at_both_edges() {
        let t = tokens::world_tokens();
        let (lo, hi) = (0.5 / 17.0, 1.0 - 0.5 / 17.0);
        let steps = 9;
        for p in &t.presets {
            let (small, large) = (lut_blob(p, 17), lut_blob(p, 33));
            let mut worst: f64 = 0.0;
            for i in 0..steps * steps * steps {
                let f = |k: usize| lo + (hi - lo) * k as f64 / (steps - 1) as f64;
                let c = [f(i % steps), f(i / steps % steps), f(i / (steps * steps))];
                let want = grade(p, c);
                let (a, b) = (godot_sample(&small, c), godot_sample(&large, c));
                for ch in 0..3 {
                    worst = worst.max((a[ch] - want[ch]).abs()).max((b[ch] - want[ch]).abs()).max((a[ch] - b[ch]).abs());
                }
            }
            assert!(worst * 255.0 <= 2.0, "{}/{}: sampled LUT strays {:.2} code values from the grade", tokens::time_name(p.time), tokens::weather_name(p.weather), worst * 255.0);
        }
        // The i / (N - 1) layout would not pass: Godot would read a contrast-stretched grade.
        let p = &t.presets[0];
        let mut naive = lut_blob(p, 17);
        naive.data.clear();
        for b in 0..17u32 { for g in 0..17u32 { for r in 0..17u32 { naive.data.extend(grade(p, [r as f64 / 16.0, g as f64 / 16.0, b as f64 / 16.0]).map(to_u8)); } } }
        let c = [0.15; 3];
        assert!((godot_sample(&naive, c)[1] - grade(p, c)[1]).abs() * 255.0 > 2.0);
    }

    #[test]
    fn dusk_grade_is_gentle_and_warm() {
        let t = tokens::world_tokens();
        let p = t.presets.iter().find(|p| p.time == TimeOfDay::Dusk && p.weather == WeatherKind::Clear).unwrap();
        let b = lut_blob(p, 17);
        // Mid grey stays near mid grey, nudged warm; black is lifted only a little (no crushed shadows).
        let i = 3 * ((8 * 17 + 8) * 17 + 8);
        let mid = [b.data[i], b.data[i + 1], b.data[i + 2]];
        assert!(mid[0] >= mid[2] && (mid[1] as i32 - 128).abs() <= 3, "{mid:?}");
        // The darkest texel (read at colour 0.5 / 17, code 7.5) is lifted by a few codes at most.
        assert!(b.data[0] >= 8 && b.data[0] <= 13, "{}", b.data[0]);
    }

    #[test]
    fn ramps_are_posterised_with_an_early_peak() {
        let t = tokens::world_tokens();
        let files = ramps(Path::new(".")).unwrap();
        assert_eq!(files.len(), RampId::ALL.len());
        for (id, f) in RampId::ALL.iter().zip(&files) {
            assert_eq!(f.path, gf::ramp_path(id.name()));
            let b = TexBlob::decode(&f.bytes).unwrap();
            assert_eq!((b.kind, b.format, b.width, b.height), (TexKind::Texture2D, TexFormat::Rgba8, 256, 1));
            let r = t.ramps.iter().find(|r| r.id == *id).unwrap();
            let texels: Vec<[u8; 4]> = b.data.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
            let mut distinct = texels.clone();
            distinct.dedup();
            assert!(distinct.len() <= r.bands as usize && distinct.len() >= 2, "{}: {} runs", id.name(), distinct.len());
            // The envelope's first maximum sits in the first 25% of the texels.
            let best = texels.iter().map(|t| t[3]).max().unwrap();
            let first = texels.iter().position(|t| t[3] == best).unwrap();
            assert!(first < 64, "{}: peak at texel {first}", id.name());
            // Texel 0 is the ramp start.
            let (rgba, _) = ramp_eval(r, 0.0);
            assert_eq!(texels[0][0], to_u8(srgb_encode(rgba[0])));
        }
        // An emitting ramp's alpha is its emission envelope; the flash starts at full and ends at half.
        let flash = b_of(&files[0]);
        assert_eq!(flash[3], 255);
        assert_eq!(flash[255 * 4 + 3], 128);
        // A non-emitting ramp's alpha is opacity: the trail thins.
        let trail = b_of(files.iter().find(|f| f.path.ends_with("ramp_trail.bin")).unwrap());
        assert!(trail[3] > trail[255 * 4 + 3]);
    }

    fn b_of(f: &GenFile) -> Vec<u8> { TexBlob::decode(&f.bytes).unwrap().data }

    #[test]
    fn noise_tiles_and_uses_the_range() {
        let files = noise(Path::new(".")).unwrap();
        assert_eq!(files.len(), 2);
        for (f, (w, h)) in files.iter().zip([(512u32, 512u32), (256, 32)]) {
            let b = TexBlob::decode(&f.bytes).unwrap();
            assert_eq!((b.format, b.width, b.height), (TexFormat::R8, w, h));
            assert_eq!(*b.data.iter().min().unwrap(), 0);
            assert_eq!(*b.data.iter().max().unwrap(), 255);
            // Wrapping: the step across the seam is no larger than the largest step inside the texture.
            let px = |x: u32, y: u32| b.data[(y * w + x) as usize] as i32;
            let inner_x = (0..h).flat_map(|y| (1..w).map(move |x| (x, y))).map(|(x, y)| (px(x, y) - px(x - 1, y)).abs()).max().unwrap();
            let seam_x = (0..h).map(|y| (px(0, y) - px(w - 1, y)).abs()).max().unwrap();
            assert!(seam_x <= inner_x + 2, "{}: seam {seam_x} vs inner {inner_x}", f.path);
            let inner_y = (1..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| (px(x, y) - px(x, y - 1)).abs()).max().unwrap();
            let seam_y = (0..w).map(|x| (px(x, 0) - px(x, h - 1)).abs()).max().unwrap();
            assert!(seam_y <= inner_y + 2, "{}: seam {seam_y} vs inner {inner_y}", f.path);
        }
        assert_eq!(files, noise(Path::new(".")).unwrap());
    }

    #[test]
    fn weathering_streaks_run_vertically() {
        let (w, h) = (256u32, 32u32);
        let d = value_noise(w, h, &[(64, 1), (128, 2), (256, 4)], WEATHERING_SEED);
        let px = |x: u32, y: u32| d[(y * w + x) as usize] as f64;
        let mut dx = 0.0;
        let mut dy = 0.0;
        for y in 0..h { for x in 0..w { dx += (px((x + 1) % w, y) - px(x, y)).abs(); dy += (px(x, (y + 1) % h) - px(x, y)).abs(); } }
        assert!(dx > 3.0 * dy, "horizontal variation {dx} should dwarf vertical {dy}");
    }

    #[test]
    fn weathering_carries_a_full_box_filtered_mip_chain() {
        let files = noise(Path::new(".")).unwrap();
        let cloud = TexBlob::decode(&files[0].bytes).unwrap();
        assert_eq!(cloud.mip_levels, 1);
        let b = TexBlob::decode(&files[1].bytes).unwrap();
        // 256 x 32 to 1 x 1: 256, 128, .., 1 across; 32, 16, .., 1, 1, 1, 1 down.
        assert_eq!(b.mip_levels, 9);
        assert_eq!(b.data.len(), [256 * 32, 128 * 16, 64 * 8, 32 * 4, 16 * 2, 8, 4, 2, 1].iter().sum::<usize>());
        // Each level is the box average of the one above, within rounding of the quantised parents.
        let (mut off, mut w, mut h) = (0usize, 256usize, 32usize);
        while w * h > 1 {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let (sx, sy) = (if w > 1 { 2 } else { 1 }, if h > 1 { 2 } else { 1 });
            let next = off + w * h;
            for y in 0..nh {
                for x in 0..nw {
                    let mut s = 0.0;
                    for dy in 0..sy { for dx in 0..sx { s += b.data[off + (y * sy + dy) * w + x * sx + dx] as f64; } }
                    let got = b.data[next + y * nw + x] as f64;
                    assert!((got - s / (sx * sy) as f64).abs() <= 1.0, "level {w}x{h} -> {nw}x{nh} at {x},{y}");
                }
            }
            (off, w, h) = (next, nw, nh);
        }
        // The 1 x 1 level is the mean, near mid-grey for stretched fBm.
        let mean = b.data[b.data.len() - 1] as f64;
        assert!((60.0..=195.0).contains(&mean), "{mean}");
    }
}
