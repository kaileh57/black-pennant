// VFX layer B process shader. DESIGN.md L1951. Unit: shader-vfx.
// Pooled GPUParticles3D, Forward+ only, secondary detail: spray, sparks, embers, flak and mist. One process
// material per kind (`particle_kind`), shared by that kind's seed emitter and its sub-emitters; at most 12
// process materials exist (L1973). Emitters run with local_coords true and fixed_fps 30 (interpolated), draw in
// INDEX order, with no trails, collision or shadows; turbulence (the swirl below) runs only on embers and mist.
//
// Emitters are translation-only nodes at their anchors, so local -Y is down and the wind is a render-frame
// vector. Particles arrive from RenderingServer.particles_emit / emit_particle (at most 128 calls per frame):
//   xform     local position (and an optional initial size in the basis; identity basis = the kind's size);
//   velocity  initial velocity, m/s (EMIT_FLAG_VELOCITY); without it the kind throws its own;
//   color     tint (EMIT_FLAG_COLOR), white otherwise;
//   custom    (0, lifetime s or 0 for the kind's own, seed 0..1, render-space y of the emission point)
//             with EMIT_FLAG_CUSTOM.
// Godot emits only the emit_particle requests on an emitter that receives any, so emitters driven from Rust
// run with emitting = false; one that emits on its own takes the RESTART_* paths below (kind defaults).
// Water contact is analytic (L1951): a particle that falls through the calm sea surface (render y = 0) ends.
//
// Output for the draw pass (vfx_opaque.gdshader with vfx_kind 32 to 35, vfx_blend.gdshader with 36 for mist):
// CUSTOM = (age s, lifetime s, seed, emission scale), the Layer A instance layout, so the draw pass samples
// the kind's ramp at age / lifetime exactly as Layer A does. Ages are integrated here from DELTA, never TIME.
shader_type particles;
render_mode disable_force, disable_velocity;

uniform int particle_kind = 0;
uniform vec3 wind = vec3(0.0);
// Shared live-cap scale from the degrade ladder (1, or less once Layer B is thinned): shortens lives.
uniform float life_scale : hint_range(0.1, 1.0) = 1.0;

const int P_SPRAY = 0;
const int P_SPARK = 1;
const int P_EMBER = 2;
const int P_FLAK = 3;
const int P_MIST = 4;
const float GRAVITY = 9.81;

float p_hash(uint n) {
	n = (n << 13u) ^ n;
	n = n * (n * n * 15731u + 789221u) + 1376312589u;
	return float(n & 0x7fffffffu) / float(0x7fffffff);
}

float p_rand(uint seed, uint k) {
	return p_hash(seed * 1597334677u + k * 3812015801u + 1u);
}

vec3 p_dir(uint seed, uint k) {
	float z = p_rand(seed, k) * 2.0 - 1.0;
	float a = p_rand(seed, k + 1u) * 6.2831853;
	float s = sqrt(max(0.0, 1.0 - z * z));
	return vec3(s * cos(a), z, s * sin(a));
}

// Per-kind lifetime range, size range (metres) and emission (L1955: sparks 4 to 8, embers 2 to 4 on fire's rung).
vec2 kind_life(int k) {
	if (k == P_SPRAY) { return vec2(1.2, 2.4); }
	if (k == P_SPARK) { return vec2(0.3, 0.8); }
	if (k == P_EMBER) { return vec2(2.0, 4.5); }
	if (k == P_FLAK) { return vec2(0.6, 1.5); }
	return vec2(3.0, 6.0);
}

vec2 kind_size(int k) {
	if (k == P_SPRAY) { return vec2(0.07, 0.22); }
	if (k == P_SPARK) { return vec2(0.05, 0.09); }
	if (k == P_EMBER) { return vec2(0.08, 0.16); }
	if (k == P_FLAK) { return vec2(0.15, 0.32); }
	return vec2(3.0, 7.0);
}

float kind_emission(int k, float r) {
	if (k == P_SPARK) { return mix(4.0, 8.0, r); }
	if (k == P_EMBER) { return mix(2.0, 3.0, r); }
	return 0.0;
}

// Linear drag per second.
float kind_drag(int k) {
	if (k == P_SPRAY) { return 0.35; }
	if (k == P_SPARK) { return 1.6; }
	if (k == P_EMBER) { return 1.2; }
	if (k == P_FLAK) { return 1.0; }
	return 0.6;
}

// Share of gravity: embers and mist are buoyant.
float kind_gravity(int k) {
	if (k == P_EMBER) { return -0.12; }
	if (k == P_MIST) { return -0.03; }
	return 1.0;
}

vec3 kind_velocity(int k, uint seed) {
	vec3 d = p_dir(seed, 11u);
	if (k == P_SPRAY) {
		vec3 up = normalize(vec3(d.x * 0.45, 1.0, d.z * 0.45));
		return up * mix(6.0, 14.0, p_rand(seed, 13u));
	}
	if (k == P_SPARK) {
		return normalize(d + vec3(0.0, 0.6, 0.0)) * mix(15.0, 40.0, p_rand(seed, 13u));
	}
	if (k == P_EMBER) {
		return vec3(d.x * 0.6, mix(1.0, 3.0, p_rand(seed, 13u)), d.z * 0.6);
	}
	if (k == P_FLAK) {
		return d * mix(30.0, 60.0, p_rand(seed, 13u));
	}
	return vec3(d.x, 0.2 + 0.3 * abs(d.y), d.z) * mix(0.3, 1.2, p_rand(seed, 13u));
}

