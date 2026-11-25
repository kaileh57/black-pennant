// Hull meshes. DESIGN.md L1927-1943. Unit: shader-hull.
// One shader, two materials, picked by the material flag `lod_vertex_colour` (never by a second shader):
// - Volume path (flag off), LOD0 chunks: the mesh has VERTEX and NORMAL only (no COLOR, UV or tangent).
//   Albedo, AO, scorch, armour and wear come from the chunk's mipless 34x34x34 RGBA8 volume, sampled at
//   (vessel-local position - chunk origin) / 0.5 m + 1 texel: R occupancy (AO), G bits 0-3 palette index
//   and bits 4-5 material class (0 paint, 1 glass, 2 emissive, 3 bare primer), B scorch, A bits 0-2
//   armour tier and bits 3-7 wear seed. The palette is 16 linear RGB entries per vessel.
// - Vertex-colour path (flag on), LOD1 to LOD4, one mesh per vessel: COLOR.rgb is swatch times AO in
//   linear, COLOR.a is 1. No volume is bound and no texture is fetched.
// Bevel, seams and weathering (L1931-1941) apply on the volume path only.
//
// Rules this file keeps (L1933): cull_back; no VERTEX writes, discard, alpha, world_vertex_coords or light()
// override, so the welded shadow mesh and Godot's lighting apply unchanged. Fetches per pixel on the volume
// path: the cell texel, the AO sample, the edge sample, the cloud shadow and (weathered paint only) the
// weathering noise. Within 300 m, near the waterline, the shared wave include gives the wet line (L1911).
//
// Material parameters (set per chunk material by gd-render-hull; all colours linear RGB):
//   lod_vertex_colour  bool       on for LOD1 to LOD4 materials, off for LOD0 chunk materials.
//   hull_volume        sampler3D  the chunk's 34^3 RGBA8 volume (Image FORMAT_RGBA8, no mipmaps).
//   chunk_origin       vec3       vessel-local metres of the chunk's minimum corner: coord * 16 m.
//   palette            vec3[16]   the blueprint paint table (linear RGB); unused entries may be zero.
//   primer_albedo      vec3       world token hull.primer (linear RGB): exposed interiors (L1941).
//   weathering_noise   sampler2D  gen NOISE_WEATHERING (R8 256 x 32, tileable, its lattice cells already 8 times
//                                 taller than wide); mapped at the same world scale on both axes, 1/16 m per
//                                 texel, so a tile is 16 m across and 2 m down and the streaks stay 1:8 (L1940).
//   weathering_level   float      0..1: 0 in the dry dock and thumbnails (L1940), 1 for a ship at sea.
//   waterline_y        float      vessel-local metres of the design waterline (the salt band sits above).
//   deck_y             float      vessel-local metres of the main deck edge: faces above it are superstructure
//                                 (seams at half contrast) and rust streaks hang below it. Default: none.
//   chunk_heat         float      0..1 fire in this chunk now (from the fire events): glow on scorched cells.
//   fire_ramp          sampler2D  gen ramp_fire (the VFX fire ramp), the heat glow's colour.
//   view_mode          int        0 normal; 1 X-ray dim (editor, overlays on top); 2 armour tiers by value.
// Global parameters: the wave set of res://shaders/gen/waves.gdshaderinc and the cloud shadow of
// res://shaders/inc/cloud_shadow.gdshaderinc.
shader_type spatial;
render_mode cull_back, depth_draw_opaque, diffuse_burley, specular_schlick_ggx;

#include "res://shaders/gen/waves.gdshaderinc"
#include "res://shaders/inc/cloud_shadow.gdshaderinc"
#include "res://shaders/inc/hull_common.gdshaderinc"

uniform bool lod_vertex_colour = false;
uniform sampler3D hull_volume : filter_linear, repeat_disable;
uniform vec3 chunk_origin;
uniform vec3 palette[16];
uniform vec3 primer_albedo;
uniform sampler2D weathering_noise : hint_default_black, filter_linear_mipmap, repeat_enable;
uniform float weathering_level : hint_range(0.0, 1.0) = 0.0;
uniform float waterline_y = 0.0;
uniform float deck_y = 100000.0;
uniform float chunk_heat : hint_range(0.0, 1.0) = 0.0;
uniform sampler2D fire_ramp : source_color, hint_default_black, filter_nearest, repeat_disable;
uniform int view_mode = 0;

