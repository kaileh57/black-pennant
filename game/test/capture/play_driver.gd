extends Node
## Interactive play driver. Boots the real game (boot.tscn, every autoload) and then feeds it real
## input read from a command file, so a person or an agent can play a flow step by step and save
## what the player sees. Windowed only; headless or instanced by check_project it does nothing.
##   HOME=/scratch/home godot --path game --resolution 1920x1080 \
##     res://test/capture/play_driver.tscn -- --dir=/abs/dir
## Each time `<dir>/cmd.txt` appears it is read, removed and run line by line; the replies go to
## `<dir>/out.txt`, written when the batch ends. Commands:
##   key <Keycode name> [shift|ctrl|alt|meta ...]   press and release a key (`key Down`, `key Tab shift`)
##   action <name>                                  press and release an input action
##   pad <JoyButton index>                          press and release a gamepad button
##   padaxis <axis> <value>                         move a gamepad axis, then centre it
##   click <x> <y>                                  left click at window pixels
##   clicktext <text>                               left click the visible Button whose text is <text>
##   wait <seconds>                                 let real time pass
##   shot <name>                                    save the viewport as <dir>/<name>.png
##   focus                                          report the focused control
##   ui                                             report every visible Button and Label
##   eval <expression>                              run a GDScript Expression on this node
##   quit                                           quit the game

const BOOT := "res://boot/boot.tscn"

var _dir := ""
var _busy := false
var _out: PackedStringArray = []


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--dir="):
			_dir = arg.trim_prefix("--dir=")
	if _dir.is_empty():
		push_error("play_driver: pass --dir=<abs dir>")
		get_tree().quit(1)
		return
	DirAccess.make_dir_recursive_absolute(_dir)
	get_window().always_on_top = true
	_boot.call_deferred()


## Stay alive as a root child while the real boot scene becomes the current scene.
func _boot() -> void:
	var tree := get_tree()
	var root := tree.root
	tree.current_scene = null
	get_parent().remove_child(self)
	root.add_child(self)
	tree.change_scene_to_file(BOOT)


func _process(_delta: float) -> void:
	if _busy or _dir.is_empty():
		return
	var path := _dir.path_join("cmd.txt")
	if not FileAccess.file_exists(path):
		return
	var text := FileAccess.get_file_as_string(path)
	DirAccess.remove_absolute(path)
	_busy = true
	# Other windows may cover this one; a covered window draws nothing on macOS, so come forward.
	DisplayServer.window_move_to_foreground()
	_run(text)


func _run(text: String) -> void:
	_out.clear()
	for raw: String in text.split("\n", false):
		var line := raw.strip_edges()
		if line.is_empty() or line.begins_with("#"):
			continue
		_out.append("> " + line)
		await _command(line)
	var f := FileAccess.open(_dir.path_join("out.tmp"), FileAccess.WRITE)
	f.store_string("\n".join(_out) + "\n")
	f.close()
	DirAccess.rename_absolute(_dir.path_join("out.tmp"), _dir.path_join("out.txt"))
	_busy = false


