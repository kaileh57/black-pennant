// Sky: atmosphere, sun disc, stars, moon, banded cirrus, overcast deck. DESIGN.md L1913-1915. Unit: shader-sky.
//
// Reads no TIME and no POSITION, so the radiance map is redrawn only when a parameter or a light changes
// (L1913): on a preset change, and when gd-render-scenery steps cirrus_offset (at most every 2 s, L1915).
// The sun is LIGHT0 and the moon LIGHT1 (DirectionalLight3D sky_mode "light and sky"); their direction,
// colour, energy and size come from the lights. The disc, stars and moon are left out of the radiance map,
// so they never double the water's own sun highlight.
//
// What the environment must do for this shader:
// - Create the sun before the moon and never hide it: at night it stays in the tree with energy 0 and
//   sky_mode "sky only", or the moon would become LIGHT0.
// - Set Environment.fog_sky_affect to 0. The shader draws its own haze; depth fog over the sky replaces
//   the whole gradient with the fog colour.
//
// Material parameters (set from the preset by res://world/environment.gd; colours are world tokens):
//   zenith_color, horizon_color   sky.<time>.zenith and .horizon. The horizon equals the preset's fog colour,
//                                 so the fogged sea meets the sky without a seam.
//   ground_color     what lies below the horizon. Only reflections and ambient light see it: sea.deep
//                    lifted towards the horizon colour is the intended value.
//   overcast_color   the preset's fog token; the colour of a full cloud layer.
//   cirrus_color     high cloud lit by the sun: sky.<time>.key_light scaled to taste.
//   energy           float  overall sky brightness.
//   horizon_curve    float  how tightly the horizon colour hugs the horizon (larger = thinner band).
//   sun_glow         float  strength of the warm scatter around the sun.
//   sun_disc         float  disc brightness as a multiple of the light's colour x energy. 0 hides the disc.
//   overcast         float  0 clear .. 1 full layer: hides disc, stars and cirrus behind overcast_color.
//   star_energy      float  0 by day and dusk.
//   moon_phase       float  0 new, 0.5 full, 1 new again.
//   cirrus_cover     float  0..1.
//   cirrus_offset    vec2   drift in cirrus-plane units (1 = the layer's height), stepped, never animated.
//   cirrus_dir       vec2   unit wind direction in world xz; streaks run along it.
//   rain             float  0..1, draws rain shafts under the deck.
//   lightning        vec4   xyz = direction of the flash, w = its brightness (0 = none).
shader_type sky;

uniform vec3 zenith_color : source_color;
uniform vec3 horizon_color : source_color;
uniform vec3 ground_color : source_color;
uniform vec3 overcast_color : source_color;
uniform vec3 cirrus_color : source_color;
uniform float energy = 1.0;
uniform float horizon_curve = 7.0;
uniform float sun_glow = 1.0;
uniform float sun_disc = 8.0;
uniform float overcast = 0.0;
uniform float star_energy = 0.0;
uniform float moon_phase = 0.5;
uniform float cirrus_cover = 0.0;
uniform vec2 cirrus_offset = vec2(0.0);
uniform vec2 cirrus_dir = vec2(1.0, 0.0);
uniform float rain = 0.0;
uniform vec4 lightning = vec4(0.0);

// Angular radius of the real sun and moon, radians; lights with no size set fall back to it.
const float DISC_RADIUS = 0.00465;
// One pixel at 1080p and 60 degrees, radians: the width of every disc edge.
const float EDGE = 0.001;
const float STAR_CELLS = 44.0;
const float STAR_DENSITY = 0.22;
// Rows of streaks per unit of the cloud plane (1 unit = the layer's height): thin for cirrus, broad rolls
// for the overcast deck.
const float CIRRUS_ROWS = 12.0;
const float DECK_ROWS = 7.0;
// Value step between the darkest and the lightest roll of the deck.
const float DECK_CONTRAST = 0.08;

