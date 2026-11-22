extends SceneTree
## Headless check of the Sound autoload. Unit: audio-player. Run from game/ after `cargo xtask gen` and
## an import:
##   godot --headless --path . -s res://test/unit/sound_test.gd
## Checks: every generated file loads and loop files carry loop points; the bus layout; an interface
## sound takes a Ui voice; the menu theme starts on Music and fades out when stopped; alert_alarm
## restarts while listed; a positional splash with a speed-of-sound delay holds while the sortie clock
## stands still, then pans on Sfx; the volume settings move and mute their buses (and are restored);
## bad events and loop lists are ignored; the pure helpers (slider curve, listener basis, sortie rate).
## Then it starts a skirmish, sees the sea and engine beds sound and stop after leaving. Exits 1 on any
## failure.

const STARTER := "starters/fac22.bp.ron"
const TIMEOUT_MS := 20000

var _sound: Node
var _feed: Node
var _session: Node
var _settings: Node
var _failures := 0
var _step := 0
var _step_ms := 0
var _master_before: Variant = 1.0
var _effects_before: Variant = 1.0
var _settings_saved := false
var _alarm_last_s := -1.0
var _alarm_wraps := 0
var _ui_seen := false


func _initialize() -> void:
	_sound = root.get_node_or_null("Sound")
	_feed = root.get_node_or_null("AudioFeed")
	_session = root.get_node_or_null("Session")
	_settings = root.get_node_or_null("Settings")
	if _sound == null or _feed == null or _session == null or _settings == null:
		_fail("an autoload is missing")
		_finish()
	_step_ms = Time.get_ticks_msec()


func _process(_delta: float) -> bool:
	if _step >= 100:
		return true
	if _failures > 0:
		return _finish()
	var waited := Time.get_ticks_msec() - _step_ms
	if waited > TIMEOUT_MS:
		_fail("step %d timed out" % _step)
		return _finish()
	var st: Dictionary = _sound.call(&"debug_state")
	match _step:
		0:
			if int(st["loading"]) == 0:
				_check_catalogue(st)
				_check_helpers()
				_check_buses()
				if int(st["busy"][&"Ui"]) != 0:
					_fail("a Ui voice is busy before any interface sound")
				_feed.call(&"ui_sound", Bp.UI_SOUND_CLICK)
				_next()
		1:
			# A click is a few tens of milliseconds long: see it busy at least once.
			_ui_seen = _ui_seen or int(st["busy"][&"Ui"]) >= 1
			if _ui_seen or waited > 300:
				if not _ui_seen:
					_fail("ui_sound started no Ui voice")
				_check_bad_input()
				_feed.call(&"set_menu_theme", true)
				_next()
		2:
			if _loop_playing(st, &"menu_theme") and float(st["loops"][&"menu_theme"]["env"]) >= 1.0:
				if st["loops"][&"menu_theme"]["bus"] != &"Music":
					_fail("menu theme is not on Music")
				_feed.call(&"set_menu_theme", false)
				_next()
		3:
			if not st["loops"].has(&"menu_theme"):
				if waited < 900:
					_fail("menu theme stopped after %d ms; it should fade over 1 s" % waited)
				# Inject an alarm pattern: the file is 1.5 s long and must restart while listed.
				_sound.call(&"_set_loops", [{"id": 4242, "sound": &"alert_alarm", "bus": &"Alert", "volume_db": 0.0, "pitch": 1.0, "positional": false}])
				_next()
		4:
			if _loop_playing(st, &"alert_alarm"):
				var at: float = st["loops"][&"alert_alarm"]["position_s"]
				if at + 0.2 < _alarm_last_s:
					_alarm_wraps += 1
				_alarm_last_s = at
			if waited > 3500:
				print("sound_test: alert_alarm restarted %d times in 3.5 s" % _alarm_wraps)
				if _alarm_wraps < 1:
					_fail("alert_alarm did not restart while listed")
				_sound.call(&"_set_loops", [])
				_next()
		5:
			if not st["loops"].has(&"alert_alarm"):
				if waited > 600:
					_fail("alert_alarm took %d ms to fade out; expected 150 ms" % waited)
				# A distant splash 0.3 s of sound travel away, while the sortie clock stands still.
				_sound.set(&"_sim_rate", 0.0)
				_sound.call(&"_take_event", {"sound": &"impact_splash_large", "bus": &"Sfx", "volume_db": -6.0, "pitch": 1.0,
					"positional": true, "pos": Vector3(80.0, 0.0, -60.0), "max_distance_m": 4000.0, "delay_s": 0.3, "priority": 3})
				_next()
		6:
			if waited > 600:
				if int(st["pending"]) != 1 or int(st["busy_3d"][&"Sfx"]) != 0:
					_fail("a delayed sound did not hold while paused: %s" % str(st))
				_sound.set(&"_sim_rate", 1.0)
				_next()
		7:
			if int(st["busy_3d"][&"Sfx"]) >= 1:
				if waited < 250:
					_fail("the delayed sound started after %d ms; it should wait 0.3 s" % waited)
				if int(st["pending"]) != 0:
					_fail("the delayed sound is still pending")
				_master_before = _settings.call(&"get_value", &"audio.master")
				_effects_before = _settings.call(&"get_value", &"audio.effects")
				_settings_saved = true
				_settings.call(&"set_value", &"audio.master", 0.0)
				_settings.call(&"set_value", &"audio.effects", 0.5)
				_next()
		8:
			var master := AudioServer.get_bus_index(&"Master")
			var sfx := AudioServer.get_bus_index(&"Sfx")
			if AudioServer.is_bus_mute(master) and absf(AudioServer.get_bus_volume_db(sfx) + 12.04) < 0.1:
				_restore_settings()
				_next()
		9:
			if not AudioServer.is_bus_mute(AudioServer.get_bus_index(&"Master")):
				_start_skirmish()
				_next()
		10:
			if int(_session.call(&"state")) == Bp.SESSION_IN_SORTIE:
				_next()
		11:
			if waited > 3000:
				var names: Array = st["loops"].keys()
				print("sound_test: sortie loops ", names, " busy ", st["busy"])
				if not names.any(func(n: StringName) -> bool: return String(n).begins_with("ambience_sea_") and _loop_playing(st, n)):
					_fail("no sea bed in the sortie")
				if not names.any(func(n: StringName) -> bool: return String(n).begins_with("ambience_engine_") and _loop_playing(st, n)):
					_fail("no engine loop in the sortie")
				_session.call(&"leave", false)
				_next()
		12:
			if (st["loops"] as Dictionary).is_empty() and int(st["pending"]) == 0:
				print("sound_test: sortie loops stopped after leaving")
				_next()
		13:
			# Give the audio thread time to retire the stopped playbacks before quitting.
			if waited > 500:
				return _finish()
	return false


