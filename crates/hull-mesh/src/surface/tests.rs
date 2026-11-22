use super::*;
use crate::types::format;

fn cube_mesh() -> MeshData {
    // Unit cube with per-face vertices, clockwise from outside.
    let mut m = MeshData::new();
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        ([1.0, 0.0, 0.0], [[1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0]]),
        ([-1.0, 0.0, 0.0], [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [0.0, 0.0, 1.0]]),
        ([0.0, 1.0, 0.0], [[0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]]),
        ([0.0, -1.0, 0.0], [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 0.0, 0.0]]),
        ([0.0, 0.0, 1.0], [[0.0, 0.0, 1.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 0.0, 1.0]]),
        ([0.0, 0.0, -1.0], [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]]),
    ];
    for (n, q) in faces {
        let b = m.positions.len() as u32;
        for p in q {
            m.positions.push(p);
            m.normals.push(n);
        }
        m.indices.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    m
}

#[test]
fn octahedral_matches_godot_reference_values() {
    // Hand-evaluated through Godot's formula: +Z -> (0.5, 0.5) -> 32767 (truncated 32767.5).
    assert_eq!(encode_octahedral([0.0, 0.0, 1.0]), [32767, 32767]);
    assert_eq!(encode_octahedral([1.0, 0.0, 0.0]), [65535, 32767]);
    assert_eq!(encode_octahedral([-1.0, 0.0, 0.0]), [0, 32767]);
    assert_eq!(encode_octahedral([0.0, 1.0, 0.0]), [32767, 65535]);
    assert_eq!(encode_octahedral([0.0, -1.0, 0.0]), [32767, 0]);
    // -Z folds to the corner (1, 1): x = (1 - 0) * 1, y = (1 - 0) * 1.
    assert_eq!(encode_octahedral([0.0, 0.0, -1.0]), [65535, 65535]);
    // A sloped normal: (1,1,0)/sqrt2 -> L1 normalised (0.5, 0.5, 0) -> 0.75 -> 49151.25 -> 49151.
    let s = std::f32::consts::FRAC_1_SQRT_2;
    assert_eq!(encode_octahedral([s, s, 0.0]), [49151, 49151]);
}

#[test]
fn octahedral_round_trip_is_close() {
    let mut worst = 0.0f32;
    for i in 0..200 {
        let a = i as f32 * 0.137;
        let b = i as f32 * 0.071 - 3.0;
        let n = [libm::cosf(a) * libm::cosf(b), libm::sinf(b), libm::sinf(a) * libm::cosf(b)];
        let d = decode_octahedral(encode_octahedral(n));
        let dot = n[0] * d[0] + n[1] * d[1] + n[2] * d[2];
        worst = worst.max(1.0 - dot);
    }
    assert!(worst < 1e-6, "worst {worst}");
    for n in [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]] {
        let d = decode_octahedral(encode_octahedral(n));
        for k in 0..3 { assert!((d[k] - n[k]).abs() < 1e-4); }
    }
    // Bad input does not panic.
    let _ = encode_octahedral([f32::NAN, 0.0, 0.0]);
    assert_eq!(encode_octahedral([0.0; 3]), [32767, 32767]);
}

#[test]
fn chunk_surface_byte_layout() {
    let m = cube_mesh();
    let s = encode_chunk_surface(&m);
    assert_eq!(s.format, 34_359_742_467);
    assert_eq!(s.primitive, 3);
    assert_eq!(s.vertex_count, 24);
    assert_eq!(s.vertex_data.len(), 24 * 16);
    assert!(s.attribute_data.is_empty());
    // Position 1 at bytes 12..24, little-endian f32.
    let f = |o: usize| f32::from_le_bytes(s.vertex_data[o..o + 4].try_into().unwrap());
    assert_eq!([f(12), f(16), f(20)], m.positions[1]);
    // Normals start after all positions; vertex 0 is +X.
    let u = |o: usize| u16::from_le_bytes(s.vertex_data[o..o + 2].try_into().unwrap());
    assert_eq!([u(24 * 12), u(24 * 12 + 2)], [65535, 32767]);
    // Vertex 8 (third face) is +Y.
    assert_eq!([u(24 * 12 + 32), u(24 * 12 + 34)], [32767, 65535]);
    assert!(!s.index_is_u32());
    assert_eq!(s.index_count, 36);
    assert_eq!(s.index_data.len(), 72);
    assert_eq!(u16::from_le_bytes([s.index_data[4], s.index_data[5]]), 2);
    assert_eq!(s.aabb.min, [0.0; 3]);
    assert_eq!(s.aabb.max, [1.0; 3]);
}

