extends SceneTree
## Headless proof of the Bridge Board, damage-control, alert and audio-feed data in a real skirmish. Units
## gd-hud-board, gd-hud-damage, gd-hud-alerts, gd-audio-feed. Run from game/:
##   godot --headless --path . -s res://test/unit/hud_board_damage_test.gd
## Starts a skirmish through Session, pulls every getter of these units inside its change signal, checks each
## result against the shapes in crates/gdclient/src/shapes.rs, changes a slider and the ship preset through
## Orders and checks the board reports the live values (the board asks the server for them), reverts one,
## drains the audio feed every frame, then leaves and checks the data clears. Exits 1 on any failure.

const STARTER := "starters/fac22.bp.ron"
const LOADING_LIMIT := 900
const RUN_FRAMES := 900

var _session: BpSession
var _hud: BpHud
var _orders: BpOrders
var _audio: BpAudioFeed
var _frame := 0
var _sortie_frame := -1
var _failures := 0
var _left := false
var _pulls: Dictionary[String, int] = {}
var _max_events := 0
var _stems_heard: Dictionary[String, bool] = {}
var _loop_stems: Dictionary[String, bool] = {}


func _initialize() -> void:
	Engine.max_fps = 60


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 1:
		return _start()
	if _audio != null:
		_drain_audio()
	if _left:
		return _after_leave()
	var state := _session.state()
	if _sortie_frame < 0:
		if state == Bp.SESSION_IN_SORTIE:
			_sortie_frame = _frame
			print("hud_board_damage_test: in sortie after %d frames" % _frame)
		elif state == Bp.SESSION_FAILED or _frame > LOADING_LIMIT:
			_fail("the sortie never started (state %d)" % state)
			return _finish()
		return false
	var since := _frame - _sortie_frame
	match since:
		30:
			_check_shapes()
		40:
			if not _orders.set_slider("gunnery.aggression", 0.9):
				_fail("set_slider refused")
			if not _orders.set_preset(Bp.NONE, Bp.PRESET_AGGRESSIVE):
				_fail("set_preset refused")
		70:
			# A second change half a second later: the board must follow it within a round trip.
			if not _orders.set_slider("gunnery.aggression", 0.7):
				_fail("second set_slider refused")
		100:
			var s := _slider("gunnery.aggression")
			if s.is_empty() or abs(float(s["value"]) - 0.7) > 0.001:
				_fail("the board lags the second slider change: %s" % str(s))
		300:
			_check_live_values()
			if not _orders.revert("slider/gunnery.aggression"):
				_fail("revert refused")
		560:
			_check_reverted()
		RUN_FRAMES:
			_report()
			_session.leave(false)
			_left = true
	return false


func _start() -> bool:
	_session = root.get_node("Session") as BpSession
	_hud = root.get_node("Hud") as BpHud
	_orders = root.get_node("Orders") as BpOrders
	_audio = root.get_node("AudioFeed") as BpAudioFeed
	if _session == null or _hud == null or _orders == null or _audio == null:
		_fail("an autoload is missing")
		return _finish()
	var pulls := {
		"board_changed": func() -> void: _hud.board(); _hud.takeable_roles(); _hud.emcon(); _hud.rules(),
		"compartments_changed": func() -> void: _hud.compartments(); _hud.damage_summary(); _hud.damage_controls(),
		"networks_changed": func() -> void: _hud.networks(),
		"crew_changed": func() -> void: _hud.crew(),
		"alerts_changed": func() -> void: _hud.alert_strip(); _hud.alerts(false); _hud.alert_level(),
		"ship_static_changed": func() -> void: _hud.compartments_static(); _hud.hull_outline(),
	}
	for sig: String in pulls:
		var pull: Callable = pulls[sig]
		_hud.connect(sig, func() -> void:
			_pulls[sig] = _pulls.get(sig, 0) + 1
			pull.call())
	var setup := {
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 2, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_RAIN, "sea_state": 4, "time_of_day": Bp.TIME_DUSK, "seed": 4321, "blueprint": STARTER,
	}
	var problems := _session.start_skirmish(setup)
	if not problems.is_empty():
		_fail("start_skirmish refused: %s" % str(problems))
		return _finish()
	return false


func _keys(what: String, d: Variant, keys: Array) -> void:
	if not d is Dictionary:
		_fail("%s is not a Dictionary" % what)
		return
	for k: String in keys:
		if not (d as Dictionary).has(k):
			_fail("%s lacks key %s" % [what, k])


