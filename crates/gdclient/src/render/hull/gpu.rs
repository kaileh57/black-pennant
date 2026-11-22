//! Part of unit `gd-render-hull`, shared with `gd-render-parts` and `gd-render-projectiles`. The thin layer
//! between the Godot-free cores of those units and `RenderingServer`: mesh and texture creation from
//! `hull-mesh` output, one growable MultiMesh with its instance, and transform conversion.
//!
//! Everything here is main-thread only. [`InstanceBuffer`] is plain Rust, so buffer writing tests headless;
//! everything else calls the engine and is exercised only inside Godot.
// Client rendering, not simulation (clippy.toml): std floats are allowed here. Nothing drawn from them feeds
// back into the sim.
#![allow(clippy::disallowed_methods)]
use crate::render::INSTANCE_FLOATS;
use crate::seams::{MeshData, SurfaceBytes};
use glam::{Affine3A, Vec3};
use godot::classes::rendering_server::{ArrayFormat, MultimeshTransformFormat, PrimitiveType, ShadowCastingSetting};
use godot::classes::{RenderingServer, Shader};
use godot::obj::EngineBitfield;
use godot::prelude::*;

pub(crate) fn rs() -> Gd<RenderingServer> { RenderingServer::singleton() }

/// Free a RID if it is one, and forget it.
pub(crate) fn free_rid(rs: &mut Gd<RenderingServer>, rid: &mut Rid) {
    if rid.is_valid() { rs.free_rid(*rid); }
    *rid = Rid::Invalid;
}

pub(crate) fn v3(v: Vec3) -> Vector3 { Vector3::new(v.x, v.y, v.z) }

/// A glam affine as the engine's transform (columns are the basis vectors).
pub(crate) fn transform(m: &Affine3A) -> Transform3D {
    Transform3D::from_cols(v3(m.matrix3.x_axis.into()), v3(m.matrix3.y_axis.into()), v3(m.matrix3.z_axis.into()), v3(m.translation.into()))
}

pub(crate) fn aabb(min: Vec3, max: Vec3) -> Aabb {
    if !(min.is_finite() && max.is_finite()) || min.cmpgt(max).any() { return Aabb::new(Vector3::ZERO, Vector3::ZERO); }
    Aabb::new(v3(min), v3(max - min))
}

/// A mesh RID with one surface from generator arrays, or `Rid::Invalid` for an empty or malformed mesh
/// (`convert::mesh_arrays` refuses those).
pub(crate) fn mesh_from_data(rs: &mut Gd<RenderingServer>, mesh: &MeshData, material: Rid) -> Rid {
    if mesh.is_empty() { return Rid::Invalid; }
    let (arrays, flags) = crate::convert::mesh_arrays(mesh);
    if arrays.at(godot::classes::rendering_server::ArrayType::VERTEX.ord() as usize).is_nil() { return Rid::Invalid; }
    let rid = rs.mesh_create();
    match ArrayFormat::try_from_ord(flags).filter(|_| flags != 0) {
        Some(format) => rs.mesh_add_surface_from_arrays_ex(rid, PrimitiveType::TRIANGLES, &arrays).compress_format(format).done(),
        None => rs.mesh_add_surface_from_arrays(rid, PrimitiveType::TRIANGLES, &arrays),
    }
    if material.is_valid() { rs.mesh_surface_set_material(rid, 0, material); }
    rid
}

/// A mesh RID from packed surface bytes (`mesh_create_from_surfaces`), or `Rid::Invalid` when it has no
/// triangle.
pub(crate) fn mesh_from_surface(rs: &mut Gd<RenderingServer>, surface: &SurfaceBytes) -> Rid {
    if surface.is_empty() || surface.vertex_count == 0 { return Rid::Invalid; }
    let mut surfaces: Array<AnyDictionary> = Array::new();
    surfaces.push(crate::convert::surface_dict(surface).upcast_any_dictionary());
    rs.mesh_create_from_surfaces(&surfaces)
}

/// A shader resource by path. `None` (logged once by the caller) when the file is missing or does not
/// compile; the unit then draws with the engine's default material rather than not at all.
pub(crate) fn load_shader(path: &str) -> Option<Gd<Shader>> {
    if !godot::classes::ResourceLoader::singleton().exists(path) { return None; }
    godot::tools::try_load::<Shader>(path).ok()
}

