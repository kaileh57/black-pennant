extends Node
## Input tests through the real input pipeline (DESIGN.md L1304: scene tests that simulate input
## live in res://test/ui and run windowed, under a virtual display in CI; L1738 focus order, focus
## restore and gamepad parity). Unit: ui-tests. Every event goes through Input.parse_input_event,
## so it takes the same path as a key press or a pad button:
##   HOME=/tmp/bp-input-home godot --path game --resolution 1600x900 res://test/ui/input_test.tscn
## Checks:
## - Main menu, keyboard and D-pad: Down from the first control visits every visible button once,
##   top to bottom, and Up walks back; focus never rests on a hidden or disabled control.
## - Enter on a menu button opens its screen over the menu; Esc closes it and focus returns to the
##   button that opened it. The same with the pad's A and B.
## - Focus stays inside the top screen: arrows and the D-pad over each pushed screen never move
##   focus to the menu beneath.
## - Sortie (BpTestFeed `battle` at the sinking Emergency): each bound key and pad button of the
##   alert, emission, helm and time actions sends its order (read back with take_commands).
## Exits 1 on any failure. Headless, or when instanced by check_project, it does nothing.

const TICK_HZ := 60
## Screens checked for focus containment and Esc back, over the main menu.
const PUSHED: Array[StringName] = [&"settings", &"credits", &"skirmish_setup", &"sandbox_setup", &"campaign_new", &"multiplayer", &"blueprint_browser", &"replay_browser"]
## Sortie actions and the command each must send (a substring of its Debug form).
const ORDERS: Array[Array] = [
	[&"alert_acknowledge", "Acknowledge"],
	[&"alert_silence", "SilenceAlerts"],
	[&"emcon_cycle", "SetEmcon"],
	[&"take_helm", "TakeControl"],
	[&"time_faster", "RequestTimeRate"],
]

var _failures := 0
var _checks := 0
var _feed: Node


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	# A covered window draws nothing on macOS and drops focus; keep this short run on top.
	get_window().always_on_top = true
	_run.call_deferred()


func _ok(cond: bool, what: String) -> void:
	_checks += 1
	if not cond:
		_failures += 1
		printerr("input_test: FAIL ", what)


func _frames(n: int) -> void:
	for _i: int in range(n):
		await get_tree().process_frame


func _run() -> void:
	Settings.reload()
	Ui.refresh()
	Screens.start_routing()
	Screens.change(&"main_menu")
	await _frames(20)
	await _menu_order(false)
	await _menu_order(true)
	await _open_and_back(false)
	await _open_and_back(true)
	for screen: StringName in PUSHED:
		await _contained(screen)
	await _sortie_keys()
	print("input_test: %d checks, %d failures" % [_checks, _failures])
	get_tree().quit(1 if _failures > 0 else 0)


# --- events ---------------------------------------------------------------------------------------

func _key(code: Key, shift: bool = false) -> void:
	for pressed: bool in [true, false]:
		var e := InputEventKey.new()
		e.keycode = code
		e.physical_keycode = code
		e.shift_pressed = shift
		e.pressed = pressed
		Input.parse_input_event(e)
		await get_tree().process_frame


func _pad(button: JoyButton) -> void:
	for pressed: bool in [true, false]:
		var e := InputEventJoypadButton.new()
		e.device = 0
		e.button_index = button
		e.pressed = pressed
		e.pressure = 1.0 if pressed else 0.0
		Input.parse_input_event(e)
		await get_tree().process_frame


## Down, Up, Left, Right, accept or cancel, from the keyboard or the pad.
func _nav(dir: StringName, pad: bool) -> void:
	if pad:
		var b: JoyButton = {&"down": JOY_BUTTON_DPAD_DOWN, &"up": JOY_BUTTON_DPAD_UP, &"left": JOY_BUTTON_DPAD_LEFT, &"right": JOY_BUTTON_DPAD_RIGHT, &"accept": JOY_BUTTON_A, &"cancel": JOY_BUTTON_B}[dir]
		await _pad(b)
	else:
		var k: Key = {&"down": KEY_DOWN, &"up": KEY_UP, &"left": KEY_LEFT, &"right": KEY_RIGHT, &"accept": KEY_ENTER, &"cancel": KEY_ESCAPE}[dir]
		await _key(k)
	await _frames(2)


func _focus() -> Control:
	return get_viewport().gui_get_focus_owner()


# --- menus ----------------------------------------------------------------------------------------

func _menu_buttons(menu: Node) -> Array[Control]:
	var out: Array[Control] = []
	for n: Node in menu.find_children("*", "BaseButton", true, false):
		var b := n as BaseButton
		if b.is_visible_in_tree() and not b.disabled and b.focus_mode != Control.FOCUS_NONE:
			out.append(b)
	out.sort_custom(func(a: Control, b: Control) -> bool: return a.global_position.y < b.global_position.y)
	return out


func _menu_order(pad: bool) -> void:
	var dev := "pad" if pad else "keys"
	var menu: Node = Screens.top()
	var buttons := _menu_buttons(menu)
	_ok(buttons.size() >= 8, "%s: the main menu has %d focusable buttons" % [dev, buttons.size()])
	if buttons.is_empty():
		return
	buttons[0].grab_focus()
	await _frames(2)
	var seen: Array[Control] = [_focus()]
	for _i: int in range(buttons.size() - 1):
		await _nav(&"down", pad)
		var f := _focus()
		_ok(f != null and f.is_visible_in_tree(), "%s: Down left focus on nothing or a hidden control" % dev)
		seen.append(f)
	_ok(seen == buttons, "%s: Down visits %s, not the buttons top to bottom %s" % [dev, _names(seen), _names(buttons)])
	for i: int in range(buttons.size() - 1, 0, -1):
		await _nav(&"up", pad)
		_ok(_focus() == buttons[i - 1], "%s: Up from %s went to %s" % [dev, _names([buttons[i]]), _names([_focus()])])