void start() {
	uint seed = RANDOM_SEED + INDEX * 7919u + NUMBER * 104729u;
	float r_size = p_rand(seed, 1u);
	float r_life = p_rand(seed, 2u);
	vec2 lr = kind_life(particle_kind);
	vec2 sr = kind_size(particle_kind);
	float given_life = RESTART_CUSTOM ? 0.0 : CUSTOM.y;
	float given_seed = RESTART_CUSTOM ? p_rand(seed, 3u) : CUSTOM.z;
	float world_y = TRANSFORM[3].y;
	if (!RESTART_CUSTOM) {
		world_y = CUSTOM.w;
	}
	if (RESTART_POSITION) {
		TRANSFORM[3].xyz = vec3(0.0);
		world_y = RESTART_CUSTOM ? 0.0 : world_y;
	}
	float size = mix(sr.x, sr.y, r_size);
	if (RESTART_ROT_SCALE) {
		TRANSFORM[0].xyz = vec3(size, 0.0, 0.0);
		TRANSFORM[1].xyz = vec3(0.0, size, 0.0);
		TRANSFORM[2].xyz = vec3(0.0, 0.0, size);
	}
	if (RESTART_VELOCITY) {
		VELOCITY = kind_velocity(particle_kind, seed);
	}
	if (RESTART_COLOR) {
		COLOR = vec4(1.0);
	}
	float life = (given_life > 0.0 ? given_life : mix(lr.x, lr.y, r_life)) * life_scale;
	CUSTOM = vec4(0.0, max(life, 0.05), given_seed, kind_emission(particle_kind, p_rand(seed, 4u)));
	// Render-space height of the local origin, for the analytic water contact; and the base size.
	USERDATA1 = vec4(world_y - TRANSFORM[3].y, length(TRANSFORM[0].xyz), 0.0, 0.0);
}

void process() {
	float dt = DELTA;
	CUSTOM.x += dt;
	float t = CUSTOM.x / max(CUSTOM.y, 1e-3);
	if (t >= 1.0) {
		ACTIVE = false;
	}
	float drag = kind_drag(particle_kind);
	vec3 rel = VELOCITY - wind;
	VELOCITY -= rel * min(drag * dt, 1.0);
	VELOCITY.y -= GRAVITY * kind_gravity(particle_kind) * dt;
	if (particle_kind == P_EMBER || particle_kind == P_MIST) {
		// Cheap swirl in place of turbulence nodes, on these two kinds only.
		vec3 p = TRANSFORM[3].xyz * 0.35;
		float phase = CUSTOM.x * 0.9 + CUSTOM.z * 31.0;
		vec3 swirl = vec3(sin(p.z + phase), sin(p.x * 1.3 + phase * 1.1) * 0.4, cos(p.y + p.x + phase));
		VELOCITY += swirl * (particle_kind == P_EMBER ? 1.8 : 0.5) * dt;
	}
	TRANSFORM[3].xyz += VELOCITY * dt;
	float world_y = USERDATA1.x + TRANSFORM[3].y;
	if (particle_kind != P_MIST && world_y < 0.0 && VELOCITY.y < 0.0) {
		ACTIVE = false;
	}
	float base = USERDATA1.y;
	float grow = 1.0;
	if (particle_kind == P_MIST) {
		grow = 1.0 + 1.5 * sqrt(clamp(t, 0.0, 1.0));
	} else if (particle_kind == P_SPRAY) {
		grow = 1.0 - smoothstep(0.7, 1.0, t);
	} else {
		grow = 1.0 - smoothstep(0.8, 1.0, t);
	}
	float s = base * grow;
	if (particle_kind == P_SPARK) {
		// Sparks stretch along their velocity (no trails): about a 60th of a second of travel.
		float speed = length(VELOCITY);
		vec3 dir = speed > 1e-3 ? VELOCITY / speed : vec3(0.0, 1.0, 0.0);
		vec3 side = normalize(abs(dir.y) > 0.9 ? cross(dir, vec3(1.0, 0.0, 0.0)) : cross(dir, vec3(0.0, 1.0, 0.0)));
		vec3 up = cross(side, dir);
		TRANSFORM[0].xyz = dir * max(s, speed / 60.0);
		TRANSFORM[1].xyz = up * s;
		TRANSFORM[2].xyz = side * s;
	} else {
		vec3 axis = normalize(vec3(0.3, 1.0, 0.2) + vec3(CUSTOM.z - 0.5));
		float spin = CUSTOM.x * (1.5 + 3.0 * CUSTOM.z);
		float c = cos(spin);
		float sn = sin(spin);
		mat3 rot = mat3(1.0) * c + (1.0 - c) * outerProduct(axis, axis)
			+ sn * mat3(vec3(0.0, axis.z, -axis.y), vec3(-axis.z, 0.0, axis.x), vec3(axis.y, -axis.x, 0.0));
		TRANSFORM[0].xyz = rot[0] * s;
		TRANSFORM[1].xyz = rot[1] * s;
		TRANSFORM[2].xyz = rot[2] * s;
	}
}