// Cell edge in metres and the volume edge in texels (L1925, L1931).
const float CELL_M = 0.5;
const float VOL_EDGE = 34.0;
// AO (L1935): AO = 1 - 0.45 clamp(1.33 occupancy, 0, 1), sampled half a cell out along the normal.
const float AO_DEPTH = 0.45;
const float AO_GAIN = 1.33;
// Bevel (L1936): 0.04 m chamfer on convex exposed edges, value lift 10 to 15%, full at 2 px, gone at 1 px.
const float BEVEL_M = 0.04;
const float BEVEL_LIFT = 0.12;
// Seams (L1937): strakes 1 m, plates 4 m with butts staggered 2 m, frames every 2 m; 8% darker; fade from
// 8 px per metre (117 m at 1080p, 60 degrees) to 4 px per metre (234 m); half contrast on superstructure.
const float SEAM_DARK = 0.08;
// Frames sit between the butts, at half the butt contrast, so the plate pattern stays readable.
const float FRAME_SHARE = 0.5;
// Weathering (L1940): at most 20% value and 0.03 chroma; salt band 0.6 m above water, 10 to 15% darker.
const float WEATHER_MAX_VALUE = 0.2;
const float SALT_BAND_M = 0.6;
const float SALT_DARK = 0.12;
// The primer hue scaled to the weathering chroma cap: primer is C 0.05, rust may reach 0.03.
const float RUST_CHROMA_SHARE = 0.6;
// Rust covers at most this share of a pixel's colour; most of a hull stays clean paint.
const float RUST_MAX = 0.75;
// Wet line (L1911): within 300 m, cells below the wave height + 0.3 m go to roughness 0.2.
const float WET_RANGE_M = 300.0;
const float WET_ABOVE_M = 0.3;
const float WET_VALUE = 0.86;
// Heat glow stays on the emission ladder's "everything else 1 or less" rung (L1955).
const float HEAT_EMISSION = 0.6;
// Where on the fire ramp the glow takes its colour: past the flame bands, in the dull red.
const float HEAT_RAMP_U = 0.7;

varying vec3 v_local;
varying vec3 v_local_n;

void vertex() {
	v_local = VERTEX;
	v_local_n = NORMAL;
}

// Volume texel -> byte.
int vbyte(float v) {
	return int(v * 255.0 + 0.5);
}

// Sum of the global wave amplitudes: a cheap bound before the per-pixel Newton solve.
float wave_amplitude_bound() {
	return abs(bp_waves_0[1].x) + abs(bp_waves_0[3].x) + abs(bp_waves_1[1].x) + abs(bp_waves_1[3].x)
		+ abs(bp_waves_2[1].x) + abs(bp_waves_2[3].x) + abs(bp_waves_3[1].x) + abs(bp_waves_3[3].x)
		+ abs(bp_waves_4[1].x) + abs(bp_waves_4[3].x) + abs(bp_waves_5[1].x) + abs(bp_waves_5[3].x)
		+ abs(bp_waves_6[1].x) + abs(bp_waves_6[3].x) + abs(bp_waves_7[1].x) + abs(bp_waves_7[3].x);
}

// 0..1 coverage of a 1 px line at distance d (metres) for a coordinate changing w metres per pixel.
float line_cover(float d, float w) {
	return clamp(1.0 - d / max(w, 1e-5), 0.0, 1.0);
}

// Distance from x to the nearest point phase + k * period.
float lattice_dist(float x, float period, float phase) {
	float t = (x - phase) / period;
	return abs(t - floor(t + 0.5)) * period;
}

