//! Unit `hm-surface`. Packs meshes into Godot surface bytes and builds the welded shadow mesh.
//! DESIGN.md L1927 (format 34359742467; N x 12 B f32 positions then N x 4 B octahedral normals as 2 x u16
//! UNORM; u16 indices up to 65,536 vertices, else u32; clockwise front faces; no UV, tangent, colour or
//! compression at LOD0), L1929 (welded position-only shadow mesh; never `mesh_clear` a visible chunk).
//!
//! The octahedral encoding must be exactly Godot 4.7's `_encode_octahedral` path for uncompressed
//! `ARRAY_NORMAL` (the implementer reads Godot's `servers/rendering_server.cpp` for the mapping and rounding and
//! pins it with byte-layout tests).
//!
//! Checked against Godot 4.7.2-stable's `servers/rendering/rendering_server.cpp` (`_surface_set_data`,
//! `mesh_surface_make_offsets_from_format`) and `core/math/vector3.cpp`:
//! - `ARRAY_NORMAL` (uncompressed): `Vector3::octahedron_encode()` maps to 0..1, then each component is
//!   `(uint16_t)CLAMP(x * 65535, 0, 65535)`: a float multiply, then truncation (no rounding).
//! - The normal stream starts after all positions (`offset = 12 * vertex_count`), 4 B per vertex, x then y.
//! - `ARRAY_COLOR`: `uint8_t(CLAMP(c * 255.0, 0.0, 255.0))` per channel in R, G, B, A order, computed in double,
//!   truncated. The attribute stream holds only colour here, so its stride is 4.
//! - Index buffers are u16 when `0 < vertex_count <= 65536`, else u32.
use crate::types::{format, Aabb, MeshData, SurfaceBytes, MAX_U16_VERTICES};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Multiplicative hasher (FxHash style) for the per-chunk weld maps: deterministic and several times faster
/// than SipHash on the small integer keys used here. Map iteration order is never relied on.
#[derive(Default, Clone, Copy)]
pub(crate) struct FastHasher(u64);
impl Hasher for FastHasher {
    fn finish(&self) -> u64 { self.0 }
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.write_u64(u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut b = [0u8; 8];
            b[..rest.len()].copy_from_slice(rest);
            self.write_u64(u64::from_le_bytes(b));
        }
    }
    fn write_u32(&mut self, v: u32) { self.write_u64(v as u64) }
    fn write_u64(&mut self, v: u64) { self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95); }
    fn write_usize(&mut self, v: usize) { self.write_u64(v as u64) }
}
pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// Octahedral-encode a unit normal to two UNORM16 values in Godot's convention. Unit: `hm-surface`.
///
/// Zero-length or non-finite input encodes as +Z. Godot itself would produce NaN there.
pub fn encode_octahedral(n: [f32; 3]) -> [u16; 2] {
    let l1 = n[0].abs() + n[1].abs() + n[2].abs();
    if !(l1 > 0.0 && l1.is_finite()) {
        return [32767, 32767];
    }
    // Godot: `n /= abs(x) + abs(y) + abs(z)` (component-wise division).
    let (x, y, z) = (n[0] / l1, n[1] / l1, n[2] / l1);
    let (mut ox, mut oy);
    if z >= 0.0 {
        ox = x;
        oy = y;
    } else {
        ox = (1.0 - y.abs()) * if x >= 0.0 { 1.0 } else { -1.0 };
        oy = (1.0 - x.abs()) * if y >= 0.0 { 1.0 } else { -1.0 };
    }
    ox = ox * 0.5 + 0.5;
    oy = oy * 0.5 + 0.5;
    let q = |v: f32| (v * 65535.0).clamp(0.0, 65535.0) as u16;
    [q(ox), q(oy)]
}

