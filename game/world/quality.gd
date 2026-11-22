extends Node
## Applies Quality.profile() to the viewport and environment: 3D scale and upscaler, MSAA or SMAA,
## shadow splits, distance and atlas, glow levels, SSAO, SSR, volumetric fog, max_fps and V-Sync.
## DESIGN.md L1835-1853, L1877-1883, L1975-2035. Unit: world-scene.
##
## Applied when the node enters the tree, on Quality.changed and when a setting it reads changes
## (graphics.vsync, graphics.pcss). It then calls BpWorldView.apply_quality on the view it was given, so the
## render units follow the same profile. Used by the sortie world and the dry-dock stage.
##
## Fixed on every tier and written in the scene's Environment, not here: AgX with agx_white 8.0 and
## agx_contrast 1.25, glow blend mode Screen with hdr threshold 1.0 and bloom 0, no SSIL, no SDFGI, background
## mode sky. Settings outside the profile that the table fixes per tier are kept below as constants.
## Reads and calls: Quality.profile, Quality.changed, Settings.get_value, Settings.changed,
## BpWorldView.apply_quality.

## Shadowed positional lights per tier (L1877-1881): Low 0, Medium 4, High 16. Each is one atlas cell.
const POSITIONAL_SHADOWS: Dictionary = {0: 0, 1: 4, 2: 16, 3: 0}
## Positional shadow atlas edge for those cells.
const POSITIONAL_ATLAS: Dictionary = {0: 0, 1: 2048, 2: 4096, 3: 0}
## SSAO (L1837): radius 1 m, fading out from 50 m to 300 m; Low quality at half size on Medium, the
## Medium quality on High.
const SSAO_RADIUS_M := 1.0
const SSAO_FADE_FROM_M := 50.0
const SSAO_FADE_TO_M := 300.0
## SSR on High (L1838): water only, half size, 32 steps. Hulls are matte enough to fall outside it.
const SSR_STEPS := 32
## Volumetric fog on High (L1839): searchlights, flares and muzzle light within 96 m.
const VOLUMETRIC_FOG_LENGTH_M := 96.0
## Sun angular diameter for PCSS (High tier option), degrees.
const SUN_ANGULAR_DIAMETER_DEG := 0.53
## Render layers no light's shadow takes casters from: VFX (L1883) and dry-dock overlays.
const LAYER_VFX := 4
const LAYER_DOCK_OVERLAYS := 6
## Glow level weights inside the profile's [glow_min, glow_max] range (1-based levels).
const GLOW_LEVEL_WEIGHT := 1.0

@export var world_environment: WorldEnvironment
## Directional lights to configure (sun first).
@export var lights: Array[DirectionalLight3D] = []
## The world view to pass the profile on to, when the scene has one.
@export var view: BpWorldView

var _applied: Dictionary = {}


func _ready() -> void:
	var quality := get_node_or_null(^"/root/Quality")
	if quality != null and quality.has_signal(&"changed"):
		quality.connect(&"changed", apply)
	var settings := get_node_or_null(^"/root/Settings")
	if settings != null and settings.has_signal(&"changed"):
		settings.connect(&"changed", _on_setting_changed)
	apply()


func _exit_tree() -> void:
	# Screens with no 3D draw at native resolution again.
	var vp := get_viewport()
	if vp != null:
		vp.scaling_3d_scale = 1.0
		vp.scaling_3d_mode = Viewport.SCALING_3D_MODE_BILINEAR


func _on_setting_changed(key: StringName) -> void:
	if key == &"graphics.vsync" or key == &"graphics.pcss":
		apply()


## The profile last applied (Quality.profile shape), for tests and captures.
func applied() -> Dictionary:
	return _applied


## Read the profile and apply it to this scene's viewport, environment and lights, then to the view.
func apply() -> void:
	var p: Dictionary = Quality.profile()
	_applied = p
	var tier := int(p.get("tier", 1))
	var fallback := bool(p.get("safe_mode", false))
	_apply_viewport(p, tier, fallback)
	_apply_shadows(p, tier, fallback)
	_apply_environment(p, fallback)
	_apply_frame_pacing(p)
	if view != null and is_instance_valid(view):
		view.apply_quality()


