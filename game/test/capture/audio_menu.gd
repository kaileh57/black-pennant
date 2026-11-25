extends Node
## Windowed audio check of the menus through the real screens (unit audio-player):
##   godot --path game --resolution 1280x720 res://test/capture/audio_menu.tscn -- --out=/abs/dir
## Opens the main menu and checks the menu theme sounds on the Music bus; clicks a real menu button with a
## synthesized mouse press and checks an interface click starts on the Ui bus; opens Settings at the audio
## page, drags the Effects slider with the mouse and checks the Sfx bus gain follows (then restores it);
## starts a sandbox sortie and checks the theme stops and never sounds while in the sortie; leaves and checks
## the theme returns on the main menu. Saves a PNG at each step and prints one line per check. Exits 1 on a
## failure. Headless, and when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
const SETTLE := 45

var _out := "user://captures"
var _failures := 0
var _played: Array[Dictionary] = []


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	DirAccess.make_dir_recursive_absolute(_out)
	Sound.played.connect(func(info: Dictionary) -> void: _played.append(info))
	_run.call_deferred()


func _frames(n: int) -> void:
	for i: int in n:
		await get_tree().process_frame


func _expect(ok: bool, what: String) -> void:
	print("audio_menu: %s %s" % ["ok  " if ok else "FAIL", what])
	if not ok:
		_failures += 1


func _shot(name: String) -> void:
	get_viewport().get_texture().get_image().save_png(_out.path_join(name + ".png"))


func _theme_loop() -> Dictionary:
	var loops: Dictionary = Sound.debug_state()["loops"]
	return loops.get(&"menu_theme", {})


## Window pixels of a canvas point (the base viewport is 1920x1080, stretched to the window).
func _win(canvas: Vector2) -> Vector2:
	return get_viewport().get_final_transform() * canvas


func _click_at(canvas_pos: Vector2) -> void:
	var pos := _win(canvas_pos)
	var motion := InputEventMouseMotion.new()
	motion.position = pos
	motion.global_position = pos
	Input.parse_input_event(motion)
	await _frames(2)
	for pressed: bool in [true, false]:
		var e := InputEventMouseButton.new()
		e.button_index = MOUSE_BUTTON_LEFT
		e.pressed = pressed
		e.position = pos
		e.global_position = pos
		Input.parse_input_event(e)
		await _frames(2)


func _find(root: Node, pred: Callable) -> Node:
	if pred.call(root):
		return root
	for c: Node in root.get_children():
		var f := _find(c, pred)
		if f != null:
			return f
	return null


## Drag the slider of `key` from its right end to its middle and check `bus` follows, then restore it.
func _drag_check(settings: Node, key: StringName, bus: StringName) -> void:
	var label := tr(&"settings." + String(key))
	var slider := _find(settings, func(n: Node) -> bool: return n is Slider and String((n as Slider).accessibility_name) == label) as Slider
	var index := AudioServer.get_bus_index(bus)
	var before_value: Variant = Settings.get_value(key)
	Settings.set_value(key, 1.0)
	await _frames(2)
	var before_db := AudioServer.get_bus_volume_db(index)
	_played.clear()
	if slider != null:
		var r := slider.get_global_rect()
		var from := _win(Vector2(r.end.x - 6.0, r.get_center().y))
		var to := _win(Vector2(r.position.x + r.size.x * 0.5, r.get_center().y))
		var motion := InputEventMouseMotion.new()
		motion.position = from
		motion.global_position = from
		Input.parse_input_event(motion)
		await _frames(2)
		var down := InputEventMouseButton.new()
		down.button_index = MOUSE_BUTTON_LEFT
		down.pressed = true
		down.position = from
		down.global_position = from
		Input.parse_input_event(down)
		await _frames(2)
		for k: int in range(1, 11):
			var m := InputEventMouseMotion.new()
			m.position = from.lerp(to, k / 10.0)
			m.global_position = m.position
			m.button_mask = MOUSE_BUTTON_MASK_LEFT
			Input.parse_input_event(m)
			await _frames(1)
		var up := InputEventMouseButton.new()
		up.button_index = MOUSE_BUTTON_LEFT
		up.pressed = false
		up.position = to
		up.global_position = to
		Input.parse_input_event(up)
		await _frames(4)
	var after_db := AudioServer.get_bus_volume_db(index)
	var after_value := float(Settings.get_value(key))
	var ticks := _played.filter(func(i: Dictionary) -> bool: return i.get("sound") == &"ui_select").size()
	_expect(slider != null and after_value < 0.75 and absf(after_db - Sound.slider_db(after_value)) < 0.01 and after_db < before_db - 3.0 and ticks >= 1,
		"dragging %s to %.2f sets the %s bus to %.1f dB (was %.1f dB), %d ticks" % [label, after_value, bus, after_db, before_db, ticks])
	Settings.set_value(key, before_value)
	await _frames(2)


