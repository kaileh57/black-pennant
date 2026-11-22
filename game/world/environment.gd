extends Node
## Applies weather and time-of-day presets to the WorldEnvironment and lights from
## BpWorldView.environment(): sun colour and energy, sky, ambient floor, fog (density 3.912 / V,
## fog_height_density 0), exposure, lightning pulses. Auto exposure stays off. The LUT is not a
## resource: assign BpWorldView.lut() (one ImageTexture3D that Rust blends and updates in place at 1 Hz)
## to the environment's colour correction once, and skip it while it is null.
## DESIGN.md L1846-1883, L2061. Unit: world-scene.
##
## Where the numbers come from:
## - Colours, sun energy, ambient (energy folded into its luminance), ambient floor, fog colour and exposure:
##   the 18 presets of res://art/tokens/world_tokens.json, keyed by time of day and weather. Token colours
##   are linear; Godot's Color properties and source_color uniforms take sRGB, so each is encoded here.
## - Sky layers, shadow softness and the sky shader's shape parameters: res://world/presets/<weather>.tres
##   (WorldWeatherLook) and <time>.tres (WorldTimeLook). They hold no colour.
## - Fog density, sun scatter, aerial perspective, sun direction, wind drift and the blend: the simulation,
##   through EnvironmentDict. Scenes with no simulation (dry dock, warm-up) call apply_static().
##
## Cadence: everything is applied on BpWorldView.environment_changed (a weather turn, once a second while a
## 20 to 60 s blend runs, and each 2 s cirrus step), interpolating the two presets by `blend`. The only
## per-frame work is a running lightning pulse.
##
## The sky shader (res://shaders/sky.gdshader) asks for: the sun created before the moon and never hidden
## (at night energy 0 and sky mode "sky only"), and fog_sky_affect 0. The scene files set both.
## Reads and calls: BpWorldView.environment, BpWorldView.environment_changed, BpWorldView.lightning,
## BpWorldView.lut, Settings.get_value.

const TOKENS_PATH := "res://art/tokens/world_tokens.json"
## Bp.WEATHER_* order: the token and preset-file word of each weather.
const WEATHER_WORDS: Array[String] = ["clear", "haze", "overcast", "rain", "storm", "fog_bank"]
## Bp.TIME_* order.
const TIME_WORDS: Array[String] = ["night", "dusk", "day"]
const PRESET_DIR := "res://world/presets/"
## Height of the cirrus layer: the sky shader's cirrus plane is one layer height above the eye, so the
## wind drift in metres divides by it.
const CIRRUS_HEIGHT_M := 9000.0
## Moon placement by night (the simulation fixes only the sun): this far round from the sun's azimuth,
## at an elevation between these bounds picked from the sortie's cloud seed.
const MOON_AZIMUTH_FROM_SUN_DEG := 165.0
const MOON_ELEVATION_MIN_DEG := 24.0
const MOON_ELEVATION_MAX_DEG := 48.0
## Specular share of moonlight (Light3D.light_specular; the sun keeps Godot's 0.5).
const MOON_SPECULAR := 0.22
## Moon phase range picked from the seed: gibbous moons, so a night sortie always has a moon to see by.
const MOON_PHASE_MIN := 0.3
const MOON_PHASE_MAX := 0.7
## Lightning (L1875): a flash in the sky towards the bearing and a short lift of ambient and fog light.
const LIGHTNING_SKY_GAIN := 3.0
const LIGHTNING_AMBIENT_GAIN := 2.5
const LIGHTNING_FOG_GAIN := 1.5
## Fraction of the pulse spent at full brightness before it decays.
const LIGHTNING_HOLD := 0.25
## Sun azimuth used before the first weather event arrives (behind the loading cover).
const DEFAULT_SUN_AZIMUTH_DEG := 250.0

@export var world_environment: WorldEnvironment
## Created before `moon` in the scene, so it is LIGHT0 of the sky shader.
@export var sun: DirectionalLight3D
@export var moon: DirectionalLight3D