func _apply_viewport(p: Dictionary, tier: int, fallback: bool) -> void:
	var vp := get_viewport()
	if vp == null:
		return
	var scale := clampf(float(p.get("scale_3d", 1.0)), 0.25, 1.0)
	var upscaler := StringName(p.get("upscaler", &"none"))
	var mode := Viewport.SCALING_3D_MODE_BILINEAR
	if not fallback and scale < 0.999:
		if upscaler == &"metalfx_spatial" and RenderingServer.get_current_rendering_driver_name() == "metal":
			mode = Viewport.SCALING_3D_MODE_METALFX_SPATIAL
		elif upscaler == &"fsr1" or upscaler == &"metalfx_spatial":
			mode = Viewport.SCALING_3D_MODE_FSR
	vp.scaling_3d_mode = mode
	vp.scaling_3d_scale = scale
	match int(p.get("msaa", 2)):
		4:
			vp.msaa_3d = Viewport.MSAA_4X
		2:
			vp.msaa_3d = Viewport.MSAA_2X
		_:
			vp.msaa_3d = Viewport.MSAA_DISABLED
	vp.screen_space_aa = Viewport.SCREEN_SPACE_AA_SMAA if bool(p.get("smaa", false)) else Viewport.SCREEN_SPACE_AA_DISABLED
	# No default uses temporal AA or motion vectors (gate T, L1850).
	vp.use_taa = false
	vp.use_debanding = true
	var cells: int = POSITIONAL_SHADOWS.get(tier, 0)
	vp.positional_shadow_atlas_size = POSITIONAL_ATLAS.get(tier, 0)
	vp.positional_shadow_atlas_16_bits = true
	# 4 cells: one per quadrant; 16: four per quadrant.
	var subdiv := Viewport.SHADOW_ATLAS_QUADRANT_SUBDIV_4 if cells > 4 else Viewport.SHADOW_ATLAS_QUADRANT_SUBDIV_1
	for q: int in 4:
		vp.set_positional_shadow_atlas_quadrant_subdiv(q, subdiv if cells > 0 else Viewport.SHADOW_ATLAS_QUADRANT_SUBDIV_DISABLED)


func _apply_shadows(p: Dictionary, tier: int, fallback: bool) -> void:
	var splits := int(p.get("shadow_splits", 2))
	var distance := maxf(float(p.get("shadow_distance_m", 250.0)), 10.0)
	var atlas := clampi(int(p.get("shadow_atlas", 2048)), 512, 16384)
	RenderingServer.directional_shadow_atlas_set_size(atlas, true)
	var soft_high := StringName(p.get("shadow_filter", &"soft_low")) == &"soft_high"
	var filter := RenderingServer.SHADOW_QUALITY_SOFT_HIGH if soft_high else RenderingServer.SHADOW_QUALITY_SOFT_LOW
	RenderingServer.directional_soft_shadow_filter_set_quality(filter)
	RenderingServer.positional_soft_shadow_filter_set_quality(filter)
	# PCSS is the High tier's option (L1881): a sun of real angular size softens contact shadows with distance.
	var pcss := tier == 2 and not fallback and bool(Settings.get_value(&"graphics.pcss"))
	var mask := 0xFFFFFFFF & ~(1 << (LAYER_VFX - 1)) & ~(1 << (LAYER_DOCK_OVERLAYS - 1))
	for light: DirectionalLight3D in lights:
		if light == null or not is_instance_valid(light):
			continue
		match splits:
			4:
				light.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_4_SPLITS
			2:
				light.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_2_SPLITS
			_:
				light.directional_shadow_mode = DirectionalLight3D.SHADOW_ORTHOGONAL
		light.directional_shadow_max_distance = distance
		light.directional_shadow_blend_splits = true
		light.shadow_blur = 1.0
		light.shadow_caster_mask = mask
		light.light_angular_distance = SUN_ANGULAR_DIAMETER_DEG if pcss else 0.0


func _apply_environment(p: Dictionary, fallback: bool) -> void:
	if world_environment == null or world_environment.environment == null:
		return
	var env := world_environment.environment
	var glow_min := int(p.get("glow_min", 2))
	var glow_max := int(p.get("glow_max", 3))
	env.glow_enabled = glow_max > 0
	for level: int in range(1, 8):
		env.set_glow_level(level - 1, GLOW_LEVEL_WEIGHT if level >= glow_min and level <= glow_max else 0.0)
	RenderingServer.environment_glow_set_use_bicubic_upscale(bool(p.get("glow_bicubic", false)))
	var ssao := StringName(p.get("ssao", &"off"))
	env.ssao_enabled = ssao != &"off" and not fallback
	env.ssao_radius = SSAO_RADIUS_M
	if ssao == &"medium":
		RenderingServer.environment_set_ssao_quality(RenderingServer.ENV_SSAO_QUALITY_MEDIUM, false, 0.5, 2, SSAO_FADE_FROM_M, SSAO_FADE_TO_M)
	else:
		RenderingServer.environment_set_ssao_quality(RenderingServer.ENV_SSAO_QUALITY_LOW, true, 0.5, 2, SSAO_FADE_FROM_M, SSAO_FADE_TO_M)
	env.ssr_enabled = bool(p.get("ssr", false)) and not fallback
	env.ssr_max_steps = SSR_STEPS
	RenderingServer.environment_set_ssr_half_size(true)
	env.volumetric_fog_enabled = bool(p.get("volumetric_fog", false)) and not fallback
	env.volumetric_fog_length = VOLUMETRIC_FOG_LENGTH_M
	env.volumetric_fog_sky_affect = 0.0
	env.volumetric_fog_ambient_inject = 0.0
	env.volumetric_fog_gi_inject = 0.0
	# Always off on every tier (L1848).
	env.ssil_enabled = false
	env.sdfgi_enabled = false


func _apply_frame_pacing(p: Dictionary) -> void:
	Engine.max_fps = maxi(int(p.get("max_fps", 0)), 0)
	if DisplayServer.get_name() == "headless":
		return
	var vsync := bool(Settings.get_value(&"graphics.vsync"))
	DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_ENABLED if vsync else DisplayServer.VSYNC_DISABLED)
