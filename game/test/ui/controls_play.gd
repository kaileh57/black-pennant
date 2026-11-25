extends Node
## Play harness for the controls and accessibility flow: drives the real router, screens and
## autoloads with real input events (Input.parse_input_event) read from a step file, and saves a
## viewport PNG at each `shot`. Windowed only:
##   HOME=/tmp/bp-play godot --path game --resolution 1920x1080 res://test/ui/controls_play.tscn \
##     -- --steps=/abs/steps.txt --out=/abs/dir
## Step file, one step per line (`#` starts a comment):
##   key <name> [shift|ctrl|cmd]   press and release a key (`down`, `enter`, `escape`, `tab`, `w`, `f1`)
##   down <name> / up <name>        press or release a key only
##   pad <button>                   press and release a pad button (a b x y lb rb lt rt start back
##                                  dpad_up dpad_down dpad_left dpad_right ls rs)
##   paddown <button> / padup <button>
##   axis <index> <value>           move a stick or trigger axis (0 to 5) and leave it there
##   click <x> <y> [right]          press and release a mouse button at canvas pixels
##   mdown <x> <y> / mup <x> <y>    press or release the left mouse button only
##   move <x> <y>                   move the mouse
##   wait <seconds>                 let real time pass
##   shot <stem>                    save <out>/<stem>.png and log the focus owner and top screen
##   call <autoload> <method> [args]  call a facade method and print the result
##   fget <property>                print a property of the focus owner
##   set <key> <value>              Settings.set_value (`access.ui_scale 2.0`)
##   screen <name>                  Screens.change (only to reach a start point quickly)
##   quit
## Every line logs as "play: <line> -> focus <path> top <screen>".

var _out := ""
var _steps: PackedStringArray = []

const KEYS: Dictionary = {
	"down": KEY_DOWN, "up": KEY_UP, "left": KEY_LEFT, "right": KEY_RIGHT, "enter": KEY_ENTER,
	"escape": KEY_ESCAPE, "tab": KEY_TAB, "space": KEY_SPACE, "backspace": KEY_BACKSPACE,
	"delete": KEY_DELETE, "pageup": KEY_PAGEUP, "pagedown": KEY_PAGEDOWN, "equal": KEY_EQUAL,
	"minus": KEY_MINUS, "bracketleft": KEY_BRACKETLEFT, "bracketright": KEY_BRACKETRIGHT,
	"period": KEY_PERIOD, "comma": KEY_COMMA, "home": KEY_HOME, "end": KEY_END,
}
const PAD: Dictionary = {
	"a": JOY_BUTTON_A, "b": JOY_BUTTON_B, "x": JOY_BUTTON_X, "y": JOY_BUTTON_Y,
	"lb": JOY_BUTTON_LEFT_SHOULDER, "rb": JOY_BUTTON_RIGHT_SHOULDER, "start": JOY_BUTTON_START,
	"back": JOY_BUTTON_BACK, "dpad_up": JOY_BUTTON_DPAD_UP, "dpad_down": JOY_BUTTON_DPAD_DOWN,
	"dpad_left": JOY_BUTTON_DPAD_LEFT, "dpad_right": JOY_BUTTON_DPAD_RIGHT,
	"ls": JOY_BUTTON_LEFT_STICK, "rs": JOY_BUTTON_RIGHT_STICK, "paddle1": JOY_BUTTON_PADDLE1,
}


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for a: String in OS.get_cmdline_user_args():
		if a.begins_with("--out="):
			_out = a.substr(6)
		elif a.begins_with("--steps="):
			_steps = FileAccess.get_file_as_string(a.substr(8)).split("\n")
	if not _out.is_empty():
		DirAccess.make_dir_recursive_absolute(_out)
	get_window().always_on_top = true
	_run.call_deferred()


func _frames(n: int) -> void:
	for _i: int in range(n):
		await get_tree().process_frame


