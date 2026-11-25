extends Node
## Interactive play driver for windowed playtests. Boots the real main scene (consent, quality,
## warm-up, Screens routing) and then executes commands appended to a command file, one per line,
## so a tester can drive the real game with simulated input and take captures between steps.
##   HOME=<scratch> godot --path game --resolution 1920x1080 res://test/ui/play_driver.tscn -- \
##       --cmd=<file> --out=<folder>
## Commands (results go to <cmd>.out, each command ends with a line "#done <n>"):
##   shot <name>             save the viewport to <out>/<name>.png
##   click <text>            left click the centre of the first visible button whose text contains <text>
##   clickn <text> <i>       the same, the i-th match (0-based)
##   clickp <node path>      left click the centre of the control at that path
##   at <x> <y>              left click at viewport coordinates (base 1920x1080 canvas)
##   rclick <x> <y>          right click at viewport coordinates
##   hover <x> <y>           move the mouse there
##   key <keycode name>      press and release a key (Enter, Escape, Tab, Up, A, ...); "shift+Tab" works
##   wait <seconds>          let real time pass
##   frames <n>              let n frames pass
##   tree [filter]           list visible buttons, labels and edits with their rects
##   eval <expression>       evaluate an Expression with the autoloads in scope and print the result
##   scale <factor>          set the UI scale setting (0.75 to 2.0)
##   quit                    quit
## Never shipped: test only.

const Ev := preload("res://test/ui/play_driver_eval.gd")

var _cmd_path := ""
var _out := "user://captures"
var _read := 0
var _queue: PackedStringArray = []
var _busy := false
var _count := 0
var _log: FileAccess


func _ready() -> void:
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--cmd="):
			_cmd_path = arg.trim_prefix("--cmd=")
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	if _cmd_path.is_empty() or get_tree().current_scene != self:
		return
	DirAccess.make_dir_recursive_absolute(_out)
	_log = FileAccess.open(_cmd_path + ".out", FileAccess.WRITE)
	get_window().always_on_top = true
	_boot.call_deferred()


func _boot() -> void:
	# Survive the scene change: the driver lives beside the main scene, the boot scene becomes current.
	var tree := get_tree()
	tree.current_scene = null
	process_mode = Node.PROCESS_MODE_ALWAYS
	tree.change_scene_to_file("res://boot/boot.tscn")


func _say(text: String) -> void:
	print("play: ", text)
	if _log != null:
		_log.store_line(text)
		_log.flush()


func _process(_delta: float) -> void:
	if _cmd_path.is_empty():
		return
	if not _busy:
		_poll()
		if not _queue.is_empty():
			var line: String = _queue[0]
			_queue.remove_at(0)
			_busy = true
			await _run(line)
			_count += 1
			_say("#done %d" % _count)
			_busy = false


func _poll() -> void:
	if not FileAccess.file_exists(_cmd_path):
		return
	var f := FileAccess.open(_cmd_path, FileAccess.READ)
	if f == null:
		return
	var text := f.get_as_text()
	if text.length() <= _read:
		return
	var fresh := text.substr(_read)
	var cut := fresh.rfind("\n")
	if cut < 0:
		return
	_read += cut + 1
	for line: String in fresh.substr(0, cut).split("\n", false):
		if not line.strip_edges().is_empty():
			_queue.append(line.strip_edges())


func _run(line: String) -> void:
	var sp := line.find(" ")
	var verb := line if sp < 0 else line.substr(0, sp)
	var rest := "" if sp < 0 else line.substr(sp + 1).strip_edges()
	_say("> " + line)
	match verb:
		"shot":
			await _frames(2)
			var img := get_viewport().get_texture().get_image()
			var path := _out.path_join(rest + ".png")
			_say("saved %s (%s)" % [path, error_string(img.save_png(path))])
		"click":
			await _click_text(rest, 0)
		"clickn":
			var parts := rest.rsplit(" ", true, 1)
			await _click_text(parts[0], int(parts[1]))
		"clickp":
			var c := get_tree().root.get_node_or_null(NodePath(rest)) as Control
			if c == null:
				_say("no control at " + rest)
			else:
				await _click_control(c)
		"at":
			var xy := rest.split(" ")
			await _click_at(Vector2(float(xy[0]), float(xy[1])), MOUSE_BUTTON_LEFT)
		"rclick":
			var xy2 := rest.split(" ")
			await _click_at(Vector2(float(xy2[0]), float(xy2[1])), MOUSE_BUTTON_RIGHT)
		"hover":
			var xy3 := rest.split(" ")
			_move(Vector2(float(xy3[0]), float(xy3[1])))
			await _frames(3)
		"wheel":
			var w := rest.split(" ")
			await _wheel(Vector2(float(w[0]), float(w[1])), int(w[2]))
		"key":
			await _key(rest)
		"wait":
			await get_tree().create_timer(float(rest), true, false, true).timeout
		"frames":
			await _frames(int(rest))
		"tree":
			_tree(rest)
		"eval":
			_say(Ev.run(rest, get_tree().root))
		"scale":
			get_tree().root.get_node(^"Settings").call(&"set_value", &"access.ui_scale", float(rest))
			await _frames(10)
		"quit":
			get_tree().quit()
		_:
			_say("unknown command " + verb)


