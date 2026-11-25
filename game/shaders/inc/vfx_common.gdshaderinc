// Layer A and Layer B draw rules shared by vfx_opaque.gdshader and vfx_blend.gdshader. DESIGN.md L1949-1969.
// Unit: shader-vfx. Include after res://shaders/gen/waves.gdshaderinc, in a shader with world_vertex_coords
// (VERTEX and NORMAL arrive in render space with the MultiMesh instance transform already applied).
//
// Kind ids (the material parameter `vfx_kind`): Layer A kinds are their index in hull_mesh::vfx::VfxKind::ALL
// (so that order is part of this interface); Layer B particle draw passes use 32 spray (foam ramp), 33 sparks
// (flash ramp), 34 embers (fire ramp), 35 flak (flak ramp) on vfx_opaque and 36 mist (smoke ramp) on vfx_blend,
// with the process shader vfx_particles.gdshader moving them.
// Instance data (hull_mesh::vfx::runtime docs): COLOR = vertex white x instance tint; INSTANCE_CUSTOM.x age s,
// .y lifetime s (1e9 for attached effects; 0 or less means no ageing), .z seed 0..1, .w per kind. CUSTOM0.x
// is the mesh's main motion coordinate, .y a secondary one, .z a per-vertex noise value.
//
// Ramps (gen ramp_<RampId::name()>, 256 x 1 RGBA8 sampled at age / lifetime, nearest, so the 4 to 6 posterised
// bands stay crisp; encoding in xtask/src/gen/art.rs): RGB is the albedo in sRGB; on an emitting ramp (flash,
// fire, hit puff, debris, flak) A is emission over the ramp's peak, multiplied here by the kind's ladder
// emission (vfx_emission_scale); on the others A is opacity, read by the blended kinds (trail, haze, mist).
const int K_TRACER = 0;
const int K_SPLASH_COLUMN = 1;
const int K_SPLASH_FOAM = 2;
const int K_HIT_FLASH = 3;
const int K_HIT_PUFF = 4;
const int K_DEBRIS = 5;
const int K_FIREBALL = 6;
const int K_FLAME_PRISM = 7;
const int K_SMOKE_PUFF = 8;
const int K_DEFENCE_SMOKE = 9;
const int K_FLAK_BURST = 10;
const int K_MISSILE_TRAIL = 11;
const int K_WAKE_SEGMENT = 12;
const int K_KELVIN_ARM = 13;
const int K_BOW_WAVE = 14;
const int K_MUZZLE_STAR = 15;
const int K_MUZZLE_HAZE = 16;
const int K_PUMP_ARC = 17;
const int K_BOIL_RING = 18;
const int K_FOAM_BAND = 19;
const int K_CHAFF_CLOUD = 20;
const int K_FLARE_BURN = 21;
const int LB_SPRAY = 32;
const int LB_SPARK = 33;
const int LB_EMBER = 34;
const int LB_FLAK = 35;
const int LB_MIST = 36;

// Emission ladder (L1955): flash 16 at the flash ramp's peak, burning down to 8 with the ramp's envelope
// (custom.w is the flash limiter's scale); tracers and sparks 4 to 8, fire 2 to 4 and the flare's 6 arrive in
// custom.w; everything else 1 or less.
const float VFX_FLASH_BASE = 16.0;
// Debris (hull_mesh::vfx::runtime::GRAVITY), m/s^2.
const float VFX_GRAVITY = 9.81;
// Foam on the sea snaps to a 0.5 m world lattice within 200 m (L1907). Beyond that the cell doubles each time
// the range doubles (1 m from 400 m, 2 m from 800 m, 4 m from 1.6 km), so a cell stays one to three pixels at
// 1080p and a far wake keeps its streaks instead of turning into a coarse mosaic or a shimmer.
const float VFX_FOAM_CELL_NEAR = 0.5;