func _names(list: Array) -> String:
	var out := PackedStringArray()
	for c: Variant in list:
		if c == null or not is_instance_valid(c):
			out.append("<none>")
		elif c is Button:
			out.append((c as Button).text)
		else:
			out.append(String((c as Node).name))
	return ", ".join(out)


## Enter or A on Settings and Credits opens them over the menu; Esc or B closes them and focus
## comes back to the same button.
func _open_and_back(pad: bool) -> void:
	var dev := "pad" if pad else "keys"
	var menu: Node = Screens.top()
	for word: String in ["settings", "credits"]:
		var target: Control = null
		for b: Control in _menu_buttons(menu):
			if b is Button and (b as Button).text.to_lower().contains(word):
				target = b
		_ok(target != null, "%s: no menu button for %s" % [dev, word])
		if target == null:
			continue
		target.grab_focus()
		await _frames(2)
		var depth := Screens.depth()
		await _nav(&"accept", pad)
		await _frames(10)
		_ok(Screens.depth() == depth + 1 and String(Screens.top_name()).contains(word), "%s: accept on %s opened %s (depth %d)" % [dev, word, Screens.top_name(), Screens.depth()])
		var f := _focus()
		_ok(f != null and Screens.top().is_ancestor_of(f), "%s: %s opened without focus inside it" % [dev, word])
		await _nav(&"cancel", pad)
		await _frames(10)
		_ok(Screens.depth() == depth, "%s: cancel did not close %s" % [dev, word])
		_ok(_focus() == target, "%s: focus came back to %s, not the %s button" % [dev, _names([_focus()]), word])


## Arrows and the D-pad over a pushed screen keep focus inside it.
func _contained(screen: StringName) -> void:
	var node: Node = Screens.push(screen)
	await _frames(15)
	_ok(node != null, "%s did not open" % screen)
	if node == null:
		return
	var escaped := 0
	var empty := 0
	var moves: Array[StringName] = [&"down", &"down", &"down", &"down", &"right", &"down", &"down", &"right", &"up", &"left", &"up", &"left", &"up", &"down"]
	for pad: bool in [false, true]:
		for m: StringName in moves:
			await _nav(m, pad)
			var f := _focus()
			if f == null:
				empty += 1
			elif not node.is_ancestor_of(f) and f != node:
				escaped += 1
	_ok(escaped == 0, "%s: focus left the screen %d times" % [screen, escaped])
	_ok(empty == 0, "%s: focus was lost %d times" % [screen, empty])
	Screens.pop()
	await _frames(5)


# --- sortie ---------------------------------------------------------------------------------------

func _sortie_keys() -> void:
	_feed = ClassDB.instantiate(&"BpTestFeed")
	add_child(_feed)
	if not bool(_feed.call(&"start", "battle")):
		_ok(false, "BpTestFeed.start(battle) refused")
		return
	_feed.call(&"advance", 12)
	for _i: int in range(1200):
		await get_tree().process_frame
		_feed.call(&"advance", 1)
		if Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading() and _hud() != null:
			break
	_ok(_hud() != null and Session.state() == Bp.SESSION_IN_SORTIE, "the battle sortie never came up")
	if _hud() == null:
		return
	# To the sinking Emergency, a second a frame so every cue reaches the HUD.
	for _i: int in range(425):
		_feed.call(&"advance", TICK_HZ)
		await get_tree().process_frame
	await _frames(10)
	# Keys go to the sortie, not to a control that happens to hold focus.
	var f := _focus()
	if f != null:
		f.release_focus()
	_feed.call(&"take_commands")
	for row: Array in ORDERS:
		var action: StringName = row[0]
		var want: String = row[1]
		var keys := 0
		var pads := 0
		for e: InputEvent in InputMap.action_get_events(action):
			if action == &"time_faster":
				# Back to 1x first: at the top rate a faster step has nothing to send.
				Session.request_time_rate(1)
				_feed.call(&"advance", 2)
				await _frames(3)
				_feed.call(&"take_commands")
			if e is InputEventKey:
				var k := e as InputEventKey
				var code := k.physical_keycode if k.physical_keycode != KEY_NONE else k.keycode
				await _key(code, k.shift_pressed)
				keys += 1
			elif e is InputEventJoypadButton:
				await _pad((e as InputEventJoypadButton).button_index)
				pads += 1
			else:
				continue
			_feed.call(&"advance", 2)
			await _frames(3)
			var sent: PackedStringArray = _feed.call(&"take_commands")
			var hit := false
			for c: String in sent:
				hit = hit or c.contains(want)
			_ok(hit, "%s (%s) sent %s, not %s" % [action, e.as_text(), sent, want])
		_ok(keys > 0, "%s has no key binding" % action)
		print("input_test: %s checked on %d keys and %d pad buttons" % [action, keys, pads])
	_feed.call(&"stop")
	await _frames(5)


func _hud() -> Node:
	var found := get_tree().root.find_children("SortieHud", "CanvasLayer", true, false)
	return null if found.is_empty() else found[0]