// Linear RGB to OKLab and back (Ottosson 2020), so the dusk gradient runs from blue to orange through a
// clean pale band, not the brown a straight RGB blend gives.
const mat3 LMS_FROM_LINEAR = mat3(vec3(0.4122214708, 0.2119034982, 0.0883024619), vec3(0.5363325363, 0.6806995451, 0.2817188376), vec3(0.0514459929, 0.1073969566, 0.6299787005));
const mat3 OKLAB_FROM_LMS = mat3(vec3(0.2104542553, 1.9779984951, 0.0259040371), vec3(0.7936177850, -2.4285922050, 0.7827717662), vec3(-0.0040720468, 0.4505937099, -0.8086757660));
const mat3 LMS_FROM_OKLAB = mat3(vec3(1.0, 1.0, 1.0), vec3(0.3963377774, -0.1055613458, -0.0894841775), vec3(0.2158037573, -0.0638541728, -1.2914855480));
const mat3 LINEAR_FROM_LMS = mat3(vec3(4.0767416621, -1.2684380046, -0.0041960863), vec3(-3.3077115913, 2.6097574011, -0.7034186147), vec3(0.2309699292, -0.3413193965, 1.7076147010));

vec3 sky_blend(vec3 from, vec3 to, float t) {
	vec3 a = OKLAB_FROM_LMS * pow(max(LMS_FROM_LINEAR * from, vec3(0.0)), vec3(1.0 / 3.0));
	vec3 b = OKLAB_FROM_LMS * pow(max(LMS_FROM_LINEAR * to, vec3(0.0)), vec3(1.0 / 3.0));
	vec3 lms = LMS_FROM_OKLAB * mix(a, b, t);
	return max(LINEAR_FROM_LMS * (lms * lms * lms), vec3(0.0));
}

float sky_hash(vec2 p) {
	vec3 p3 = fract(vec3(p.xyx) * 0.1031);
	p3 += dot(p3, p3.yzx + 33.33);
	return fract((p3.x + p3.y) * p3.z);
}

float sky_value(vec2 p) {
	vec2 i = floor(p);
	vec2 f = p - i;
	f = f * f * (3.0 - 2.0 * f);
	return mix(mix(sky_hash(i), sky_hash(i + vec2(1.0, 0.0)), f.x), mix(sky_hash(i + vec2(0.0, 1.0)), sky_hash(i + vec2(1.0, 1.0)), f.x), f.y);
}

// Banded cloud at plane position q (x along the streaks): rows of long flat shards, each row with its own
// shard length and each shard one of three tone bands. Shard ends are cut on a slant that differs from row
// to row, so a streak tapers to a point and never ends square like a plank.
float sky_bands(vec2 q, float rows_per_unit, float cover) {
	float v = q.y * rows_per_unit;
	float row = floor(v);
	float stretch = 0.3 + 0.5 * sky_hash(vec2(row, 19.0));
	float shift = 9.0 * sky_hash(vec2(row, 7.0));
	float slant = (sky_hash(vec2(row, 3.0)) - 0.5) * 0.5;
	float shard = floor(q.x * stretch + shift + slant * (v - row - 0.5));
	vec2 c = vec2((shard + 0.5 - shift) / stretch, (row + 0.5) / rows_per_unit);
	float density = 0.62 * sky_value(c * vec2(0.9, 1.7) + 3.1) + 0.38 * sky_value(c * vec2(2.3, 5.9) + 11.7);
	float d = clamp((density - (1.0 - cover)) * 3.2, 0.0, 1.0);
	return floor(d * 3.0 + 0.5) / 3.0;
}