float vfx_foam_cell(float cam_dist) {
	return VFX_FOAM_CELL_NEAR * exp2(clamp(floor(log2(max(cam_dist, 1.0) / 200.0)), 0.0, 3.0));
}

struct VfxOut {
	vec3 vertex;
	vec3 normal;
	float t;
	float scissor;
};

float vfx_hash(vec2 p) {
	vec3 q = fract(vec3(p.xyx) * 0.1031);
	q += dot(q, q.yzx + 33.33);
	return fract((q.x + q.y) * q.z);
}

vec3 vfx_hash3(float s) {
	return fract(sin(vec3(s * 12.9898, s * 78.233, s * 37.719)) * 43758.5453);
}

// Peak in the first 25% of life with an ease-out rise (L1953).
float vfx_grow(float t) {
	float e = clamp(t / 0.25, 0.0, 1.0);
	return 1.0 - (1.0 - e) * (1.0 - e);
}

// Effects end by scaling to 0 (L1953).
float vfx_end(float t, float from) {
	return 1.0 - smoothstep(from, 1.0, t);
}

// Rotate v about unit axis a by angle r (Rodrigues).
vec3 vfx_rotate(vec3 v, vec3 a, float r) {
	float c = cos(r);
	float s = sin(r);
	return v * c + cross(a, v) * s + a * dot(a, v) * (1.0 - c);
}

bool vfx_surface_kind(int kind) {
	return kind == K_SPLASH_FOAM || kind == K_WAKE_SEGMENT || kind == K_KELVIN_ARM || kind == K_BOIL_RING;
}

bool vfx_hull_side_kind(int kind) {
	return kind == K_FOAM_BAND || kind == K_BOW_WAVE;
}

bool vfx_dissolving_kind(int kind) {
	return vfx_surface_kind(kind) || kind == K_FOAM_BAND || kind == K_PUMP_ARC || kind == K_SPLASH_COLUMN;
}

// A ship's own water: the wake ribbon, the Kelvin arms and the bow wave. Their foam is laid out in the frame
// of the ship's track (streaks run along it) rather than as round clumps (vfx_wake_keep).
bool vfx_wake_kind(int kind) {
	return kind == K_WAKE_SEGMENT || kind == K_KELVIN_ARM || kind == K_BOW_WAVE;
}

// Metres per second at the wake speed factor 1 (30 kn, hull_mesh::vfx::runtime::WAKE_FULL_KN).
const float VFX_KN30_MS = 15.43;

// Normalised age of the water at `along` (0 start, 1 end) of a wake segment `length_m` long: the stern laid
// its start earlier than its end by the time it took to cross it, at the speed the instance carries in `.w`.
float vfx_wake_age(vec4 ic, float length_m, float along) {
	float crossing_s = length_m / max(ic.w * VFX_KN30_MS, 0.5);
	return ic.y > 0.0 ? clamp((max(ic.x, 0.0) + (1.0 - along) * crossing_s) / ic.y, 0.0, 1.0) : 0.0;
}

// How much foam a wake piece still holds, before the across-track profile: the churn behind the stern is
// solid white and thins to sparse streaks over the ribbon's 30 s; a Kelvin arm is a broken line from the
// start; a slow ship lays less of both. `w` is the 0..1 speed factor (0 to 30 kn).
float vfx_wake_fade(int kind, float t, float w) {
	float speed = 0.45 + 0.55 * w;
	if (kind == K_WAKE_SEGMENT) {
		return speed * (pow(1.0 - t, 1.7) * 0.95 + 0.15 * (1.0 - smoothstep(0.0, 0.08, t)));
	}
	if (kind == K_KELVIN_ARM) {
		return speed * 0.8 * pow(1.0 - t, 1.3);
	}
	return clamp(0.35 + 0.9 * w, 0.0, 1.0);
}

bool vfx_airborne_kind(int kind) {
	return kind == K_PUMP_ARC || kind == K_SPLASH_COLUMN;
}

