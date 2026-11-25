// Shared surface rules of the hull and part shaders. DESIGN.md L1772-1782 (light, value before hue),
// L1905 (cloud shadows: one noise fetch), L1933-1941 (hull terms). Units: shader-hull.
// Include after res://shaders/inc/cloud_shadow.gdshaderinc. Everything here is maths: no colour constants,
// since world colours come only from the tokens (palette, primer) and the generated ramps.

// Rec. 709 luminance of a linear colour (the Y the world tokens are authored in).
float bp_luma(vec3 c) {
	return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

// CIE lightness L* / 100 of a luminance, and back (the spec's "value" is perceptual, L1782).
float bp_lightness(float y) {
	return y > 0.008856 ? 1.16 * pow(y, 1.0 / 3.0) - 0.16 : 9.033 * y;
}

float bp_luminance(float l) {
	return l > 0.08 ? pow((l + 0.16) / 1.16, 3.0) : l / 9.033;
}

// Scorch (L1941): value (lightness) to 40% and chroma to 0 at full scorch, so haze grey goes to soot. The
// lightness falls linearly with scorch, so half-scorched plate reads half way to soot.
vec3 bp_scorch(vec3 albedo, float scorch) {
	float s = clamp(scorch, 0.0, 1.0);
	float y = bp_luma(albedo);
	if (y <= 1e-5 || s <= 0.0) {
		return albedo;
	}
	float y2 = bp_luminance(bp_lightness(y) * mix(1.0, 0.4, s));
	return mix(albedo, vec3(y), s) * (y2 / y);
}

// Share of a sunlit face's light that is direct sun at the hero dusk preset. Without a light() override the
// cloud shadow cannot touch the sun alone, so it scales the albedo and specular by this share instead; the
// sky fill under a cloud keeps the rest.
const float BP_DIRECT_SHARE = 0.72;

// Albedo and specular factor of the cloud shadow at a render-space position (one noise fetch).
float bp_cloud_factor(vec3 world_pos) {
	float c = bp_cloud_light(world_pos.xz);
	return 1.0 - BP_DIRECT_SHARE * (1.0 - c);
}

// Painted steel and its wear (L1933-1941). Roughness values of the material classes.
const float BP_ROUGH_PAINT = 0.64;
const float BP_ROUGH_PRIMER = 0.78;
const float BP_ROUGH_SOOT = 0.92;
const float BP_ROUGH_WORN_EDGE = 0.46;
const float BP_ROUGH_GLASS = 0.06;
const float BP_ROUGH_WET = 0.2;
// Glass (L1933): opaque, dark and glossy. Its albedo is its swatch times this.
const float BP_GLASS_VALUE = 0.22;
// Rim light (L1774): a faint grazing lift that separates silhouettes from the sea without an outline. Tinted
// fully by the albedo, so soot and dark paint stay dark; its width follows roughness, so it is kept off rough
// surfaces by the caller (bp_rim).
const float BP_RIM = 0.2;
const float BP_RIM_TINT = 1.0;

float bp_rim(float roughness) {
	return BP_RIM * (1.0 - smoothstep(0.5, 0.8, roughness));
}