/// A `ShaderMaterial` RID on `shader`; `Rid::Invalid` without one.
pub(crate) fn material_on(rs: &mut Gd<RenderingServer>, shader: &Option<Gd<Shader>>) -> Rid {
    let Some(shader) = shader else { return Rid::Invalid };
    let material = rs.material_create();
    rs.material_set_shader(material, shader.get_rid());
    material
}

/// A 2D texture RID from one generated `.bin` file (`seams::gen` path), or `Rid::Invalid` when
/// `cargo xtask gen` has not written it.
pub(crate) fn texture_from_gen(rs: &mut Gd<RenderingServer>, relative: &str) -> Rid {
    let Some(blob) = crate::convert::load_tex_blob(relative) else { return Rid::Invalid };
    match crate::convert::tex_blob_images(&blob).into_iter().next() {
        Some(image) => rs.texture_2d_create(&image),
        None => Rid::Invalid,
    }
}

/// The CPU side of one MultiMesh: floats in the `hull_mesh::vfx::VfxInstance` layout (3 x 4 transform by
/// rows, colour, custom) and the box around what was written, which becomes the MultiMesh's custom AABB so
/// the engine never recomputes one from the buffer.
#[derive(Clone, Debug)]
pub(crate) struct InstanceBuffer { pub data: Vec<f32>, pub min: Vec3, pub max: Vec3 }
impl Default for InstanceBuffer { fn default() -> Self { Self { data: Vec::new(), min: Vec3::splat(f32::MAX), max: Vec3::splat(f32::MIN) } } }
impl InstanceBuffer {
    /// Start a frame: no instances, empty box. Keeps the allocation.
    pub fn begin(&mut self) { self.data.clear(); self.min = Vec3::splat(f32::MAX); self.max = Vec3::splat(f32::MIN); }
    pub fn count(&self) -> usize { self.data.len() / INSTANCE_FLOATS }
    /// Append one instance. `radius` bounds the mesh about the transform's origin after the transform (the
    /// caller's rest-pose radius times its largest scale).
    #[inline]
    pub fn push(&mut self, m: &Affine3A, color: [f32; 4], custom: [f32; 4], radius: f32) {
        let (x, y, z, o) = (m.matrix3.x_axis, m.matrix3.y_axis, m.matrix3.z_axis, m.translation);
        self.data.extend_from_slice(&[x.x, y.x, z.x, o.x, x.y, y.y, z.y, o.y, x.z, y.z, z.z, o.z, color[0], color[1], color[2], color[3], custom[0], custom[1], custom[2], custom[3]]);
        let o = Vec3::from(o);
        self.min = self.min.min(o - radius);
        self.max = self.max.max(o + radius);
    }
}