static var _tokens: Dictionary = {}

var _view: BpWorldView
var _env: Environment
var _sky: ShaderMaterial
var _lut_bound: bool = false
## What the last apply set, so a lightning pulse can return to it.
var _ambient_energy: float = 1.0
var _fog_energy: float = 1.0
var _flash_age_s: float = -1.0
var _flash_len_s: float = 0.1
var _flash_intensity: float = 0.0
var _flash_dir: Vector3 = Vector3.UP


func _ready() -> void:
	set_process(false)
	_bind_environment()


## Follow a world view: apply its environment now and on every change, and play its lightning.
func bind_view(view: BpWorldView) -> void:
	if _view != null and is_instance_valid(_view):
		if _view.environment_changed.is_connected(_on_environment_changed):
			_view.environment_changed.disconnect(_on_environment_changed)
		if _view.lightning.is_connected(_on_lightning):
			_view.lightning.disconnect(_on_lightning)
	_view = view
	_lut_bound = false
	if view == null:
		return
	view.environment_changed.connect(_on_environment_changed)
	view.lightning.connect(_on_lightning)


## The preset a scene with no simulation shows: `time` is Bp.TIME_*, `weather` Bp.WEATHER_*. The sun
## stands at the time's static elevation on `azimuth_deg` unless `elevation_deg` is given (not NAN).
func apply_static(time: int, weather: int, azimuth_deg: float, elevation_deg: float = NAN, sortie_seed: int = 0) -> void:
	time = clampi(time, 0, TIME_WORDS.size() - 1)
	weather = clampi(weather, 0, WEATHER_WORDS.size() - 1)
	var elevation := _time_look(time).static_sun_elevation_deg if is_nan(elevation_deg) else elevation_deg
	var density := float(_preset(time, weather).get("fog_density", 1.3e-4))
	var scatter := 0.2 * (1.0 - smoothstep(10.0, 25.0, elevation))
	apply(time, weather, weather, 1.0, sky_dir(azimuth_deg, elevation), density, scatter,
		_weather_look(weather).aerial_perspective, Vector2.ZERO, azimuth_deg + 90.0, sortie_seed, 0.0)