func _command(line: String) -> void:
	var parts := line.split(" ", false)
	var verb := parts[0]
	var rest := line.substr(verb.length()).strip_edges()
	match verb:
		"key":
			await _key(parts.slice(1))
		"action":
			var ev := InputEventAction.new()
			ev.action = StringName(rest)
			ev.pressed = true
			Input.parse_input_event(ev)
			await _frames(2)
			var up := InputEventAction.new()
			up.action = StringName(rest)
			up.pressed = false
			Input.parse_input_event(up)
			await _frames(4)
		"pad":
			var b := InputEventJoypadButton.new()
			b.button_index = int(rest) as JoyButton
			b.pressed = true
			Input.parse_input_event(b)
			await _frames(2)
			var bu := InputEventJoypadButton.new()
			bu.button_index = int(rest) as JoyButton
			bu.pressed = false
			Input.parse_input_event(bu)
			await _frames(4)
		"padaxis":
			var m := InputEventJoypadMotion.new()
			m.axis = int(parts[1]) as JoyAxis
			m.axis_value = float(parts[2])
			Input.parse_input_event(m)
			await _frames(3)
			var c := InputEventJoypadMotion.new()
			c.axis = int(parts[1]) as JoyAxis
			c.axis_value = 0.0
			Input.parse_input_event(c)
			await _frames(4)
		"click":
			await _click(Vector2(float(parts[1]), float(parts[2])))
		"clicktext":
			var target := _find_button(get_tree().root, rest)
			if target == null:
				_out.append("no visible button '%s'" % rest)
				return
			var at := get_viewport().get_final_transform() * target.get_global_transform_with_canvas() * (target.size * 0.5)
			_out.append("clicking %s at %s" % [target.get_path(), at])
			await _click(at)
		"wait":
			var t := float(rest)
			var end := Time.get_ticks_msec() + int(t * 1000.0)
			while Time.get_ticks_msec() < end:
				await get_tree().process_frame
		"shot":
			await _frames(3)
			var img := get_viewport().get_texture().get_image()
			var p := _dir.path_join(rest + ".png")
			img.save_png(p)
			_out.append("saved " + p + " " + str(img.get_size()))
		"focus":
			var f := get_viewport().gui_get_focus_owner()
			if f == null:
				_out.append("focus: none")
			else:
				_out.append("focus: %s '%s' %s" % [f.get_path(), _text_of(f), f.get_global_rect()])
		"ui":
			# The top screen and any open dialog; `ui all` walks the whole tree.
			var screens := get_tree().root.get_node_or_null(^"Screens")
			if rest == "all" or screens == null:
				_dump(get_tree().root)
			else:
				var top: Variant = screens.call(&"top")
				if top is Node:
					_dump(top)
				_dump(screens.get_node(^"Modals"))
		"eval":
			var e := Expression.new()
			if e.parse(rest) != OK:
				_out.append("parse error: " + e.get_error_text())
				return
			var r: Variant = e.execute([], self)
			_out.append("= " + str(r) + (" ERR " + e.get_error_text() if e.has_execute_failed() else ""))
			await _frames(2)
		"quit":
			get_tree().quit()
		_:
			_out.append("unknown command")


func _key(parts: PackedStringArray) -> void:
	var code := OS.find_keycode_from_string(parts[0])
	if code == KEY_NONE:
		_out.append("unknown key " + parts[0])
		return
	var ev := InputEventKey.new()
	ev.keycode = code
	ev.physical_keycode = code
	for mod: String in parts.slice(1):
		match mod:
			"shift": ev.shift_pressed = true
			"ctrl": ev.ctrl_pressed = true
			"alt": ev.alt_pressed = true
			"meta": ev.meta_pressed = true
	ev.pressed = true
	Input.parse_input_event(ev)
	await _frames(2)
	var up := ev.duplicate() as InputEventKey
	up.pressed = false
	Input.parse_input_event(up)
	await _frames(4)


func _click(at: Vector2) -> void:
	var mv := InputEventMouseMotion.new()
	mv.position = at
	mv.global_position = at
	Input.parse_input_event(mv)
	await _frames(2)
	var down := InputEventMouseButton.new()
	down.position = at
	down.global_position = at
	down.button_index = MOUSE_BUTTON_LEFT
	down.button_mask = MOUSE_BUTTON_MASK_LEFT
	down.pressed = true
	Input.parse_input_event(down)
	await _frames(2)
	var up := down.duplicate() as InputEventMouseButton
	up.pressed = false
	up.button_mask = 0
	Input.parse_input_event(up)
	await _frames(4)


func _frames(n: int) -> void:
	for i in n:
		await get_tree().process_frame


func _find_button(node: Node, text: String) -> Control:
	if node is BaseButton and (node as Control).is_visible_in_tree() and _text_of(node as Control) == text:
		return node as Control
	for child: Node in node.get_children():
		var found := _find_button(child, text)
		if found != null:
			return found
	return null


## What the player reads: the text through the control's own translation.
static func _text_of(c: Control) -> String:
	var raw := ""
	if c is Button:
		raw = (c as Button).text
	elif c is Label:
		raw = (c as Label).text
	elif c is LineEdit:
		raw = (c as LineEdit).text
	return c.atr(raw) if not raw.is_empty() else ""


func _dump(node: Node) -> void:
	if node is CanvasItem and not (node as CanvasItem).is_visible_in_tree():
		return
	if node is Button or node is Label:
		var c := node as Control
		var t := _text_of(c).replace("\n", " / ")
		if not t.is_empty():
			_out.append("%s %s '%s' %s" % [node.get_class(), node.name, t.left(90), c.get_global_rect()])
	for child: Node in node.get_children():
		_dump(child)