func _next() -> void:
	_step += 1
	_step_ms = Time.get_ticks_msec()


func _loop_playing(st: Dictionary, stem: StringName) -> bool:
	var loops: Dictionary = st["loops"]
	return loops.has(stem) and loops[stem]["playing"] == true and loops[stem]["leaving"] == false


func _check_catalogue(st: Dictionary) -> void:
	print("sound_test: catalogue ", st["known"], " known, ", st["loaded"], " loaded, ", st["missing"], " missing")
	if int(st["known"]) == 0:
		_fail("AudioFeed.sound_ids is empty")
	if int(st["missing"]) > 0:
		_fail("%d sound files are missing; run cargo xtask gen and import" % int(st["missing"]))
	for stem: String in _feed.call(&"sound_ids"):
		var info: Dictionary = _feed.call(&"sound_info", StringName(stem))
		var path := "res://audio/gen/%s.wav" % stem
		if not ResourceLoader.exists(path):
			continue
		var wav := load(path) as AudioStreamWAV
		if wav == null:
			_fail("%s is not an AudioStreamWAV" % path)
			continue
		var loops := wav.loop_mode != AudioStreamWAV.LOOP_DISABLED
		if loops != (info.get("loop", false) == true):
			_fail("%s: loop mode %d but the catalogue says loop=%s" % [stem, wav.loop_mode, info.get("loop")])
		if loops and wav.loop_end <= wav.loop_begin:
			_fail("%s: empty loop range" % stem)


func _check_buses() -> void:
	for bus: StringName in [&"Master", &"Sfx", &"Ambience", &"Alert", &"Radio", &"Ui", &"Music"]:
		if AudioServer.get_bus_index(bus) < 0:
			_fail("bus %s is missing from the layout" % bus)
		elif bus != &"Master" and AudioServer.get_bus_send(AudioServer.get_bus_index(bus)) != &"Master":
			_fail("bus %s does not send to Master" % bus)
	if AudioServer.get_bus_effect_count(0) < 1 or not AudioServer.get_bus_effect(0, 0) is AudioEffectHardLimiter:
		_fail("Master has no limiter")