## Apply one environment state. `blend` runs 0..1 from `weather_from` to `weather_to` (Bp.WEATHER_*);
## `time` is Bp.TIME_*; `sun_dir` a unit vector towards the sun in render space; `wind_toward_deg` the
## compass bearing the wind blows towards; `rain` the live weather's rain amount 0..1.
func apply(time: int, weather_from: int, weather_to: int, blend: float, sun_dir: Vector3, fog_density: float,
		fog_sun_scatter: float, aerial: float, wind_offset_m: Vector2, wind_toward_deg: float, sortie_seed: int,
		rain: float) -> void:
	if _env == null:
		_bind_environment()
	if _env == null:
		return
	time = clampi(time, 0, TIME_WORDS.size() - 1)
	weather_from = clampi(weather_from, 0, WEATHER_WORDS.size() - 1)
	weather_to = clampi(weather_to, 0, WEATHER_WORDS.size() - 1)
	blend = clampf(blend, 0.0, 1.0) if is_finite(blend) else 1.0
	if not sun_dir.is_finite() or sun_dir.length_squared() < 1e-6:
		sun_dir = sky_dir(DEFAULT_SUN_AZIMUTH_DEG, _time_look(time).static_sun_elevation_deg)
	sun_dir = sun_dir.normalized()
	var a := _preset(time, weather_from)
	var b := _preset(time, weather_to)
	var wa := _weather_look(weather_from)
	var wb := _weather_look(weather_to)
	var tl := _time_look(time)
	var night := time == 0

	# Sun (LIGHT0). By night it stays in the tree, dark and drawn in the sky only, so the moon stays LIGHT1.
	if sun != null:
		sun.transform = Transform3D(_basis_towards(-sun_dir), Vector3.ZERO)
		sun.light_volumetric_fog_energy = 0.0
		if night:
			sun.light_energy = 0.0
			sun.sky_mode = DirectionalLight3D.SKY_MODE_SKY_ONLY
			sun.shadow_enabled = false
		else:
			sun.light_color = _srgb(_mix3(a, b, "sun_color", blend))
			sun.light_energy = lerpf(float(a.get("sun_energy", 1.0)), float(b.get("sun_energy", 1.0)), blend)
			sun.sky_mode = DirectionalLight3D.SKY_MODE_LIGHT_AND_SKY
			sun.shadow_enabled = true
			sun.shadow_opacity = lerpf(wa.shadow_opacity, wb.shadow_opacity, blend)
	# Moon (LIGHT1): by night the preset's key light is the moon's.
	if moon != null:
		moon.visible = night
		moon.light_volumetric_fog_energy = 0.0
		if night:
			var sun_az := rad_to_deg(atan2(sun_dir.x, -sun_dir.z))
			var elev := lerpf(MOON_ELEVATION_MIN_DEG, MOON_ELEVATION_MAX_DEG, _seed_unit(sortie_seed, 1))
			moon.transform = Transform3D(_basis_towards(-sky_dir(sun_az + MOON_AZIMUTH_FROM_SUN_DEG, elev)), Vector3.ZERO)
			moon.light_color = _srgb(_mix3(a, b, "sun_color", blend))
			moon.light_energy = lerpf(float(a.get("sun_energy", 0.2)), float(b.get("sun_energy", 0.2)), blend)
			moon.sky_mode = DirectionalLight3D.SKY_MODE_LIGHT_AND_SKY
			# The moon path stays a path: at full specular the far glitter band, where the sea is roughest,
			# blooms into a bright dome on the horizon that reads as a moonrise under a moon high in the sky.
			moon.light_specular = MOON_SPECULAR
			moon.shadow_enabled = true
			moon.shadow_opacity = lerpf(wa.shadow_opacity, wb.shadow_opacity, blend)

	# Fog: Koschmieder density from the simulation, colour from the presets, no height fog.
	_env.fog_enabled = true
	_env.fog_mode = Environment.FOG_MODE_EXPONENTIAL
	_env.fog_density = maxf(fog_density, 0.0) if is_finite(fog_density) else 1.3e-4
	_env.fog_height_density = 0.0
	_env.fog_sky_affect = 0.0
	_env.fog_light_color = _srgb(_mix3(a, b, "fog_color", blend))
	_env.fog_sun_scatter = clampf(fog_sun_scatter, 0.0, 1.0) if is_finite(fog_sun_scatter) else 0.0
	if night:
		# Godot scatters every directional light into the fog. By night the key light is the moon, so the
		# L1871 rule reads the moon's elevation: a moon well up the sky lays no glowing dome on the horizon.
		var moon_elev := lerpf(MOON_ELEVATION_MIN_DEG, MOON_ELEVATION_MAX_DEG, _seed_unit(sortie_seed, 1))
		_env.fog_sun_scatter = 0.2 * (1.0 - smoothstep(10.0, 25.0, moon_elev))
	_env.fog_aerial_perspective = clampf(aerial, 0.0, 1.0) if is_finite(aerial) else 0.5
	# The same air for searchlight beams where the tier draws volumetric fog.
	_env.volumetric_fog_density = _env.fog_density
	_fog_energy = 1.0

	# Ambient: a share of the sky's radiance (so shadows stay directional and cool) and the rest the preset's
	# ambient colour, the sky share capped so the total never falls under the preset's floor (L1821).
	var ambient := _mix3(a, b, "ambient", blend)
	var ambient_floor := lerpf(float(a.get("ambient_floor", 0.1)), float(b.get("ambient_floor", 0.1)), blend)
	var share := lerpf(wa.ambient_sky_share, wb.ambient_sky_share, blend)
	var lum := _luminance(ambient)
	if lum > 1e-6:
		share = minf(share, 1.0 - ambient_floor / lum)
	_env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	_env.reflected_light_source = Environment.REFLECTION_SOURCE_SKY
	_env.ambient_light_color = _srgb(ambient)
	_env.ambient_light_sky_contribution = clampf(share, 0.0, 1.0)
	_ambient_energy = 1.0
	_env.ambient_light_energy = _ambient_energy

	# Exposure and grade. The LUT carries the preset's lift, gamma, gain and saturation.
	_env.tonemap_exposure = lerpf(float(a.get("exposure", 1.0)), float(b.get("exposure", 1.0)), blend)
	_bind_lut()

	# Sky.
	if _sky != null:
		var horizon := _mix3(a, b, "sky_horizon", blend)
		var deep := _sea_token("sea.deep")
		var key := _vec3((_sky_tokens(time).get("key_light", {}) as Dictionary).get("linear_rgb", [1.0, 1.0, 1.0]))
		_sky.set_shader_parameter(&"zenith_color", _srgb(_mix3(a, b, "sky_zenith", blend)))
		_sky.set_shader_parameter(&"horizon_color", _srgb(horizon))
		_sky.set_shader_parameter(&"ground_color", _srgb(deep.lerp(horizon, tl.ground_lift)))
		_sky.set_shader_parameter(&"overcast_color", _srgb(_mix3(a, b, "fog_color", blend)))
		var cirrus := horizon * tl.cirrus_night_gain if night else horizon.lerp(key, tl.cirrus_key_mix)
		_sky.set_shader_parameter(&"cirrus_color", _srgb(cirrus))
		_sky.set_shader_parameter(&"energy", tl.sky_energy)
		_sky.set_shader_parameter(&"horizon_curve", tl.horizon_curve)
		_sky.set_shader_parameter(&"sun_glow", tl.sun_glow * lerpf(wa.sun_glow, wb.sun_glow, blend))
		_sky.set_shader_parameter(&"sun_disc", tl.sun_disc)
		_sky.set_shader_parameter(&"overcast", lerpf(wa.overcast, wb.overcast, blend))
		_sky.set_shader_parameter(&"star_energy", tl.star_energy)
		_sky.set_shader_parameter(&"moon_phase", lerpf(MOON_PHASE_MIN, MOON_PHASE_MAX, _seed_unit(sortie_seed, 2)))
		_sky.set_shader_parameter(&"cirrus_cover", lerpf(wa.cirrus_cover, wb.cirrus_cover, blend))
		var toward := deg_to_rad(wind_toward_deg) if is_finite(wind_toward_deg) else 0.0
		_sky.set_shader_parameter(&"cirrus_dir", Vector2(sin(toward), -cos(toward)))
		var drift := wind_offset_m / CIRRUS_HEIGHT_M if wind_offset_m.is_finite() else Vector2.ZERO
		# Wrapped far from the origin to keep the shader's precision; the bands repeat long before 1024.
		_sky.set_shader_parameter(&"cirrus_offset", Vector2(fposmod(drift.x, 1024.0), fposmod(drift.y, 1024.0)))
		var amount := clampf(rain, 0.0, 1.0) if is_finite(rain) else 0.0
		_sky.set_shader_parameter(&"rain", lerpf(wa.rain_shafts, wb.rain_shafts, blend) * lerpf(0.5, 1.0, amount))
		if _flash_age_s < 0.0:
			_sky.set_shader_parameter(&"lightning", Vector4.ZERO)


