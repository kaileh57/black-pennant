extends Node3D
## Warm-up scene: draws every material and effect variant once at load.
## DESIGN.md L1358, L1973. Unit: world-scene.
## Reads and calls: BpWorldView.warm_up, BpWorldView.warmed_up.
##
## Boot instances this scene behind the precompile screen and waits for the `warmed_up` of its BpWorldView
## (World/View, an instance of res://world/world.tscn). There is no session at boot, so the view has
## nothing of its own to draw: this script puts one sample of every project shader in front of the world
## camera, under the real world environment (sky shader, fog, shadows, glow, the quality profile's MSAA),
## lets them draw for SAMPLE_FRAMES frames so their pipelines compile, and only then asks the view to warm
## up, so `warmed_up` follows the samples. A sortie's own load repeats the warm-up with the real meshes, VFX
## instances and Layer B emitters (BpWorldView.warm_up, called by res://world/world.gd).
##
## Samples use the vertex formats of the real meshes where the shader reads them: hull, island, water and
## cloud as position and normal triangles (water and cloud with CUSTOM0), parts and both VFX kinds as a
## MultiMesh with colour and custom data, and the particle process shader on a GPUParticles3D asked to
## process 0.1 s. Every sample casts a shadow except water and cloud, as in a sortie. The sky is drawn by
## the environment.

const SHADERS: Dictionary = {
	&"hull": "res://shaders/hull.gdshader",
	&"part": "res://shaders/part.gdshader",
	&"water": "res://shaders/water.gdshader",
	&"island": "res://shaders/island.gdshader",
	&"cloud": "res://shaders/cloud.gdshader",
	&"vfx_opaque": "res://shaders/vfx_opaque.gdshader",
	&"vfx_blend": "res://shaders/vfx_blend.gdshader",
	&"vfx_particles": "res://shaders/vfx_particles.gdshader",
}
## Frames the samples stay in view before the view is asked to warm up.
const SAMPLE_FRAMES := 4
## Camera height, distance of the sample row in front of it and spacing between samples, metres.
const CAMERA_HEIGHT_M := 4.0
const SAMPLE_DEPTH_M := 14.0
const SAMPLE_STEP_M := 2.4

@onready var _world: Node3D = $World
@onready var _view: BpWorldView = $World/View
@onready var _camera: Camera3D = $World/Camera

var _samples: Node3D
var _frames: int = -1


func _ready() -> void:
	_camera.transform = Transform3D(Basis.IDENTITY, Vector3(0.0, CAMERA_HEIGHT_M, 0.0))
	_samples = Node3D.new()
	_samples.name = &"Samples"
	_samples.transform = _camera.transform
	add_child(_samples)
	_place_samples()
	_frames = 0


func _process(_delta: float) -> void:
	if _frames < 0:
		return
	_frames += 1
	if _frames >= SAMPLE_FRAMES:
		_frames = -1
		_view.warm_up()


func _place_samples() -> void:
	var names: Array = SHADERS.keys()
	var count := names.size()
	for i: int in count:
		var key: StringName = names[i]
		var shader := load(SHADERS[key]) as Shader
		if shader == null:
			push_warning("warmup: %s did not load" % SHADERS[key])
			continue
		var node: Node3D
		match key:
			&"vfx_particles":
				node = _particles(shader)
			&"part", &"vfx_opaque", &"vfx_blend":
				node = _multimesh(shader)
			_:
				var custom := key == &"water" or key == &"cloud"
				var instance := MeshInstance3D.new()
				instance.mesh = _mesh(custom)
				instance.material_override = _material(shader)
				if custom:
					instance.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
				node = instance
		node.name = String(key).to_pascal_case()
		node.transform = Transform3D(Basis.IDENTITY, Vector3((float(i) - float(count - 1) * 0.5) * SAMPLE_STEP_M, 0.0, -SAMPLE_DEPTH_M))
		_samples.add_child(node)
		if node is GPUParticles3D:
			(node as GPUParticles3D).request_particles_process(0.1)


func _material(shader: Shader) -> ShaderMaterial:
	var material := ShaderMaterial.new()
	material.shader = shader
	return material


## A unit box as position and normal triangles (the hull mesh format), with CUSTOM0 when asked.
func _mesh(with_custom: bool) -> ArrayMesh:
	var src: Array = BoxMesh.new().get_mesh_arrays()
	var arrays: Array = []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = src[Mesh.ARRAY_VERTEX]
	arrays[Mesh.ARRAY_NORMAL] = src[Mesh.ARRAY_NORMAL]
	arrays[Mesh.ARRAY_INDEX] = src[Mesh.ARRAY_INDEX]
	var flags := 0
	if with_custom:
		var n: int = (src[Mesh.ARRAY_VERTEX] as PackedVector3Array).size()
		var custom := PackedFloat32Array()
		custom.resize(n * 4)
		for v: int in n:
			# Water: level spacing 1 m, no morph, level 0. Cloud: a mid wrap term.
			custom[v * 4] = 1.0 if v % 2 == 0 else 0.5
		arrays[Mesh.ARRAY_CUSTOM0] = custom
		flags = Mesh.ARRAY_CUSTOM_RGBA_FLOAT << Mesh.ARRAY_FORMAT_CUSTOM0_SHIFT
	var mesh := ArrayMesh.new()
	mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays, [], {}, flags)
	return mesh


func _multimesh(shader: Shader) -> MultiMeshInstance3D:
	var mm := MultiMesh.new()
	mm.transform_format = MultiMesh.TRANSFORM_3D
	mm.use_colors = true
	mm.use_custom_data = true
	mm.mesh = _mesh(false)
	mm.instance_count = 1
	mm.set_instance_transform(0, Transform3D.IDENTITY)
	var node := MultiMeshInstance3D.new()
	node.multimesh = mm
	node.material_override = _material(shader)
	return node


func _particles(shader: Shader) -> GPUParticles3D:
	var particles := GPUParticles3D.new()
	particles.process_material = _material(shader)
	particles.amount = 8
	particles.local_coords = true
	particles.fixed_fps = 30
	particles.interpolate = true
	particles.draw_order = GPUParticles3D.DRAW_ORDER_INDEX
	particles.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	var draw := _mesh(false)
	draw.surface_set_material(0, _material(load(SHADERS[&"vfx_opaque"]) as Shader))
	particles.draw_pass_1 = draw
	particles.emitting = true
	return particles