// Normalised age: 0 for instances that do not age (lifetime 0 or less), else age / lifetime clamped to 0..1.
float vfx_t(vec4 ic) {
	return ic.y > 0.0 ? clamp(ic.x / ic.y, 0.0, 1.0) : 0.0;
}

// Per-kind motion in render space. `model` is MODEL_MATRIX (the instance transform), `c0` CUSTOM0, `ic`
// INSTANCE_CUSTOM. Ages come from Rust only; TIME is used for nothing but the sea surface.
VfxOut vfx_motion(int kind, vec3 vtx, vec3 nrm, mat4 model, vec4 c0, vec4 ic, float time) {
	VfxOut o;
	o.vertex = vtx;
	o.normal = nrm;
	o.t = vfx_t(ic);
	o.scissor = 1.0;
	if (kind >= LB_SPRAY) {
		// Layer B: the process shader moves and scales each particle.
		return o;
	}
	vec3 origin = model[3].xyz;
	vec3 bx = model[0].xyz;
	vec3 by = model[1].xyz;
	vec3 bz = model[2].xyz;
	float lx = length(bx);
	float ly = length(by);
	float lz = length(bz);
	float age = max(ic.x, 0.0);
	bool expired = ic.y > 0.0 && ic.x >= ic.y;
	if (lx * ly * lz < 1e-12 || expired) {
		// An empty or reused slot (VfxInstance::HIDDEN) or a finished instance: scale 0.
		o.vertex = origin;
		return o;
	}
	vec3 xn = bx / lx;
	vec3 yn = by / ly;
	vec3 zn = bz / lz;
	vec3 r = vtx - origin;
	// Boil displacement goes along the radial direction, not the flat face normal: coincident vertices share
	// their position-hashed noise (CUSTOM0.z), so the faceted mesh stays closed while it churns.
	float rl = length(r);
	vec3 radial_dir = rl > 1e-6 ? r / rl : nrm;
	float t = o.t;
	float seed = ic.z;
	float w = ic.w;
	float wob = sin(age * 2.3 + c0.z * 6.2832 + seed * 17.0);

	if (kind == K_SPLASH_COLUMN) {
		// Rise 0.6 to 1.0 s, hold 0.4 s, collapse 2 to 3 s: lifetime = 0.9 + 3.5 rise (splash_column_phases).
		float life = max(ic.y, 0.1);
		float rise = clamp((life - 0.9) / 3.5, 0.6, 1.0);
		float hold_end = rise + 0.4;
		float collapse = max(life - hold_end, 0.1);
		float h = 1.0;
		float spread = 1.0;
		if (age < rise) {
			float e = age / rise;
			h = 1.0 - pow(1.0 - e, 3.0);
			spread = mix(0.55, 1.0, e);
		} else if (age > hold_end) {
			float c = clamp((age - hold_end) / collapse, 0.0, 1.0);
			h = 1.0 - c * c;
			spread = 1.0 + 0.45 * c;
		}
		float hy = dot(r, yn);
		vec3 radial = r - yn * hy;
		float boil = 1.0 + 0.07 * sin(age * 9.0 + c0.z * 6.2832 + c0.x * 5.0);
		// The crown frays and opens as the column tops out, then the falling sheet spreads at the base.
		float crown = 1.0 + c0.x * c0.x * (0.25 * min(age / rise, 1.0) + 0.5 * (1.0 - h) + 0.35 * (c0.z - 0.5));
		o.vertex = origin + yn * (hy * h) + radial * (spread * crown * boil);
		// The falling crown breaks up into lattice clumps; the base stays whole.
		float fall = age > hold_end ? clamp((age - hold_end) / collapse, 0.0, 1.0) : 0.0;
		o.scissor = 1.0 - smoothstep(0.35, 1.0, c0.x) * fall * 1.2;
	} else if (kind == K_SPLASH_FOAM) {
		float e = 0.35 + 0.65 * (1.0 - (1.0 - t) * (1.0 - t));
		float f = max(c0.x, 1e-3);
		float f2 = mix(f, 1.0, pow(t, 1.3));
		vec3 radial = r - yn * dot(r, yn);
		o.vertex = origin + radial * (e * f2 / f);
		o.scissor = 1.0 - t;
	} else if (kind == K_HIT_FLASH || kind == K_MUZZLE_STAR) {
		o.vertex = origin + r * mix(0.75, 1.12, t);
	} else if (kind == K_HIT_PUFF) {
		float drift = w * (1.0 - exp(-2.5 * age)) / 2.5;
		float s = (0.5 + 0.9 * vfx_grow(t) + 0.2 * t) * vfx_end(t, 0.75);
		o.vertex = origin + yn * drift + r * s + radial_dir * (0.04 * ly * wob * s);
	} else if (kind == K_DEBRIS) {
		vec3 axis = normalize(vfx_hash3(seed * 91.7 + 3.1) - 0.5 + vec3(1e-3));
		float spin = age * (4.0 + 8.0 * seed);
		vec3 centre = origin + yn * (w * age) - vec3(0.0, 0.5 * VFX_GRAVITY * age * age, 0.0);
		float s = vfx_end(t, 0.92);
		o.vertex = centre + vfx_rotate(r, axis, spin) * s;
		o.normal = vfx_rotate(nrm, axis, spin);
	} else if (kind == K_FIREBALL) {
		float s = (0.45 + 0.55 * vfx_grow(t) + 0.15 * t) * vfx_end(t, 0.8);
		o.vertex = origin + vec3(0.0, 1.2 * age, 0.0) + r * s + radial_dir * (0.05 * ly * wob * s);
	} else if (kind == K_FLAME_PRISM) {
		float env = t < 0.25 ? vfx_grow(t) : 1.0 - pow((t - 0.25) / 0.75, 1.6);
		float sway = sin(age * 7.0 + seed * 20.0 + c0.z * 3.0) * 0.14 * c0.x * ly;
		o.vertex = origin + r * env + xn * (sway * env);
	} else if (kind == K_SMOKE_PUFF || kind == K_MUZZLE_HAZE) {
		float drift = kind == K_MUZZLE_HAZE ? w * (1.0 - exp(-1.6 * age)) / 1.6 : w * age;
		float s = (0.6 + 1.2 * sqrt(t)) * vfx_end(t, 0.85);
		o.vertex = origin + yn * drift + r * s + radial_dir * (0.035 * ly * wob * s);
		// Opaque smoke cannot fade, so in the second half of its life it breaks up through the puff's own
		// cell lattice (vfx_smoke_keep) instead of hanging whole and then popping.
		o.scissor = 1.0 - 0.8 * smoothstep(0.3, 1.0, t);
	} else if (kind == K_DEFENCE_SMOKE) {
		vec3 rr = vfx_rotate(r, vec3(0.0, 1.0, 0.0), age * w);
		o.vertex = origin + rr + vfx_rotate(radial_dir, vec3(0.0, 1.0, 0.0), age * w) * (0.03 * lx * sin(age * 0.6 + c0.z * 6.2832));
		o.normal = vfx_rotate(nrm, vec3(0.0, 1.0, 0.0), age * w);
	} else if (kind == K_FLAK_BURST) {
		float burst = 1.0 - pow(1.0 - clamp(t / 0.12, 0.0, 1.0), 3.0);
		float s = (0.35 + 0.65 * burst + 0.25 * t) * vfx_end(t, 0.7);
		o.vertex = origin + vec3(0.0, 0.4 * age, 0.0) + r * s + radial_dir * (0.04 * ly * wob * s);
	} else if (kind == K_MISSILE_TRAIL) {
		float along = dot(r, xn);
		vec3 perp = r - xn * along;
		// Each puff of the trail drifts on its own: buoyant rise plus a seeded sideways wander.
		// A smooth field of the emission point, so neighbouring segments (0.1 s apart) stay joined.
		vec3 wander = vec3(sin(vtx.x * 0.05 + vtx.z * 0.031), 0.5 * sin(vtx.z * 0.043 - vtx.y * 0.06), cos(vtx.x * 0.037 - vtx.z * 0.05)) * 0.6;
		// Puffs: the cross-section swells and narrows along the trail, a smooth field of position so joints meet.
		float puff = 1.0 + 0.35 * sin(dot(vtx, vec3(0.41, 0.23, 0.33)) + seed * 0.0);
		// Tapered: 0.4 of the emitted diameter at the motor (about the plume, 1 m), spreading to 1.3 times it
		// (3 m) at the trail's end, so a trail passing the camera stays a thin line of smoke.
		o.vertex = origin + xn * along + perp * ((0.4 + 0.9 * sqrt(t)) * puff) + (vec3(0.0, 0.3, 0.0) + wander) * age;
	} else if (kind == K_WAKE_SEGMENT) {
		// The churned band spreads as it ages; the foam in it thins out (vfx_wake_keep, per pixel).
		float across = dot(r, zn);
		float tv = vfx_wake_age(ic, lx, clamp(c0.x, 0.0, 1.0));
		o.vertex = origin + (r - zn * across) + zn * (across * (1.0 + 0.8 * tv));
		o.scissor = vfx_wake_fade(kind, tv, ic.w);
	} else if (kind == K_KELVIN_ARM) {
		o.vertex = vtx + zn * (w * age);
		o.scissor = vfx_wake_fade(kind, t, 1.0);
	} else if (kind == K_BOW_WAVE) {
		float ripple = sin(age * 7.0 - c0.x * 9.0 + seed * 6.2832) * 0.08 * ly * c0.y;
		o.vertex = vtx + vec3(0.0, ripple, 0.0);
		o.scissor = vfx_wake_fade(kind, t, w);
	} else if (kind == K_PUMP_ARC) {
		// The jet pulses along the arc and breaks into droplets toward its end (cut by the foam lattice, so the
		// square tube never cracks open).
		float pulse = 0.5 + 0.5 * sin(c0.x * 14.0 - age * 12.0 * (0.5 + w));
		o.scissor = 1.0 - smoothstep(0.55, 1.0, c0.x) * (0.5 + 0.5 * pulse);
	} else if (kind == K_BOIL_RING) {
		vec3 rr = vfx_rotate(r, vec3(0.0, 1.0, 0.0), age * 0.3);
		float hy = dot(rr, yn);
		float bob = hy > 1e-4 ? sin(age * 4.0 + c0.z * 6.2832) * 0.35 * ly * w : 0.0;
		o.vertex = origin + rr + vec3(0.0, bob, 0.0);
		o.scissor = 0.7 + 0.3 * w;
	} else if (kind == K_FOAM_BAND) {
		o.scissor = (0.45 + 0.55 * w) * (1.0 - 0.7 * c0.y);
	} else if (kind == K_CHAFF_CLOUD) {
		o.vertex = vtx + radial_dir * (0.02 * lx * sin(age * 5.0 + c0.z * 6.2832));
	} else if (kind == K_FLARE_BURN) {
		o.vertex = origin + r * (1.0 + 0.12 * sin(age * 31.0 + seed * 40.0));
	}

	if (vfx_surface_kind(kind)) {
		// On the sea: the same Gerstner displacement the water mesh takes, so foam lies on the waves.
		vec3 d = wave_displace(o.vertex.xz, time);
		o.vertex = vec3(d.x, d.y + (o.vertex.y - origin.y) + 0.06, d.z);
	} else if (vfx_hull_side_kind(kind)) {
		// Against a hull: height only, so the foam never slides into the plating.
		float h = wave_height(o.vertex.xz, time);
		o.vertex.y = h + (o.vertex.y - origin.y) + 0.05;
	}
	return o;
}

