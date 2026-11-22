extends SceneTree
## Headless proof of the sortie HUD data and the player orders. Units gd-orders, gd-hud-core, gd-hud-ship,
## gd-hud-tracks. Run from game/:
##   godot --headless --path . -s res://test/unit/hud_orders_test.gd
## Starts a skirmish through Session, connects every Hud change signal to a handler that pulls the getter it
## announces (the re-entrancy the UI relies on), and drives the sortie only through Orders: a course, a
## weapons-group takeover by a short key press, a designation, the helm by stick, queries, bad arguments.
## Checks the results through Hud getters and the log, then leaves and checks the HUD clears. Exits 1 on any
## failure.

const STARTER := "starters/fac22.bp.ron"
const LOADING_LIMIT := 900
const RUN_FRAMES := 720

var _session: BpSession
var _hud: BpHud
var _orders: BpOrders
var _frame := 0
var _sortie_frame := -1
var _failures := 0
var _signals: Dictionary[String, int] = {}
var _rejected: Array[String] = []
var _designated := -1
var _gun_slot := -1
var _replies: Dictionary[String, int] = {"why": 0, "card": 0, "overrides": 0}
var _course_to := Vector2.ZERO
var _start_pos := Vector2.ZERO
var _left := false
var _cached_tracks: Array[Dictionary] = []
var _cached_rev := -1
var _override_ids: Array[String] = []
var _override_off := 0


func _initialize() -> void:
	Engine.max_fps = 60


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 1:
		return _start()
	if _left:
		return _after_leave()
	var state := _session.state()
	if _sortie_frame < 0:
		if state == Bp.SESSION_IN_SORTIE:
			_sortie_frame = _frame
			print("hud_orders_test: in sortie after %d frames" % _frame)
		elif state == Bp.SESSION_FAILED or _frame > LOADING_LIMIT:
			_fail("the sortie never started (state %d)" % state)
			return _finish()
		elif _frame == 5:
			_check_refusals("while loading")
		return false
	var since := _frame - _sortie_frame
	match since:
		30:
			_check_ship()
		45:
			_check_refusals("in a sortie")
			_check_cache()
		46:
			_check_cache_kept()
		60:
			_order_course()
		90:
			_take_group(true)
		94:
			_take_group(false)
		150:
			_check_group(Bp.HOLDER_PLAYER)
			_designate()
			_query()
		160:
			_fire_by_hand()
			if not _orders.set_preset(Bp.NONE, Bp.PRESET_CAUTIOUS):
				_fail("set_preset was not sent")
		230:
			_revert_override()
		200:
			_orders.set_helm_input(0.6, -0.4)
		260:
			_orders.set_helm_input(0.0, 0.0)
		320:
			_check_selection()
			_take_group(true)
		322:
			_take_group(false)
		400:
			_check_group(Bp.HOLDER_OFFICER)
			_check_log()
		700:
			_check_designation()
	if since >= RUN_FRAMES or state != Bp.SESSION_IN_SORTIE:
		_report()
		if state != Bp.SESSION_IN_SORTIE:
			_fail("the sortie ended early (state %d)" % state)
		_session.leave(false)
		_left = true
		_sortie_frame = _frame
	return false


func _start() -> bool:
	_session = root.get_node("Session") as BpSession
	_hud = root.get_node("Hud") as BpHud
	_orders = root.get_node("Orders") as BpOrders
	if _session == null or _hud == null or _orders == null:
		_fail("an autoload is missing")
		return _finish()
	_connect_hud()
	_session.command_rejected.connect(func(t: Dictionary) -> void: _rejected.append(str(t.get("key", ""))))
	var setup := {
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 2, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": STARTER,
	}
	var problems := _session.start_skirmish(setup)
	if not problems.is_empty():
		_fail("start_skirmish refused: %s" % str(problems))
		return _finish()
	return false


