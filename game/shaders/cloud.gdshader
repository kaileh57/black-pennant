// Faceted opaque cloud clusters with wrap lighting. DESIGN.md L1915. Unit: shader-sky.
//
// Mesh: hull_mesh::env::scenery::cloud_cluster. Flat NORMAL per facet; COLOR white; CUSTOM0.x the wrap term
// w in 0.2..0.8 (the vertex is lit by max((N.L + w) / (1 + w), 0)); CUSTOM0.y the height in the cluster,
// 0 at the base to 1 at the top.
// Drawn opaque in the main pass and fogged by the environment like everything else; never blurred. Clouds
// take their value from light and fog only: there is no colour parameter. Instances cast no shadow (set on
// the instance); the sea's cloud shadows are the noise of res://shaders/inc/cloud_shadow.gdshaderinc.
//
// Lighting is the spec's per-vertex wrap term, evaluated in light(): every facet has one normal and one
// wrap value, so each facet still takes one flat tone. The vertex_lighting render mode is not used because
// Godot then lights with its built-in Lambert and never calls light(), which loses the wrap and leaves a
// backlit cloud as a flat cut-out.
//
// Material parameters (set by gd-render-scenery):
//   value          float  grey reflectance of cloud. It stays below sea.foam so wakes and foam remain the
//                         brightest matte surfaces (L1781).
//   base_shade     float  how much darker the flat base is than the body, 0..1.
//   silver_lining  float  strength of the light that leaks round thin edges when the sun is behind the cloud.
//   haze_color     vec3   the preset's fog colour, linear (the environment's fog_light_color before encoding).
//   haze_density   float  the environment's fog density (3.912 / V). Negative leaves the environment's own fog.
//
// Haze: the environment's fog is uniform with height (fog_height_density stays 0, L1872, so hulls fog
// evenly), which would hide every cloud mass on the horizon behind the same haze as the sea at that range.
// Cloud bases stand 300 m to 3 km up, above the haze the boundary layer holds, so the shader writes its own
// fog: the same density, thinned along the sight line by an exponential haze layer of HAZE_SCALE_HEIGHT_M.
// Near a cloud at sea level it equals the environment's fog; a far bank keeps enough of its value to stand out
// as the far depth layer (L1779) and fill the horizon (G5).
shader_type spatial;
render_mode cull_back, shadows_disabled, specular_disabled;

uniform float value = 0.6;
uniform float base_shade = 0.4;
uniform float silver_lining = 1.6;
uniform vec3 haze_color;
uniform float haze_density = -1.0;

const float HAZE_SCALE_HEIGHT_M = 700.0;

varying float v_wrap;

void vertex() {
	v_wrap = CUSTOM0.x;
	// The base reads as one flat grey slab: darken it here so ambient light is dimmed with it.
	COLOR.rgb *= value * (1.0 - base_shade * (1.0 - smoothstep(0.0, 0.3, CUSTOM0.y)));
}

void fragment() {
	ALBEDO = COLOR.rgb;
	ROUGHNESS = 1.0;
	SPECULAR = 0.0;
	if (haze_density >= 0.0) {
		vec3 world = (INV_VIEW_MATRIX * vec4(VERTEX, 1.0)).xyz;
		float y0 = max(CAMERA_POSITION_WORLD.y, 0.0);
		float y1 = max(world.y, 0.0);
		float e0 = exp(-y0 / HAZE_SCALE_HEIGHT_M);
		float e1 = exp(-y1 / HAZE_SCALE_HEIGHT_M);
		// Mean density along the sight line as a share of the sea-level density.
		float share = abs(y1 - y0) > 1.0 ? HAZE_SCALE_HEIGHT_M * (e0 - e1) / (y1 - y0) : e0;
		FOG = vec4(haze_color, 1.0 - exp(-haze_density * length(VERTEX) * share));
	}
}

void light() {
	float wrap = max((dot(NORMAL, LIGHT) + v_wrap) / (1.0 + v_wrap), 0.0);
	// Thin outer puffs carry a large wrap term; with the sun behind them they let light through.
	// The leak shows on facets seen edge-on, so a backlit cloud keeps a dark core and a bright rim.
	float behind = max(dot(-VIEW, LIGHT), 0.0);
	float edge_on = 1.0 - abs(dot(NORMAL, VIEW));
	float leak = silver_lining * v_wrap * behind * behind * behind * behind * edge_on * edge_on;
	DIFFUSE_LIGHT += LIGHT_COLOR * ATTENUATION * (wrap + leak) / PI;
}
