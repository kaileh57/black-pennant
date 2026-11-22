extends Node3D
## Windowed look test of the hull, part and VFX shaders (units shader-hull, shader-vfx). Not a gameplay scene:
## it builds its own dusk environment from res://art/tokens/world_tokens.json, a test sea displaced by the shared
## wave include, a hull, two turrets and a VFX showcase, then captures each shot to PNG and quits.
##   godot --path game --resolution 1600x900 res://test/capture/shader_look.tscn -- \
##     --out=<dir> [--hull=<hull fixture>] [--vfx=<vfx fixture>] [--waves=<wave json>] [--shots=hero,close]
## Fixtures are optional throwaway dumps: a hull fixture ("BPHD": grid size, waterline, deck, palette, then
## per chunk its coordinate, positions, normals, indices and the 34^3 RGBA8 volume, as hull-mesh builds them),
## a VFX fixture ("BPVX": every VfxKind base mesh, plus `<path>.inst` instances from the runtime builders)
## and a wave json ({"mats": 8 x 16 floats, "t_ref": t}). Without a hull fixture a procedural block hull is
## built here in the same vertex format and volume layout. Headless runs never compile shaders; run windowed.

const CELL := 0.5
const EDGE := 34
const SHOTS := {
	"hero": [Vector3(46.0, 8.5, 52.0), Vector3(2.0, 3.0, 0.0), 50.0],
	"close": [Vector3(15.0, 3.6, 13.0), Vector3(6.0, 2.6, 4.0), 50.0],
	"deck": [Vector3(-8.0, 13.0, 16.0), Vector3(-2.0, 5.0, 0.0), 55.0],
	"damage": [Vector3(26.0, 4.0, 15.0), Vector3(16.0, 2.6, 4.5), 45.0],
	"vfx": [Vector3(70.0, 14.0, 80.0), Vector3(0.0, 4.0, 10.0), 55.0],
	"wake": [Vector3(-10.0, 22.0, 55.0), Vector3(-75.0, 0.0, 0.0), 55.0],
	"fire": [Vector3(-2.0, 9.0, 22.0), Vector3(-14.0, 5.0, 1.0), 50.0],
	"far": [Vector3(250.0, 30.0, 170.0), Vector3(0.0, 3.0, 0.0), 30.0],
	"sheet_a": [Vector3(270.0, 42.0, 48.0), Vector3(270.0, 2.0, -16.0), 62.0],
	"sheet_b": [Vector3(270.0, 42.0, -52.0), Vector3(270.0, 2.0, -116.0), 62.0],
	"particles": [Vector3(30.0, 8.0, -110.0), Vector3(30.0, 4.0, -150.0), 55.0],
	"lod": [Vector3(46.0, 10.0, 52.0), Vector3(-600.0, 4.0, -500.0), 12.0],
}

var _tokens: Dictionary = {}
var _args: Dictionary = {}
var _cam: Camera3D
var _shots: PackedStringArray = []
var _shot := 0
var _frames := 0
var _out_dir := ""
var _damaged := false


func _ready() -> void:
	# The project check instantiates every scene headless: there is nothing to render, so do nothing.
	if DisplayServer.get_name() == "headless":
		set_process(false)
		return
	for a: String in OS.get_cmdline_user_args():
		if a.begins_with("--") and a.contains("="):
			var kv := a.substr(2).split("=", true, 1)
			_args[kv[0]] = kv[1]
	_out_dir = str(_args.get("out", "user://captures"))
	DirAccess.make_dir_recursive_absolute(_out_dir)
	_shots = str(_args.get("shots", "hero,close,deck,damage,vfx,far,lod")).split(",")
	_damaged = str(_args.get("hull", "")).contains("dmg")
	_tokens = JSON.parse_string(FileAccess.get_file_as_string("res://art/tokens/world_tokens.json"))
	print("shader_look: driver ", RenderingServer.get_current_rendering_driver_name(), ", method ", RenderingServer.get_current_rendering_method())
	_register_globals()
	_build_environment()
	_build_sea()
	var hull_path := str(_args.get("hull", ""))
	var ship := Node3D.new()
	add_child(ship)
	var info: Dictionary = _load_hull(hull_path) if hull_path != "" else _procedural_hull()
	_add_hull(ship, info, _args.has("lod_near"))
	_add_parts(ship, info)
	var far_ship := Node3D.new()
	far_ship.position = Vector3(-600.0, 0.0, -500.0)
	far_ship.rotation.y = 0.5
	add_child(far_ship)
	_add_hull(far_ship, info, true)
	var vfx_path := str(_args.get("vfx", ""))
	if vfx_path != "":
		var meshes := _load_vfx_meshes(vfx_path)
		if not meshes.is_empty():
			_add_vfx(vfx_path, meshes)
			_add_sheet(meshes)
			_add_particles(meshes)
	_cam = Camera3D.new()
	_cam.far = 20000.0
	add_child(_cam)
	_apply_shot()


func _process(_delta: float) -> void:
	_frames += 1
	if _frames < 60:
		return
	var img := get_viewport().get_texture().get_image()
	var path := _out_dir.path_join("w2_%s%s.png" % [_shots[_shot], "_dmg" if _damaged else ""])
	img.save_png(path)
	print("shader_look: saved ", path)
	_shot += 1
	_frames = 0
	if _shot >= _shots.size():
		get_tree().quit()
		return
	_apply_shot()


