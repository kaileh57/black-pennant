extends SceneTree
## Project compile check, headless:
##   godot --headless --path . -s res://test/tools/check_project.gd
## Loads every script and shader, instantiates every scene under res://, runs the API probe and
## exits 1 on any failure. It tests no behaviour.

var _failures := 0
var _frame := 0


func _process(_delta: float) -> bool:
	# Frame 1: the tree and the autoloads are ready. Frame 3: every scene has run _ready and one _process.
	_frame += 1
	if _frame == 1:
		_check()
	if _frame < 3:
		return false
	quit(1 if _failures > 0 else 0)
	return true


func _check() -> void:
	var paths: Array[String] = []
	_collect("res://", paths)
	paths.sort()
	var scenes := 0
	for path: String in paths:
		var resource: Resource = load(path)
		if resource == null:
			_fail("cannot load " + path)
			continue
		if resource is PackedScene:
			var node: Node = (resource as PackedScene).instantiate()
			if node == null:
				_fail("cannot instantiate " + path)
				continue
			root.add_child(node)
			node.queue_free()
			scenes += 1
	var probe: Node = (load("res://test/tools/api_probe.gd") as GDScript).new()
	var line: String = probe.call(&"probe")
	probe.free()
	print("check_project: ", paths.size(), " files, ", scenes, " scenes, probe ", line)
	for action: StringName in [&"view_switch", &"splash", &"designate", &"editor_undo"]:
		if InputMap.action_get_events(action).is_empty():
			_fail("input action has no events: " + action)
	for action: StringName in [&"ui_accept", &"ui_select", &"ui_focus_next"]:
		for event: InputEvent in InputMap.action_get_events(action):
			var key := event as InputEventKey
			if key != null and (key.physical_keycode == KEY_TAB or key.physical_keycode == KEY_SPACE):
				_fail("Tab or Space still bound to " + action)
	# The I18n autoload reads res://i18n/*.csv as raw text. An export ships the raw file only when its
	# importer is `keep`; the default translation importer ships a compiled copy instead, and an exported
	# build would show raw keys.
	for file: String in DirAccess.get_files_at("res://i18n"):
		if file.get_extension() == "csv":
			var cfg := ConfigFile.new()
			if cfg.load("res://i18n/%s.import" % file) != OK or String(cfg.get_value("remap", "importer", "")) != "keep":
				_fail("res://i18n/%s must import as importer=\"keep\" (Import dock: Keep file)" % file)
	if root.get_node("Session").process_priority != -100:
		_fail("Session did not run its Rust _ready (is a script overriding it?)")


func _collect(dir_path: String, out: Array[String]) -> void:
	var dir := DirAccess.open(dir_path)
	if dir == null:
		return
	for sub: String in dir.get_directories():
		if sub.begins_with(".") or sub == "addons":
			continue
		_collect(dir_path.path_join(sub), out)
	for file: String in dir.get_files():
		var ext := file.get_extension()
		if ext == "tscn" or ext == "gd" or ext == "gdshader" or ext == "tres":
			out.append(dir_path.path_join(file))


func _fail(message: String) -> void:
	_failures += 1
	printerr("check_project: FAIL ", message)