/// Inverse of [`encode_octahedral`], for tests and tools. Unit: `hm-surface`.
pub fn decode_octahedral(v: [u16; 2]) -> [f32; 3] {
    let fx = (v[0] as f32 / 65535.0) * 2.0 - 1.0;
    let fy = (v[1] as f32 / 65535.0) * 2.0 - 1.0;
    let mut n = [fx, fy, 1.0 - fx.abs() - fy.abs()];
    let t = (-n[2]).clamp(0.0, 1.0);
    n[0] += if n[0] >= 0.0 { -t } else { t };
    n[1] += if n[1] >= 0.0 { -t } else { t };
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 0.0 { [n[0] / len, n[1] / len, n[2] / len] } else { [0.0, 0.0, 1.0] }
}

fn push_positions(out: &mut Vec<u8>, positions: &[[f32; 3]]) {
    for p in positions {
        for c in p {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
}

fn push_normals(out: &mut Vec<u8>, mesh: &MeshData) {
    for i in 0..mesh.positions.len() {
        let e = encode_octahedral(mesh.normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]));
        out.extend_from_slice(&e[0].to_le_bytes());
        out.extend_from_slice(&e[1].to_le_bytes());
    }
}

/// Triangles whose indices all point inside the vertex array. An index past the end would crash the GPU
/// upload, so a triangle that holds one is dropped, along with a trailing partial triangle.
fn valid_triangles(indices: &[u32], vertex_count: usize) -> impl Iterator<Item = [u32; 3]> + '_ {
    indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).filter(move |t| t.iter().all(|&i| (i as usize) < vertex_count))
}

fn pack_indices(tris: impl Iterator<Item = [u32; 3]>, vertex_count: usize) -> (Vec<u8>, u32) {
    let wide = vertex_count > MAX_U16_VERTICES;
    let mut out = Vec::new();
    let mut count = 0u32;
    for t in tris {
        for i in t {
            if wide { out.extend_from_slice(&i.to_le_bytes()); } else { out.extend_from_slice(&(i as u16).to_le_bytes()); }
            count += 1;
        }
    }
    (out, count)
}

/// Bounds of the positions; an empty mesh gets the zero box (Godot rejects an inverted AABB).
fn surface_aabb(positions: &[[f32; 3]]) -> Aabb {
    if positions.is_empty() {
        return Aabb::default();
    }
    let mut b = Aabb::EMPTY;
    for p in positions {
        b.include(*p);
    }
    b
}

fn encode_with_format(mesh: &MeshData, fmt: u64) -> SurfaceBytes {
    let n = mesh.positions.len();
    let mut vertex_data = Vec::with_capacity(n * 16);
    push_positions(&mut vertex_data, &mesh.positions);
    push_normals(&mut vertex_data, mesh);
    let mut attribute_data = Vec::new();
    if fmt & format::COLOR != 0 {
        attribute_data.reserve(n * 4);
        for i in 0..n {
            for ch in mesh.colors.get(i).copied().unwrap_or([1.0; 4]) {
                // NaN clamps to NaN and casts to 0.
                attribute_data.push(((ch as f64) * 255.0).clamp(0.0, 255.0) as u8);
            }
        }
    }
    let (index_data, index_count) = pack_indices(valid_triangles(&mesh.indices, n), n);
    SurfaceBytes {
        format: fmt,
        primitive: format::PRIMITIVE_TRIANGLES,
        vertex_data,
        attribute_data,
        vertex_count: n as u32,
        index_data,
        index_count,
        aabb: surface_aabb(&mesh.positions),
    }
}

/// Pack a LOD0 chunk mesh (positions, normals, indices) as `format::CHUNK_SURFACE` bytes with its AABB.
/// Ignores colours and custom channels. Unit: `hm-surface`.
pub fn encode_chunk_surface(mesh: &MeshData) -> SurfaceBytes { encode_with_format(mesh, format::CHUNK_SURFACE) }