func _apply_shot() -> void:
	var s: Array = SHOTS.get(_shots[_shot], SHOTS["hero"])
	_cam.position = s[0]
	_cam.fov = s[2]
	_cam.look_at(s[1], Vector3.UP)


# ---- tokens -------------------------------------------------------------------------------------------

func _lin(entry: Dictionary) -> Color:
	var v: Array = entry["linear_rgb"]
	return Color(float(v[0]), float(v[1]), float(v[2]))


func _token(group: String, name: String) -> Dictionary:
	for e: Dictionary in _tokens[group]:
		if e.get("token", "") == name:
			return e
	return {}


func _sky(time: String) -> Dictionary:
	for e: Dictionary in _tokens["sky"]:
		if e["time"] == time:
			return e
	return {}


func _fog(weather: String, time: String) -> Dictionary:
	for e: Dictionary in _tokens["fog"]:
		if e["weather"] == weather and e["time"] == time:
			return e
	return {}


# ---- globals, environment, sea -------------------------------------------------------------------------

func _register_globals() -> void:
	var mats: Array = []
	var t_ref := 0.0
	var wp := str(_args.get("waves", ""))
	if wp != "" and FileAccess.file_exists(wp):
		var j: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(wp))
		mats = j["mats"]
		t_ref = float(j["t_ref"])
	for i in 8:
		var p := Projection()
		if i < mats.size():
			var m: Array = mats[i]
			p = Projection(Vector4(m[0], m[1], m[2], m[3]), Vector4(m[4], m[5], m[6], m[7]), Vector4(m[8], m[9], m[10], m[11]), Vector4(m[12], m[13], m[14], m[15]))
		else:
			p = Projection(Vector4.ZERO, Vector4.ZERO, Vector4.ZERO, Vector4.ZERO)
		_global("bp_waves_%d" % i, RenderingServer.GLOBAL_VAR_TYPE_MAT4, p)
	_global("bp_waves_t_ref", RenderingServer.GLOBAL_VAR_TYPE_FLOAT, t_ref)
	var noise := FastNoiseLite.new()
	noise.noise_type = FastNoiseLite.TYPE_VALUE_CUBIC
	noise.frequency = 0.012
	var tex := NoiseTexture2D.new()
	tex.width = 512
	tex.height = 512
	tex.seamless = true
	tex.noise = noise
	var gen := _tex_blob("res://art/gen/noise_cloud_shadow.bin")
	_global("bp_cloud_shadow", RenderingServer.GLOBAL_VAR_TYPE_SAMPLER2D, gen if gen != null else tex)
	_global("bp_cloud_shadow_xf", RenderingServer.GLOBAL_VAR_TYPE_VEC4, Vector4(0.37, 0.61, 1.0 / 900.0, 0.3))


func _global(name: StringName, type: RenderingServer.GlobalShaderParameterType, value: Variant) -> void:
	# The project may already register it (project.godot [shader_globals]); remove then add with test data.
	if ProjectSettings.has_setting("shader_globals/" + name):
		RenderingServer.global_shader_parameter_set(name, value)
	else:
		RenderingServer.global_shader_parameter_add(name, type, value)


func _build_environment() -> void:
	var dusk := _sky("dusk")
	var sun_elev := float(_args.get("sun_elev", "10"))
	var sun_az := float(_args.get("sun_az", "55"))
	var sun := DirectionalLight3D.new()
	sun.light_color = _lin(dusk["key_light"]).linear_to_srgb()
	sun.light_energy = float(_args.get("sun", "2.0"))
	sun.shadow_enabled = true
	sun.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_4_SPLITS
	sun.directional_shadow_max_distance = 500.0
	sun.shadow_blur = 1.0
	sun.shadow_normal_bias = 1.5
	sun.rotation_degrees = Vector3(-sun_elev, sun_az, 0.0)
	add_child(sun)
	var sky_shader := Shader.new()
	sky_shader.code = """
shader_type sky;
uniform vec3 zenith;
uniform vec3 horizon;
uniform vec3 sun_col;
void sky() {
	float h = clamp(EYEDIR.y, -1.0, 1.0);
	float up = pow(clamp(h, 0.0, 1.0), 0.45);
	vec3 c = mix(horizon, zenith, up);
	if (h < 0.0) { c = horizon * mix(0.6, 0.25, clamp(-h * 4.0, 0.0, 1.0)); }
	float sd = dot(EYEDIR, LIGHT0_DIRECTION);
	c += sun_col * (pow(max(sd, 0.0), 6.0) * 0.6 + smoothstep(0.9995, 0.9998, sd) * 30.0);
	COLOR = c;
}
"""
	var sky_mat := ShaderMaterial.new()
	sky_mat.shader = sky_shader
	sky_mat.set_shader_parameter("zenith", _v3(_lin(dusk["zenith"])))
	sky_mat.set_shader_parameter("horizon", _v3(_lin(dusk["horizon"])))
	sky_mat.set_shader_parameter("sun_col", _v3(_lin(dusk["key_light"])))
	var sky := Sky.new()
	sky.sky_material = sky_mat
	sky.radiance_size = Sky.RADIANCE_SIZE_256
	var env := Environment.new()
	env.background_mode = Environment.BG_SKY
	env.sky = sky
	env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	env.ambient_light_energy = 1.0
	env.reflected_light_source = Environment.REFLECTION_SOURCE_SKY
	env.tonemap_mode = Environment.TONE_MAPPER_AGX
	env.tonemap_white = 8.0
	if "tonemap_agx_contrast" in env:
		env.set("tonemap_agx_contrast", 1.25)
	env.tonemap_exposure = 1.0
	env.fog_enabled = true
	env.fog_light_color = _lin(_fog("clear", "dusk")).linear_to_srgb()
	env.fog_density = 1.3e-4
	env.fog_sun_scatter = 0.2 * (1.0 - smoothstep(10.0, 25.0, sun_elev))
	env.fog_aerial_perspective = 0.7
	env.fog_height_density = 0.0
	env.glow_enabled = true
	env.glow_hdr_threshold = 1.0
	env.glow_blend_mode = Environment.GLOW_BLEND_MODE_SCREEN
	env.glow_bloom = 0.0
	for lvl in 7:
		env.set_glow_level(lvl, 1.0 if lvl >= 1 and lvl <= 3 else 0.0)
	env.ssao_enabled = true
	env.ssao_radius = 1.0
	var we := WorldEnvironment.new()
	we.environment = env
	add_child(we)
	get_viewport().msaa_3d = Viewport.MSAA_2X