func _check_helpers() -> void:
	var script: GDScript = _sound.get_script()
	_near(script.call(&"slider_db", 0.5), -12.04, 0.05, "slider_db(0.5)")
	_near(script.call(&"slider_db", 1.0), 0.0, 0.001, "slider_db(1)")
	_near(script.call(&"slider_db", 0.0), -80.0, 0.001, "slider_db(0)")
	_near(script.call(&"slider_db", NAN), 0.0, 0.001, "slider_db(nan)")
	_near(script.call(&"sim_rate", {"paused": true, "rate": 4}), 0.0, 0.0, "sim_rate paused")
	_near(script.call(&"sim_rate", {"paused": false, "rate": 4, "time_scale": 0.5}), 2.0, 0.0, "sim_rate 4x at 0.5")
	_near(script.call(&"sim_rate", {"rate": "x"}), 1.0, 0.0, "sim_rate bad")
	_near(script.call(&"loop_volume_db", -6.0, 0.0), -80.0, 0.0, "loop_volume_db silent")
	_near(script.call(&"loop_volume_db", -6.0, 1.0), -6.0, 0.001, "loop_volume_db full")
	_near(script.call(&"loop_volume_db", -6.0, 0.5), -12.02, 0.05, "loop_volume_db half")
	for args: Array in [[Vector3.ZERO, Vector3.UP, Vector3.UP], [Vector3(1, 2, 3), Vector3.ZERO, Vector3.ZERO], [Vector3(INF, 0, 0), Vector3(NAN, 0, 0), Vector3.DOWN], [Vector3.ZERO, Vector3(0, 0, -1), Vector3.UP]]:
		var t: Transform3D = script.callv(&"listener_transform", args)
		if not (t.origin.is_finite() and t.basis.x.is_finite() and is_equal_approx(t.basis.determinant(), 1.0)):
			_fail("listener_transform%s gave %s" % [str(args), str(t)])
	var t2: Transform3D = script.call(&"listener_transform", Vector3.ZERO, Vector3(1, 0, 0), Vector3.UP)
	if not (-t2.basis.z).is_equal_approx(Vector3(1, 0, 0)):
		_fail("listener does not face forward")


func _check_bad_input() -> void:
	for e: Dictionary in [{}, {"sound": 5}, {"sound": &"no_such_sound"}, {"sound": &"ui_click", "volume_db": NAN, "pitch": -3.0},
			{"sound": &"impact_splash_small", "positional": true, "pos": Vector3(NAN, 0, 0), "delay_s": INF, "priority": "high"},
			{"sound": &"ui_click", "bus": &"Nope", "pitch": 0.0, "volume_db": 200.0}]:
		_sound.call(&"_take_event", e)
	_sound.call(&"_set_loops", [3, {}, {"sound": &"no_such_sound", "id": 1}, {"sound": &"impact_fire_loop", "id": -5},
		{"sound": &"impact_fire_loop", "id": 9, "volume_db": INF, "pitch": NAN, "positional": true, "pos": "x"}])
	_sound.call(&"_set_loops", [])


func _start_skirmish() -> void:
	var setup := {
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 1, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 77, "blueprint": STARTER,
	}
	var problems: Array = _session.call(&"start_skirmish", setup)
	if not problems.is_empty():
		_fail("start_skirmish refused: %s" % str(problems))


func _near(got: Variant, want: float, tol: float, what: String) -> void:
	if not (got is float or got is int) or absf(float(got) - want) > tol:
		_fail("%s = %s, want %s" % [what, str(got), str(want)])


func _fail(text: String) -> void:
	_failures += 1
	printerr("sound_test: FAIL ", text)


func _restore_settings() -> void:
	if _settings_saved and _settings != null:
		_settings.call(&"set_value", &"audio.master", _master_before)
		_settings.call(&"set_value", &"audio.effects", _effects_before)
	_settings_saved = false


func _finish() -> bool:
	_step = 100
	_restore_settings()
	print("sound_test: ", "ok" if _failures == 0 else "%d failures" % _failures)
	quit(1 if _failures > 0 else 0)
	return true