/// One MultiMesh and the instance that draws it. Capacity doubles; unused slots are cut off with
/// `multimesh_set_visible_instances`, and the instance is hidden while nothing is written.
pub(crate) struct MultiMeshSlot {
    pub multimesh: Rid,
    pub instance: Rid,
    capacity: usize,
    packed: PackedFloat32Array,
    shown: bool,
}
impl MultiMeshSlot {
    /// `material` overrides the mesh's surface material when valid.
    pub fn new(rs: &mut Gd<RenderingServer>, scenario: Rid, mesh: Rid, layer: u32, shadows: bool, material: Rid) -> Self {
        let multimesh = rs.multimesh_create();
        rs.multimesh_set_mesh(multimesh, mesh);
        let instance = rs.instance_create2(multimesh, scenario);
        rs.instance_set_layer_mask(instance, layer);
        rs.instance_geometry_set_cast_shadows_setting(instance, if shadows { ShadowCastingSetting::ON } else { ShadowCastingSetting::OFF });
        if material.is_valid() { rs.instance_geometry_set_material_override(instance, material); }
        rs.instance_set_visible(instance, false);
        Self { multimesh, instance, capacity: 0, packed: PackedFloat32Array::new(), shown: false }
    }
    pub fn capacity(&self) -> usize { self.capacity }
    /// Reallocate to exactly `instances` (a ring pool's fixed cap). Contents are undefined until the next
    /// upload.
    pub fn allocate(&mut self, rs: &mut Gd<RenderingServer>, instances: usize) {
        self.capacity = instances;
        rs.multimesh_allocate_data_ex(self.multimesh, instances as i32, MultimeshTransformFormat::TRANSFORM_3D).color_format(true).custom_data_format(true).done();
        self.packed.resize(instances * INSTANCE_FLOATS);
    }
    /// One `multimesh_set_buffer` with the first `floats.len() / 20` instances drawn.
    pub fn upload(&mut self, rs: &mut Gd<RenderingServer>, floats: &[f32], min: Vec3, max: Vec3) {
        let count = floats.len() / INSTANCE_FLOATS;
        if count == 0 {
            if self.shown { rs.instance_set_visible(self.instance, false); self.shown = false; }
            return;
        }
        if count > self.capacity { self.allocate(rs, count.next_power_of_two().max(16)); }
        self.packed.as_mut_slice()[..count * INSTANCE_FLOATS].copy_from_slice(&floats[..count * INSTANCE_FLOATS]);
        rs.multimesh_set_buffer(self.multimesh, &self.packed);
        rs.multimesh_set_visible_instances(self.multimesh, count as i32);
        rs.multimesh_set_custom_aabb(self.multimesh, aabb(min, max));
        if !self.shown { rs.instance_set_visible(self.instance, true); self.shown = true; }
    }
    pub fn upload_buffer(&mut self, rs: &mut Gd<RenderingServer>, buffer: &InstanceBuffer) { self.upload(rs, &buffer.data, buffer.min, buffer.max); }
    pub fn free(&mut self, rs: &mut Gd<RenderingServer>) {
        free_rid(rs, &mut self.instance);
        free_rid(rs, &mut self.multimesh);
        self.capacity = 0;
        self.shown = false;
    }
}

/// Pixels one metre covers at one metre from the camera: `viewport height / (2 tan(fov / 2))`. 0 for a
/// camera that is not set up, which callers read as "size unknown, do not cull".
pub(crate) fn px_per_m_at_1m(camera: &crate::interp::CameraState) -> f32 {
    let fov = camera.fov_y_deg.to_radians();
    let h = camera.viewport_px[1];
    if !camera.valid || !(fov > 0.01 && fov < 3.1) || !(h > 0.0) || !h.is_finite() { return 0.0; }
    h / (2.0 * (0.5 * fov).tan())
}

/// The camera in render space.
pub(crate) fn camera_render_pos(ctx: &crate::render::FrameContext) -> Vec3 {
    let c = ctx.camera.pos;
    if !ctx.camera.valid || !c.iter().all(|v| v.is_finite()) { return Vec3::ZERO; }
    Vec3::from(ctx.view.origin.to_render(glam::DVec3::new(c[0], c[1], c[2])))
}

/// A vessel pose as an affine; a degenerate quaternion gives the identity rotation.
pub(crate) fn pose_affine(pos: [f32; 3], rot: [f32; 4]) -> Affine3A {
    let q = glam::Quat::from_xyzw(rot[0], rot[1], rot[2], rot[3]);
    let q = if q.is_finite() && q.length_squared() > 1e-6 { q.normalize() } else { glam::Quat::IDENTITY };
    let p = Vec3::from(pos);
    Affine3A::from_rotation_translation(q, if p.is_finite() { p } else { Vec3::ZERO })
}

// ---------------------------------------------------------------------------------------------------------
// The hull unit's engine side
// ---------------------------------------------------------------------------------------------------------