func _v3(c: Color) -> Vector3:
	return Vector3(c.r, c.g, c.b)


func _build_sea() -> void:
	var sh := Shader.new()
	sh.code = """
shader_type spatial;
render_mode world_vertex_coords;
#include "res://shaders/gen/waves.gdshaderinc"
uniform vec3 deep;
uniform vec3 scatter;
varying vec3 wn;
void vertex() {
	VERTEX = wave_displace(VERTEX.xz, TIME);
	wn = wave_normal(VERTEX.xz, TIME);
}
void fragment() {
	NORMAL = normalize((VIEW_MATRIX * vec4(wn, 0.0)).xyz);
	float crest = clamp(wn.y < 0.98 ? (0.98 - wn.y) * 6.0 : 0.0, 0.0, 1.0);
	ALBEDO = mix(deep, scatter, crest * 0.5);
	ROUGHNESS = 0.09;
	SPECULAR = 0.5;
}
"""
	var mat := ShaderMaterial.new()
	mat.shader = sh
	mat.set_shader_parameter("deep", _v3(_lin(_token("sea", "sea.deep"))))
	mat.set_shader_parameter("scatter", _v3(_lin(_token("sea", "sea.scatter"))))
	var pm := PlaneMesh.new()
	pm.size = Vector2(4000.0, 4000.0)
	pm.subdivide_width = 1200
	pm.subdivide_depth = 1200
	pm.material = mat
	var mi := MeshInstance3D.new()
	mi.mesh = pm
	add_child(mi)


# ---- hull ----------------------------------------------------------------------------------------------

func _load_hull(path: String) -> Dictionary:
	var b := FileAccess.get_file_as_bytes(path)
	if b.size() < 32 or b.slice(0, 4).get_string_from_ascii() != "BPHD":
		push_error("shader_look: bad hull fixture " + path)
		return _procedural_hull()
	var o := 8
	var size := Vector3i(b.decode_u32(o), b.decode_u32(o + 4), b.decode_u32(o + 8))
	o += 12
	var wl := b.decode_s32(o)
	var deck := b.decode_s32(o + 4)
	o += 8
	var np := b.decode_u32(o)
	o += 4
	var palette := PackedVector3Array()
	for i in np:
		palette.append(Vector3(b.decode_float(o), b.decode_float(o + 4), b.decode_float(o + 8)))
		o += 12
	var chunks: Array = []
	var nc := b.decode_u32(o)
	o += 4
	for c in nc:
		var coord := Vector3i(b.decode_u32(o), b.decode_u32(o + 4), b.decode_u32(o + 8))
		var nv := b.decode_u32(o + 12)
		var ni := b.decode_u32(o + 16)
		o += 20
		var pos := b.slice(o, o + nv * 12).to_float32_array()
		o += nv * 12
		var nrm := b.slice(o, o + nv * 12).to_float32_array()
		o += nv * 12
		var idx := b.slice(o, o + ni * 4).to_int32_array()
		o += ni * 4
		var vol := b.slice(o, o + EDGE * EDGE * EDGE * 4)
		o += EDGE * EDGE * EDGE * 4
		chunks.append({"coord": coord, "pos": pos, "nrm": nrm, "idx": idx, "vol": vol})
	return {"size": size, "waterline": wl, "deck": deck, "palette": palette, "chunks": chunks}


