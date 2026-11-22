// Sea surface. DESIGN.md L1885-1911. Unit: shader-water.
//
// Geometry: the clipmap of hull_mesh::env::ocean. VERTEX is level-local metres with y = 0; CUSTOM0.x is the
// level spacing s in metres, .y the morph weight w (0 inside, 1 across the outer band), .z the level index,
// .w 1 on skirt vertices. No COLOR. The instance transform is a translation: the level's snapped centre
// minus the render origin, with y the mean sea level (0 in a sortie, the design waterline in the dry dock).
// Instances cast no shadow (set on the instance); the surface receives them.
//
// Waves: the Gerstner set of res://shaders/gen/waves.gdshaderinc (written by cargo xtask gen from sim-core).
// Vertices are displaced with the include's own bp_waves_add, so the drawn surface is the simulation's.
// The only reduction is the Nyquist fade A x smoothstep(2s', 4s', lambda), s' = s (1 + w) (L1887).
//
// Material parameters (set by gd-render-ocean; every default is neutral, so an unbound input draws calm,
// foam-free water rather than an artefact):
//   deep_color, scatter_color, shallow_color, foam_color   the sea.* world tokens, linear.
//   sea_state        float  WMO code 0..8, fractional during weather blends. Whitecaps start at 3.
//   rain             float  0..1, roughens the surface.
//   nyquist_fade     float  1 normally. Set 0 while the faded amplitude under any ship would exceed
//                           0.001 x camera distance (L1887); the mesh then draws every component everywhere.
//   detail_normals   sampler2DArray  art/gen/ocean_detail_normals.bin: 64 looping frames. RG = x and z of the
//                           unit surface normal (y up, U along +X, V along +Z), stored as n * 0.5 + 0.5.
//                           The shader expects ripples only: wavelengths under 8 m at the 41 m tiling (the
//                           Gerstner set owns 8 m and up), with a slope variance near 0.012 per axis pair
//                           (RMS slope about 0.11), which is what DETAIL_SLOPE_VARIANCE assumes.
//   detail_turbulence sampler2DArray art/gen/ocean_detail_turbulence.bin: R = turbulence 0..1, 0.5 at rest
//                           (standard deviation about 0.17), towards 1 where the baked surface folds.
//   detail_strength  float  0 until both arrays are bound, then 1 (0.25 in the dry dock).
//   detail_tilings   int    2, or 1 at degrade step 6 (L2031).
//   detail_phase     float  loop position 0..1 of the 24 s bake at bp_waves_t_ref: fract(t_render / 24).
//   detail_origin    vec4   UV of the render origin in each tiling, computed in f64 so a rebase does not
//                           shift the detail: xy = fract(origin.xz / 41.0), zw = fract(origin.xz / 11.3).
//   hull_count       int    hulls masked this frame, 0..8.
//   hull_mask_from_render  mat4[8]  render space to mask metres of layer i: x = 0.5 m x texel u,
//                           z = 0.5 m x (texel v - 64), y = vessel-local height (the centreline offset of
//                           hull_mesh::mask::HullMask is folded into the translation).
//   hull_masks       sampler2DArray  512 x 128 RGBA16F layers: R dry-column bottom, G top (vessel-local
//                           metres), B signed distance to the waterline outline, A 1 where the column holds hull.
//   shore_enabled    bool   true once shore_height is bound.
//   shore_height     sampler2D  camera-centred R16 heightfield, 2048 squared at 4 m.
//   shore_window     vec4   xy = render-space xz of the texture's (0, 0) corner, z = 1 / its extent in metres.
//   shore_decode     vec2   ground height in metres above mean sea level = R * x + y.
//   wind_dir         vec2   unit render-space xz the wind blows towards: whitecaps streak along it.
//   foam_streaks     bool   false on Low and Fallback: whitecaps keep the fold's own outline, no wind streaks
//                           (visual detail degrades first).
//   wake_count       int    hull tracks drawn this frame, 0..8 (gd-render-vfx's wake ribbons, L1967); the first
//                           4 also draw Kelvin arms.
//   wake_info        vec4[24]  three per track: [3w] bow xz, beam and waterline length (metres); [3w + 1] x = the
//                           hull's odometer (metres, wrapped), y = its speed in m/s for the Kelvin arms (0: none),
//                           zw = its heading (unit xz); [3w + 2] the arms' bounds, min xz and max xz.
//   wake_points      vec4[128] sixteen per track, newest first: xz, age in seconds, metres along the track
//                           from the stern. Point 0 is the bow (negative age and distance: alongside the hull),
//                           point 1 the stern now; an age over 1e8 ends the track.
// Global parameters: bp_waves_0..7 and bp_waves_t_ref (wave include); bp_cloud_shadow and bp_cloud_shadow_xf
// (res://shaders/inc/cloud_shadow.gdshaderinc). The cloud cover in bp_cloud_shadow_xf.w also spreads the sun's
// and the moon's highlight: above 0.6 cover the glitter path widens, and under a full layer it is a broad dull sheen.
//
// The material is opaque: no ALPHA and no screen, depth or normal reads, so it writes depth and takes SSAO
// and SSR (L1905). Hull columns are cut with discard, which keeps it in the opaque pass (L1909).
//
// Cost per pixel: 16 sine and cosine pairs at most (unused components are skipped), 4 normal fetches,
// 2 to 6 turbulence fetches, 1 cloud-shadow fetch, and per masked hull in range 2 mask fetches. Wakes: the
// ribbon's cover near a track, the Kelvin crest inside a bow's wedge, and two value-noise lookups on foam;
// the search for the nearest track (15 segments per hull) and for the wedge runs per vertex.
// Lagged whitecap terms run per vertex. Capture harness: res://test/capture/sea_capture.tscn.
shader_type spatial;
render_mode cull_back, depth_draw_opaque;

#include "res://shaders/gen/waves.gdshaderinc"
#include "res://shaders/inc/cloud_shadow.gdshaderinc"

uniform vec3 deep_color : source_color;
uniform vec3 scatter_color : source_color;
uniform vec3 shallow_color : source_color;
uniform vec3 foam_color : source_color;
uniform float sea_state = 0.0;
uniform float rain = 0.0;
uniform float nyquist_fade = 1.0;

uniform sampler2DArray detail_normals : repeat_enable, filter_linear_mipmap_anisotropic;
uniform sampler2DArray detail_turbulence : repeat_enable, filter_linear_mipmap_anisotropic;
uniform float detail_strength = 0.0;
uniform int detail_tilings = 2;
uniform float detail_phase = 0.0;
uniform vec4 detail_origin = vec4(0.0);

