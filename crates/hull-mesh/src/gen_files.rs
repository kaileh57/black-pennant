//! Contract-owned (no work unit). Names and container format of the art files `cargo xtask gen` writes and
//! gdclient reads, so writer and reader share one definition. DESIGN.md L1076-1078 (generated files live under
//! `res://shaders/gen/`, `res://art/gen/`, `res://audio/gen/`, are gitignored, deterministic), L1788 (LUTs,
//! ramps and noise come from the tokens via Rust build steps), L1891 (wave include), L1903 (FFT detail),
//! L1846 (LUT per preset), L1953 (256x1 ramps).
//!
//! Paths here are relative to the Godot project root `game/` (prefix `res://` for Godot). Audio file names live
//! in the audio crate (`audio::res_path`).
//!
//! DECISION: textures are written as raw [`TexBlob`] files (`.bin`), not PNG plus guessed import settings.
//! gdclient reads them with `FileAccess.get_file_as_bytes`, builds one `Image` per layer with
//! `Image.create_from_data(width, height, mipmaps > 1, format, layer_bytes)` and wraps them in `ImageTexture`,
//! `Texture2DArray` or `ImageTexture3D`. The FFT detail is Rust-baked straight to BC5 (normals,
//! `FORMAT_RGTC_RG`) and BC4 (turbulence, `FORMAT_RGTC_R`) blocks with full mip chains (L1903), because
//! Godot's BC compressors are not guaranteed in export templates. Export presets must include `art/gen/*.bin`
//! as non-resource files.
use content::enums::TimeOfDay;
use content::weather::WeatherKind;

/// The wave shader include written from `sim_core::waves::emit_wave_shader_include` (L1891).
pub const WAVES_INCLUDE: &str = "shaders/gen/waves.gdshaderinc";
/// Baked Tessendorf detail: 64 frames of 256^2, looping every 24 s (L1903).
/// Normals (BC5): R = 0.5 + 0.5 n.x, G = 0.5 + 0.5 n.z of the unit normal in patch axes; n.y = sqrt(1 - x^2 -
/// z^2). Turbulence (BC4): 0.5 on average, toward 1 where the detail's Jacobian compresses (crests). See
/// `xtask/src/gen/fft.rs`.
pub const OCEAN_DETAIL_NORMALS: &str = "art/gen/ocean_detail_normals.bin";
pub const OCEAN_DETAIL_TURBULENCE: &str = "art/gen/ocean_detail_turbulence.bin";
pub const OCEAN_DETAIL_FRAMES: u32 = 64;
pub const OCEAN_DETAIL_SIZE: u32 = 256;
pub const OCEAN_DETAIL_LOOP_S: f64 = 24.0;
/// World sample scales of the two detail tilings in metres (L1903).
pub const OCEAN_DETAIL_SCALES_M: [f64; 2] = [41.0, 11.3];
/// Tileable value noise for cloud shadows (one fetch in water and hull shaders, L1905), R8 512^2.
pub const NOISE_CLOUD_SHADOW: &str = "art/gen/noise_cloud_shadow.bin";
/// 1:8 vertical noise for rust bleed (L1940), R8 256 x 32 tileable, with a full mip chain (9 levels). The
/// streaks are 1:8 in texel space: map u and v at the same world size per texel.
pub const NOISE_WEATHERING: &str = "art/gen/noise_weathering.bin";
/// LUT edge: 17^3 per preset, 33^3 on High (L1846). Texel i of an edge holds the grade at colour
/// (i + 0.5) / edge, because Godot samples `adjustment_color_correction` with no half-texel scale. See
/// `xtask/src/gen/art.rs`.
pub const LUT_EDGE: u32 = 17;
pub const LUT_EDGE_HIGH: u32 = 33;

/// Path of the colour-grading LUT of one weather x time-of-day preset (L1846, L1859-1868).
/// Example: `art/gen/lut_dusk_clear_17.bin`.
pub fn lut_path(weather: WeatherKind, time: TimeOfDay, high: bool) -> String {
    let w = match weather { WeatherKind::Clear => "clear", WeatherKind::Haze => "haze", WeatherKind::Overcast => "overcast", WeatherKind::Rain => "rain", WeatherKind::Storm => "storm", WeatherKind::FogBank => "fog" };
    let t = match time { TimeOfDay::Night => "night", TimeOfDay::Dusk => "dusk", TimeOfDay::Day => "day" };
    format!("art/gen/lut_{t}_{w}_{}.bin", if high { LUT_EDGE_HIGH } else { LUT_EDGE })
}