## A block hull in one chunk, meshed one quad per exposed cell face, with the volume layout of hull-mesh.
func _procedural_hull() -> Dictionary:
	var size := Vector3i(32, 20, 12)
	var solid := PackedByteArray()
	solid.resize(size.x * size.y * size.z)
	var paint := PackedByteArray()
	paint.resize(solid.size())
	for z in size.z:
		for y in size.y:
			for x in size.x:
				var hull_part := y < 10 and absi(z * 2 - size.z + 1) <= int(size.z - 1 - maxi(0, 6 - y)) and x >= y / 3 and x < size.x - maxi(0, 4 - y / 2)
				var sup := y >= 10 and y < 15 and x >= 10 and x < 20 and z >= 2 and z < size.z - 2
				var i := x + size.x * (z + size.z * y)
				solid[i] = 1 if hull_part or sup else 0
				paint[i] = 8 if y < 4 else (7 if y == 4 else (6 if y == 9 else 2))
	var vol := PackedByteArray()
	vol.resize(EDGE * EDGE * EDGE * 4)
	var at := func(x: int, y: int, z: int) -> int:
		if x < 0 or y < 0 or z < 0 or x >= size.x or y >= size.y or z >= size.z:
			return 0
		return solid[x + size.x * (z + size.z * y)]
	for z in size.z:
		for y in size.y:
			for x in size.x:
				if at.call(x, y, z) == 1:
					var t := 4 * ((x + 1) + EDGE * ((y + 1) + EDGE * (z + 1)))
					vol[t] = 255
					vol[t + 1] = paint[x + size.x * (z + size.z * y)]
					vol[t + 3] = ((x * 7 + y * 13 + z * 5) % 32) << 3
	var pos := PackedFloat32Array()
	var nrm := PackedFloat32Array()
	var idx := PackedInt32Array()
	var dirs: Array[Vector3i] = [Vector3i(1, 0, 0), Vector3i(-1, 0, 0), Vector3i(0, 1, 0), Vector3i(0, -1, 0), Vector3i(0, 0, 1), Vector3i(0, 0, -1)]
	for z in size.z:
		for y in size.y:
			for x in size.x:
				if at.call(x, y, z) == 0:
					continue
				for d: Vector3i in dirs:
					if at.call(x + d.x, y + d.y, z + d.z) == 1:
						continue
					_face(pos, nrm, idx, Vector3(x, y, z) * CELL, Vector3(d), CELL)
	return {"size": size, "waterline": 4, "deck": 9, "palette": _default_palette(), "chunks": [{"coord": Vector3i.ZERO, "pos": pos, "nrm": nrm, "idx": idx, "vol": vol}]}


func _face(pos: PackedFloat32Array, nrm: PackedFloat32Array, idx: PackedInt32Array, mn: Vector3, n: Vector3, s: float) -> void:
	var a := n.abs()
	var u := Vector3(0, 0, 1) if a.x > 0.5 else Vector3(1, 0, 0)
	var v := n.cross(u)
	var c := mn + Vector3(s, s, s) * 0.5 + n * s * 0.5
	var base := pos.size() / 3
	for k: Vector2 in [Vector2(-1, -1), Vector2(1, -1), Vector2(1, 1), Vector2(-1, 1)]:
		var p := c + (u * k.x + v * k.y) * s * 0.5
		pos.append_array([p.x, p.y, p.z])
		nrm.append_array([n.x, n.y, n.z])
	idx.append_array([base, base + 2, base + 1, base, base + 3, base + 2])


func _default_palette() -> PackedVector3Array:
	var out := PackedVector3Array()
	for e: Dictionary in _tokens["hull"]:
		out.append(_v3(_lin(e)))
	out.append(_v3(_lin(_tokens["antifouling"])))
	return out


## A generated TexBlob (`hull_mesh::gen_files`: "BPTX", version, kind, format, width, height, layers, mips, then
## the texels) as an ImageTexture, or null when `cargo xtask gen` has not written it.
func _tex_blob(res_path: String) -> Texture2D:
	if not FileAccess.file_exists(res_path):
		return null
	var b := FileAccess.get_file_as_bytes(res_path)
	if b.size() < 32 or b.slice(0, 4).get_string_from_ascii() != "BPTX":
		return null
	var fmt := b.decode_u32(12)
	var w := b.decode_u32(16)
	var h := b.decode_u32(20)
	var bpp := 4 if fmt == Image.FORMAT_RGBA8 else 1
	var img := Image.create_from_data(w, h, false, fmt, b.slice(32, 32 + w * h * bpp))
	return ImageTexture.create_from_image(img)


func _weathering_noise() -> Texture2D:
	var gen := _tex_blob("res://art/gen/noise_weathering.bin")
	if gen != null:
		return gen
	var noise := FastNoiseLite.new()
	noise.noise_type = FastNoiseLite.TYPE_VALUE_CUBIC
	noise.frequency = 0.08
	noise.fractal_octaves = 3
	var tex := NoiseTexture2D.new()
	tex.width = 256
	tex.height = 32
	tex.seamless = true
	tex.noise = noise
	return tex