uniform int hull_count = 0;
uniform mat4 hull_mask_from_render[8];
uniform sampler2DArray hull_masks : repeat_disable, filter_linear;

uniform bool shore_enabled = false;
uniform sampler2D shore_height : repeat_disable, filter_linear;
uniform vec4 shore_window = vec4(0.0);
uniform vec2 shore_decode = vec2(256.0, -128.0);

uniform vec2 wind_dir = vec2(0.0, 1.0);
uniform bool foam_streaks = true;
uniform int wake_count = 0;
uniform vec4 wake_info[24];
uniform vec4 wake_points[128];

const float TAU_F = 6.2831853;
const float PI_F = 3.14159265;
// Detail tilings and loop (L1903; hull_mesh::gen_files::OCEAN_DETAIL_SCALES_M, OCEAN_DETAIL_LOOP_S).
const float DETAIL_SCALE_A_M = 41.0;
const float DETAIL_SCALE_B_M = 11.3;
const float DETAIL_LOOP_S = 24.0;
const float DETAIL_SIZE = 256.0;
// Slope variance one detail tiling adds once it is too small to see (Cox and Munk at a moderate wind).
const float DETAIL_SLOPE_VARIANCE = 0.012;
// Foam lattice (L1907): 0.5 m world cells, snapped within 200 m.
const float FOAM_CELL_M = 0.5;
const float FOAM_SNAP_FULL_M = 150.0;
const float FOAM_SNAP_END_M = 200.0;
// Smallest footprint the foam's turbulence is read through, metres.
const float FOAM_CLUMP_M = 0.7;
// Whitecap lags (L1907).
const float FOAM_LAG_1_S = 0.6;
const float FOAM_LAG_2_S = 1.2;
const float WHITECAP_FROM_SEA_STATE = 3.0;
// How far a gust patch moves the whitecap threshold, in standard deviations per unit of turbulence.
const float WHITECAP_GUST_SHIFT = 0.9;
// Tone of the foam left 0.6 s and 1.2 s behind a breaker: after the four-tone snap, two thirds and one third.
const float WHITECAP_TRAIL_1 = 0.7;
const float WHITECAP_TRAIL_2 = 0.4;
// Mean foam tone over the share of the sea past the threshold, for the unresolved distance.
const float WHITECAP_MEAN_TONE = 0.55;
// Tone lost by the cells of a fresh breaker's core where the turbulence is low: one step of the four-tone snap.
const float WHITECAP_CORE_BREAK = 0.3;
// Peak whitecap tone at state 3, rising to 1 at state 8.
const float WHITECAP_SLIGHT_PEAK = 0.7;
// The 41 m turbulence tiling stretched by this factor gives the gust patches (about 300 m).
const float GUST_SCALE = 7.3;
// Hull mask layout (hull_mesh::mask).
const float HULL_MASK_TEXEL_M = 0.5;
const vec2 HULL_MASK_SIZE = vec2(512.0, 128.0);
const float HULL_MASK_CENTRE_V = 64.0;
// Water optics (L1905).
const float WATER_F0 = 0.02;
const float SUN_ROUGHNESS_FLOOR = 0.08;
// Share of the sun the water body still scatters inside a hull's shadow. Light under the surface comes up
// from metres down and from the sky as well, so a hull's shadow is a soft dimming of the body, never a cut
// with a hard straight edge across the sea.
const float BODY_SHADOW_FLOOR = 0.8;
// The sun's glint on the water rolls off above GLINT_KNEE towards GLINT_PEAK (scene-linear, before exposure),
// so the glitter path keeps its sparkle and tone under clipping and blooms only at its brightest facets
// (L1780: only emission and sun glints bloom, restrained).
const float GLINT_KNEE = 0.45;
const float GLINT_PEAK = 1.25;
// Wakes (L1967): the ribbon lives 30 s, 1.0 to 2.5 beams wide by speed (full at 30 kn); Kelvin arms at
// 19.47 degrees above 8 kn. Speeds in m/s.
const float WAKE_LIFE_S = 30.0;
const float WAKE_FULL_MS = 15.43;
const float KELVIN_TAN = 0.35355;
const float KELVIN_MIN_MS = 4.12;
// Under a cloud layer the sun is a bright patch of the deck, not a disc: its highlight spreads to this GGX
// alpha at full cover, starting from VEIL_FROM_COVER. A crisp glitter path under a sky with no sun in it
// reads as a lighting error.
const float VEILED_SUN_ALPHA = 0.5;
const float VEIL_FROM_COVER = 0.6;
const float BASE_ALPHA = 0.035;
// Atlas scatter weights: crest transmission, look-down, Lambert, bubbles.
const float SCATTER_HEIGHT = 1.9;
const float SCATTER_VIEW = 0.34;
const float SCATTER_LAMBERT = 0.12;
const float SCATTER_BUBBLE = 0.35;

// Undisplaced render-space xz of the surface point (the Gerstner parameter).
varying vec2 v_p0;
// Displaced render-space position.
varying vec3 v_world;
// x, y: Jacobian determinants 0.6 s and 1.2 s ago; z: standard deviation of the determinant; w: wave height.
varying vec4 v_surf;
// Fragment to light.
varying float v_foam;
varying float v_sun;
// 0 under an open sky, 1 under a full cloud layer.
varying float v_veil;
varying float v_crest;
// View-space normal of the Gerstner surface without the baked detail: light enters the water through the
// swell as a whole, so the body colour must not flicker with every ripple.
varying vec3 v_swell;
// 0..1: the disturbed water of a wake (bubbles under a smoothed surface), which scatters more light.
varying float v_scar;
// The nearest hull track at this vertex (water_track): x = age of the water in seconds, y = metres along the
// track behind the stern, z = signed metres across it, w = the hull's speed there (m/s); and xy = the unit
// vector of +z, z = that hull's beam, w = the water's fixed distance along the track (odometer less y), for
// the foam's streaks. Within one segment every term is linear in position, so interpolation is exact.
varying vec4 v_wake;
varying vec4 v_wake_b;
// The Kelvin wedge (water_kelvin) this vertex lies in: x = metres aft of that bow, y = metres off its line (both
// linear in position), and the hull's index, -1 for none.
varying vec2 v_kelvin;
varying flat int v_kelvin_w;