## Every section signal pulls its getter inside the handler.
func _connect_hud() -> void:
	var pulls := {
		"motion_changed": func() -> void: _hud.motion(),
		"power_changed": func() -> void: _hud.power(),
		"parts_changed": func() -> void: _hud.parts(),
		"mounts_changed": func() -> void: _hud.mounts(); _hud.weapon_groups(),
		"objectives_changed": func() -> void: _hud.objectives(),
		"allies_changed": func() -> void: _hud.allies(),
		"actions_changed": func() -> void: _hud.actions(); _hud.available_actions(),
		"offboard_changed": func() -> void: _hud.offboard(),
		"ship_static_changed": func() -> void: _hud.parts_static(); _hud.motion_limits(); _hud.has_ship(),
		"tracks_changed": func() -> void: _hud.track_table(&"threat", false); _hud.track_counts(); _hud.track_legend(),
	}
	for s: String in pulls:
		_signals[s] = 0
		var pull: Callable = pulls[s]
		_hud.connect(s, func() -> void: _signals[s] += 1; pull.call())
	for s: String in ["compartments_changed", "crew_changed", "networks_changed", "board_changed", "alerts_changed"]:
		_signals[s] = 0
		_hud.connect(s, func() -> void: _signals[s] += 1)
	for s: String in ["control_changed", "officer_line", "track_event", "selection_changed", "alert_raised", "log_added", "override_changed"]:
		_signals[s] = 0
	_hud.control_changed.connect(func(_r: Dictionary, _h: int) -> void: _signals["control_changed"] += 1; _hud.weapon_groups())
	_hud.officer_line.connect(func(_l: Dictionary) -> void: _signals["officer_line"] += 1; _hud.officer_lines(4))
	_hud.track_event.connect(func(_k: StringName, t: int, _o: int) -> void: _signals["track_event"] += 1; _hud.track(t))
	_hud.selection_changed.connect(func(t: int) -> void: _signals["selection_changed"] += 1; _hud.track(t); _hud.track_history(t))
	_hud.alert_raised.connect(func(_a: Dictionary) -> void: _signals["alert_raised"] += 1)
	_hud.log_added.connect(func(e: Dictionary) -> void: _signals["log_added"] += 1; _hud.log_entries({}, int(e.get("seq", 0)) - 1, 1))
	_hud.override_changed.connect(_on_override)
	_hud.why_reply.connect(func(w: Dictionary) -> void: _replies["why"] += 1; print("hud_orders_test: why ", w))
	_hud.track_card_reply.connect(func(c: Dictionary) -> void: _replies["card"] += 1; print("hud_orders_test: card candidates %d" % (c.get("candidates", []) as Array).size()))
	_hud.overrides_reply.connect(func(_o: int, v: Array) -> void: _replies["overrides"] += 1; print("hud_orders_test: overrides ", v))


func _check_ship() -> void:
	if not _hud.has_ship():
		_fail("has_ship is false in the sortie")
	var m := _hud.motion()
	_start_pos = m.get("pos", Vector2.ZERO)
	print("hud_orders_test: motion ", m)
	print("hud_orders_test: limits ", _hud.motion_limits())
	print("hud_orders_test: power ", _hud.power())
	var statics := _hud.parts_static()
	var parts := _hud.parts()
	print("hud_orders_test: %d parts, %d mounts, groups %s" % [statics.size(), _hud.mounts().size(), str(_hud.weapon_groups())])
	if statics.is_empty() or statics.size() != parts.size():
		_fail("parts_static (%d) and parts (%d) do not match" % [statics.size(), parts.size()])
	for i in mini(statics.size(), parts.size()):
		if int(statics[i].get("slot", -1)) != int(parts[i].get("slot", -2)):
			_fail("parts order differs at %d" % i)
			break
	for mt: Dictionary in _hud.mounts():
		if (mt.get("reason", {}) as Dictionary).get("key", &"") == &"":
			_fail("mount %d has no reason" % int(mt.get("slot", -1)))
		if _gun_slot < 0 and int(mt.get("group", -1)) == 0:
			_gun_slot = int(mt.get("slot", -1))
	if _hud.weapon_groups().is_empty():
		_fail("the starter has no weapons group")
	if _hud.rev(&"motion") <= 0 or _hud.rev(&"bogus") != -1:
		_fail("rev is wrong: motion %d, bogus %d" % [_hud.rev(&"motion"), _hud.rev(&"bogus")])
	print("hud_orders_test: available actions ", _hud.available_actions(), " objectives ", _hud.objectives())


## Bad arguments are refused without sending.
func _check_refusals(when: String) -> void:
	var refused := [
		_orders.designate(1 << 40), _orders.set_course(PackedVector2Array(), 10.0, false), _orders.set_emcon(99),
		_orders.set_speed(-1.0), _orders.set_speed(NAN), _orders.reorder_repairs(PackedInt32Array([0, 0, 1, 2, 3, 4])),
		_orders.assign_team(0, 7, 0), _orders.set_power_priority(1, 9), _orders.revert("nonsense/1"), _orders.take_control(9, 0, true),
		_orders.fire_at_reticle(Bp.ROLE_HELM, 0), _orders.start_action(Bp.STATION_CARRY_PASSENGERS, {}), _orders.cancel_action(0),
		_orders.offboard(0, {"kind": Bp.OFFBOARD_ORBIT, "centre": Vector2(INF, 0), "radius_m": 500.0}), _orders.query_scope(-1, 0, true),
	]
	for i in refused.size():
		if refused[i]:
			_fail("bad argument %d was accepted %s" % [i, when])
	if when == "while loading" and _orders.withdraw():
		_fail("withdraw was sent while loading")
	if when == "while loading" and _orders.set_rules([]).is_empty():
		_fail("set_rules gave no issue while loading")