func _add_hull(parent: Node3D, info: Dictionary, lod: bool) -> void:
	var shader: Shader = load("res://shaders/hull.gdshader")
	var palette: PackedVector3Array = info["palette"].duplicate()
	while palette.size() < 16:
		palette.append(Vector3.ZERO)
	var size: Vector3i = info["size"]
	var waterline_y := float(info["waterline"]) * CELL + 0.25
	var deck_y := float(info["deck"]) * CELL + CELL
	parent.get_child_count()
	var root := Node3D.new()
	root.position = Vector3(-size.x * CELL * 0.5, -waterline_y, -size.z * CELL * 0.5)
	parent.add_child(root)
	var noise := _weathering_noise()
	var lod_mat := ShaderMaterial.new()
	lod_mat.shader = shader
	lod_mat.set_shader_parameter("lod_vertex_colour", true)
	for ch: Dictionary in info["chunks"]:
		var coord: Vector3i = ch["coord"]
		var vol: PackedByteArray = ch["vol"].duplicate()
		# The simulation scorches solid cells only: clear scorch on air texels, as live volumes have it.
		for t in range(0, vol.size(), 4):
			if vol[t] == 0:
				vol[t + 2] = 0
		var arrays := []
		arrays.resize(Mesh.ARRAY_MAX)
		var pos: PackedFloat32Array = ch["pos"]
		var nrm: PackedFloat32Array = ch["nrm"]
		var idx: PackedInt32Array = ch["idx"]
		if idx.is_empty():
			continue
		var pv := PackedVector3Array()
		var nv := PackedVector3Array()
		for i in pos.size() / 3:
			pv.append(Vector3(pos[3 * i], pos[3 * i + 1], pos[3 * i + 2]))
			nv.append(Vector3(nrm[3 * i], nrm[3 * i + 1], nrm[3 * i + 2]))
		var mesh := ArrayMesh.new()
		if lod:
			# Vertex-colour path: per-triangle swatch from the cell behind the face, as a LOD mesh carries.
			var tp := PackedVector3Array()
			var tn := PackedVector3Array()
			var tc := PackedColorArray()
			for t in idx.size() / 3:
				var a := pv[idx[3 * t]]
				var b2 := pv[idx[3 * t + 1]]
				var c2 := pv[idx[3 * t + 2]]
				var n := nv[idx[3 * t]]
				var centre := (a + b2 + c2) / 3.0 - n * 0.1
				var cell := Vector3i((centre / CELL).floor()) - coord * 32 + Vector3i.ONE
				cell = cell.clamp(Vector3i.ZERO, Vector3i(33, 33, 33))
				var g := vol[4 * (cell.x + EDGE * (cell.y + EDGE * cell.z)) + 1]
				var sw: Vector3 = palette[g & 15]
				var col := Color(sw.x, sw.y, sw.z, 1.0)
				for p: Vector3 in [a, b2, c2]:
					tp.append(p)
					tn.append(n)
					tc.append(col)
			arrays[Mesh.ARRAY_VERTEX] = tp
			arrays[Mesh.ARRAY_NORMAL] = tn
			arrays[Mesh.ARRAY_COLOR] = tc
			mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
			mesh.surface_set_material(0, lod_mat)
		else:
			arrays[Mesh.ARRAY_VERTEX] = pv
			arrays[Mesh.ARRAY_NORMAL] = nv
			arrays[Mesh.ARRAY_INDEX] = idx
			mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
			var images: Array[Image] = []
			var slice := EDGE * EDGE * 4
			for z in EDGE:
				images.append(Image.create_from_data(EDGE, EDGE, false, Image.FORMAT_RGBA8, vol.slice(z * slice, (z + 1) * slice)))
			var tex := ImageTexture3D.new()
			tex.create(Image.FORMAT_RGBA8, EDGE, EDGE, EDGE, false, images)
			var mat := ShaderMaterial.new()
			mat.shader = shader
			mat.set_shader_parameter("hull_volume", tex)
			mat.set_shader_parameter("chunk_origin", Vector3(coord) * 16.0)
			mat.set_shader_parameter("palette", palette)
			mat.set_shader_parameter("primer_albedo", _v3(_lin(_tokens["primer"])))
			mat.set_shader_parameter("weathering_noise", noise)
			mat.set_shader_parameter("weathering_level", 1.0)
			mat.set_shader_parameter("waterline_y", waterline_y)
			mat.set_shader_parameter("deck_y", deck_y)
			mat.set_shader_parameter("chunk_heat", float(_args.get("heat", "0.0")))
			mat.set_shader_parameter("fire_ramp", _ramp("fire"))
			mesh.surface_set_material(0, mat)
		var mi := MeshInstance3D.new()
		mi.mesh = mesh
		root.add_child(mi)


# ---- parts ---------------------------------------------------------------------------------------------

func _add_parts(parent: Node3D, info: Dictionary) -> void:
	var st := SurfaceTool.new()
	st.begin(Mesh.PRIMITIVE_TRIANGLES)
	st.set_custom_format(0, SurfaceTool.CUSTOM_RGBA_FLOAT)
	var white := Color(1, 1, 1, 1)
	var black: Color = _lin(_token("hull", "hull.black"))
	# Housing: painted (mask 1). Barrels: fixed dull black (mask 0). A lamp: emission 1.
	_box(st, Vector3(-1.6, 0.0, -1.4), Vector3(1.4, 1.4, 1.4), white, 0.0)
	_box(st, Vector3(-1.0, 1.4, -0.9), Vector3(0.6, 1.8, 0.9), white, 0.0)
	_box(st, Vector3(1.4, 0.55, -0.6), Vector3(6.0, 0.85, -0.3), Color(black.r, black.g, black.b, 0.0), 0.0)
	_box(st, Vector3(1.4, 0.55, 0.3), Vector3(6.0, 0.85, 0.6), Color(black.r, black.g, black.b, 0.0), 0.0)
	_box(st, Vector3(-0.3, 1.8, -0.15), Vector3(0.0, 2.1, 0.15), Color(1.0, 0.8, 0.55, 0.0), 1.0)
	var mesh := st.commit()
	var mat := ShaderMaterial.new()
	mat.shader = load("res://shaders/part.gdshader")
	mesh.surface_set_material(0, mat)
	var mm := MultiMesh.new()
	mm.transform_format = MultiMesh.TRANSFORM_3D
	mm.use_colors = true
	mm.use_custom_data = true
	mm.mesh = mesh
	mm.instance_count = 2
	var size: Vector3i = info["size"]
	var deck := float(info["deck"]) * CELL + CELL - (float(info["waterline"]) * CELL + 0.25)
	var pal: PackedVector3Array = info["palette"]
	var sw: Vector3 = pal[1] if pal.size() > 1 else Vector3(0.3, 0.3, 0.3)
	mm.set_instance_transform(0, Transform3D(Basis.IDENTITY, Vector3(size.x * CELL * 0.28, deck, 0.0)))
	mm.set_instance_transform(1, Transform3D(Basis(Vector3.UP, PI), Vector3(-size.x * CELL * 0.3, deck, 0.0)))
	for i in 2:
		mm.set_instance_color(i, Color(1, 1, 1, 1))
		mm.set_instance_custom_data(i, Color(1.0 if i == 1 and _damaged else 0.0, sw.x, sw.y, sw.z))
	var mi := MultiMeshInstance3D.new()
	mi.multimesh = mm
	parent.add_child(mi)