## Unit vector towards a body at a compass azimuth (0 = north = -Z, clockwise) and elevation, degrees.
static func sky_dir(azimuth_deg: float, elevation_deg: float) -> Vector3:
	var az := deg_to_rad(azimuth_deg)
	var el := deg_to_rad(elevation_deg)
	return Vector3(sin(az) * cos(el), sin(el), -cos(az) * cos(el))


## Re-read the bound view's environment and apply it.
func refresh() -> void:
	_on_environment_changed()


func _on_environment_changed() -> void:
	if _view == null or not is_instance_valid(_view):
		return
	var e: Dictionary = _view.environment()
	var weather: Dictionary = e.get("weather", {})
	var wind_from := float(weather.get("wind_from_deg", 0.0))
	var sun_dir: Variant = e.get("sun_dir", Vector3.ZERO)
	var wind: Variant = e.get("wind_offset", Vector2.ZERO)
	apply(int(e.get("time_of_day", 1)), int(e.get("blend_from", 0)), int(e.get("blend_to", 0)), float(e.get("blend", 1.0)),
		sun_dir if sun_dir is Vector3 else Vector3.ZERO, float(e.get("fog_density", 1.3e-4)), float(e.get("fog_sun_scatter", 0.0)),
		float(e.get("aerial_perspective", 0.7)), wind if wind is Vector2 else Vector2.ZERO, wind_from + 180.0,
		int(e.get("cloud_seed", 0)), float(weather.get("rain", 0.0)))