## A second call in the same revision returns the same Array.
func _check_cache() -> void:
	var a := _hud.tracks()
	var b := _hud.tracks()
	if not is_same(a, b):
		_fail("tracks() rebuilt within one revision")
	if not is_same(_hud.mounts(), _hud.mounts()):
		_fail("mounts() rebuilt within one revision")
	if not is_same(_hud.parts_static(), _hud.parts_static()):
		_fail("parts_static() rebuilt within one revision")
	_cached_tracks = a
	_cached_rev = _hud.rev(&"tracks")


func _on_override(id: String, on: bool) -> void:
	_signals["override_changed"] += 1
	print("hud_orders_test: override_changed %s %s" % [id, on])
	if on and not _override_ids.has(id):
		_override_ids.append(id)
	if not on:
		_override_off += 1


## The same Array on a later frame while the revision stands.
func _check_cache_kept() -> void:
	if _hud.rev(&"tracks") == _cached_rev and not is_same(_hud.tracks(), _cached_tracks):
		_fail("tracks() rebuilt on a later frame of the same revision")


func _fire_by_hand() -> void:
	if _designated <= 0:
		return
	if not _orders.fire_at_track(Bp.ROLE_WEAPON_GROUP, 0, _designated, Vector3.ZERO):
		_fail("fire_at_track was refused with the group held")
	if _orders.fire_at_track(Bp.ROLE_WEAPON_GROUP, 3, _designated, Vector3.ZERO):
		_fail("fire_at_track was sent for a group not held")


func _revert_override() -> void:
	if _override_ids.is_empty():
		_fail("set_preset raised no override_changed")
		return
	if not _orders.query_overrides(Bp.OFFICER_GUNNERY):
		_fail("query_overrides was not sent")
	if not _orders.revert(_override_ids[0]):
		_fail("revert %s was not sent" % _override_ids[0])


func _order_course() -> void:
	var m := _hud.motion()
	var here: Vector2 = m.get("pos", Vector2.ZERO)
	var rad := deg_to_rad(fmod(float(m.get("heading_deg", 0.0)) + 60.0, 360.0))
	_course_to = here + Vector2(sin(rad), -cos(rad)) * 3000.0
	if not _orders.set_course(PackedVector2Array([_course_to]), 25.0, false):
		_fail("set_course was not sent")


func _take_group(pressed: bool) -> void:
	if not _orders.role_key(Bp.ROLE_WEAPON_GROUP, 0, pressed):
		_fail("role_key %s was refused" % ("press" if pressed else "release"))


func _check_group(holder: int) -> void:
	var groups := _hud.weapon_groups()
	if groups.is_empty() or int(groups[0].get("holder", -1)) != holder:
		_fail("group 0 holder is not %d: %s" % [holder, str(groups)])


func _designate() -> void:
	var table := _hud.track_table(&"threat", false)
	print("hud_orders_test: %d tracks, counts %s, legend %s" % [table.size(), str(_hud.track_counts()), str(_hud.track_legend())])
	for t: Dictionary in table:
		if int(t.get("stance", -1)) != Bp.STANCE_FRIEND and not bool(t.get("last_known", false)):
			_designated = int(t.get("id", -1))
			print("hud_orders_test: designating ", t.get("name", ""), " range %.0f m" % float(t.get("range_m", -1.0)))
			break
	if _designated > 0 and not _orders.designate(_designated):
		_fail("designate was not sent")


func _query() -> void:
	if _gun_slot >= 0 and not _orders.query_why(_gun_slot):
		_fail("query_why was not sent")
	if _designated > 0 and not _orders.query_track_card(_designated):
		_fail("query_track_card was not sent")
	if not _orders.query_overrides(Bp.OFFICER_GUNNERY):
		_fail("query_overrides was not sent")
	if not _orders.acknowledge(Bp.NONE):
		_fail("acknowledge was not sent")
	if not _orders.set_rules([]).is_empty():
		_fail("an empty rules table was refused")


