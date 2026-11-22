// VFX layer A, blended: missile trails and muzzle haze, sorted back to front by Rust. DESIGN.md L1953.
// Unit: shader-vfx. Vertex and instance data as in vfx_opaque.gdshader.
// Alpha blending is only for mist, missile trails and muzzle haze (L1953). The ramp's A is the opacity; the
// instance colour's alpha scales it. Lit and fogged; never emissive, never written to depth.
// Material parameters: vfx_kind (11 missile trail, 16 muzzle haze, 36 Layer B mist), ramp, emission_cap.
shader_type spatial;
render_mode blend_mix, depth_draw_never, cull_back, world_vertex_coords, diffuse_burley, specular_schlick_ggx;

#include "res://shaders/gen/waves.gdshaderinc"
#include "res://shaders/inc/vfx_common.gdshaderinc"

uniform int vfx_kind = 11;
uniform sampler2D ramp : source_color, hint_default_white, filter_nearest, repeat_disable;
uniform float emission_cap = 16.0;

varying float v_t;
varying float v_along;
varying vec3 v_world;

void vertex() {
	VfxOut o = vfx_motion(vfx_kind, VERTEX, NORMAL, MODEL_MATRIX, CUSTOM0, INSTANCE_CUSTOM, TIME);
	VERTEX = o.vertex;
	NORMAL = o.normal;
	v_t = o.t;
	v_along = CUSTOM0.x;
	v_world = VERTEX;
}

void fragment() {
	vec4 rs = texture(ramp, vec2(clamp(v_t, 0.0, 1.0) * (255.0 / 256.0) + 0.5 / 256.0, 0.5));
	float opacity = rs.a * COLOR.a;
	if (vfx_kind == K_MISSILE_TRAIL) {
		// A motor's smoke is a translucent veil that thins as it spreads; the newest end of each segment meets
		// the next one's oldest end.
		opacity *= 0.55 * (1.0 - 0.75 * v_t);
		// Near the eye the veil thins out, so a trail the camera passes close to never walls off the frame.
		opacity *= smoothstep(6.0, 45.0, length(v_world - CAMERA_POSITION_WORLD));
	} else if (vfx_kind == K_MUZZLE_HAZE) {
		// Haze is a veil over the muzzle smoke, never a solid ball.
		opacity *= 0.45 * (1.0 - smoothstep(0.3, 1.0, v_t));
	} else if (vfx_kind == LB_MIST) {
		opacity *= 1.0 - smoothstep(0.6, 1.0, v_t);
	}
	ALBEDO = rs.rgb * COLOR.rgb;
	ROUGHNESS = 0.95;
	SPECULAR = 0.2;
	ALPHA = clamp(opacity, 0.0, 1.0);
}