/// Path of a VFX colour ramp, 256x1 RGBA8, posterised to 4..=6 bands (L1953). `name` is
/// `crate::vfx::RampId::name`. Texels: RGB sRGB-encoded colour (sample with `source_color`, nearest filter);
/// A one 0..1 envelope, emission / the ramp's peak emission on emitting ramps (flash, fire, hit puff, debris,
/// flak), opacity on the others (foam, smoke, defence smoke, trail). See `xtask/src/gen/art.rs`.
pub fn ramp_path(name: &str) -> String { format!("art/gen/ramp_{name}.bin") }

/// Pixel format of a [`TexBlob`]. Discriminants are Godot 4.7 `Image.Format` values (checked against
/// gdextension-api 4.7 `extension_api.json`), so the reader passes them straight to `Image.create_from_data`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TexFormat { R8 = 2, Rg8 = 3, Rgb8 = 4, Rgba8 = 5, /** BC4, 8 bytes per 4x4 block. */ RgtcR = 20, /** BC5, 16 bytes per 4x4 block. */ RgtcRg = 21 }
impl TexFormat {
    /// Bytes of one mip level of `w` x `h` texels (block formats round up to whole 4x4 blocks).
    pub fn level_bytes(self, w: u32, h: u32) -> usize {
        let (w, h) = (w.max(1) as usize, h.max(1) as usize);
        match self {
            TexFormat::R8 => w * h, TexFormat::Rg8 => 2 * w * h, TexFormat::Rgb8 => 3 * w * h, TexFormat::Rgba8 => 4 * w * h,
            TexFormat::RgtcR => 8 * ((w + 3) / 4) * ((h + 3) / 4),
            TexFormat::RgtcRg => 16 * ((w + 3) / 4) * ((h + 3) / 4),
        }
    }
    pub fn from_u32(v: u32) -> Option<Self> { match v { 2 => Some(Self::R8), 3 => Some(Self::Rg8), 4 => Some(Self::Rgb8), 5 => Some(Self::Rgba8), 20 => Some(Self::RgtcR), 21 => Some(Self::RgtcRg), _ => None } }
}

/// What the layers of a [`TexBlob`] mean to the reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TexKind { Texture2D = 0, Array = 1, Volume = 2 }

/// Raw texture container. Byte layout (little-endian): magic `BPTX`, u32 version (1), u32 kind, u32 format,
/// u32 width, u32 height, u32 layers, u32 mip levels (1 = no mipmaps; else the full chain down to 1x1, Godot's
/// order), then for each layer its mip levels in order, each `format.level_bytes(w >> l, h >> l)` bytes, texels x
/// fastest then y. A `Volume` stores its depth slices as layers (no mipmaps).
#[derive(Clone, Debug, PartialEq)]
pub struct TexBlob { pub kind: TexKind, pub format: TexFormat, pub width: u32, pub height: u32, pub layers: u32, pub mip_levels: u32, pub data: Vec<u8> }
impl TexBlob {
    pub const MAGIC: [u8; 4] = *b"BPTX";
    pub const VERSION: u32 = 1;
    pub const HEADER_BYTES: usize = 32;
    /// Bytes of one layer including its mip chain.
    pub fn layer_bytes(&self) -> usize { (0..self.mip_levels.max(1)).map(|l| self.format.level_bytes(self.width >> l, self.height >> l)).sum() }
    pub fn expected_len(&self) -> usize { self.layer_bytes() * self.layers as usize }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::HEADER_BYTES + self.data.len());
        out.extend_from_slice(&Self::MAGIC);
        for v in [Self::VERSION, self.kind as u32, self.format as u32, self.width, self.height, self.layers, self.mip_levels] { out.extend_from_slice(&v.to_le_bytes()); }
        out.extend_from_slice(&self.data);
        out
    }
    pub fn decode(bytes: &[u8]) -> Option<TexBlob> {
        if bytes.len() < Self::HEADER_BYTES || bytes[0..4] != Self::MAGIC { return None; }
        let u = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        if u(4) != Self::VERSION { return None; }
        let kind = match u(8) { 0 => TexKind::Texture2D, 1 => TexKind::Array, 2 => TexKind::Volume, _ => return None };
        let blob = TexBlob { kind, format: TexFormat::from_u32(u(12))?, width: u(16), height: u(20), layers: u(24), mip_levels: u(28), data: bytes[Self::HEADER_BYTES..].to_vec() };
        if blob.mip_levels == 0 || blob.data.len() != blob.expected_len() { return None; }
        Some(blob)
    }
}
