// Parts and small bodies drawn as MultiMesh instances. DESIGN.md L1945. Unit: shader-hull.
// Vertex: COLOR.rgb linear albedo, COLOR.a paint mask (1 = multiply by the ship paint, 0 = fixed albedo);
// CUSTOM0.x emission strength (lamps at most 1), CUSTOM0.y 1 on glass.
// Instance: INSTANCE_CUSTOM.x scorch 0..1 (1 = destroyed); INSTANCE_CUSTOM.yzw the ship paint the part wears
// (linear RGB). The MultiMesh instance colour stays white (1, 1, 1, 1): Godot multiplies the instance colour
// into COLOR before vertex() runs, so a paint carried there would also tint the fixed-albedo surfaces.
// If custom.yzw is all zero (no paint written) the part draws its vertex colour times the instance colour.
// Same paint, glass, scorch, cloud-shadow and rim rules as the hull (inc/hull_common.gdshaderinc); parts are
// small primitive meshes, so they carry no bevel, seams or weathering.
shader_type spatial;
render_mode cull_back, depth_draw_opaque, diffuse_burley, specular_schlick_ggx;

#include "res://shaders/inc/cloud_shadow.gdshaderinc"
#include "res://shaders/inc/hull_common.gdshaderinc"

varying vec4 v_colour;
varying vec2 v_extra;
varying flat vec4 v_ic;

void vertex() {
	v_colour = COLOR;
	v_extra = CUSTOM0.xy;
	v_ic = INSTANCE_CUSTOM;
}

void fragment() {
	vec3 world = (INV_VIEW_MATRIX * vec4(VERTEX, 1.0)).xyz;
	float cloud = bp_cloud_factor(world);
	vec3 paint = v_ic.yzw;
	bool painted = paint.r + paint.g + paint.b > 0.0;
	float mask = clamp(v_colour.a, 0.0, 1.0);
	vec3 albedo = painted ? v_colour.rgb * mix(vec3(1.0), paint, mask) : v_colour.rgb;
	float rough = BP_ROUGH_PAINT;
	float glass = step(0.5, v_extra.y);
	if (glass > 0.0) {
		albedo *= BP_GLASS_VALUE;
		rough = BP_ROUGH_GLASS;
	}
	float scorch = clamp(v_ic.x, 0.0, 1.0);
	albedo = bp_scorch(albedo, scorch);
	rough = mix(rough, BP_ROUGH_SOOT, scorch);
	// Lamps sit on the "everything else 1 or less" rung (L1955); a destroyed part's lamps are out.
	float lamp = clamp(v_extra.x, 0.0, 1.0) * (1.0 - step(0.999, scorch));
	ALBEDO = albedo * cloud;
	ROUGHNESS = rough;
	METALLIC = 0.0;
	SPECULAR = 0.5 * cloud;
	RIM = bp_rim(rough);
	RIM_TINT = BP_RIM_TINT;
	EMISSION = v_colour.rgb * lamp;
}