// Nyquist fade of a component of wavenumber k on a grid of effective spacing se.
float water_fade(float k, float se) {
	float lambda = TAU_F / max(k, 1.0e-6);
	return mix(1.0, smoothstep(2.0 * se, 4.0 * se, lambda), nyquist_fade);
}

// Horizontal Jacobian terms of one component at two past times, for the lagged whitecaps.
void water_lag(vec4 a, vec4 b, vec2 p, float dt, float se, inout vec3 j1, inout vec3 j2, inout float variance) {
	float qak = b.y * b.x * a.z;
	if (qak == 0.0) {
		return;
	}
	variance += 0.5 * qak * qak;
	float th = a.z * (a.x * p.x + a.y * p.y) - a.w * dt + b.z;
	vec3 dd = vec3(a.x * a.x, a.x * a.y, a.y * a.y) * (qak * water_fade(a.z, se));
	j1 -= dd * cos(th + a.w * FOAM_LAG_1_S);
	j2 -= dd * cos(th + a.w * FOAM_LAG_2_S);
}

// Slopes and Jacobian of one component at p, box-filtered over the pixel footprint (fx, fy are the screen
// derivatives of p). `lost` collects the slope variance the filter removed, `grad` the gradient of the
// Jacobian's trace, `seen` the share of its variance still resolved.
void water_shade(vec4 a, vec4 b, vec2 p, float dt, vec2 fx, vec2 fy, inout vec3 j, inout vec2 sl, inout vec2 grad, inout float lost, inout float seen) {
	if (b.x == 0.0) {
		return;
	}
	float th = a.z * (a.x * p.x + a.y * p.y) - a.w * dt + b.z;
	float span = a.z * (abs(dot(a.xy, fx)) + abs(dot(a.xy, fy)));
	float keep = clamp(1.0 - span * span * 0.04, 0.0, 1.0);
	float sn = sin(th);
	float cs = cos(th);
	float ak = b.x * a.z;
	float qak = b.y * ak * keep;
	j.x -= qak * cs * a.x * a.x;
	j.y -= qak * cs * a.x * a.y;
	j.z -= qak * cs * a.y * a.y;
	sl += (ak * keep * sn) * a.xy;
	grad += (qak * sn * a.z) * a.xy;
	lost += 0.5 * ak * ak * (1.0 - keep * keep);
	seen += 0.5 * qak * qak;
}

#define WATER_DISPLACE(M) bp_waves_add(M[0], vec4(M[1].x * water_fade(M[0].z, se), M[1].yzw), p0, dt, o); bp_waves_add(M[2], vec4(M[3].x * water_fade(M[2].z, se), M[3].yzw), p0, dt, o);
#define WATER_LAG(M) water_lag(M[0], M[1], p0, dt, se, j1, j2, variance); water_lag(M[2], M[3], p0, dt, se, j1, j2, variance);
#define WATER_SHADE(M) water_shade(M[0], M[1], v_p0, dt, fx, fy, jac, slope, grad, lost, seen); water_shade(M[2], M[3], v_p0, dt, fx, fy, jac, slope, grad, lost, seen);

// The nearest point of any hull track (L1967) to the undisplaced surface point p: see v_wake and v_wake_b.
// The search runs per vertex (tens of thousands), never per pixel.
void water_track(vec2 p, out vec4 tr, out vec4 tb) {
	float best = 1.0e12;
	int bw = -1;
	int bi = 0;
	float bt = 0.0;
	for (int w = 0; w < wake_count && w < 8; w++) {
		for (int i = 0; i < 15; i++) {
			vec4 a = wake_points[16 * w + i];
			vec4 b = wake_points[16 * w + i + 1];
			if (b.z > 1.0e8) {
				break;
			}
			vec2 ab = b.xy - a.xy;
			vec2 ap = p - a.xy;
			float t = clamp(dot(ap, ab) / max(dot(ab, ab), 1.0e-6), 0.0, 1.0);
			vec2 off = ap - ab * t;
			float d2 = dot(off, off);
			if (d2 < best) {
				best = d2;
				bw = w;
				bi = i;
				bt = t;
			}
		}
	}
	tr = vec4(0.0, 0.0, 1.0e4, 0.0);
	tb = vec4(1.0, 0.0, 1.0, 0.0);
	if (bw < 0) {
		return;
	}
	vec4 a = wake_points[16 * bw + bi];
	vec4 b = wake_points[16 * bw + bi + 1];
	vec2 dir = normalize(b.xy - a.xy + vec2(1.0e-6, 0.0));
	vec2 across = vec2(-dir.y, dir.x);
	// Signed distance across the segment's line (linear in p), and age and distance along it.
	float side = dot(p - a.xy, across);
	float along = mix(a.w, b.w, bt);
	tr = vec4(mix(a.z, b.z, bt), along, side, (b.w - a.w) / max(b.z - a.z, 0.05));
	// Ahead of the bow the hull has not passed yet: no wake there.
	if (bi == 0 && bt <= 0.0) {
		tr.x = -1.0;
	}
	tb = vec4(across, wake_info[3 * bw].z, wake_info[3 * bw + 1].x - along);
}