// Point stars on a cube-face grid: fixed to the sky, one per occupied cell.
float sky_stars(vec3 dir) {
	vec3 a = abs(dir);
	vec2 uv;
	float face;
	if (a.x >= a.y && a.x >= a.z) {
		uv = dir.yz / a.x;
		face = dir.x > 0.0 ? 0.0 : 1.0;
	} else if (a.y >= a.z) {
		uv = dir.xz / a.y;
		face = dir.y > 0.0 ? 2.0 : 3.0;
	} else {
		uv = dir.xy / a.z;
		face = dir.z > 0.0 ? 4.0 : 5.0;
	}
	vec2 g = uv * STAR_CELLS;
	vec2 id = floor(g) + face * 97.0;
	float pick = sky_hash(id);
	if (pick > STAR_DENSITY) {
		return 0.0;
	}
	vec2 at = floor(g) + 0.2 + 0.6 * vec2(sky_hash(id + 17.3), sky_hash(id + 41.9));
	float bright = pick / STAR_DENSITY;
	bright = 0.12 + 2.4 * bright * bright * bright * bright * bright * bright;
	float size = 0.045 + 0.03 * min(bright, 1.0);
	return bright * (1.0 - smoothstep(size * 0.5, size, length(g - at)));
}

void sky() {
	vec3 dir = EYEDIR;
	float up = dir.y;
	float h = max(up, 0.0);
	float clear = 1.0 - clamp(overcast, 0.0, 1.0);

	float toward = 0.0;
	float side = 0.5;
	if (LIGHT0_ENABLED) {
		toward = max(dot(dir, LIGHT0_DIRECTION), 0.0);
		side = 0.5 + 0.5 * dot(normalize(dir.xz + vec2(1.0e-5)), normalize(LIGHT0_DIRECTION.xz + vec2(1.0e-5)));
	}

	// Atmosphere: zenith to horizon, the horizon cooler and darker away from the sun.
	vec3 horizon = sky_blend(horizon_color, zenith_color, 0.45 * (1.0 - side * side));
	vec3 col = sky_blend(zenith_color, horizon, pow(1.0 - h, horizon_curve));
	// Scatter around the sun: a wide lobe lying along the horizon and a tight aureole.
	float low = 1.0 - h;
	float lobe = 0.3 * pow(toward, 8.0) * low * low * low * low + 0.35 * pow(toward, 60.0) + 1.4 * pow(toward, 900.0);
	if (LIGHT0_ENABLED) {
		col += LIGHT0_COLOR * (sun_glow * lobe);
	}

	// Banded cirrus on a plane one unit up, fading into the horizon haze.
	if (cirrus_cover > 0.0 && up > 0.0 && clear > 0.0) {
		vec2 plane = dir.xz / (up + 0.12);
		vec2 q = vec2(dot(plane, cirrus_dir), dot(plane, vec2(-cirrus_dir.y, cirrus_dir.x))) + cirrus_offset;
		// Looking along the wind the rows converge on a vanishing point; the layer thins out there, so the
		// bands never fan into a burst, whichever way the wind blows.
		float along = abs(dot(dir.xz, cirrus_dir));
		float thin = 1.0 - smoothstep(0.55, 0.92, along);
		// The layer gathers into broad banks with clear sky between them.
		float banks = smoothstep(0.3, 0.75, sky_value(q * vec2(0.13, 0.4) + 5.7));
		float cover = sky_bands(q, CIRRUS_ROWS, cirrus_cover * (0.3 + 1.2 * banks)) * 0.24;
		vec3 lit = cirrus_color * (1.0 + 0.35 * pow(toward, 6.0));
		col = mix(col, lit, cover * thin * smoothstep(0.03, 0.2, up) * clear);
	}

	// Full cloud layer: brighter overhead, and built from broad flat rolls lying across the wind, a few
	// percent apart in value, so the deck is never one flat tone and still has no blurred noise in it.
	if (clear < 1.0) {
		vec2 plane = dir.xz / (h + 0.12);
		vec2 q = vec2(dot(plane, vec2(-cirrus_dir.y, cirrus_dir.x)), dot(plane, cirrus_dir)) + cirrus_offset.yx * 0.25;
		float rolls = sky_bands(q, DECK_ROWS, 0.62);
		// Seen end-on the rolls would converge into a fan of wedges; they flatten out there, as the cirrus does.
		float end_on = abs(dot(normalize(dir.xz + vec2(1.0e-5)), vec2(-cirrus_dir.y, cirrus_dir.x)));
		float relief = DECK_CONTRAST * (1.0 - smoothstep(0.5, 0.9, end_on)) * smoothstep(0.02, 0.3, h);
		vec3 deck = overcast_color * (1.0 + 0.55 * h) * (1.0 + relief * (rolls - 0.5));
		// Rain shafts: broad soft curtains hanging between the deck and the sea, a darker veil with a faint
		// slanted grain in it. The grain stays fine and faint: strong stripes read as a striped backdrop.
		if (rain > 0.0) {
			float bearing = atan(dir.z, dir.x);
			float curtain = smoothstep(0.35, 0.75, sky_value(vec2(bearing * 4.0 + up * 2.0, 3.0)));
			float streak = sky_hash(vec2(floor(bearing * 320.0 + up * 160.0), 5.0));
			deck *= 1.0 - rain * curtain * (0.12 + 0.04 * streak) * (1.0 - smoothstep(0.02, 0.2, h));
		}
		col = mix(col, deck, 1.0 - clear);
	}

	if (lightning.w > 0.0) {
		float flash = max(dot(dir, normalize(lightning.xyz + vec3(1.0e-5))), 0.0);
		col += overcast_color * lightning.w * (0.35 + 2.5 * flash * flash * flash * flash);
	}

	// Below the horizon: the horizon's own colour for the first few degrees (a rough sea at a grazing angle
	// mirrors sky from just above the horizon, not the water in front of it), then down to the ground colour.
	// The sea covers this on screen; only reflections and ambient light see it.
	if (up < 0.0) {
		col = mix(col, ground_color, smoothstep(0.12, 0.55, -up));
	}

	if (!AT_CUBEMAP_PASS && up > -0.02) {
		// Stars, dimmed by haze near the horizon.
		if (star_energy > 0.0 && clear > 0.0) {
			col += vec3(sky_stars(dir) * star_energy * clear * clear * smoothstep(0.02, 0.3, up));
		}
		// Moon: a sphere lit from the side by its phase.
		if (LIGHT1_ENABLED) {
			float r = max(LIGHT1_SIZE, DISC_RADIUS);
			vec3 east = normalize(cross(vec3(0.0, 1.0, 0.0), LIGHT1_DIRECTION) + vec3(1.0e-5, 0.0, 0.0));
			vec3 north = cross(LIGHT1_DIRECTION, east);
			vec2 p = vec2(dot(dir, east), dot(dir, north)) / r;
			float rr = dot(p, p);
			if (rr < 1.3 && dot(dir, LIGHT1_DIRECTION) > 0.0) {
				vec3 n = vec3(p, sqrt(max(1.0 - rr, 0.0)));
				float turn = moon_phase * 6.2831853;
				// The terminator leans 20 degrees off vertical.
				vec3 from = vec3(sin(turn) * 0.94, sin(turn) * 0.34, -cos(turn));
				float lit = smoothstep(-0.03, 0.06, dot(n, from)) * (0.8 + 0.2 * n.z) + 0.02;
				float disc = 1.0 - smoothstep(1.0 - EDGE / r, 1.0, sqrt(rr));
				col = mix(col, LIGHT1_COLOR * LIGHT1_ENERGY * sun_disc * lit, disc * clear * clear);
			}
		}
		// Sun disc: crisp, with a little limb darkening, cut off by the horizon.
		if (LIGHT0_ENABLED && sun_disc > 0.0 && up > 0.0) {
			float r = max(LIGHT0_SIZE, DISC_RADIUS);
			float ang = acos(clamp(dot(dir, LIGHT0_DIRECTION), -1.0, 1.0));
			float disc = 1.0 - smoothstep(r - EDGE, r, ang);
			float limb = 0.7 + 0.3 * sqrt(max(1.0 - (ang * ang) / (r * r), 0.0));
			col += LIGHT0_COLOR * LIGHT0_ENERGY * (sun_disc * disc * limb * clear * clear);
		}
	}

	COLOR = col * energy;
}