#[test]
fn index_width_switches_above_65536_vertices() {
    let mut m = MeshData::new();
    for i in 0..65_536u32 {
        m.positions.push([i as f32, 0.0, 0.0]);
        m.normals.push([0.0, 1.0, 0.0]);
    }
    m.indices.extend_from_slice(&[0, 1, 65_535]);
    let s = encode_chunk_surface(&m);
    assert!(!s.index_is_u32());
    assert_eq!(s.index_data.len(), 6);
    m.positions.push([0.0, 1.0, 0.0]);
    m.normals.push([0.0, 1.0, 0.0]);
    m.indices.extend_from_slice(&[0, 65_536, 1]);
    let s = encode_chunk_surface(&m);
    assert!(s.index_is_u32());
    assert_eq!(s.index_data.len(), 24);
    assert_eq!(u32::from_le_bytes(s.index_data[16..20].try_into().unwrap()), 65_536);
}

#[test]
fn colored_surface_layout_and_truncation() {
    let mut m = cube_mesh();
    m.colors = vec![[1.0, 0.5, 0.0, 0.999]; 24];
    m.colors[1] = [2.0, -1.0, 0.25, 1.0];
    let s = encode_colored_surface(&m);
    assert_eq!(s.format, format::COLORED_SURFACE);
    assert_eq!(s.vertex_data.len(), 24 * 16);
    assert_eq!(s.attribute_data.len(), 24 * 4);
    // 0.5 * 255 = 127.5 -> 127; 0.999 * 255 = 254.7 -> 254 (Godot truncates).
    assert_eq!(&s.attribute_data[0..4], &[255, 127, 0, 254]);
    assert_eq!(&s.attribute_data[4..8], &[255, 0, 63, 255]);
    // Missing colours default to white.
    m.colors.clear();
    assert_eq!(&encode_colored_surface(&m).attribute_data[0..4], &[255; 4]);
}

#[test]
fn shadow_surface_welds_and_cube_is_watertight() {
    let m = cube_mesh();
    let s = encode_shadow_surface(&m);
    assert_eq!(s.format, format::SHADOW_SURFACE);
    assert_eq!(s.vertex_count, 8);
    assert_eq!(s.vertex_data.len(), 8 * 12);
    assert_eq!(s.index_count, 36);
    assert!(s.attribute_data.is_empty());
    assert!(is_watertight(&m));
    assert_eq!(encode_shadow_surface(&m), s, "deterministic");
}

#[test]
fn watertight_rejects_holes_flips_and_bad_indices() {
    let mut m = cube_mesh();
    m.indices.truncate(30);
    assert!(!is_watertight(&m));
    let mut m = cube_mesh();
    m.indices.swap(0, 1);
    m.indices.swap(3, 4);
    assert!(!is_watertight(&m));
    let mut m = cube_mesh();
    m.indices[0] = 999;
    assert!(!is_watertight(&m));
    let mut m = cube_mesh();
    m.indices[2] = m.indices[1];
    assert!(!is_watertight(&m));
    // Two cubes touching along one edge are still closed.
    let mut m = cube_mesh();
    let n = m.positions.len() as u32;
    let c = cube_mesh();
    m.positions.extend(c.positions.iter().map(|p| [p[0] + 1.0, p[1] + 1.0, p[2]]));
    m.normals.extend(c.normals.iter().copied());
    m.indices.extend(c.indices.iter().map(|i| i + n));
    assert!(is_watertight(&m));
    // Negative zero welds with zero.
    let mut m = cube_mesh();
    m.positions[4] = [-0.0, 0.0, -0.0];
    assert!(is_watertight(&m));
}

#[test]
fn out_of_range_indices_are_dropped_not_uploaded() {
    let mut m = cube_mesh();
    m.indices.extend_from_slice(&[0, 1, 500, 7]);
    let s = encode_chunk_surface(&m);
    assert_eq!(s.index_count, 36);
    let e = encode_chunk_surface(&MeshData::new());
    assert!(e.is_empty());
    assert_eq!(e.aabb, Aabb::default());
}