func _check_shapes() -> void:
	var cs := _hud.compartments_static()
	if cs.is_empty():
		_fail("compartments_static is empty")
	else:
		_keys("CompartmentStaticDict", cs[0], ["id", "name", "deck", "rect", "side_rect", "volume_m3", "has_magazine", "below_waterline", "neighbours"])
		if not (cs[0]["rect"] is Rect2) or (cs[0]["rect"] as Rect2).size.x <= 0.0:
			_fail("compartment rect is not a sized Rect2")
	var live := _hud.compartments()
	if live.size() != cs.size():
		_fail("compartments() has %d rows, compartments_static() %d" % [live.size(), cs.size()])
	elif not live.is_empty():
		_keys("CompartmentDict", live[0], ["id", "fill", "fire", "breached", "valve_open", "sealed"])
	var outline := _hud.hull_outline()
	_keys("hull outline", outline, ["plan", "profile", "waterline_m", "length_m", "beam_m", "decks"])
	if (outline.get("plan", PackedVector2Array()) as PackedVector2Array).size() < 4:
		_fail("hull outline plan has fewer than 4 points")
	_keys("CrewDict", _hud.crew(), ["original_berths", "surviving_berths", "passengers", "passengers_lost", "teams", "repair_order"])
	var teams: Array = _hud.crew().get("teams", [])
	if teams.is_empty():
		_fail("no damage-control teams")
	else:
		_keys("TeamDict", teams[0], ["index", "kind", "target", "progress", "pinned", "text"])
	if (_hud.crew().get("repair_order", PackedInt32Array()) as PackedInt32Array).size() != 6:
		_fail("repair order is not 6 long")
	var nets := _hud.networks()
	if nets.size() != 3:
		_fail("networks() has %d entries" % nets.size())
	else:
		for n: Dictionary in nets:
			_keys("NetworkDict", n, ["kind", "conduits", "consumers_dead", "consumers_on_buffer"])
		var power: Array = nets[0]["conduits"]
		if power.is_empty():
			_fail("no power conduits")
		else:
			_keys("conduit", power[0], ["id", "from", "to", "cut", "jury_rigged", "redundant", "pinned"])
	_keys("damage summary", _hud.damage_summary(), ["flooding", "fires", "breached", "parts_damaged", "parts_heavy", "parts_destroyed", "cells_lost", "heel_deg", "trim_deg", "teams_idle", "pumps_running"])
	for c: Dictionary in _hud.damage_controls():
		_keys("damage control", c, ["slot", "kind", "compartment", "on", "usable", "name"])
	var board := _hud.board()
	_keys("BoardDict", board, ["officers", "radiating", "esm_warning", "emcon", "held"])
	var officers: Array = board.get("officers", [])
	if officers.size() != 5:
		_fail("board has %d officers" % officers.size())
	else:
		_keys("OfficerDict", officers[0], ["role", "name", "task", "reason", "next", "holder", "layer", "overrides", "advisory"])
	if _hud.officer(Bp.OFFICER_SENSORS).get("role", -1) != Bp.OFFICER_SENSORS:
		_fail("officer(SENSORS) is not the sensors officer")
	if not _hud.officer(99).is_empty():
		_fail("officer(99) is not empty")
	var roles := _hud.takeable_roles()
	if roles.is_empty() or roles[0]["action"] != &"take_helm":
		_fail("takeable roles do not start with the helm: %s" % str(roles))
	if _hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_OFFICER:
		_fail("the helm is not with the officer at the start")
	var sliders := _hud.sliders(Bp.OFFICER_GUNNERY)
	if sliders.is_empty():
		_fail("no gunnery sliders")
	else:
		_keys("SliderDict", sliders[0], ["key", "label", "tooltip", "officer", "min", "max", "step", "default", "value", "overridden", "unit", "basic"])
		if not sliders[0]["basic"]:
			_fail("the first slider is not a basic one")
	if not _hud.sliders(-5).is_empty():
		_fail("sliders(-5) is not empty")
	var doctrine := _hud.doctrine(Bp.NONE)
	if doctrine.is_empty():
		_fail("no ship doctrine rows")
	else:
		_keys("DoctrineEntryDict", doctrine[0], ["weapon", "target", "min_stance", "min_id", "min_grade", "salvo", "effective_range_m", "overridden"])
	if not _hud.doctrine(99999).is_empty():
		_fail("doctrine(99999) is not empty")
	for r: Dictionary in _hud.rules():
		_keys("RuleDict", r, ["index", "enabled", "when", "sink", "value", "text", "valid", "active"])
	_keys("emcon", _hud.emcon(), ["emcon", "overridden", "burst_trigger_range_m", "self_defence_range_m", "radiating", "esm_warning", "reason", "recommended"])
	if _hud.max_layer() != Bp.LAYER_RULES:
		_fail("max_layer is %d" % _hud.max_layer())
	_keys("preset", _hud.preset(Bp.NONE), ["preset", "overridden"])
	var strip := _hud.alert_strip()
	_keys("AlertStripDict", strip, ["shown", "more", "flood", "silenced_s", "next_ack"])
	if not _hud.alerts_for(Bp.NONE, Bp.NONE).is_empty():
		_fail("alerts_for(NONE, NONE) is not empty")
	if _hud.alert(-3) != null:
		_fail("alert(-3) is not null")
	_keys("listener", _audio.listener(), ["pos", "forward", "up"])
	_keys("sound_info", _audio.sound_info(&"alert_alarm"), ["bus", "loop", "positional", "channels", "duration_s"])
	if not _audio.sound_info(&"no_such_sound").is_empty():
		_fail("sound_info of an unknown stem is not empty")
	print("hud_board_damage_test: %d compartments, %d conduits, %d officers, %d sliders, %d doctrine rows, %d rules, %d controls" % [cs.size(), (nets[0]["conduits"] as Array).size() if nets.size() == 3 else -1, officers.size(), sliders.size(), doctrine.size(), _hud.rules().size(), _hud.damage_controls().size()])