func _box(st: SurfaceTool, a: Vector3, b: Vector3, col: Color, emission: float) -> void:
	var faces := [
		[Vector3(1, 0, 0), [Vector3(b.x, a.y, a.z), Vector3(b.x, b.y, a.z), Vector3(b.x, b.y, b.z), Vector3(b.x, a.y, b.z)]],
		[Vector3(-1, 0, 0), [Vector3(a.x, a.y, b.z), Vector3(a.x, b.y, b.z), Vector3(a.x, b.y, a.z), Vector3(a.x, a.y, a.z)]],
		[Vector3(0, 1, 0), [Vector3(a.x, b.y, a.z), Vector3(a.x, b.y, b.z), Vector3(b.x, b.y, b.z), Vector3(b.x, b.y, a.z)]],
		[Vector3(0, -1, 0), [Vector3(a.x, a.y, b.z), Vector3(a.x, a.y, a.z), Vector3(b.x, a.y, a.z), Vector3(b.x, a.y, b.z)]],
		[Vector3(0, 0, 1), [Vector3(b.x, a.y, b.z), Vector3(b.x, b.y, b.z), Vector3(a.x, b.y, b.z), Vector3(a.x, a.y, b.z)]],
		[Vector3(0, 0, -1), [Vector3(a.x, a.y, a.z), Vector3(a.x, b.y, a.z), Vector3(b.x, b.y, a.z), Vector3(b.x, a.y, a.z)]],
	]
	for f: Array in faces:
		var q: Array = f[1]
		for k: int in [0, 2, 1, 0, 3, 2]:
			st.set_normal(f[0])
			st.set_color(col)
			st.set_custom(0, Color(emission, 0.0, 0.0, 0.0))
			st.add_vertex(q[k])


# ---- VFX -----------------------------------------------------------------------------------------------

const RAMP_OF := ["flash", "foam", "foam", "flash", "hit_puff", "debris", "fire", "fire", "smoke", "defence_smoke", "flak", "trail", "foam", "foam", "foam", "flash", "smoke", "foam", "foam", "foam", "flak", "flash"]


func _load_vfx_meshes(path: String) -> Array[ArrayMesh]:
	var meshes: Array[ArrayMesh] = []
	var b := FileAccess.get_file_as_bytes(path)
	if b.size() < 8 or b.slice(0, 4).get_string_from_ascii() != "BPVX":
		push_error("shader_look: bad vfx fixture " + path)
		return meshes
	var o := 4
	var nk := b.decode_u32(o)
	o += 4
	for k in nk:
		var nv := b.decode_u32(o)
		var ni := b.decode_u32(o + 4)
		o += 8
		var pos := b.slice(o, o + nv * 12).to_float32_array()
		o += nv * 12
		var nrm := b.slice(o, o + nv * 12).to_float32_array()
		o += nv * 12
		var col := b.slice(o, o + nv * 16).to_float32_array()
		o += nv * 16
		var c0 := b.slice(o, o + nv * 16).to_float32_array()
		o += nv * 16
		var idx := b.slice(o, o + ni * 4).to_int32_array()
		o += ni * 4
		var pv := PackedVector3Array()
		var nn := PackedVector3Array()
		var cc := PackedColorArray()
		for i in nv:
			pv.append(Vector3(pos[3 * i], pos[3 * i + 1], pos[3 * i + 2]))
			nn.append(Vector3(nrm[3 * i], nrm[3 * i + 1], nrm[3 * i + 2]))
			cc.append(Color(col[4 * i], col[4 * i + 1], col[4 * i + 2], col[4 * i + 3]))
		var arrays := []
		arrays.resize(Mesh.ARRAY_MAX)
		arrays[Mesh.ARRAY_VERTEX] = pv
		arrays[Mesh.ARRAY_NORMAL] = nn
		arrays[Mesh.ARRAY_COLOR] = cc
		arrays[Mesh.ARRAY_CUSTOM0] = c0
		arrays[Mesh.ARRAY_INDEX] = idx
		var m := ArrayMesh.new()
		m.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays, [], {}, Mesh.ARRAY_CUSTOM_RGBA_FLOAT << Mesh.ARRAY_FORMAT_CUSTOM0_SHIFT)
		meshes.append(m)
	return meshes