func _run() -> void:
	# Main menu: the theme fades in on Music.
	Screens.change(&"main_menu")
	await _frames(SETTLE * 2)
	var theme := _theme_loop()
	_expect(not theme.is_empty() and theme.get("bus") == &"Music" and theme.get("playing") == true and float(theme.get("env", 0.0)) > 0.9, "menu theme plays on Music in the main menu: %s" % [theme])
	_shot("menu_main")
	# A real button pressed with the mouse clicks on Ui.
	var menu := Screens.top()
	var button := _find(menu, func(n: Node) -> bool: return n is Button and (n as Button).is_visible_in_tree() and not (n as Button).disabled and (n as Button).text.to_lower().contains("credits")) as Button
	if button == null:
		button = _find(menu, func(n: Node) -> bool: return n is Button and (n as Button).is_visible_in_tree() and not (n as Button).disabled) as Button
	_played.clear()
	if button != null:
		await _click_at(button.get_global_rect().get_center())
		await _frames(4)
	var clicks := _played.filter(func(i: Dictionary) -> bool: return i.get("sound") == &"ui_click" and i.get("bus") == &"Ui")
	_expect(button != null and clicks.size() == 1, "pressing '%s' with the mouse plays one ui_click on Ui (heard %s)" % [button.text if button != null else "?", _played.map(func(i: Dictionary) -> String: return String(i.get("sound")))])
	_shot("menu_after_click")
	if Screens.depth() > 0 and Screens.top_name() != &"main_menu":
		Screens.pop()
		await _frames(SETTLE)
	# Screens opened and pages switched from code make no interface sound.
	await _frames(30)
	_played.clear()
	var settings: Node = Screens.push(&"settings", {"section": &"graphics"})
	await _frames(SETTLE)
	settings.call(&"_show_section", &"accessibility")
	await _frames(10)
	settings.call(&"_show_section", &"audio")
	await _frames(SETTLE)
	var ui_heard := _played.filter(func(i: Dictionary) -> bool: return i.get("bus") == &"Ui")
	_expect(ui_heard.is_empty(), "opening settings and switching pages from code is silent (heard %s)" % [ui_heard.map(func(i: Dictionary) -> String: return String(i.get("sound")))])
	_shot("settings_audio")
	# Every volume slider, dragged with the mouse from full to half, moves its bus; then it is restored.
	for row: Array in [[&"audio.master", &"Master"], [&"audio.effects", &"Sfx"], [&"audio.ambience", &"Ambience"], [&"audio.alerts", &"Alert"], [&"audio.radio", &"Radio"], [&"audio.interface", &"Ui"], [&"audio.menu_theme", &"Music"]]:
		await _drag_check(settings, row[0], row[1])
	_shot("settings_audio_dragged")
	Screens.pop()
	await _frames(SETTLE)
	# A sortie: the theme stops and stays silent.
	_played.clear()
	var problems := Session.start_sandbox({"blueprint": STARTER, "weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 7, "targets": []})
	_expect(problems.is_empty(), "a sandbox starts: %s" % [problems])
	var world := (load("res://world/world.tscn") as PackedScene).instantiate()
	add_child(world)
	Screens.clear()
	var waited := 0
	while Session.state() != Bp.SESSION_IN_SORTIE and waited < 1200:
		await _frames(1)
		waited += 1
	var theme_in_sortie := false
	for i: int in 240:
		await _frames(1)
		theme_in_sortie = theme_in_sortie or (not _theme_loop().is_empty() and _theme_loop().get("leaving") == false)
	_expect(Session.state() == Bp.SESSION_IN_SORTIE and not theme_in_sortie, "no menu theme while in the sortie (state %d)" % Session.state())
	var beds := (Sound.debug_state()["loops"] as Dictionary).keys()
	_expect(beds.has(&"ambience_sea_moderate") or beds.has(&"ambience_sea_calm"), "sea beds sound in the sortie: %s" % [beds])
	_shot("sortie")
	# Pause: the pause chime, and the sea sits back.
	var sea_key := &"ambience_sea_moderate" if (Sound.debug_state()["loops"] as Dictionary).has(&"ambience_sea_moderate") else &"ambience_sea_calm"
	var sea_db := float(((Sound.debug_state()["loops"] as Dictionary).get(sea_key, {}) as Dictionary).get("volume_db", 0.0))
	_played.clear()
	Session.set_paused(true)
	await _frames(30)
	var sea_paused := float(((Sound.debug_state()["loops"] as Dictionary).get(sea_key, {}) as Dictionary).get("volume_db", 0.0))
	_expect(_played.any(func(i: Dictionary) -> bool: return i.get("sound") == &"ui_pause") and sea_paused < sea_db - 10.0, "pausing chimes and ducks the sea (%.1f -> %.1f dB)" % [sea_db, sea_paused])
	Session.set_paused(false)
	await _frames(30)
	_expect(_played.any(func(i: Dictionary) -> bool: return i.get("sound") == &"ui_resume"), "resuming chimes")
	Session.leave(false)
	world.queue_free()
	Screens.change(&"main_menu")
	await _frames(SETTLE * 3)
	theme = _theme_loop()
	_expect(not theme.is_empty() and theme.get("leaving") == false, "the menu theme returns on the main menu: %s" % [theme])
	print("audio_menu: %d failures" % _failures)
	get_tree().quit(1 if _failures > 0 else 0)
