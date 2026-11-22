// Flat-shaded islands in token colour bands. DESIGN.md L1917. Unit: shader-sky.
//
// Mesh: hull_mesh::env::scenery::island_mesh. VERTEX is metres from the mesh origin with y the height above
// mean sea level; flat NORMAL per triangle; COLOR.rgb the height band's token colour, linear. No CUSTOM0.
// The instance transform is a translation with y = 0. Fog comes from the environment, the same as the sea
// and the clouds, so far islands sink into the haze as silhouettes.
//
// Matte rock and scrub with no highlight. The strip the waves wet is darker, and cloud shadows cross the
// land with the same noise as the sea (res://shaders/inc/cloud_shadow.gdshaderinc).
//
// Material parameter (set by gd-render-scenery):
//   wet_height_m  float  height above mean sea level up to which the shore is wet (about half the
//                        significant wave height, at least 0.5).
shader_type spatial;
render_mode cull_back, specular_disabled;

#include "res://shaders/inc/cloud_shadow.gdshaderinc"

uniform float wet_height_m = 1.0;

// How much darker wet shingle is than dry.
const float WET_VALUE = 0.62;

varying vec3 v_world;
varying float v_cloud;

void vertex() {
	v_world = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
}

void fragment() {
	float wet = 1.0 - smoothstep(0.4 * wet_height_m, wet_height_m, v_world.y);
	ALBEDO = COLOR.rgb * mix(1.0, WET_VALUE, wet);
	ROUGHNESS = 1.0;
	SPECULAR = 0.0;
	v_cloud = bp_cloud_light(v_world.xz);
}

void light() {
	float lit = ATTENUATION * (LIGHT_IS_DIRECTIONAL ? v_cloud : 1.0);
	DIFFUSE_LIGHT += LIGHT_COLOR * lit * max(dot(NORMAL, LIGHT), 0.0) / PI;
}
