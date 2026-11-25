// VFX layer A, opaque. DESIGN.md L1949-1955. Unit: shader-vfx.
// Vertex: white COLOR; CUSTOM0 a per-vertex motion term (height fraction on splash columns, radius
// fraction on foam rings). Instance: COLOR tint; INSTANCE_CUSTOM.x age in seconds (written by Rust, never
// TIME), .y lifetime in seconds, .z seed 0..1, .w kind-specific (emission scale after the flash limiter,
// calibre factor on splashes). Albedo and emission come from the kind's 256x1 ramp at age / lifetime.
//
// One material per kind (gd-render-vfx; gd-render-projectiles for tracers), all on this shader:
//   vfx_kind       int        the kind's index in hull_mesh::vfx::VfxKind::ALL, or a Layer B draw pass
//                             (32 spray, 33 sparks, 34 embers, 35 flak), see inc/vfx_common.gdshaderinc.
//   ramp           sampler2D  gen ramp_path(kind.ramp().name()), RGBA8 256 x 1.
//   emission_cap   float      16, or 4 under Reduce flashing (L1957); clamps every emission after the ladder.
// Faceted flat-normal meshes, lit by the sun and fogged like everything else; only emission blooms. Foam on the
// sea dissolves through a 0.5 m lattice (alpha scissor, still in the opaque pass); nothing else cuts pixels.
// Draw on the VFX render layer, which the shadow caster mask excludes (L1883).
shader_type spatial;
render_mode cull_back, depth_draw_opaque, world_vertex_coords, diffuse_burley, specular_schlick_ggx;

#include "res://shaders/gen/waves.gdshaderinc"
#include "res://shaders/inc/vfx_common.gdshaderinc"

uniform int vfx_kind = 1;
uniform sampler2D ramp : source_color, hint_default_white, filter_nearest, repeat_disable;
uniform float emission_cap = 16.0;

// Value of the chaff foil (metallic albedo is its specular colour).
const float CHAFF_VALUE = 0.62;

varying flat vec4 v_ic;
varying float v_t;
varying float v_scissor;
varying vec3 v_world;
varying float v_main;
varying float v_sec;
// The instance's origin and unit +X direction in xz, and the lengths of its X, Z and Y axes (the wake
// pieces' foam layout, vfx_wake_keep).
varying flat vec4 v_frame_a;
varying flat vec4 v_frame_b;
// Offset of the vertex from the instance origin before any motion (smoke break-up lattice).
varying vec3 v_local;

void vertex() {
	v_local = VERTEX - MODEL_MATRIX[3].xyz;
	VfxOut o = vfx_motion(vfx_kind, VERTEX, NORMAL, MODEL_MATRIX, CUSTOM0, INSTANCE_CUSTOM, TIME);
	VERTEX = o.vertex;
	NORMAL = o.normal;
	v_ic = INSTANCE_CUSTOM;
	v_t = o.t;
	v_scissor = o.scissor;
	v_world = o.vertex;
	v_main = CUSTOM0.x;
	v_sec = CUSTOM0.y;
	vec3 bx = MODEL_MATRIX[0].xyz;
	v_frame_a = vec4(MODEL_MATRIX[3].xz, normalize(bx.xz + vec2(1e-6, 0.0)));
	v_frame_b = vec4(length(bx), length(MODEL_MATRIX[2].xyz), length(MODEL_MATRIX[1].xyz), 0.0);
}

void fragment() {
	vec4 rs = texture(ramp, vec2(clamp(v_t, 0.0, 1.0) * (255.0 / 256.0) + 0.5 / 256.0, 0.5));
	vec3 albedo = rs.rgb * COLOR.rgb;
	if (vfx_kind == K_SPLASH_COLUMN) {
		// Denser water at the foot reads a little greyer than the spray at the crown.
		albedo *= mix(0.82, 1.0, clamp(v_main, 0.0, 1.0));
	}
	float e = vfx_emission_scale(vfx_kind, v_ic) * rs.a;
	float rough = 0.85;
	float metal = 0.0;
	if (vfx_kind == K_SPLASH_COLUMN || vfx_surface_kind(vfx_kind) || vfx_hull_side_kind(vfx_kind) || vfx_kind == K_PUMP_ARC || vfx_kind == LB_SPRAY) {
		rough = 0.55;
	} else if (vfx_kind == K_DEBRIS || vfx_kind == LB_FLAK) {
		rough = 0.6;
	} else if (vfx_kind == K_CHAFF_CLOUD) {
		// Foil strips: the flak ramp's burst colours do not apply; a bright metal that glints as it tumbles.
		albedo = COLOR.rgb * CHAFF_VALUE;
		e = 0.0;
		rough = 0.25;
		metal = 0.85;
	}
	float keep = 1.0;
	if (vfx_wake_kind(vfx_kind)) {
		float tone = vfx_wake_keep(vfx_kind, v_world, length(VERTEX), v_frame_a, v_frame_b, v_ic, v_sec, v_main);
		keep = step(0.5, tone);
		albedo *= max(tone, 0.5);
	} else if (vfx_kind == K_SMOKE_PUFF) {
		keep = vfx_smoke_keep(v_local, length(VERTEX), v_scissor, v_ic.z);
	} else if (vfx_dissolving_kind(vfx_kind)) {
		keep = vfx_foam_keep(v_world, length(VERTEX), v_scissor, v_ic.z, vfx_airborne_kind(vfx_kind));
	}
	ALBEDO = albedo;
	ROUGHNESS = rough;
	METALLIC = metal;
	EMISSION = albedo * min(e, emission_cap);
	ALPHA = keep;
	ALPHA_SCISSOR_THRESHOLD = 0.5;
}