void vertex() {
	float s = max(CUSTOM0.x, 1.0e-3);
	float w = CUSTOM0.y;
	// hm-ocean's seam rule: odd grid lines slide to the lower even line across the morph band, in level-local
	// coordinates; the skirt is never snapped. Every spacing-dependent term uses s' = s (1 + w).
	vec2 lp = VERTEX.xz;
	lp -= mod(lp / s, 2.0) * s * w * (1.0 - CUSTOM0.w);
	float se = s * (1.0 + w);
	vec3 origin = MODEL_MATRIX[3].xyz;
	vec2 p0 = lp + origin.xz;
	float dt = bp_waves_dt(TIME);

	vec3 o = vec3(0.0);
	WATER_DISPLACE(bp_waves_0)
	WATER_DISPLACE(bp_waves_1)
	WATER_DISPLACE(bp_waves_2)
	WATER_DISPLACE(bp_waves_3)
	WATER_DISPLACE(bp_waves_4)
	WATER_DISPLACE(bp_waves_5)
	WATER_DISPLACE(bp_waves_6)
	WATER_DISPLACE(bp_waves_7)

	vec3 j1 = vec3(1.0, 0.0, 1.0);
	vec3 j2 = vec3(1.0, 0.0, 1.0);
	float variance = 0.0;
	if (sea_state > WHITECAP_FROM_SEA_STATE - 0.5) {
		WATER_LAG(bp_waves_0)
		WATER_LAG(bp_waves_1)
		WATER_LAG(bp_waves_2)
		WATER_LAG(bp_waves_3)
		WATER_LAG(bp_waves_4)
		WATER_LAG(bp_waves_5)
		WATER_LAG(bp_waves_6)
		WATER_LAG(bp_waves_7)
	}

	VERTEX = vec3(lp.x + o.x, o.y, lp.y + o.z);
	NORMAL = vec3(0.0, 1.0, 0.0);
	v_p0 = p0;
	v_world = vec3(p0.x + o.x, origin.y + o.y, p0.y + o.z);
	v_surf = vec4(j1.x * j1.z - j1.y * j1.y, j2.x * j2.z - j2.y * j2.y, sqrt(max(variance, 1.0e-8)), o.y);
	v_wake = vec4(0.0, 0.0, 1.0e4, 0.0);
	v_wake_b = vec4(1.0, 0.0, 1.0, 0.0);
	v_kelvin = vec2(0.0);
	v_kelvin_w = -1;
	if (wake_count > 0) {
		water_track(p0, v_wake, v_wake_b);
		// The first hull (nearest the camera) whose arms' bounds hold the vertex, if the vertex is near a crest.
		for (int w = 0; w < wake_count && w < 4; w++) {
			vec4 ex = wake_info[3 * w + 1];
			vec4 kb = wake_info[3 * w + 2];
			if (ex.y > 0.0 && all(greaterThanEqual(p0, kb.xy)) && all(lessThanEqual(p0, kb.zw))) {
				vec4 hd = wake_info[3 * w];
				vec2 d = p0 - hd.xy;
				vec2 sl = vec2(-dot(d, ex.zw), dot(d, vec2(-ex.w, ex.z)));
				// Only near a crest line: within its fade (2.6 crest widths) and two grid spacings, so every pixel
				// of a triangle that touches the crest gets it from whichever vertex leads.
				float edge = abs(abs(sl.y) - 0.5 * hd.z - max(sl.x, 0.0) * KELVIN_TAN);
				// The position is always written, so a triangle that holds a crest interpolates it right.
				v_kelvin = sl;
				if (sl.x > -2.0 * se && edge < 2.6 * (0.8 + 0.02 * max(sl.x, 0.0)) + 2.0 * se + 1.0) {
					v_kelvin_w = w;
				}
				break;
			}
		}
	}
}

// Two looping frames of a detail array, blended.
vec4 water_detail(sampler2DArray tex, vec2 uv, vec2 gx, vec2 gy, float frame, float layers) {
	float f0 = floor(frame);
	float f1 = mod(f0 + 1.0, layers);
	return mix(textureGrad(tex, vec3(uv, f0), gx, gy), textureGrad(tex, vec3(uv, f1), gx, gy), frame - f0);
}

// Surface slope (dh/dx, dh/dz) from a detail sample's stored normal.
vec2 water_detail_slope(vec2 rg) {
	vec2 n = rg * 2.0 - 1.0;
	return -n / sqrt(max(1.0 - dot(n, n), 0.08));
}

// Share of a tiling's slope variance that mip filtering has averaged away.
float water_detail_lost(vec2 gx, vec2 gy) {
	float texels = max(length(gx), length(gy)) * DETAIL_SIZE;
	return clamp(log2(max(texels, 1.0)) / 5.0, 0.0, 1.0);
}