void fragment() {
	vec3 world = (INV_VIEW_MATRIX * vec4(VERTEX, 1.0)).xyz;
	float cam_dist = length(VERTEX);
	float cloud = bp_cloud_factor(world);
	vec3 n = normalize(v_local_n);
	vec3 albedo;
	float rough = BP_ROUGH_PAINT;
	float ao = 1.0;
	vec3 emission = vec3(0.0);
	vec3 n_out = n;

	if (lod_vertex_colour) {
		// LOD1 to LOD4: swatch times AO already baked; no texture fetch.
		albedo = COLOR.rgb;
	} else {
		vec3 tc = (v_local - chunk_origin) / CELL_M + 1.0;
		vec3 an = abs(n);
		bool flat_face = max(an.x, max(an.y, an.z)) > 0.999;
		int k = an.x >= an.y && an.x >= an.z ? 0 : (an.y >= an.z ? 1 : 2);
		// The solid cell behind the face. Flat faces lie on a cell boundary: step half a cell in. Shaped faces
		// cut through their own cell: a small step stays inside it.
		vec3 inward = flat_face ? n * 0.5 : n * 0.1;
		ivec3 sc = clamp(ivec3(floor(tc - inward)), ivec3(0), ivec3(33));
		vec4 cell = texelFetch(hull_volume, sc, 0);
		// With MSAA a pixel on a face edge is shaded at its centre, which may lie just past the edge, so the
		// interpolated position names the neighbouring (air) cell. Step back into the face across the nearest
		// cell boundary and use the position just inside it.
		if (cell.r < 0.5) {
			vec3 f = fract(tc - inward);
			vec3 off = abs(f - 0.5);
			int ax = off.x >= off.y && off.x >= off.z ? 0 : (off.y >= off.z ? 1 : 2);
			vec3 axis = ax == 0 ? vec3(1.0, 0.0, 0.0) : (ax == 1 ? vec3(0.0, 1.0, 0.0) : vec3(0.0, 0.0, 1.0));
			float f0 = dot(f, axis);
			float dir = f0 > 0.5 ? 1.0 : -1.0;
			ivec3 sc2 = clamp(sc + ivec3(axis * dir), ivec3(0), ivec3(33));
			vec4 c2 = texelFetch(hull_volume, sc2, 0);
			if (c2.r >= 0.5) {
				sc = sc2;
				cell = c2;
				tc += axis * (dir > 0.0 ? 1.0 - f0 + 0.01 : -(f0 + 0.01));
			}
		}
		int g = vbyte(cell.g);
		int pidx = g & 15;
		int cls = (g >> 4) & 3;
		float scorch = cell.b;
		int a_byte = vbyte(cell.a);
		int armour = a_byte & 7;
		float wear01 = float(a_byte >> 3) / 31.0;

		// AO: one linear R fetch at face + 0.5 cell along the normal. A shaped face passes through its own
		// cell, whose texel would darken every slope evenly: its share of the filtered sample is removed.
		vec3 ao_tc = tc + n * 0.5;
		float occ = textureLod(hull_volume, ao_tc / VOL_EDGE, 0.0).r;
		if (!flat_face) {
			vec3 own = max(vec3(0.0), 1.0 - abs(ao_tc - (vec3(sc) + 0.5)));
			float w_own = own.x * own.y * own.z;
			occ = clamp((occ - w_own) / max(1.0 - w_own, 0.25), 0.0, 1.0);
		}
		ao = 1.0 - AO_DEPTH * clamp(AO_GAIN * occ, 0.0, 1.0);

		vec3 base = palette[pidx];
		if (cls == 3) {
			base = primer_albedo;
			rough = BP_ROUGH_PRIMER;
		} else if (cls == 1) {
			base *= BP_GLASS_VALUE;
			rough = BP_ROUGH_GLASS;
		}
		albedo = base;

		// In-plane axes of a flat face: V is world up on walls; on decks U runs fore and aft.
		vec3 U = k == 0 ? vec3(0.0, 0.0, 1.0) : vec3(1.0, 0.0, 0.0);
		vec3 V = k == 1 ? vec3(0.0, 0.0, 1.0) : vec3(0.0, 1.0, 0.0);
		float pu = dot(v_local, U);
		float pv = dot(v_local, V);
		// Metres per pixel along U and V, from the projection rather than fwidth: the T-junction splits leave
		// sliver triangles along edges whose 2x2 derivative quads would spike and dash the bevel line.
		vec3 view_dir = normalize(VERTEX);
		bool ortho = PROJECTION_MATRIX[3][3] > 0.5;
		float px = 2.0 / (abs(PROJECTION_MATRIX[1][1]) * VIEWPORT_SIZE.y) * (ortho ? 1.0 : cam_dist);
		// A band of width w along U (edge along V) is w |n.d| / |V_perp| on screen; and the same for V.
		vec3 u_view = normalize((VIEW_MATRIX * vec4(MODEL_NORMAL_MATRIX * U, 0.0)).xyz);
		vec3 v_view = normalize((VIEW_MATRIX * vec4(MODEL_NORMAL_MATRIX * V, 0.0)).xyz);
		vec3 n_view = normalize((VIEW_MATRIX * vec4(MODEL_NORMAL_MATRIX * n, 0.0)).xyz);
		float ndv = max(abs(dot(n_view, view_dir)), 0.02);
		float u_perp = sqrt(max(1.0 - pow(dot(u_view, view_dir), 2.0), 0.0));
		float v_perp = sqrt(max(1.0 - pow(dot(v_view, view_dir), 2.0), 0.0));
		float mpp_u = px * max(v_perp, 0.05) / ndv;
		float mpp_v = px * max(u_perp, 0.05) / ndv;
		float mpp = max(mpp_u, mpp_v);

		float edge_top = 0.0;
		float edge_any = 0.0;
		if (flat_face) {
			// Edge sample: one linear R fetch on the solid cell's own layer, offset a third of a texel toward
			// the nearer U side and a fifth toward the nearer V side. The weights 8/15 (own), 4/15 (U
			// neighbour), 2/15 (V neighbour) and 1/15 (diagonal) make the three neighbour bits decodable.
			float fu = fract(dot(tc, U));
			float fv = fract(dot(tc, V));
			float su = fu < 0.5 ? -1.0 : 1.0;
			float sv = fv < 0.5 ? -1.0 : 1.0;
			float du = (su < 0.0 ? fu : 1.0 - fu) * CELL_M;
			float dv = (sv < 0.0 ? fv : 1.0 - fv) * CELL_M;
			vec3 e_tc = vec3(sc) + 0.5 + U * (su / 3.0) + V * (sv / 5.0);
			vec4 e_s = textureLod(hull_volume, e_tc / VOL_EDGE, 0.0);
			float code_f = e_s.r * 15.0 - 8.0;
			// The same sample's B blends the scorch of the neighbouring plates, so soot shades across cells
			// instead of stepping per cell. The simulation scorches solid cells only, so B over R is the
			// weighted mean over the solid neighbours (the own cell alone weighs 8/15).
			scorch = clamp(e_s.b / max(e_s.r, 0.5), 0.0, 1.0);
			int code = clamp(int(code_f + 0.5), 0, 7);
			bool cvx_u = (code & 4) == 0;
			bool cvx_v = (code & 2) == 0;

			// Bevel: bend the normal 45 degrees toward a convex edge inside the 0.04 m chamfer and lift value.
			float bu = cvx_u ? (1.0 - smoothstep(BEVEL_M - mpp_u, BEVEL_M + mpp_u, du)) * smoothstep(1.0, 2.0, BEVEL_M / mpp_u) : 0.0;
			float bv = cvx_v ? (1.0 - smoothstep(BEVEL_M - mpp_v, BEVEL_M + mpp_v, dv)) * smoothstep(1.0, 2.0, BEVEL_M / mpp_v) : 0.0;
			if (bu + bv > 0.0 && cls != 1) {
				n_out = normalize(n + U * (su * bu) + V * (sv * bv));
				albedo *= 1.0 + BEVEL_LIFT * max(bu, bv);
				rough = mix(rough, BP_ROUGH_WORN_EDGE, max(bu, bv));
			}

			// Edge masks for the rust bleed: nearness to a convex edge within the half cell.
			float near_u = cvx_u ? 1.0 - du / (0.5 * CELL_M) : 0.0;
			float near_v = cvx_v ? 1.0 - dv / (0.5 * CELL_M) : 0.0;
			edge_top = (k != 1 && sv > 0.0) ? near_v * near_v : 0.0;
			edge_any = max(near_u * near_u, near_v * near_v);

			// Plating seams on exposed painted flat faces.
			if (cls == 0) {
				float seam_fade = smoothstep(4.0, 8.0, 1.0 / mpp);
				if (seam_fade > 0.0) {
					float stagger = 2.0 * mod(floor(pv), 2.0);
					float c_strake = line_cover(lattice_dist(pv, 1.0, 0.0), mpp_v);
					float c_butt = line_cover(lattice_dist(pu, 4.0, stagger), mpp_u);
					float c_frame = line_cover(lattice_dist(pu, 2.0, 1.0), mpp_u) * FRAME_SHARE;
					float seam = max(c_strake, max(c_butt, c_frame));
					float contrast = v_local.y > deck_y ? 0.5 : 1.0;
					albedo *= 1.0 - SEAM_DARK * contrast * seam * seam_fade;
				}
			}
		}

		// Weathering, seeded per ship and cell (L1940): rust bleed from edges and below the deck edge, the
		// salt band above the waterline. Paint only; level 0 in the dry dock.
		if (cls == 0 && weathering_level > 0.0) {
			float horiz = k == 0 ? v_local.z : v_local.x;
			float noise = texture(weathering_noise, vec2(horiz / 16.0, v_local.y / 2.0)).r;
			bool side = abs(n.y) < 0.35;
			float streak = side ? smoothstep(deck_y - 3.0, deck_y - 0.25, v_local.y) * step(v_local.y, deck_y) : 0.0;
			// The cell's wear seed picks which edges are rust sources (about four in ten); the colour itself
			// never steps per cell, so no cell reads as a tinted block (L1815).
			float source = step(0.6, wear01);
			float bleed = clamp(source * (edge_top + 0.3 * edge_any) + 0.45 * streak, 0.0, 1.0);
			float rust = clamp(smoothstep(0.5, 0.85, noise) * bleed * weathering_level, 0.0, 1.0) * RUST_MAX;
			float salt = 0.0;
			if (side) {
				float top = waterline_y + SALT_BAND_M + 0.15 * (noise - 0.5);
				salt = step(waterline_y, v_local.y) * (1.0 - smoothstep(top - 0.05, top + 0.05, v_local.y));
			}
			float p_lum = bp_luma(primer_albedo);
			vec3 hue = p_lum > 1e-4 ? primer_albedo / p_lum - 1.0 : vec3(0.0);
			albedo = mix(albedo, bp_luma(albedo) * (1.0 + hue * RUST_CHROMA_SHARE), rust);
			float darken = 0.2 * rust + SALT_DARK * salt * weathering_level;
			albedo *= 1.0 - min(darken, WEATHER_MAX_VALUE);
		}

		// Damage (L1941): scorch takes value to 40% and chroma to 0; fire in the chunk glows on the soot.
		if (scorch > 0.0) {
			albedo = bp_scorch(albedo, scorch);
			rough = mix(rough, BP_ROUGH_SOOT, scorch);
			if (chunk_heat > 0.0) {
				// Only the most scorched cells glow, plate by plate (about half of them), each flickering at
				// its own pace; the colour is the fire ramp's deep band.
				float h = fract(sin(dot(vec3(sc), vec3(12.9898, 78.233, 37.719))) * 43758.5453);
				float flick = 0.6 + 0.4 * sin(TIME * (1.5 + 2.0 * h) + h * 6.2832);
				float glow = chunk_heat * smoothstep(0.75, 1.0, scorch) * step(0.45, h) * flick;
				emission += textureLod(fire_ramp, vec2(HEAT_RAMP_U, 0.5), 0.0).rgb * glow * HEAT_EMISSION;
			}
		}
		if (cls == 2) {
			emission += base;
		}

		if (view_mode == 1) {
			albedo = vec3(0.35 * bp_luma(albedo));
			emission = vec3(0.0);
		} else if (view_mode == 2) {
			// Armour by value only, so it reads in greyscale and for every colour vision: unarmoured plate
			// dark, each tier a step lighter.
			albedo = vec3(armour == 0 ? 0.05 : 0.12 + 0.11 * float(armour));
			emission = vec3(0.0);
		}
	}

	// Wet line within 300 m: one Newton wave solve, only below the amplitude bound.
	if (cam_dist < WET_RANGE_M && view_mode == 0) {
		if (world.y < wave_amplitude_bound() + WET_ABOVE_M + 0.1) {
			float h = wave_height(world.xz, TIME);
			float wet = 1.0 - smoothstep(h + WET_ABOVE_M - 0.08, h + WET_ABOVE_M + 0.08, world.y);
			wet *= 1.0 - smoothstep(WET_RANGE_M * 0.8, WET_RANGE_M, cam_dist);
			rough = mix(rough, BP_ROUGH_WET, wet);
			albedo *= mix(1.0, WET_VALUE, wet);
		}
	}

	ALBEDO = albedo * cloud;
	AO = ao;
	AO_LIGHT_AFFECT = 0.6;
	ROUGHNESS = rough;
	METALLIC = 0.0;
	SPECULAR = 0.5 * cloud;
	RIM = bp_rim(rough);
	RIM_TINT = BP_RIM_TINT;
	EMISSION = emission;
	NORMAL = normalize((VIEW_MATRIX * vec4(MODEL_NORMAL_MATRIX * n_out, 0.0)).xyz);
}