/// Parameter names of `res://shaders/hull.gdshader` this unit sets. All colours are linear RGB.
mod hull_param {
    pub const LOD_VERTEX_COLOUR: &str = "lod_vertex_colour";
    pub const HULL_VOLUME: &str = "hull_volume";
    pub const CHUNK_ORIGIN: &str = "chunk_origin";
    pub const PALETTE: &str = "palette";
    pub const PRIMER_ALBEDO: &str = "primer_albedo";
    pub const WEATHERING_NOISE: &str = "weathering_noise";
    pub const WEATHERING_LEVEL: &str = "weathering_level";
    pub const WATERLINE_Y: &str = "waterline_y";
    pub const DECK_Y: &str = "deck_y";
    pub const CHUNK_HEAT: &str = "chunk_heat";
    pub const FIRE_RAMP: &str = "fire_ramp";
    pub const VIEW_MODE: &str = "view_mode";
}
const HULL_SHADER: &str = "res://shaders/hull.gdshader";
const WORLD_TOKENS: &str = "res://art/tokens/world_tokens.json";

/// The world token `hull.primer` (exposed interiors, L1941) in linear RGB, read from the token file; the
/// generators' own primer tone when the file cannot be read.
fn primer_token() -> Vector3 {
    let fallback = hull_mesh::parts::Tone::Primer.rgba();
    let fallback = Vector3::new(fallback[0], fallback[1], fallback[2]);
    if !godot::classes::FileAccess::file_exists(WORLD_TOKENS) { return fallback; }
    let text = godot::classes::FileAccess::get_file_as_string(WORLD_TOKENS);
    let Ok(root) = godot::classes::Json::parse_string(&text).try_to::<VarDictionary>() else { return fallback };
    let Some(rgb) = root.get("primer").and_then(|p| p.try_to::<VarDictionary>().ok()).and_then(|p| p.get("linear_rgb")).and_then(|v| v.try_to::<VarArray>().ok()) else { return fallback };
    let channel = |i: usize| rgb.get(i).and_then(|v| crate::convert::variant_f64(&v)).filter(|c| (0.0..=1.0).contains(c)).map(|c| c as f32);
    match (channel(0), channel(1), channel(2)) { (Some(r), Some(g), Some(b)) => Vector3::new(r, g, b), _ => fallback }
}

/// `core::Device` on `RenderingServer`.
pub(crate) struct GodotDevice {
    rs: Gd<RenderingServer>,
    scenario: Rid,
    shader: Option<Gd<Shader>>,
    lod_material: Rid,
    weathering: Rid,
    fire_ramp: Rid,
    primer: Vector3,
    mask_texture: Rid,
}

impl GodotDevice {
    pub fn new(scenario: Rid) -> Self {
        let mut rs = rs();
        let shader = load_shader(HULL_SHADER);
        if shader.is_none() { godot_warn!("hull renderer: {HULL_SHADER} is missing; drawing with the default material"); }
        let lod_material = material_on(&mut rs, &shader);
        if lod_material.is_valid() { rs.material_set_param(lod_material, hull_param::LOD_VERTEX_COLOUR, &true.to_variant()); }
        let weathering = texture_from_gen(&mut rs, crate::seams::gen::NOISE_WEATHERING);
        let fire_ramp = texture_from_gen(&mut rs, &crate::seams::gen::ramp_path(hull_mesh::vfx::RampId::Fire.name()));
        if !weathering.is_valid() { godot_warn!("hull renderer: the generated weathering noise is missing (run cargo xtask gen); hulls draw unweathered"); }
        Self { rs, scenario, shader, lod_material, weathering, fire_ramp, primer: primer_token(), mask_texture: Rid::Invalid }
    }
    /// Free the unit-wide RIDs (the per-vessel ones go through `core::HullCore::free_all`).
    pub fn free(&mut self) {
        for mut rid in [self.lod_material, self.weathering, self.fire_ramp, self.mask_texture] { free_rid(&mut self.rs, &mut rid); }
        self.lod_material = Rid::Invalid;
        self.weathering = Rid::Invalid;
        self.fire_ramp = Rid::Invalid;
        self.mask_texture = Rid::Invalid;
    }
    fn mask_image(texels: &[[u16; 4]]) -> Option<Gd<godot::classes::Image>> {
        use hull_mesh::mask::{MASK_H, MASK_W};
        if texels.len() != MASK_W * MASK_H { return None; }
        let mut bytes = Vec::with_capacity(texels.len() * 8);
        for t in texels { for c in t { bytes.extend_from_slice(&c.to_le_bytes()); } }
        godot::classes::Image::create_from_data(MASK_W as i32, MASK_H as i32, false, godot::classes::image::Format::RGBAH, &PackedByteArray::from(bytes.as_slice()))
    }
}

