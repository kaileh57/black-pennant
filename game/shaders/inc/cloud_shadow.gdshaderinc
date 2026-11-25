// Cloud shadows on the sea, hulls and islands: one noise fetch (DESIGN.md L1905). Unit: shader-water.
// Include from any spatial shader and multiply the directional light by bp_cloud_light(p), where p is the
// position's xz in render space (metres relative to the render origin).
//
// Global shader parameters (registered in project.godot [shader_globals]; set by gd-render-ocean):
//   bp_cloud_shadow     sampler2D  the tileable value noise `art/gen/noise_cloud_shadow.bin` (R8, 512 squared).
//   bp_cloud_shadow_xf  vec4       xy: the UV of the render origin, that is fract((origin.xz + wind_offset) * z)
//                                  computed in f64, so a rebase or drift never shifts the pattern;
//                                  z: UV per metre (1 / tile size; 1 / 6000 m is the intended value);
//                                  w: cloud cover 0..1. 0 or less turns the shadows off without a fetch.
// Unset, bp_cloud_shadow_xf is zero and the function returns 1.
global uniform sampler2D bp_cloud_shadow : repeat_enable, filter_linear;
global uniform vec4 bp_cloud_shadow_xf;

// Share of the direct sun that is lost in the middle of a cloud shadow.
const float BP_CLOUD_SHADOW_DEPTH = 0.65;

// Direct-sun factor at a render-space xz position: 1 in the open, 1 - BP_CLOUD_SHADOW_DEPTH under a cloud.
// The noise is thresholded by cover, so shadows grow from scattered patches to a full layer; under a full
// layer the preset has already dimmed the sun, so the contrast fades out above 0.8 cover.
float bp_cloud_light(vec2 p) {
	float cover = bp_cloud_shadow_xf.w;
	if (cover <= 0.0) {
		return 1.0;
	}
	float n = textureLod(bp_cloud_shadow, p * bp_cloud_shadow_xf.z + bp_cloud_shadow_xf.xy, 0.0).r;
	float edge = mix(0.86, 0.24, clamp(cover, 0.0, 1.0));
	float shadow = smoothstep(edge - 0.07, edge + 0.07, n);
	return 1.0 - BP_CLOUD_SHADOW_DEPTH * shadow * (1.0 - smoothstep(0.8, 1.0, cover));
}