## A lightning pulse starts now (the view already holds them back under Reduce flashing or the lightning
## setting; checked again here so a stale signal never flashes).
func _on_lightning(intensity: float, duration_s: float, bearing_deg: float) -> void:
	if bool(Settings.get_value(&"access.reduce_flashing")) or not bool(Settings.get_value(&"graphics.lightning")):
		return
	if not (is_finite(intensity) and is_finite(duration_s) and is_finite(bearing_deg)):
		return
	_flash_intensity = clampf(intensity, 0.0, 1.0)
	_flash_len_s = clampf(duration_s, 0.08, 0.15)
	var b := deg_to_rad(bearing_deg)
	_flash_dir = Vector3(sin(b), 0.3, -cos(b)).normalized()
	_flash_age_s = 0.0
	set_process(true)
	_flash_step()


func _process(delta: float) -> void:
	if _flash_age_s < 0.0:
		set_process(false)
		return
	_flash_age_s += maxf(delta, 0.0)
	_flash_step()


## One frame of the pulse: full for the hold, then a quadratic fall to nothing at the end.
func _flash_step() -> void:
	var t := _flash_age_s / _flash_len_s
	var level := 0.0
	if t < 1.0:
		level = 1.0 if t < LIGHTNING_HOLD else pow(1.0 - (t - LIGHTNING_HOLD) / (1.0 - LIGHTNING_HOLD), 2.0)
	else:
		_flash_age_s = -1.0
	level *= _flash_intensity
	if _sky != null:
		_sky.set_shader_parameter(&"lightning", Vector4(_flash_dir.x, _flash_dir.y, _flash_dir.z, level * LIGHTNING_SKY_GAIN))
	if _env != null:
		_env.ambient_light_energy = _ambient_energy * (1.0 + level * LIGHTNING_AMBIENT_GAIN)
		_env.fog_light_energy = _fog_energy * (1.0 + level * LIGHTNING_FOG_GAIN)


func _bind_environment() -> void:
	if world_environment == null or world_environment.environment == null:
		return
	_env = world_environment.environment
	var sky := _env.sky
	if sky != null and sky.sky_material is ShaderMaterial:
		_sky = sky.sky_material as ShaderMaterial


## The view's LUT exists once its first weather event is in; bound once, then Rust updates it in place.
func _bind_lut() -> void:
	if _lut_bound or _view == null or not is_instance_valid(_view) or _env == null:
		return
	var lut: ImageTexture3D = _view.lut()
	if lut == null:
		return
	_env.adjustment_enabled = true
	_env.adjustment_brightness = 1.0
	_env.adjustment_contrast = 1.0
	_env.adjustment_saturation = 1.0
	_env.adjustment_color_correction = lut
	_lut_bound = true


# --- data ---------------------------------------------------------------------------------------------