/// Pack a coloured mesh (hull LOD1-4, part pieces) as `format::COLORED_SURFACE`: positions and octahedral
/// normals as for chunks, colours in `attribute_data`. Unit: `hm-surface`.
///
/// Vertices without a colour pack as opaque white.
pub fn encode_colored_surface(mesh: &MeshData) -> SurfaceBytes { encode_with_format(mesh, format::COLORED_SURFACE) }

/// Bit key of a position for exact-equality welding; -0.0 and +0.0 weld together.
fn weld_key(p: [f32; 3]) -> [u32; 3] {
    let k = |v: f32| if v == 0.0 { 0u32 } else { v.to_bits() };
    [k(p[0]), k(p[1]), k(p[2])]
}

/// Weld positions by exact equality in first-occurrence order. Returns the unique positions (with -0.0
/// written as +0.0) and the welded id of every input vertex.
pub(crate) fn weld(positions: &[[f32; 3]]) -> (Vec<[f32; 3]>, Vec<u32>) {
    let mut map: FastMap<[u32; 3], u32> = FastMap::with_capacity_and_hasher(positions.len(), Default::default());
    let mut unique: Vec<[f32; 3]> = Vec::new();
    let mut remap = Vec::with_capacity(positions.len());
    for p in positions {
        let id = *map.entry(weld_key(*p)).or_insert_with(|| {
            unique.push([p[0] + 0.0, p[1] + 0.0, p[2] + 0.0]);
            (unique.len() - 1) as u32
        });
        remap.push(id);
    }
    (unique, remap)
}

/// Weld positions (exact f32 equality, first occurrence order) and pack a position-only
/// `format::SHADOW_SURFACE` with the same triangles. Deterministic. Unit: `hm-surface`.
pub fn encode_shadow_surface(mesh: &MeshData) -> SurfaceBytes {
    let (unique, remap) = weld(&mesh.positions);
    let n = unique.len();
    let mut vertex_data = Vec::with_capacity(n * 12);
    push_positions(&mut vertex_data, &unique);
    let tris = valid_triangles(&mesh.indices, mesh.positions.len())
        .map(|t| [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]]);
    let (index_data, index_count) = pack_indices(tris, n);
    SurfaceBytes {
        format: format::SHADOW_SURFACE,
        primitive: format::PRIMITIVE_TRIANGLES,
        vertex_data,
        attribute_data: Vec::new(),
        vertex_count: n as u32,
        index_data,
        index_count,
        aabb: surface_aabb(&unique),
    }
}

/// True when every edge of the welded mesh is shared by exactly two triangles with opposite winding (the
/// watertightness test helper, L1925). Unit: `hm-surface`.
///
/// Voxel solids can touch along a single edge (two cubes meeting diagonally), where four triangles share
/// the edge, two each way, and the surface is still closed. So the check generalises "exactly two" to: for
/// every welded edge, as many triangles cross it a->b as cross it b->a. A triangle that welds to a repeated
/// vertex or indexes past the vertex array fails the mesh. One chunk's mesh is open where the hull continues
/// into the next chunk, so callers check the concatenated chunks of a whole vessel.
pub fn is_watertight(mesh: &MeshData) -> bool {
    if mesh.indices.len() % 3 != 0 {
        return false;
    }
    let (_, remap) = weld(&mesh.positions);
    let mut edges: FastMap<(u32, u32), i32> = FastMap::with_capacity_and_hasher(mesh.indices.len(), Default::default());
    for t in mesh.indices.chunks_exact(3) {
        if t.iter().any(|&i| i as usize >= remap.len()) {
            return false;
        }
        let v = [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]];
        if v[0] == v[1] || v[1] == v[2] || v[0] == v[2] {
            return false;
        }
        for k in 0..3 {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            // Signed count per undirected edge: +1 for low->high, -1 for high->low.
            if a < b { *edges.entry((a, b)).or_insert(0) += 1; } else { *edges.entry((b, a)).or_insert(0) -= 1; }
        }
    }
    edges.values().all(|&c| c == 0)
}

#[cfg(test)]
mod tests;