func _frames(n: int) -> void:
	for _i: int in range(n):
		await get_tree().process_frame


## Canvas point (base resolution) to window pixels for the input pipeline.
func _to_window(p: Vector2) -> Vector2:
	return get_tree().root.get_final_transform() * p


func _move(p: Vector2) -> void:
	var ev := InputEventMouseMotion.new()
	ev.position = _to_window(p)
	ev.global_position = ev.position
	Input.parse_input_event(ev)


func _click_at(p: Vector2, button: MouseButton) -> void:
	_move(p)
	await _frames(2)
	for pressed: bool in [true, false]:
		var ev := InputEventMouseButton.new()
		ev.button_index = button
		ev.pressed = pressed
		ev.position = _to_window(p)
		ev.global_position = ev.position
		ev.button_mask = MOUSE_BUTTON_MASK_LEFT if (pressed and button == MOUSE_BUTTON_LEFT) else 0
		Input.parse_input_event(ev)
		await _frames(2)
	_say("clicked %s" % str(p))


## Mouse wheel at a point: positive steps scroll down, negative up.
func _wheel(p: Vector2, steps: int) -> void:
	_move(p)
	await _frames(2)
	var button := MOUSE_BUTTON_WHEEL_DOWN if steps > 0 else MOUSE_BUTTON_WHEEL_UP
	for _i: int in range(absi(steps)):
		for pressed: bool in [true, false]:
			var ev := InputEventMouseButton.new()
			ev.button_index = button
			ev.pressed = pressed
			ev.factor = 1.0
			ev.position = _to_window(p)
			ev.global_position = ev.position
			Input.parse_input_event(ev)
		await _frames(1)
	await _frames(3)


func _centre(c: Control) -> Vector2:
	var xf := c.get_global_transform_with_canvas()
	return xf * (c.size * 0.5)


func _click_control(c: Control) -> void:
	_say("clicking %s '%s'" % [c.get_path(), _text_of(c)])
	await _click_at(_centre(c), MOUSE_BUTTON_LEFT)


func _text_of(c: Control) -> String:
	if c is Button:
		return (c as Button).text
	if c is Label:
		return (c as Label).text
	if c is LineEdit:
		return (c as LineEdit).text
	if c is RichTextLabel:
		return (c as RichTextLabel).get_parsed_text().substr(0, 120)
	return ""


func _visible_controls() -> Array[Control]:
	var out: Array[Control] = []
	for n: Node in get_tree().root.find_children("*", "Control", true, false):
		var c := n as Control
		if c.is_visible_in_tree():
			out.append(c)
	return out


func _click_text(text: String, index: int) -> void:
	var matches: Array[Control] = []
	var needle := text.to_lower()
	for c: Control in _visible_controls():
		if c is BaseButton and tr(_text_of(c)).to_lower().contains(needle):
			matches.append(c)
	if matches.size() <= index:
		_say("no button matching '%s' (%d found)" % [text, matches.size()])
		return
	await _click_control(matches[index])


func _key(spec: String) -> void:
	var parts := spec.split("+")
	var keyname := parts[parts.size() - 1]
	var code := OS.find_keycode_from_string(keyname)
	if code == KEY_NONE:
		_say("unknown key " + keyname)
		return
	for pressed: bool in [true, false]:
		var ev := InputEventKey.new()
		ev.keycode = code
		ev.physical_keycode = code
		ev.pressed = pressed
		ev.shift_pressed = "shift" in parts
		ev.ctrl_pressed = "ctrl" in parts
		ev.meta_pressed = "cmd" in parts
		ev.alt_pressed = "alt" in parts
		Input.parse_input_event(ev)
		await _frames(2)


func _tree(filter: String) -> void:
	var needle := filter.to_lower()
	for c: Control in _visible_controls():
		if not (c is BaseButton or c is Label or c is LineEdit or c is RichTextLabel or c is TabBar or c is OptionButton):
			continue
		var t := _text_of(c)
		var line := "%s [%s] '%s' %s" % [c.get_path(), c.get_class(), t.replace("\n", " | ").substr(0, 140), str(Rect2(c.get_global_transform_with_canvas().origin, c.size))]
		if c is BaseButton and (c as BaseButton).disabled:
			line += " DISABLED"
		if needle.is_empty() or line.to_lower().contains(needle):
			_say(line)