// Value noise on the integer lattice (smooth between lattice points).
float vfx_value_noise(vec2 p) {
	vec2 i = floor(p);
	vec2 f = fract(p);
	f = f * f * (3.0 - 2.0 * f);
	float a = vfx_hash(i);
	float b = vfx_hash(i + vec2(1.0, 0.0));
	float c = vfx_hash(i + vec2(0.0, 1.0));
	float d = vfx_hash(i + vec2(1.0, 1.0));
	return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Dissolve pattern for foam: clumps of coherent noise, evaluated once per lattice cell so every edge snaps to
// the 0.5 m world lattice (L1907; coarser past 400 m, vfx_foam_cell), kept while the clump value is under `cover`. Surface
// foam uses the horizontal lattice; spray in the air (splash crowns, pump jets) a 3D one. Returns 1 to keep.
float vfx_foam_keep(vec3 world, float cam_dist, float cover, float seed, bool in_air) {
	float cell = vfx_foam_cell(cam_dist);
	vec3 q = (floor(world / cell) + 0.5) * cell;
	vec2 p = in_air ? q.xz + vec2(q.y * 1.7, -q.y * 1.3) : q.xz;
	float n = 0.65 * vfx_value_noise(p * 0.32 + seed * 37.0) + 0.35 * vfx_value_noise(p * 0.9 - seed * 11.0);
	return step(n, cover);
}

// Break-up of an opaque smoke puff (fire and muzzle smoke): the same clumped cell noise as airborne foam, on
// a lattice in the puff's own frame (`local` is the vertex offset from the spawn origin before growth, metres),
// so the cells grow with the puff and do not swim through it as it drifts. Returns 1 to keep.
float vfx_smoke_keep(vec3 local, float cam_dist, float cover, float seed) {
	return vfx_foam_keep(local + vec3(seed * 97.0, seed * 53.0, seed * 31.0), cam_dist, cover, seed, true);
}

// Two octaves of value noise stretched so its values spread nearly evenly over 0..1: a threshold `c` then
// keeps about a share `c` of the cells, so cover reads as area.
float vfx_even_noise(vec2 a, vec2 b) {
	float n = 0.62 * vfx_value_noise(a) + 0.38 * vfx_value_noise(b);
	return clamp((n - 0.5) * 2.4 + 0.5, 0.0, 1.0);
}

// Foam of a ship's own water (vfx_wake_kind), per pixel. Returns the tone: 0 cut, else the albedo factor (1 in
// the solid part, one ramp band darker on the lacy edge, so foam reads as built from cells, not as a blur).
// `frame_a` is the instance's origin (xz) and unit +X direction (xz); `frame_b` the lengths of its X, Z and Y
// axes; `ic` INSTANCE_CUSTOM; `sec` and `main` the mesh's CUSTOM0.y and .x.
// - Wake ribbon and Kelvin arms lie on the sea: cells snap to the world lattice (vfx_foam_cell), and the
//   pattern is a function of world position plus the distance from the track, never of the segment's own
//   axes, so a curving wake laid as straight segments shows no seams at the joints. Foam stays where the
//   water is and forms streaks parallel to the track; the churn is densest down the middle and frays at the
//   sides. A segment's water is older at its start than at its end by the time the stern took to cross it, so
//   the fade runs continuously down the wake rather than in steps.
// - The bow wave is a sheet against the hull: its cells are 0.5 m squares of the sheet (`main` x its length
//   aft of the stem, world height up), and the pattern flows aft at the ship's speed, so the sheet breaks into
//   spray clumps at its crest and tail while the foot at the stem stays white.
float vfx_wake_keep(int kind, vec3 world, float cam_dist, vec4 frame_a, vec4 frame_b, vec4 ic, float sec, float main) {
	float n;
	float c;
	if (kind == K_BOW_WAVE) {
		float flow = ic.w * VFX_KN30_MS * ic.x;
		vec2 q = (floor(vec2(main * frame_b.x, world.y) / VFX_FOAM_CELL_NEAR) + 0.5) * VFX_FOAM_CELL_NEAR;
		n = vfx_even_noise(vec2(q.x - flow, q.y * 2.0) * 0.45, vec2(q.x * 1.3 - flow * 1.3, q.y * 3.1) * 0.6 + 5.3);
		c = vfx_wake_fade(kind, 0.0, ic.w) * (1.0 - 0.8 * main) * (1.0 - 0.5 * sec * sec);
	} else {
		float cell = vfx_foam_cell(cam_dist);
		vec2 q = (floor(world.xz / cell) + 0.5) * cell;
		vec2 xn = frame_a.zw;
		vec2 zn = vec2(-xn.y, xn.x);
		vec2 rel = q - frame_a.xy;
		float iso = 0.6 * vfx_value_noise(q * 0.11 + 3.1) + 0.4 * vfx_value_noise(q * 0.37 - 5.2);
		if (kind == K_WAKE_SEGMENT) {
			float along = clamp(dot(rel, xn) / max(frame_b.x, 1e-3), 0.0, 1.0);
			float t = vfx_wake_age(ic, frame_b.x, along);
			float across_m = dot(rel, zn);
			float a = abs(across_m) / max(0.5 * frame_b.y * (1.0 + 0.8 * t), 1e-3);
			float streak = vfx_value_noise(vec2(across_m * 0.5, 7.3));
			n = clamp((0.5 * streak + 0.5 * iso - 0.5) * 2.6 + 0.5, 0.0, 1.0);
			// The ribbon spans the whole disturbed band (1 to 2.5 beams); the white water is a core down its middle
			// that frays out across it.
			c = vfx_wake_fade(kind, t, ic.w) * (1.0 - smoothstep(0.0, 0.85, a));
		} else {
			// Kelvin arm: the instance drifts outward along its Z by `ic.w` m/s; undo it to find the crest line.
			vec2 zd = normalize(vec2(-xn.y, xn.x));
			float across_m = dot(rel - zd * (ic.w * ic.x), zd);
			float a = abs(across_m) / max(0.5 * frame_b.y, 1e-3);
			// Dashes along the crest line, keyed on world position and the arm's seed: keyed on the distance
			// from the segment's own start, every segment would draw the same dashes.
			float dash = vfx_value_noise(vec2(dot(q, xn) * 0.22, ic.z * 61.0));
			n = clamp((0.6 * dash + 0.4 * iso - 0.5) * 2.6 + 0.5, 0.0, 1.0);
			c = vfx_wake_fade(kind, vfx_t(ic), 1.0) * (1.0 - smoothstep(0.15, 0.85, a));
		}
	}
	if (n >= c) {
		return 0.0;
	}
	if (n >= c - 0.12) {
		return 0.8;
	}
	// Inside the solid churn a scatter of cells one band greyer, so the white water behind the stern reads as
	// broken foam rather than one flat slab.
	vec2 m = floor(world.xz / VFX_FOAM_CELL_NEAR);
	return kind == K_WAKE_SEGMENT && vfx_value_noise(m * 0.45 + 11.0) > 0.66 ? 0.88 : 1.0;
}

// Emission scale of a kind before the ramp's weight (L1955).
float vfx_emission_scale(int kind, vec4 ic) {
	if (kind == K_HIT_FLASH || kind == K_MUZZLE_STAR) {
		return VFX_FLASH_BASE * ic.w;
	}
	if (kind == K_TRACER || kind == K_FIREBALL || kind == K_FLAME_PRISM || kind == LB_SPARK || kind == LB_EMBER) {
		return ic.w;
	}
	if (kind == K_FLARE_BURN) {
		return ic.w * (0.85 + 0.15 * sin(ic.x * 40.0 + ic.z * 30.0));
	}
	if (kind == K_DEBRIS || kind == K_HIT_PUFF || kind == K_FLAK_BURST || kind == LB_FLAK) {
		return 1.0;
	}
	return 0.0;
}