static func _load_tokens() -> void:
	if not _tokens.is_empty():
		return
	var parsed: Variant = null
	if FileAccess.file_exists(TOKENS_PATH):
		parsed = JSON.parse_string(FileAccess.get_file_as_string(TOKENS_PATH))
	var root: Dictionary = parsed if parsed is Dictionary else {}
	if root.is_empty():
		push_error("world environment: %s is missing or unreadable; the world draws with neutral light" % TOKENS_PATH)
	var presets: Dictionary = {}
	for p: Variant in root.get("presets", []):
		if p is Dictionary:
			presets["%s/%s" % [p.get("time", ""), p.get("weather", "")]] = p
	var skies: Dictionary = {}
	for s: Variant in root.get("sky", []):
		if s is Dictionary:
			skies[String(s.get("time", ""))] = s
	var sea: Dictionary = {}
	for s: Variant in root.get("sea", []):
		if s is Dictionary:
			sea[String(s.get("token", ""))] = s
	_tokens = {"presets": presets, "sky": skies, "sea": sea}


## The token preset of a time and weather; the dusk-clear preset, then an empty one, when missing.
static func _preset(time: int, weather: int) -> Dictionary:
	_load_tokens()
	var presets: Dictionary = _tokens["presets"]
	var key := "%s/%s" % [TIME_WORDS[time], WEATHER_WORDS[weather]]
	if presets.has(key):
		return presets[key]
	return presets.get("dusk/clear", {})


static func _sky_tokens(time: int) -> Dictionary:
	_load_tokens()
	return (_tokens["sky"] as Dictionary).get(TIME_WORDS[time], {})


## A sea token's linear colour.
static func sea_token(token: String) -> Vector3:
	return _sea_token(token)


static func _sea_token(token: String) -> Vector3:
	_load_tokens()
	var entry: Dictionary = (_tokens["sea"] as Dictionary).get(token, {})
	return _vec3(entry.get("linear_rgb", [0.0, 0.0, 0.0]))


static func _weather_look(weather: int) -> WorldWeatherLook:
	var res := load(PRESET_DIR + WEATHER_WORDS[weather] + ".tres") as WorldWeatherLook
	return res if res != null else WorldWeatherLook.new()


static func _time_look(time: int) -> WorldTimeLook:
	var res := load(PRESET_DIR + TIME_WORDS[time] + ".tres") as WorldTimeLook
	return res if res != null else WorldTimeLook.new()


static func _vec3(v: Variant) -> Vector3:
	if v is Array and (v as Array).size() >= 3:
		var a: Array = v
		var out := Vector3(float(a[0]), float(a[1]), float(a[2]))
		return out if out.is_finite() else Vector3.ZERO
	return Vector3.ZERO


## A token colour field of two presets, mixed in linear light.
static func _mix3(a: Dictionary, b: Dictionary, field: String, t: float) -> Vector3:
	return _vec3(a.get(field, [0.0, 0.0, 0.0])).lerp(_vec3(b.get(field, [0.0, 0.0, 0.0])), t)


## Rec. 709 luminance of a linear colour.
static func _luminance(c: Vector3) -> float:
	return 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z


## A linear token colour as the sRGB-encoded Color that Godot's properties and source_color uniforms take.
static func srgb(c: Vector3) -> Color:
	return _srgb(c)


static func _srgb(c: Vector3) -> Color:
	return Color(maxf(c.x, 0.0), maxf(c.y, 0.0), maxf(c.z, 0.0)).linear_to_srgb()


## A deterministic 0..1 value from the sortie seed, one per `salt`.
static func _seed_unit(sortie_seed: int, salt: int) -> float:
	var h := hash([sortie_seed, salt])
	return float(h & 0xFFFF) / 65535.0


## Basis whose -Z points along `forward` (lights shine down their -Z).
static func _basis_towards(forward: Vector3) -> Basis:
	var up := Vector3.UP if absf(forward.normalized().y) < 0.999 else Vector3.FORWARD
	return Basis.looking_at(forward, up)