func _vfx_material(kind: int, ramp: String) -> ShaderMaterial:
	var blended := kind == 11 or kind == 16 or kind == 36
	var mat := ShaderMaterial.new()
	mat.shader = load("res://shaders/vfx_blend.gdshader" if blended else "res://shaders/vfx_opaque.gdshader")
	mat.set_shader_parameter("vfx_kind", kind)
	mat.set_shader_parameter("ramp", _ramp(ramp))
	mat.set_shader_parameter("emission_cap", 16.0)
	return mat


func _multimesh(mesh: ArrayMesh, mat: ShaderMaterial, data: PackedFloat32Array) -> void:
	var m: ArrayMesh = mesh.duplicate()
	m.surface_set_material(0, mat)
	var mm := MultiMesh.new()
	mm.transform_format = MultiMesh.TRANSFORM_3D
	mm.use_colors = true
	mm.use_custom_data = true
	mm.mesh = m
	mm.instance_count = data.size() / 20
	mm.buffer = data
	var mi := MultiMeshInstance3D.new()
	mi.multimesh = mm
	mi.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	add_child(mi)


func _add_vfx(path: String, meshes: Array[ArrayMesh]) -> void:
	var ib := FileAccess.get_file_as_bytes(path + ".inst")
	var count := ib.decode_u32(0)
	var per_kind := {}
	var q := 4
	for i in count:
		var kind := ib.decode_u32(q)
		var vals := ib.slice(q + 4, q + 4 + 80).to_float32_array()
		q += 84
		if not per_kind.has(kind):
			per_kind[kind] = PackedFloat32Array()
		var arr: PackedFloat32Array = per_kind[kind]
		arr.append_array(vals)
		per_kind[kind] = arr
	for kind: int in per_kind:
		_multimesh(meshes[kind], _vfx_material(kind, RAMP_OF[kind]), per_kind[kind])


## Contact sheet: every Layer A kind at three ages (15%, 45% and 80% of a typical life), one kind per column,
## from x = 200, kinds 0 to 10 near z = 0 and 11 to 21 near z = -100, rows 14 m apart.
func _add_sheet(meshes: Array[ArrayMesh]) -> void:
	# kind: [size m, life s, custom.w, lift above the sea]
	var spec := {
		0: [Vector3(6.0, 0.3, 0.3), 0.0, 6.0, 4.0], 1: [Vector3(3.0, 12.0, 3.0), 3.9, 0.4, -0.5], 2: [Vector3(9.0, 1.0, 9.0), 6.0, 0.4, 0.0],
		3: [Vector3(4.0, 4.0, 4.0), 0.045, 1.0, 3.0], 4: [Vector3(3.0, 3.0, 3.0), 1.2, 2.0, 3.0], 5: [Vector3(0.5, 0.5, 0.5), 2.0, 9.0, 2.0],
		6: [Vector3(5.0, 5.0, 5.0), 1.5, 3.0, 3.0], 7: [Vector3(2.0, 4.0, 2.0), 1.0, 3.0, 1.0], 8: [Vector3(4.0, 4.0, 4.0), 9.0, 2.0, 2.0],
		9: [Vector3(10.0, 6.0, 10.0), 0.0, 0.05, 3.0], 10: [Vector3(5.0, 5.0, 5.0), 3.0, 0.0, 6.0], 11: [Vector3(3.0, 2.4, 2.4), 12.0, 0.0, 6.0],
		12: [Vector3(4.0, 1.0, 9.0), 30.0, 0.6, 0.0], 13: [Vector3(4.0, 2.0, 3.0), 10.0, 2.0, 0.0], 14: [Vector3(6.0, 1.5, 8.0), 0.0, 0.6, 0.0],
		15: [Vector3(3.0, 1.5, 1.5), 0.045, 1.0, 3.0], 16: [Vector3(3.0, 3.0, 3.0), 1.6, 1.0, 3.0], 17: [Vector3(4.0, 4.0, 1.4), 0.0, 0.7, 4.0],
		18: [Vector3(4.0, 2.0, 4.0), 0.0, 0.8, 0.0], 19: [Vector3(6.0, 1.0, 4.0), 0.0, 0.6, 0.0], 20: [Vector3(8.0, 5.0, 8.0), 20.0, 0.8, 6.0],
		21: [Vector3(2.0, 2.0, 2.0), 6.0, 6.0, 5.0],
	}
	for kind: int in spec:
		var sp: Array = spec[kind]
		var size: Vector3 = sp[0]
		var life: float = sp[1]
		var data := PackedFloat32Array()
		var col := kind % 11
		var base_z := 0.0 if kind < 11 else -100.0
		for row in 3:
			var fracs: Array[float] = [0.15, 0.45, 0.8]
			var age: float = life * fracs[row] if life > 0.0 else float(row) * 2.0
			var pos := Vector3(200.0 + 14.0 * col, float(sp[3]), base_z - 14.0 * row)
			var basis := Basis.from_scale(size)
			var t := Transform3D(basis, pos)
			var tint := 0.45 if kind == 8 else 1.0
			data.append_array([t.basis.x.x, t.basis.y.x, t.basis.z.x, t.origin.x, t.basis.x.y, t.basis.y.y, t.basis.z.y, t.origin.y, t.basis.x.z, t.basis.y.z, t.basis.z.z, t.origin.z,
				tint, tint, tint, 1.0, age, life, 0.17 + 0.29 * float(row), float(sp[2])])
		_multimesh(meshes[kind], _vfx_material(kind, RAMP_OF[kind]), data)