func _check_designation() -> void:
	if _designated > 0 and _hud.designated_track() != _designated:
		_fail("designated track is %d, not %d" % [_hud.designated_track(), _designated])
	var m := _hud.motion()
	var pos: Vector2 = m.get("pos", Vector2.ZERO)
	print("hud_orders_test: moved %.0f m, ordered heading %.0f, route %d" % [pos.distance_to(_start_pos), float(m.get("ordered_heading_deg", -1.0)), (m.get("route", PackedVector2Array()) as PackedVector2Array).size()])
	if pos.distance_to(_start_pos) < 20.0:
		_fail("the ship did not move")


func _check_selection() -> void:
	var first := _hud.select_step(1)
	# The side picture also holds the own hull as a track; it is no contact, so stepping skips it (as the
	# track list does), and only another track can be selected.
	var own := int(_hud.motion().get("vessel", Bp.NONE))
	var contacts := 0
	for t: Dictionary in _hud.track_table(&"threat", false):
		if own == Bp.NONE or int(t.get("vessel", Bp.NONE)) != own:
			contacts += 1
	print("hud_orders_test: %d contacts besides the own hull" % contacts)
	if contacts == 0 and first != Bp.NONE:
		_fail("select_step selected the own hull (%d) with no other contact" % first)
	if contacts > 0:
		if first < 0 or _hud.selected_track() != first or _hud.track(first) == null:
			_fail("select_step did not select a track (%d)" % first)
		elif not bool((_hud.track(first) as Dictionary).get("selected", false)):
			_fail("the selected track is not marked selected")
		if _hud.track_history(first).is_empty():
			_fail("the selected track has no history")
	_hud.select_track(Bp.NONE)
	if _hud.selected_track() != Bp.NONE:
		_fail("select_track(NONE) did not clear")
	if _hud.track(123456) != null or _hud.mount(70000) != null:
		_fail("unknown ids are not null")


func _check_log() -> void:
	var entries := _hud.log_entries({}, 0, 0)
	var control := _hud.log_entries({"kinds": PackedInt32Array([Bp.LOG_CONTROL])}, 0, 0)
	print("hud_orders_test: log %d entries, %d control, %d unread" % [entries.size(), control.size(), _hud.log_unread()])
	var lines := _hud.log_text().split("\n", false)
	for i in mini(6, lines.size()):
		print("hud_orders_test: log | ", lines[i])
	for i in range(maxi(6, lines.size() - 6), lines.size()):
		print("hud_orders_test: log | ", lines[i])
	if control.size() < 2:
		_fail("the group and helm takeovers left fewer than 2 control entries")
	var helm := 0
	for e: Dictionary in control:
		if str((e.get("text", {}) as Dictionary).get("key", "")).contains("helm"):
			helm += 1
	if helm < 2:
		_fail("the helm by stick did not take and give back (%d entries)" % helm)
	if not entries.is_empty():
		_hud.log_mark_read(int(entries[-1].get("seq", 0)))
		if _hud.log_unread() != 0:
			_fail("log_mark_read left %d unread" % _hud.log_unread())


func _report() -> void:
	print("hud_orders_test: signals ", _signals)
	print("hud_orders_test: replies ", _replies, " rejected ", _rejected)
	print("hud_orders_test: officer lines ", _hud.officer_lines(3))
	for s: String in ["motion_changed", "mounts_changed", "tracks_changed", "ship_static_changed", "log_added", "control_changed"]:
		if _signals[s] == 0:
			_fail("%s never fired" % s)
	if _signals["motion_changed"] > (_frame - _sortie_frame) * 2 / 3:
		_fail("motion_changed fired %d times in %d frames" % [_signals["motion_changed"], _frame - _sortie_frame])
	if _replies["why"] == 0 or _replies["overrides"] == 0:
		_fail("query replies missing: %s" % str(_replies))
	if _override_off == 0:
		_fail("revert raised no override_changed(id, false)")
	if _rejected.has("sim.reject.role_not_held"):
		_fail("a manual shot was refused as role not held")


func _after_leave() -> bool:
	if _frame - _sortie_frame < 3:
		return false
	if _hud.has_ship() or not _hud.motion().is_empty() or not _hud.tracks().is_empty() or not _hud.log_entries({}, 0, 0).is_empty():
		_fail("the HUD kept sortie data after leave")
	return _finish()


func _finish() -> bool:
	if _session != null and _session.state() != Bp.SESSION_IDLE:
		_session.leave(false)
	print("hud_orders_test: ", "FAILED (%d)" % _failures if _failures > 0 else "passed")
	quit(1 if _failures > 0 else 0)
	return true


func _fail(text: String) -> void:
	_failures += 1
	push_error("hud_orders_test: " + text)