// Hash and value noise on a lattice, for the wake's and the whitecaps' foam streaks.
float water_hash(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

float water_noise(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	return mix(mix(water_hash(i), water_hash(i + vec2(1.0, 0.0)), f.x), mix(water_hash(i + vec2(0.0, 1.0)), water_hash(i + vec2(1.0, 1.0)), f.x), f.y);
}

// Foam cover of a wake's ribbon at a place on its track (water_track's x, y, z, w; beam in metres): x = the
// white churn behind the stern, y = the lace it leaves over the disturbed band, z = the band's strength (the
// scar). Nothing at rest; everything grows with speed.
vec3 water_wake_cover(vec4 tr, float beam) {
	float age = tr.x;
	float spd = tr.w;
	if (age <= 0.0 || age >= WAKE_LIFE_S || spd < 0.3) {
		return vec3(0.0);
	}
	float sf = clamp(spd / WAKE_FULL_MS, 0.0, 1.0);
	float on = smoothstep(0.3, 2.5, spd);
	float u = age / WAKE_LIFE_S;
	// Half the ribbon width: one beam behind the stern, spreading to 1.0 to 2.5 beams by speed as it ages.
	float hw = 0.5 * beam * (1.0 + (mix(1.0, 2.5, sf) - 1.0) * sqrt(u));
	float x = abs(tr.z) / max(hw, 0.5);
	if (x >= 1.05) {
		return vec3(0.0);
	}
	float behind = on * (1.0 - smoothstep(0.75, 1.0, u));
	float churn = behind * exp(-age / (3.0 + 6.0 * sf)) * (1.0 - smoothstep(0.2, 0.55 + 0.25 * sf, x));
	float lace = behind * (0.25 + 0.45 * sf) * pow(1.0 - u, 1.4) * (1.0 - smoothstep(0.3, 0.95, x));
	float scar = behind * (0.45 + 0.55 * sf) * (1.0 - smoothstep(0.6, 1.0, u)) * (1.0 - smoothstep(0.55, 1.05, x));
	return vec3(churn, lace, scar);
}

// A Kelvin arm (L1967: 19.47 degrees, above 8 kn) at `sb` metres aft of the bow and `lat` metres off the hull's
// line: 0..1 on the crest, which runs from the bow's shoulders and fades over several hull lengths at speed.
float water_kelvin(float sb, float lat, float beam, float len, float spd) {
	float sf = clamp(spd / WAKE_FULL_MS, 0.0, 1.0);
	float reach = len * (1.5 + 4.5 * sf);
	if (sb <= 0.0 || sb >= reach) {
		return 0.0;
	}
	float off = (abs(lat) - 0.5 * beam - sb * KELVIN_TAN) / (0.8 + 0.02 * sb);
	if (abs(off) > 2.6) {
		return 0.0;
	}
	return smoothstep(KELVIN_MIN_MS, KELVIN_MIN_MS * 1.6, spd) * (1.0 - smoothstep(0.3 * reach, reach, sb)) * exp(-off * off);
}

void fragment() {
	vec3 to_camera = CAMERA_POSITION_WORLD - v_world;
	float dist = length(to_camera);
	vec3 view_dir = to_camera / max(dist, 1.0e-3);

	// Foam lattice: inside a cell every pixel evaluates foam at the cell centre; across one pixel at a cell
	// edge the sample point slides to the next centre, which antialiases the edge. Beyond 200 m the cells
	// shrink to the pixel and the snap is off.
	vec2 cell = v_world.xz / FOAM_CELL_M;
	float unsnap = smoothstep(FOAM_SNAP_FULL_M, FOAM_SNAP_END_M, dist);
	vec2 cell_w = mix(clamp(fwidth(cell), vec2(1.0e-4), vec2(1.0)), vec2(1.0), unsnap);
	vec2 cell_f = fract(cell);
	vec2 cell_s = clamp(cell_f / cell_w, 0.0, 0.5) + clamp((cell_f - 1.0) / cell_w + 0.5, 0.0, 0.5);
	vec2 snap = (cell_s - cell_f) * FOAM_CELL_M;
	// The turbulence differs from cell to cell, so it is read at this cell's centre and, inside the edge
	// pixel, at the neighbour's across the nearer edge; the two are blended by the pixel's cover.
	vec2 cell_e = cell_s - 0.5;
	vec2 centre = (0.5 - cell_f) * FOAM_CELL_M;
	vec2 across = abs(cell_e.x) > abs(cell_e.y) ? vec2(sign(cell_e.x), 0.0) : vec2(0.0, sign(cell_e.y));
	float edge_cover = max(abs(cell_e.x), abs(cell_e.y));

	// Hull columns: no water inside a dry hull. The waterline distance is read at the foam cell.
	float outline_m = 1.0e3;
	for (int i = 0; i < hull_count && i < 8; i++) {
		vec3 hp = (hull_mask_from_render[i] * vec4(v_world, 1.0)).xyz;
		vec2 texel = hp.xz / HULL_MASK_TEXEL_M + vec2(0.0, HULL_MASK_CENTRE_V);
		if (all(greaterThanEqual(texel, vec2(0.0))) && all(lessThan(texel, HULL_MASK_SIZE))) {
			vec4 column = texelFetch(hull_masks, ivec3(ivec2(texel), i), 0);
			if (column.a > 0.5 && hp.y >= column.r && hp.y <= column.g) {
				discard;
			}
		}
		vec2 texel_s = texel + (hull_mask_from_render[i][0].xz * snap.x + hull_mask_from_render[i][2].xz * snap.y) / HULL_MASK_TEXEL_M;
		vec2 muv = texel_s / HULL_MASK_SIZE;
		if (all(greaterThan(muv, vec2(0.0))) && all(lessThan(muv, vec2(1.0)))) {
			outline_m = min(outline_m, textureLod(hull_masks, vec3(muv, float(i)), 0.0).b);
		}
	}

	// Wakes (L1967), drawn by the sea itself so they light and move with the waves: each hull's track is the
	// ribbon's centre line. The foam is read at the foam cell, the disturbance and the Kelvin crests at the pixel.
	float wake_foam = 0.0;
	float scar = 0.0;
	vec2 wake_slope = vec2(0.0);
	if (wake_count > 0) {
		float churn_c = 0.0;
		float lace_c = 0.0;
		float arm_c = 0.0;
		vec2 streak_at = vec2(0.0);
		// Kelvin arms, from each bow along its heading: a raised crest whose slope across it lights it like a
		// wave, and near the bow, where it breaks, the bow wave's foam running out along it.
		if (v_kelvin_w >= 0) {
			int w = v_kelvin_w;
			vec4 ex = wake_info[3 * w + 1];
			vec4 hd = wake_info[3 * w];
			vec2 right = vec2(-ex.w, ex.z);
			float sb = v_kelvin.x;
			float lat = v_kelvin.y;
			float crest = water_kelvin(sb, lat, hd.z, hd.w, ex.y);
			if (crest > 0.002) {
				float th = 0.8 + 0.02 * sb;
				float off = (abs(lat) - 0.5 * hd.z - sb * KELVIN_TAN) / th;
				float crest_m = min(0.3, 0.04 * hd.z * clamp(ex.y / WAKE_FULL_MS, 0.0, 1.0));
				wake_slope += (-2.0 * off * crest * crest_m / th) * sign(lat) * right;
				scar = max(scar, 0.6 * crest);
				float sb_c = sb - dot(snap, ex.zw);
				float lat_c = lat + dot(snap, right);
				arm_c = max(arm_c, water_kelvin(sb_c, lat_c, hd.z, hd.w, ex.y) * (1.0 - smoothstep(0.2 * hd.w, 1.4 * hd.w, sb_c)));
				streak_at = vec2(lat_c, ex.x - (sb_c - hd.w));
			}
		}
		// The ribbon, from the nearest track the vertices found: churn and lace at the foam cell, the smoothed
		// band at the pixel.
		vec3 cv = vec3(0.0);
		if (v_wake.x > 0.0 && v_wake.x < WAKE_LIFE_S && abs(v_wake.z) < 1.3 * v_wake_b.z + 0.6) {
			cv = water_wake_cover(v_wake, v_wake_b.z);
		}
		if (max(cv.x, max(cv.y, cv.z)) > 0.002) {
			scar = max(scar, cv.z);
			// The same place at the foam cell's centre: the cell moves along and across the track.
			vec2 along_dir = vec2(v_wake_b.y, -v_wake_b.x);
			float da = dot(snap, along_dir);
			vec4 tc = v_wake + vec4(da / max(v_wake.w, 0.5), da, dot(snap, v_wake_b.xy), 0.0);
			vec3 cc = water_wake_cover(tc, v_wake_b.z);
			churn_c = cc.x;
			lace_c = cc.y;
			// Streaks run along the track and stay with the water.
			streak_at = vec2(tc.z, v_wake_b.w - da);
		}
		if (max(churn_c, max(lace_c, arm_c)) > 0.002) {
			vec2 cellc = floor((v_world.xz + snap) / FOAM_CELL_M);
			float n = 0.6 * water_noise(vec2(streak_at.x * 0.8, streak_at.y * 0.06)) + 0.4 * water_noise(cellc * 0.37 + 7.0);
			n = clamp((n - 0.5) * 2.2 + 0.5, 0.0, 1.0);
			// A cell is foam where the cover beats its noise; just short of it, one tone lighter (the frayed edge).
			float churn = churn_c > n ? 1.0 : (churn_c > max(n - 0.18, 0.02) ? 2.0 / 3.0 : 0.0);
			float lace = lace_c > n ? 2.0 / 3.0 : (lace_c > max(n - 0.12, 0.02) ? 1.0 / 3.0 : 0.0);
			float arm = arm_c > n ? 2.0 / 3.0 : (arm_c > max(n - 0.15, 0.02) ? 1.0 / 3.0 : 0.0);
			wake_foam = max(churn, max(lace, arm));
		}
	}

	// The Gerstner set at this pixel: smooth normal, current Jacobian, and what the pixel is too coarse to show.
	float dt = bp_waves_dt(TIME);
	vec2 fx = dFdx(v_p0);
	vec2 fy = dFdy(v_p0);
	vec3 jac = vec3(1.0, 0.0, 1.0);
	vec2 slope = vec2(0.0);
	vec2 grad = vec2(0.0);
	float lost = 0.0;
	float seen = 0.0;
	WATER_SHADE(bp_waves_0)
	WATER_SHADE(bp_waves_1)
	WATER_SHADE(bp_waves_2)
	WATER_SHADE(bp_waves_3)
	WATER_SHADE(bp_waves_4)
	WATER_SHADE(bp_waves_5)
	WATER_SHADE(bp_waves_6)
	WATER_SHADE(bp_waves_7)
	float det = jac.x * jac.z - jac.y * jac.y;
	// The include's wave_normal, written as the slope (dh/dx, dh/dz) so the detail can be added to it.
	vec2 swell_slope = -vec2(jac.z * slope.x - jac.y * slope.y, jac.x * slope.y - jac.y * slope.x) / max(det, 0.05);
	vec2 surface_slope = swell_slope + wake_slope;

	// Baked detail: two tilings of looping normals, and turbulence for the foam at the foam cell.
	float turbulence = 0.5;
	float gusts = 0.5;
	// Lace: the fine tiling's turbulence at the foam cell, which eats holes into a breaker's foam so a wide
	// fold of a long wave leaves streaks and lace, not one slab of cells. Its sample is set up here and read
	// only where a whitecap is drawn (a few percent of the sea).
	float lace = 1.0;
	bool lace_on = false;
	vec2 uv_l = vec2(0.0);
	vec2 gl_x = vec2(0.0);
	vec2 gl_y = vec2(0.0);
	float lace_frame = 0.0;
	float lace_layers = 1.0;
	if (detail_strength > 0.0) {
		float layers = float(textureSize(detail_normals, 0).z);
		float frame = fract(detail_phase + dt / DETAIL_LOOP_S) * layers;
		vec2 uv_a = v_p0 / DETAIL_SCALE_A_M + detail_origin.xy;
		vec2 ga_x = fx / DETAIL_SCALE_A_M;
		vec2 ga_y = fy / DETAIL_SCALE_A_M;
		vec2 detail_slope = water_detail_slope(water_detail(detail_normals, uv_a, ga_x, ga_y, frame, layers).rg);
		lost += DETAIL_SLOPE_VARIANCE * detail_strength * detail_strength * water_detail_lost(ga_x, ga_y);
		if (detail_tilings > 1) {
			vec2 uv_b = v_p0 / DETAIL_SCALE_B_M + detail_origin.zw;
			vec2 gb_x = fx / DETAIL_SCALE_B_M;
			vec2 gb_y = fy / DETAIL_SCALE_B_M;
			float frame_b = mod(frame + 0.5 * layers, layers);
			detail_slope += water_detail_slope(water_detail(detail_normals, uv_b, gb_x, gb_y, frame_b, layers).rg);
			lost += DETAIL_SLOPE_VARIANCE * detail_strength * detail_strength * water_detail_lost(gb_x, gb_y);
		}
		// A wake's disturbed band is smoothed: its bubbles damp the ripples.
		surface_slope += detail_slope * (detail_strength * (1.0 - 0.75 * scar));
		float t_layers = float(textureSize(detail_turbulence, 0).z);
		float t_frame = fract(detail_phase + dt / DETAIL_LOOP_S) * t_layers;
		// Read through a footprint of a few cells, so neighbouring cells agree and foam breaks up in clumps.
		vec2 gt_x = ga_x + vec2(FOAM_CLUMP_M / DETAIL_SCALE_A_M, 0.0);
		vec2 gt_y = ga_y + vec2(0.0, FOAM_CLUMP_M / DETAIL_SCALE_A_M);
		turbulence = water_detail(detail_turbulence, uv_a + centre / DETAIL_SCALE_A_M, gt_x, gt_y, t_frame, t_layers).r;
		if (edge_cover > 0.0) {
			vec2 uv_n = uv_a + (centre + across * FOAM_CELL_M) / DETAIL_SCALE_A_M;
			turbulence = mix(turbulence, water_detail(detail_turbulence, uv_n, gt_x, gt_y, t_frame, t_layers).r, edge_cover);
		}
		if (sea_state > WHITECAP_FROM_SEA_STATE - 0.5) {
			gusts = water_detail(detail_turbulence, uv_a / GUST_SCALE, ga_x / GUST_SCALE, ga_y / GUST_SCALE, mod(t_frame + 0.25 * t_layers, t_layers), t_layers).r;
			if (detail_tilings > 1) {
				lace_on = true;
				uv_l = (v_p0 + centre) / DETAIL_SCALE_B_M + detail_origin.zw;
				gl_x = fx / DETAIL_SCALE_B_M + vec2(FOAM_CELL_M / DETAIL_SCALE_B_M, 0.0);
				gl_y = fy / DETAIL_SCALE_B_M + vec2(0.0, FOAM_CELL_M / DETAIL_SCALE_B_M);
				lace_frame = mod(t_frame + 0.5 * t_layers, t_layers);
				lace_layers = t_layers;
			}
		}
	}
	float ragged = (turbulence - 0.5) * 2.0;

	// Whitecaps: where the surface folds now, and where it folded 0.6 s and 1.2 s ago (L1907).
	float whitecaps = 0.0;
	float caps_on = smoothstep(WHITECAP_FROM_SEA_STATE - 0.5, WHITECAP_FROM_SEA_STATE, sea_state);
	if (caps_on > 0.0) {
		float sigma = v_surf.z;
		// Screen gradient of the lagged determinants, moved to the foam cell like the current one.
		mat2 screen = mat2(dFdx(v_world.xz), dFdy(v_world.xz));
		float screen_det = screen[0].x * screen[1].y - screen[0].y * screen[1].x;
		vec2 shift = vec2(0.0);
		if (abs(screen_det) > 1.0e-9) {
			shift = vec2(screen[1].y * snap.x - screen[1].x * snap.y, screen[0].x * snap.y - screen[0].y * snap.x) / screen_det;
		}
		vec2 lagged = v_surf.xy + dFdx(v_surf.xy) * shift.x + dFdy(v_surf.xy) * shift.y;
		float z0 = (1.0 - det - dot(grad, snap)) / sigma;
		float z1 = (1.0 - lagged.x) / sigma;
		float z2 = (1.0 - lagged.y) / sigma;
		// Threshold in standard deviations of the determinant: the fold passes it over about 3% of the sea at
		// state 3 and 20% at state 8; solid foam covers about a third of that and lace a quarter more.
		float rough = clamp((sea_state - WHITECAP_FROM_SEA_STATE) / 5.0, 0.0, 1.0);
		float level = mix(1.9, 0.85, rough);
		// Gusts: the same turbulence read at a 300 m scale moves the threshold, so breakers come in patches.
		level += (0.5 - gusts) * WHITECAP_GUST_SHIFT;
		// The fold masks times the baked turbulence (L1907). A fresh breaker is a solid core on the crest; the
		// foam it leaves behind is the same fold 0.6 s and 1.2 s ago, and the turbulence eats holes into it,
		// more the older it is, so a whitecap trails lace and never spreads into a sheet.
		// The core is not one slab either: where the turbulence is low its cells drop a tone. In a heavy sea the
		// fold is tens of metres wide, so the core keeps to its steepest part and the rest is left as lace.
		// Streaks along the wind: foam stays on the water while the crest runs on, so a breaker leaves short
		// streaks downwind and its front is ragged. A noise stretched about 6 to 1 down the wind (cells about
		// 0.8 m across and 5 m along) pushes the fold's threshold up and down, so the foam's outline follows
		// the streaks instead of the fold's round contour.
		// The streaks and the lace are read only near a fold (a few percent of the sea): elsewhere no shift of
		// the threshold by them can make foam.
		float core = level + mix(0.4, 0.95, rough);
		// Without streaks (Low and Fallback) the foam keeps the fold's own outline: k = 0 gives the plain fold
		// thresholds.
		float k = foam_streaks ? 1.0 : 0.0;
		float streak = 0.0;
		if (max(z0 + ragged * 0.5 + 0.35 * k - core, max(z1 + 0.55 * k - level - 0.15 * (1.0 - k), z2 + 0.7 * k - level)) > 0.0) {
			if (foam_streaks) {
				vec2 cell_c = floor((v_world.xz + snap) / FOAM_CELL_M) + 0.5;
				vec2 cross_wind = vec2(-wind_dir.y, wind_dir.x);
				streak = clamp((water_noise(vec2(dot(cell_c, cross_wind) * 0.62, dot(cell_c, wind_dir) * 0.1)) - 0.5) * 2.4, -1.0, 1.0);
			}
			// The lace texture only where a breaker's foam is drawn after the streak's shift.
			if (lace_on && max(z0 + ragged * 0.5 + streak * 0.35 - core, max(z1 + streak * 0.55 - level - 0.15 * (1.0 - k), z2 + streak * 0.6 - level + 0.1 * k)) > 0.0) {
				lace = smoothstep(0.4, 0.6, water_detail(detail_turbulence, uv_l, gl_x, gl_y, lace_frame, lace_layers).r);
			}
		}
		float fresh = smoothstep(core, core + 0.3, z0 + ragged * 0.5 + streak * 0.35) * (1.0 - WHITECAP_CORE_BREAK * (1.0 - smoothstep(-0.2, 0.1, ragged))) * mix(1.0, lace, 0.6);
		// What the breaker left behind keeps only the streaks: cells off a streak drop out, so the trailing foam
		// is a comb of short lines down the wind rather than a rounded patch.
		float trail_1 = smoothstep(level + 0.15 * (1.0 - k), level + 0.45 - 0.15 * k, z1 + streak * 0.55) * mix(1.0, step(-0.15, streak), k) * smoothstep(-0.2, 0.1, ragged) * lace;
		float trail_2 = smoothstep(level - 0.1 * k, level + 0.3 - 0.1 * k, z2 + streak * 0.6) * mix(1.0, step(0.25, streak), k) * smoothstep(0.05, 0.35, ragged) * lace * lace;
		whitecaps = max(fresh, max(trail_1 * WHITECAP_TRAIL_1, trail_2 * WHITECAP_TRAIL_2));
		// Past the distance where pixels resolve the crests, the same mean cover as an even tint: the share of
		// the sea past the threshold (a fit to the normal tail), times the mean tone of the three layers.
		float resolved = clamp(seen / max(sigma * sigma, 1.0e-8), 0.0, 1.0);
		float coverage = 0.5 * exp(-(0.717 * level + 0.416 * level * level));
		whitecaps = mix(coverage * WHITECAP_MEAN_TONE, whitecaps, resolved) * caps_on;
		// In a slight sea a breaker is a thin film a step below full white (two thirds after the snap, its lace
		// one third), so the hull's own wake stays the brightest water (L1780); by state 8 it is full white.
		whitecaps *= mix(WHITECAP_SLIGHT_PEAK, 1.0, rough);
	}

	// Contact foam from the waterline distance: a ragged band one to three cells wide.
	float contact = 1.0 - smoothstep(0.5, 1.7 + ragged * 1.0, outline_m);

	// Shore: shallow tint and a wash line that the waves push up and down the beach.
	float shallow = 0.0;
	float wash = 0.0;
	if (shore_enabled) {
		vec2 suv = (v_world.xz + snap - shore_window.xy) * shore_window.z;
		if (all(greaterThan(suv, vec2(0.0))) && all(lessThan(suv, vec2(1.0)))) {
			float ground = textureLod(shore_height, suv, 0.0).r * shore_decode.x + shore_decode.y;
			float depth = v_world.y - ground;
			shallow = exp(-max(depth, 0.0) / 5.0);
			float edge = 1.0 - smoothstep(0.1, 0.8 + ragged * 0.4, depth);
			float lace = (1.0 - smoothstep(0.5, 1.2, abs(depth - 2.0 - ragged))) * 0.45;
			wash = max(edge, lace);
		}
	}

	float foam = clamp(max(max(whitecaps, wake_foam), max(contact, wash)), 0.0, 1.0);
	// Inside the lattice a cell is one of four tones, so foam reads as built from cells, not as a blur.
	foam = mix(floor(foam * 3.0 + 0.5) / 3.0, foam, unsnap);

	// Smooth normal (L1905), kept facing the eye so a wave's far slope never reflects the underside.
	v_swell = normalize((VIEW_MATRIX * vec4(-swell_slope.x, 1.0, -swell_slope.y, 0.0)).xyz);
	vec3 n = normalize(vec3(-surface_slope.x, 1.0, -surface_slope.y));
	float facing = dot(n, view_dir);
	if (facing < 0.03) {
		n = normalize(n + view_dir * (0.03 - facing));
	}
	// A rough sea seen at a low angle shows the eye its near-facing slopes: lean the mean normal that way by
	// the slope spread the pixel hides, so the far sea mirrors the sky above the horizon band.
	n = normalize(n + vec3(view_dir.x, 0.0, view_dir.z) * min(sqrt(lost) * 0.6, 0.2));
	NORMAL = normalize((VIEW_MATRIX * vec4(n, 0.0)).xyz);

	// What the pixel cannot resolve becomes roughness (alpha squared = 2 x slope variance, split over two axes).
	float base = BASE_ALPHA + 0.12 * rain;
	float alpha = sqrt(min(base * base + lost, 0.2));
	float crest = 1.0 - exp(-max(v_surf.w, 0.0) / 1.2);
	// Under the sky alone the body is the deep colour, lifting towards the scatter colour in the crests.
	vec3 body = mix(mix(deep_color, scatter_color, 0.3 + 0.5 * crest), shallow_color, shallow);
	// Bubbles in a wake's band lift the water a little towards the foam's colour.
	body = mix(body, mix(scatter_color, foam_color, 0.35), 0.55 * scar);
	ALBEDO = mix(body, foam_color, foam);
	ROUGHNESS = mix(sqrt(alpha), 0.9, foam);
	METALLIC = 0.0;
	// Godot's F0 is 0.16 x SPECULAR squared: 0.3536 gives water's 0.02.
	SPECULAR = sqrt(WATER_F0 / 0.16) * (1.0 - foam);

	v_foam = foam;
	v_sun = bp_cloud_light(v_world.xz);
	v_veil = smoothstep(VEIL_FROM_COVER, 1.0, bp_cloud_shadow_xf.w);
	v_crest = crest;
	v_scar = scar;
}

// Smith Lambda, Walter's rational fit, for a direction at cosine c to the normal.
float water_lambda(float c, float alpha) {
	float a = c / (alpha * sqrt(max(1.0 - c * c, 1.0e-6)));
	return a < 1.6 ? (1.0 - 1.259 * a + 0.396 * a * a) / (3.535 * a + 2.181 * a * a) : 0.0;
}

// Atlas water lighting (L1905): GGX with Walter's Smith term, roughness-aware Fresnel, four-term scatter.
// Scatter goes out through SPECULAR_LIGHT because Godot multiplies DIFFUSE_LIGHT by ALBEDO afterwards;
// only foam is lit as a diffuse surface.
void light() {
	float nl = dot(NORMAL, LIGHT);
	float nl_pos = max(nl, 0.0);
	float nv = clamp(dot(NORMAL, VIEW), 1.0e-3, 1.0);
	vec3 half_dir = normalize(LIGHT + VIEW + vec3(1.0e-5));
	float nh = max(dot(NORMAL, half_dir), 0.0);
	float alpha = max(ROUGHNESS * ROUGHNESS, SUN_ROUGHNESS_FLOOR);
	if (LIGHT_IS_DIRECTIONAL) {
		alpha = max(alpha, mix(SUN_ROUGHNESS_FLOOR, VEILED_SUN_ALPHA, v_veil));
	}
	float a2 = alpha * alpha;
	float lit = ATTENUATION * (LIGHT_IS_DIRECTIONAL ? v_sun : 1.0);
	float water = 1.0 - v_foam;

	float fresnel = WATER_F0 + (1.0 - WATER_F0) * pow(1.0 - nv, 5.0 * exp(-2.69 * alpha)) / (1.0 + 22.7 * pow(alpha, 1.5));
	float lambda_l = water_lambda(max(nl_pos, 1.0e-3), alpha);
	float lambda_v = water_lambda(nv, alpha);
	float d = nh * nh * (a2 - 1.0) + 1.0;
	float ggx = a2 / (PI_F * d * d);
	// F D G / (4 N.L N.V) x N.L, with the light's colour carrying Godot's factor of pi.
	float specular = nl > 0.0 ? fresnel * ggx / ((1.0 + lambda_l + lambda_v) * 4.0 * nv) : 0.0;

	// The body is lit through the swell's normal, wrapped: under the surface the light has already spread.
	float nl_swell = dot(v_swell, LIGHT);
	float nl_body = max((nl_swell + 0.3) / 1.3, 0.0);
	float lambda_body = water_lambda(max(nl_body, 1.0e-3), alpha);
	float through = pow(max(dot(LIGHT, -VIEW), 0.0), 4.0) * pow(0.5 - 0.5 * nl_swell, 3.0);
	float scatter = (SCATTER_HEIGHT * v_crest * through + SCATTER_VIEW * nv * nv) / (1.0 + lambda_body) + SCATTER_LAMBERT * nl_body;
	vec3 body = scatter_color * scatter + foam_color * (SCATTER_BUBBLE * v_foam * nl_pos) + mix(scatter_color, foam_color, 0.35) * (1.5 * SCATTER_BUBBLE * v_scar * nl_body);

	// Light under the surface spreads sideways, so a hull's shadow dims the body only part way; the glint and
	// the foam keep the full shadow.
	float lit_body = LIGHT_IS_DIRECTIONAL ? v_sun * mix(BODY_SHADOW_FLOOR, 1.0, ATTENUATION) : ATTENUATION;
	vec3 glint = LIGHT_COLOR * (water * lit * specular * SPECULAR_AMOUNT);
	if (LIGHT_IS_DIRECTIONAL) {
		// Roll the glint off above the knee, keeping its hue: each facet still sparkles, none clips to a slab.
		float peak = max(glint.r, max(glint.g, glint.b));
		if (peak > GLINT_KNEE) {
			float room = GLINT_PEAK - GLINT_KNEE;
			glint *= (GLINT_KNEE + room * (1.0 - exp(-(peak - GLINT_KNEE) / room))) / peak;
		}
	}
	SPECULAR_LIGHT += glint + LIGHT_COLOR * (water * lit_body * (1.0 - fresnel) / PI_F) * body;
	// Foam: a matte surface with a little wrap, so it stays readable on a wave's shaded side.
	DIFFUSE_LIGHT += LIGHT_COLOR * lit * v_foam * max((nl + 0.3) / 1.3, 0.0) / PI_F;
}