## Layer B: one emitter per process kind (spray, sparks, embers, flak, mist) in a row near z = -150, drawn by
## the Layer A shaders with vfx_kind 32 to 36. These emit continuously (the RESTART_* paths); an emitter that
## takes emit_particle calls emits only those, which the runtime's emitters do.
func _add_particles(meshes: Array[ArrayMesh]) -> void:
	var process: Shader = load("res://shaders/vfx_particles.gdshader")
	var ramps := ["foam", "flash", "fire", "flak", "smoke"]
	for k in 5:
		var pm := ShaderMaterial.new()
		pm.shader = process
		pm.set_shader_parameter("particle_kind", k)
		pm.set_shader_parameter("wind", Vector3(3.0, 0.0, 1.0))
		var gp := GPUParticles3D.new()
		gp.process_material = pm
		gp.amount = 96
		gp.lifetime = 6.0
		gp.local_coords = true
		gp.fixed_fps = 30
		gp.interpolate = true
		gp.draw_order = GPUParticles3D.DRAW_ORDER_INDEX
		gp.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
		gp.position = Vector3(12.0 * k, 2.0 if k != 1 else 6.0, -150.0)
		gp.visibility_aabb = AABB(Vector3(-60, -20, -60), Vector3(120, 80, 120))
		var draw: Mesh = (meshes[8] if k == 4 or k == 0 else meshes[5]).duplicate()
		draw.surface_set_material(0, _vfx_material(32 + k, ramps[k]))
		gp.draw_pass_1 = draw
		add_child(gp)
		gp.preprocess = 2.0


## The generated ramp when `cargo xtask gen` has run; otherwise a stand-in with the same encoding (bands in sRGB,
## A the emission envelope on emitting ramps and opacity on the others).
func _ramp(name: String) -> Texture2D:
	var gen := _tex_blob("res://art/gen/ramp_%s.bin" % name)
	if gen != null:
		return gen
	var foam: Color = _lin(_token("sea", "sea.foam")).linear_to_srgb()
	var bands: Array = []
	match name:
		"foam":
			bands = [[foam, 0.0], [foam.darkened(0.06), 0.0], [foam.darkened(0.14), 0.0], [foam.darkened(0.26), 0.0], [foam.darkened(0.4), 0.0]]
		"fire":
			bands = [[Color(1.0, 0.93, 0.72), 1.0], [Color(1.0, 0.72, 0.36), 0.9], [Color(0.92, 0.45, 0.16), 0.7], [Color(0.55, 0.2, 0.08), 0.35], [Color(0.22, 0.2, 0.19), 0.0]]
		"flash":
			bands = [[Color(1.0, 0.97, 0.88), 1.0], [Color(1.0, 0.86, 0.6), 1.0], [Color(1.0, 0.66, 0.34), 0.75], [Color(0.8, 0.42, 0.2), 0.4]]
		"smoke":
			bands = [[Color(0.7, 0.69, 0.67), 0.7], [Color(0.6, 0.59, 0.58), 0.6], [Color(0.5, 0.49, 0.48), 0.45], [Color(0.42, 0.41, 0.41), 0.3], [Color(0.36, 0.36, 0.36), 0.15]]
		"hit_puff":
			bands = [[Color(0.95, 0.94, 0.92), 0.0], [Color(0.78, 0.77, 0.75), 0.0], [Color(0.6, 0.59, 0.58), 0.0], [Color(0.45, 0.45, 0.45), 0.0]]
		"debris":
			bands = [[Color(0.55, 0.45, 0.4), 0.6], [Color(0.36, 0.35, 0.35), 0.15], [Color(0.3, 0.3, 0.31), 0.0], [Color(0.27, 0.27, 0.28), 0.0]]
		"flak":
			bands = [[Color(0.3, 0.28, 0.26), 0.0], [Color(0.26, 0.25, 0.24), 0.0], [Color(0.22, 0.22, 0.22), 0.0], [Color(0.2, 0.2, 0.2), 0.0]]
		"trail":
			bands = [[Color(0.92, 0.91, 0.9), 0.85], [Color(0.86, 0.85, 0.84), 0.65], [Color(0.8, 0.8, 0.79), 0.45], [Color(0.75, 0.75, 0.75), 0.28], [Color(0.7, 0.7, 0.7), 0.12]]
		_:
			bands = [[Color(0.82, 0.82, 0.81), 0.0], [Color(0.76, 0.76, 0.75), 0.0], [Color(0.7, 0.7, 0.7), 0.0], [Color(0.66, 0.66, 0.66), 0.0]]
	var img := Image.create_empty(256, 1, false, Image.FORMAT_RGBA8)
	for x in 256:
		var bi := mini(int(float(x) / 256.0 * bands.size()), bands.size() - 1)
		var c: Color = bands[bi][0]
		img.set_pixel(x, 0, Color(c.r, c.g, c.b, float(bands[bi][1])))
	return ImageTexture.create_from_image(img)