impl super::core::Device for GodotDevice {
    fn mesh(&mut self, surface: &SurfaceBytes) -> Rid { mesh_from_surface(&mut self.rs, surface) }
    fn shadow_mesh(&mut self, mesh: Rid, shadow: &SurfaceBytes) -> Rid {
        let rid = mesh_from_surface(&mut self.rs, shadow);
        if rid.is_valid() { self.rs.mesh_set_shadow_mesh(mesh, rid); }
        rid
    }
    fn volume(&mut self, existing: Rid, volume: &crate::seams::VolumeTexture) -> Rid {
        let images = crate::convert::volume_images(volume);
        if images.is_empty() { return existing; }
        if existing.is_valid() {
            self.rs.texture_3d_update(existing, &images);
            return existing;
        }
        let edge = crate::seams::VolumeTexture::EDGE as i32;
        self.rs.texture_3d_create(godot::classes::image::Format::RGBA8, edge, edge, edge, false, &images)
    }
    fn chunk_material(&mut self, volume: Rid, origin_m: [f32; 3], look: &super::core::ChunkLook) -> Rid {
        let material = material_on(&mut self.rs, &self.shader);
        if !material.is_valid() { return material; }
        let rs = &mut self.rs;
        rs.material_set_param(material, hull_param::LOD_VERTEX_COLOUR, &false.to_variant());
        rs.material_set_param(material, hull_param::HULL_VOLUME, &volume.to_variant());
        rs.material_set_param(material, hull_param::CHUNK_ORIGIN, &Vector3::new(origin_m[0], origin_m[1], origin_m[2]).to_variant());
        rs.material_set_param(material, hull_param::PRIMER_ALBEDO, &self.primer.to_variant());
        if self.weathering.is_valid() { rs.material_set_param(material, hull_param::WEATHERING_NOISE, &self.weathering.to_variant()); }
        if self.fire_ramp.is_valid() { rs.material_set_param(material, hull_param::FIRE_RAMP, &self.fire_ramp.to_variant()); }
        super::core::Device::set_look(self, material, look);
        material
    }
    fn set_look(&mut self, material: Rid, look: &super::core::ChunkLook) {
        if !material.is_valid() { return; }
        let palette: PackedVector3Array = look.palette.iter().map(|c| Vector3::new(c[0], c[1], c[2])).collect();
        let rs = &mut self.rs;
        rs.material_set_param(material, hull_param::PALETTE, &palette.to_variant());
        rs.material_set_param(material, hull_param::WEATHERING_LEVEL, &(if self.weathering.is_valid() { look.weathering } else { 0.0 }).to_variant());
        rs.material_set_param(material, hull_param::WATERLINE_Y, &look.waterline_m.to_variant());
        // No deck known: far above any hull, the shader's own default.
        rs.material_set_param(material, hull_param::DECK_Y, &look.deck_m.unwrap_or(100_000.0).to_variant());
        rs.material_set_param(material, hull_param::VIEW_MODE, &(look.view_mode as i64).to_variant());
    }
    fn set_heat(&mut self, material: Rid, heat: f32) { self.rs.material_set_param(material, hull_param::CHUNK_HEAT, &heat.to_variant()); }
    fn lod_material(&mut self) -> Rid { self.lod_material }
    fn instance(&mut self, mesh: Rid, material: Rid, xf: &Affine3A, visible: bool, shadows: bool) -> Rid {
        let rs = &mut self.rs;
        let instance = rs.instance_create2(mesh, self.scenario);
        rs.instance_set_layer_mask(instance, crate::render::LAYER_HULL);
        if material.is_valid() { rs.instance_geometry_set_material_override(instance, material); }
        rs.instance_set_transform(instance, transform(xf));
        rs.instance_geometry_set_cast_shadows_setting(instance, if shadows { ShadowCastingSetting::ON } else { ShadowCastingSetting::OFF });
        rs.instance_set_visible(instance, visible);
        instance
    }
    fn set_base(&mut self, instance: Rid, mesh: Rid) { self.rs.instance_set_base(instance, mesh); }
    fn set_transform(&mut self, instance: Rid, xf: &Affine3A) { self.rs.instance_set_transform(instance, transform(xf)); }
    fn set_visible(&mut self, instance: Rid, visible: bool) { self.rs.instance_set_visible(instance, visible); }
    fn set_shadows(&mut self, instance: Rid, on: bool) { self.rs.instance_geometry_set_cast_shadows_setting(instance, if on { ShadowCastingSetting::ON } else { ShadowCastingSetting::OFF }); }
    fn free(&mut self, rid: Rid) { if rid.is_valid() { self.rs.free_rid(rid); } }
    fn mask_texture(&mut self) -> Rid {
        if self.mask_texture.is_valid() { return self.mask_texture; }
        use hull_mesh::mask::{f32_to_f16_bits, MASK_H, MASK_LARGE, MASK_W, MAX_MASKED_HULLS};
        // Every layer starts as "no hull anywhere".
        let none = [f32_to_f16_bits(MASK_LARGE), f32_to_f16_bits(-MASK_LARGE), f32_to_f16_bits(MASK_LARGE), 0];
        let Some(blank) = Self::mask_image(&vec![none; MASK_W * MASK_H]) else { return Rid::Invalid };
        let mut layers: Array<Gd<godot::classes::Image>> = Array::new();
        for _ in 0..MAX_MASKED_HULLS { layers.push(&blank); }
        self.mask_texture = self.rs.texture_2d_layered_create(&layers, godot::classes::rendering_server::TextureLayeredType::LAYERED_2D_ARRAY);
        self.mask_texture
    }
    fn mask_layer(&mut self, layer: u32, mask: &hull_mesh::mask::HullMask) {
        let texture = self.mask_texture();
        if !texture.is_valid() { return; }
        if let Some(image) = Self::mask_image(&mask.texels) { self.rs.texture_2d_update(texture, &image, layer as i32); }
    }
    fn warn(&mut self, message: &str) { godot_warn!("{message}"); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_buffer_layout_and_bounds() {
        let mut b = InstanceBuffer::default();
        b.begin();
        let m = Affine3A::from_rotation_translation(glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2), Vec3::new(10.0, 2.0, -3.0));
        b.push(&m, [1.0, 0.5, 0.25, 1.0], [0.3, 0.0, 0.0, 0.0], 2.0);
        assert_eq!(b.count(), 1);
        assert_eq!(b.data.len(), INSTANCE_FLOATS);
        // Row-major 3 x 4: a quarter turn about +Y carries +X to -Z, so basis.x = (0, 0, -1).
        assert!((b.data[0]).abs() < 1e-6 && (b.data[8] + 1.0).abs() < 1e-6);
        assert_eq!((b.data[3], b.data[7], b.data[11]), (10.0, 2.0, -3.0));
        assert_eq!(&b.data[12..20], &[1.0, 0.5, 0.25, 1.0, 0.3, 0.0, 0.0, 0.0]);
        assert_eq!((b.min, b.max), (Vec3::new(8.0, 0.0, -5.0), Vec3::new(12.0, 4.0, -1.0)));
        // The same layout hull-mesh writes.
        let v = hull_mesh::vfx::VfxInstance { xform: b.data[..12].try_into().unwrap(), color: [0.0; 4], custom: [0.0; 4] };
        let mut out = [0.0f32; INSTANCE_FLOATS];
        v.write(&mut out);
        assert_eq!(&out[..12], &b.data[..12]);
        b.begin();
        assert_eq!(b.count(), 0);
    }

    #[test]
    fn pixel_scale_matches_the_spec_reference() {
        // L1786: 1 m at d m covers 935 / d px at 1080p with a 60 degree vertical field of view.
        let cam = crate::interp::CameraState { fov_y_deg: 60.0, viewport_px: [1920.0, 1080.0], valid: true, ..Default::default() };
        assert!((px_per_m_at_1m(&cam) - 935.3).abs() < 0.5);
        assert_eq!(px_per_m_at_1m(&crate::interp::CameraState::default()), 0.0);
        let p = pose_affine([1.0, 2.0, 3.0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(p.matrix3, glam::Mat3A::IDENTITY);
    }
}