func _slider(key: String) -> Dictionary:
	for s: Dictionary in _hud.sliders(Bp.OFFICER_GUNNERY):
		if s["key"] == key:
			return s
	return {}


func _check_live_values() -> void:
	var s := _slider("gunnery.aggression")
	if s.is_empty() or abs(float(s["value"]) - 0.7) > 0.001 or not s["overridden"]:
		_fail("the board does not show the overridden aggression: %s" % str(s))
	var p := _hud.preset(Bp.NONE)
	if p.get("preset", -1) != Bp.PRESET_AGGRESSIVE or not p.get("overridden", false):
		_fail("the board does not show the aggressive preset: %s" % str(p))
	# The aggressive preset raises the helm's cruise speed to 0.9 (tuning preset table).
	for h: Dictionary in _hud.sliders(Bp.OFFICER_HELM):
		if h["key"] == "helm.cruise_speed" and abs(float(h["value"]) - 0.9) > 0.001:
			_fail("the helm cruise slider does not follow the preset: %s" % str(h))
	print("hud_board_damage_test: live aggression %s, preset %s" % [str(s.get("value")), str(p)])


func _check_reverted() -> void:
	var s := _slider("gunnery.aggression")
	# Reverted to the design: the preset's value now stands (aggressive sets 0.85). The server still lists it
	# as overridden, because the aggressive preset makes it differ from the design's 0.5.
	if s.is_empty() or not s["overridden"] or abs(float(s["value"]) - 0.85) > 0.001:
		_fail("the reverted aggression is wrong: %s" % str(s))


func _drain_audio() -> void:
	var events := _audio.drain_events()
	_max_events = maxi(_max_events, events.size())
	for e: Dictionary in events:
		_stems_heard[str(e["sound"])] = true
		if not float(e["volume_db"]) <= 6.0 or float(e["delay_s"]) < 0.0:
			_fail("bad audio event %s" % str(e))
	for l: Dictionary in _audio.loops():
		_loop_stems[str(l["sound"])] = true


func _report() -> void:
	print("hud_board_damage_test: pulls %s" % str(_pulls))
	print("hud_board_damage_test: one-shots %s (at most %d a frame)" % [str(_stems_heard.keys()), _max_events])
	print("hud_board_damage_test: loops %s" % str(_loop_stems.keys()))
	print("hud_board_damage_test: strip %s, level %d" % [str(_hud.alert_strip()), _hud.alert_level()])
	if _max_events > 24:
		_fail("more than 24 one-shots in a frame")
	var sea := false
	var engine := false
	for k: String in _loop_stems:
		sea = sea or k.begins_with("ambience_sea_")
		engine = engine or k.begins_with("ambience_engine_")
	if not sea:
		_fail("no sea bed loop")
	if not engine:
		_fail("no engine loop")
	for sig: String in ["board_changed", "ship_static_changed"]:
		if _pulls.get(sig, 0) == 0:
			_fail("%s never fired" % sig)


func _after_leave() -> bool:
	if _frame - _sortie_frame < RUN_FRAMES + 5:
		return false
	if not _hud.compartments_static().is_empty() or not _hud.networks().is_empty() or not _hud.alerts(false).is_empty():
		_fail("the HUD kept damage data after leave")
	for l: Dictionary in _audio.loops():
		if str(l["sound"]).begins_with("ambience_"):
			_fail("an ambience loop kept playing after leave")
	return _finish()


func _finish() -> bool:
	if _session != null and _session.state() != Bp.SESSION_IDLE:
		_session.leave(false)
	print("hud_board_damage_test: ", "FAILED (%d)" % _failures if _failures > 0 else "passed")
	quit(1 if _failures > 0 else 0)
	return true


func _fail(text: String) -> void:
	_failures += 1
	push_error("hud_board_damage_test: " + text)