func _run() -> void:
	Settings.reload()
	Ui.refresh()
	Screens.start_routing()
	Screens.change(&"main_menu")
	Session.command_rejected.connect(func(text: Dictionary) -> void: print("play: rejected ", Tx.msg(text)))
	await _frames(20)
	for raw: String in _steps:
		var line := raw.strip_edges()
		if line.is_empty() or line.begins_with("#"):
			continue
		var w := line.split(" ", false)
		match w[0]:
			"key":
				await _key(w[1], w.slice(2), true)
				await _key(w[1], w.slice(2), false)
			"down":
				await _key(w[1], w.slice(2), true)
			"up":
				await _key(w[1], w.slice(2), false)
			"pad":
				await _pad(w[1], true)
				await _pad(w[1], false)
			"paddown":
				await _pad(w[1], true)
			"padup":
				await _pad(w[1], false)
			"axis":
				var m := InputEventJoypadMotion.new()
				m.device = 0
				m.axis = int(w[1]) as JoyAxis
				m.axis_value = float(w[2])
				Input.parse_input_event(m)
				await _frames(1)
			"click":
				await _click(Vector2(float(w[1]), float(w[2])), w.size() > 3 and w[3] == "right")
			"mdown", "mup":
				var mb := InputEventMouseButton.new()
				var mp := Vector2(float(w[1]), float(w[2]))
				mb.button_index = MOUSE_BUTTON_LEFT
				mb.position = mp
				mb.global_position = mp
				mb.pressed = w[0] == "mdown"
				Input.parse_input_event(mb)
				await _frames(1)
			"move":
				var mm := InputEventMouseMotion.new()
				var p := Vector2(float(w[1]), float(w[2]))
				mm.position = p
				mm.global_position = p
				mm.relative = Vector2(8, 0)
				Input.parse_input_event(mm)
				get_viewport().warp_mouse(p)
				await _frames(1)
			"call":
				# call <Autoload> <method> [args as Godot expressions]: print the result.
				var target := get_node_or_null(NodePath("/root/" + w[1]))
				var args: Array = []
				for a: String in w.slice(3):
					args.append(str_to_var(a))
				print("play: call ", w[1], ".", w[2], " -> ", target.callv(StringName(w[2]), args) if target != null else "no autoload")
			"fget":
				# fget <property>: print a property of the focus owner.
				var fo := get_viewport().gui_get_focus_owner()
				print("play: fget ", w[1], " -> ", fo.get(w[1]) if fo != null else "no focus")
			"wait":
				await get_tree().create_timer(float(w[1]), true, false, true).timeout
			"shot":
				await _frames(3)
				var img := get_viewport().get_texture().get_image()
				img.save_png(_out.path_join(w[1] + ".png"))
			"set":
				var v: Variant = str_to_var(w[2])
				Settings.set_value(StringName(w[1]), v)
				await _frames(5)
			"screen":
				Screens.change(StringName(w[1]))
				await _frames(10)
			"quit":
				break
		await _frames(2)
		_log(line)
	print("play: done")
	get_tree().quit(0)


func _log(line: String) -> void:
	var f := get_viewport().gui_get_focus_owner()
	var fp := "<none>"
	if f != null:
		fp = String(f.get_path()).trim_prefix("/root/Screens/")
		if f is Button:
			fp += " [%s]" % (f as Button).text
	print("play: %s -> focus %s top %s paused %s" % [line, fp, Screens.top_name(), get_tree().paused])


func _key(name: String, mods: PackedStringArray, pressed: bool) -> void:
	var e := InputEventKey.new()
	var code: Key
	if KEYS.has(name):
		code = KEYS[name]
	elif name.length() == 1:
		code = OS.find_keycode_from_string(name.to_upper())
	else:
		code = OS.find_keycode_from_string(name)
	e.keycode = code
	e.physical_keycode = code
	e.shift_pressed = "shift" in mods
	e.ctrl_pressed = "ctrl" in mods
	e.meta_pressed = "cmd" in mods
	e.pressed = pressed
	Input.parse_input_event(e)
	await _frames(1)


func _pad(name: String, pressed: bool) -> void:
	if name == "lt" or name == "rt":
		var m := InputEventJoypadMotion.new()
		m.device = 0
		m.axis = JOY_AXIS_TRIGGER_LEFT if name == "lt" else JOY_AXIS_TRIGGER_RIGHT
		m.axis_value = 1.0 if pressed else 0.0
		Input.parse_input_event(m)
		await _frames(1)
		return
	var e := InputEventJoypadButton.new()
	e.device = 0
	e.button_index = PAD[name]
	e.pressed = pressed
	e.pressure = 1.0 if pressed else 0.0
	Input.parse_input_event(e)
	await _frames(1)


func _click(p: Vector2, right: bool) -> void:
	get_viewport().warp_mouse(p)
	for pressed: bool in [true, false]:
		var e := InputEventMouseButton.new()
		e.button_index = MOUSE_BUTTON_RIGHT if right else MOUSE_BUTTON_LEFT
		e.position = p
		e.global_position = p
		e.pressed = pressed
		Input.parse_input_event(e)
		await _frames(2)
