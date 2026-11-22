extends SceneTree
## Headless checks for the Bridge Board, officer lines, Why?, the control station, the alert strip
## and the log, on hand-built data. Units: ui-hud-board, ui-hud-control, ui-hud-alerts. Run from
## game/:
##   godot --headless --path . -s res://test/unit/hud_board_control_alerts_test.gd
## Exits 1 on any failure. No session runs; the facades answer as when idle. The windowed
## end-to-end checks (live skirmish and scripted battles, clicks, keys, commands sent) are
## res://test/capture/u7_capture.tscn.

var _failures := 0
var _checks := 0
var _frame := 0
var _hud: Node


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 2:
		_hud = (load("res://ui/hud/sortie_hud.tscn") as PackedScene).instantiate()
		root.add_child(_hud)
	if _frame == 5:
		_strip_checks()
		_log_checks()
		_lines_checks()
		_board_checks()
		_why_checks()
		_control_checks()
	if _frame == 7:
		_after_frame_checks()
		_hud.queue_free()
	if _frame < 10:
		return false
	print("hud_board_control_alerts_test: %d checks, %d failures" % [_checks, _failures])
	quit(1 if _failures > 0 else 0)
	return true


func _expect(ok: bool, what: String) -> void:
	_checks += 1
	if not ok:
		_failures += 1
		push_error("hud_board_control_alerts_test: " + what)


func _node(path: String) -> Node:
	return _hud.get_node(path)


static func _msg(key: String, args: Array = []) -> Dictionary:
	return {"key": StringName(key), "args": args}


# --- alert strip ---------------------------------------------------------------------------------

func _alert(id: int, priority: int, state: int, fast: bool = false) -> Dictionary:
	var words: Array[StringName] = [&"units.word.priority.caution", &"units.word.priority.warning", &"units.word.priority.alarm", &"units.word.priority.emergency"]
	return {
		"id": id, "priority": priority, "subsystem": 0, "state": state, "text": _msg("hud_alerts.log.kind.alert"),
		"word": words[priority], "raised_s": float(id), "officer": -1, "blink_fast": fast,
		"track": 12 if id == 1 else -1, "part": -1, "compartment": 3 if id == 2 else -1, "sound": &"alert_alarm",
	}


func _strip_checks() -> void:
	var strip: Control = _node("Frame/Docks/Centre/Margin/Column/Top/Middle/AlertStrip")
	strip.call(&"show_strip", {})
	_expect(not strip.visible, "a calm strip is hidden, so it draws no alert colour")
	var shown: Array = [_alert(1, Bp.PRIORITY_EMERGENCY, Bp.ALERT_UNACKNOWLEDGED, true), _alert(2, Bp.PRIORITY_ALARM, Bp.ALERT_ACKNOWLEDGED), _alert(3, Bp.PRIORITY_ALARM, Bp.ALERT_CLEARED), _alert(4, Bp.PRIORITY_WARNING, Bp.ALERT_UNACKNOWLEDGED), _alert(5, Bp.PRIORITY_CAUTION, Bp.ALERT_UNACKNOWLEDGED)]
	strip.call(&"show_strip", {"shown": shown, "more": 3, "flood": true, "silenced_s": 12.4, "next_ack": 1})
	_expect(strip.visible, "a strip with alerts shows")
	var rows := strip.get_node("Margin/Column/Rows")
	var visible_rows := 0
	for r: Node in rows.get_children():
		if (r as Control).visible:
			visible_rows += 1
	_expect(visible_rows == 5, "five alerts show five rows (got %d)" % visible_rows)
	var more: Button = strip.get_node("Margin/Column/More")
	_expect(more.visible and more.text.contains("3"), "+N names the alerts beyond five: " + more.text)
	_expect((strip.get_node("Margin/Column/Flood") as Control).visible, "flood mode adds its line")
	var ack: Button = strip.get_node("Margin/Column/Actions/Acknowledge")
	_expect(not ack.disabled, "Acknowledge is live while an alert needs it")
	var silenced: Label = strip.get_node("Margin/Column/Actions/Silenced")
	_expect(silenced.visible and silenced.text.contains("13"), "the silence countdown rounds up: " + silenced.text)
	_expect(int(strip.get(&"accessibility_live")) == 2, "an unacknowledged Emergency makes the live region assertive")
	var w0 := (rows.get_child(0).get_child(0) as Control).custom_minimum_size.x
	var w4 := (rows.get_child(4).get_child(0) as Control).custom_minimum_size.x
	_expect(w0 > 0.0 and is_equal_approx(w0, w4), "badges share one width so sentences align (%f, %f)" % [w0, w4])
	var badge := rows.get_child(2).get_child(0)
	_expect(int(badge.get(&"state")) == Bp.ALERT_CLEARED, "a Cleared alert reaches its badge as Cleared")
	strip.call(&"show_strip", {"shown": [_alert(5, Bp.PRIORITY_CAUTION, Bp.ALERT_UNACKNOWLEDGED)], "more": 0, "flood": false, "silenced_s": 0.0, "next_ack": -1})
	_expect(ack.disabled, "Acknowledge is disabled when nothing needs it (cautions need none)")
	_expect(not silenced.visible and not more.visible, "no silence and no +N when there is none")
	_expect(int(strip.get(&"accessibility_live")) == 1, "a caution-only strip is a polite live region")


# --- log -----------------------------------------------------------------------------------------

func _entry(seq: int, kind: int, unread: bool, silent: bool, track: int = -1, part: int = -1) -> Dictionary:
	return {"seq": seq, "time_s": float(seq * 10), "kind": kind, "text": _msg("hud_alerts.log.kind.track"), "track": track, "part": part, "vessel": -1, "pos": Vector2(seq, seq) if track > 0 else null, "unread": unread, "silent": silent}


func _rows(table: Tree) -> int:
	var root := table.get_root()
	return root.get_child_count() if root != null else 0


func _log_checks() -> void:
	var log: Control = _node("Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Log")
	_expect(log.is_in_group(&"hud_log_panel"), "the log joins the group the alert strip calls")
	var entries: Array[Dictionary] = [
		_entry(1, Bp.LOG_TRACK, true, false, 412), _entry(2, Bp.LOG_ALERT, true, false), _entry(3, Bp.LOG_DAMAGE, false, false, -1, 5),
		_entry(4, Bp.LOG_OFFICER, true, true), _entry(5, Bp.LOG_ALERT, true, false, 7), _entry(6, Bp.LOG_TRACK, false, false, 412),
	]
	log.call(&"show_entries", entries)
	var table: Tree = log.get_node("Table")
	_expect(_rows(table) == 6, "every entry has a row (got %d)" % _rows(table))
	var marks := 0
	var item := table.get_root().get_first_child()
	while item != null:
		if item.get_icon(0) != null:
			marks += 1
		item = item.get_next()
	_expect(marks == 3, "unread marks skip routine entries (got %d)" % marks)
	var summary: Label = log.get_node("Counts/Summary")
	_expect(summary.text.contains("6") and summary.text.contains("3"), "the count line names entries and unread: " + summary.text)
	log.call(&"show_kind", Bp.LOG_ALERT)
	_expect(_rows(table) == 2, "the type filter keeps alert entries only (got %d)" % _rows(table))
	log.call(&"show_kind", Bp.NONE)
	_expect(_rows(table) == 6, "all types again")
	var unit: OptionButton = log.get_node("Filters/Unit")
	_expect(unit.item_count == 4, "the unit filter lists all, own ship and the two tracks named (got %d)" % unit.item_count)
	unit.select(unit.get_item_index(1000 + 412))
	unit.item_selected.emit(unit.selected)
	_expect(_rows(table) == 2, "the unit filter keeps one track's entries (got %d)" % _rows(table))
	unit.select(0)
	unit.item_selected.emit(0)
	(log.get_node("Filters/Unread") as CheckBox).button_pressed = true
	_expect(_rows(table) == 3, "unread only hides read and routine entries (got %d)" % _rows(table))
	(log.get_node("Filters/Unread") as CheckBox).button_pressed = false
	log.call(&"_on_log_added", _entry(7, Bp.LOG_CONTROL, true, false))
	_expect(_rows(table) == 7, "a new entry appends a row")
	log.call(&"mark_all_read")
	_expect(not summary.text.contains("unread"), "Mark all read clears the unread count: " + summary.text)
	_expect((log.get_node("Counts/MarkRead") as Button).disabled, "Mark all read is disabled with nothing unread")
	# A sequence number going backwards is a new log: the panel reads it again (empty when idle).
	log.call(&"_on_log_added", _entry(2, Bp.LOG_CONTROL, true, false))
	_expect(_rows(table) == 0 and (log.get_node("Empty") as Label).visible, "a restarted log re-reads and shows its empty state")


# --- officer lines -------------------------------------------------------------------------------

func _line(role: int, speaker_key: String, text: String, new_speaker: bool) -> Dictionary:
	return {"speaker": _msg(speaker_key), "text": {"key": &"", "args": []} if text.is_empty() else _msg(text), "ace": &"", "role": role, "track": -1, "time_s": 0.0, "new_speaker": new_speaker, "sound": &"radio_squelch_open"}


func _lines_checks() -> void:
	var lines: Control = _node("Frame/Docks/Centre/Margin/Column/Bottom/OfficerLines")
	_expect(not lines.visible, "no line, no box")
	lines.call(&"show_line", _line(Bp.OFFICER_GUNNERY, "sim.word.officer.gunnery", "hud_board.esm_warning", true))
	_expect(lines.visible, "a line shows at once")
	var column := lines.get_node("Margin/Column")
	_expect(column.get_child_count() == 2, "the first line names its speaker (children %d)" % column.get_child_count())
	lines.call(&"show_line", _line(Bp.OFFICER_GUNNERY, "sim.word.officer.gunnery", "hud_board.emitters.none", false))
	_expect(column.get_child_count() == 3, "the same speaker is not named again (children %d)" % column.get_child_count())
	lines.call(&"show_line", _line(Bp.OFFICER_HELM, "sim.word.officer.helm", "hud_board.waiting", false))
	_expect(column.get_child_count() == 4, "two lines kept; a change of speaker names the speaker (children %d)" % column.get_child_count())
	lines.call(&"show_line", _line(Bp.OFFICER_HELM, "sim.word.officer.helm", "", true))
	_expect(column.get_child_count() == 4, "an empty line is ignored")
	var text := ""
	for c: Node in column.get_children():
		text = (c as Label).text
	for part: String in text.split("\n"):
		_expect(part.length() <= 40, "radio text keeps to 40 characters a line: " + part)
	_expect(text.split("\n").size() <= 2, "radio text keeps to two lines")
	var kept: Array = lines.get(&"_lines")
	for l: Dictionary in kept:
		l["until_ms"] = 0
	lines.call(&"_expire")
	_expect(not lines.visible, "expired lines leave no box")


# --- Bridge Board --------------------------------------------------------------------------------

func _officer(role: int, holder: int, overrides: int) -> Dictionary:
	return {"role": role, "name": StringName(["sim.word.officer.helm", "sim.word.officer.engineer", "sim.word.officer.sensors", "sim.word.officer.gunnery", "sim.word.officer.damage_control"][role]), "task": _msg("hud_board.emitters.none"), "reason": _msg("hud_board.waiting"), "next": _msg("hud_board.diff.none"), "holder": holder, "layer": 0, "overrides": overrides, "advisory": _msg("hud_board.why.none") if holder == 1 else {"key": &"", "args": []}}


func _board_checks() -> void:
	var board: Control = _node("Frame/Docks/RightDock/Column/BridgeBoard")
	board.call(&"show_board", {}, [] as Array[Dictionary], {})
	_expect((board.get_node("Scroll/Margin/Groups/Empty") as Control).visible, "an empty board says it waits for the first report")
	_expect(not (board.get_node("Scroll/Margin/Groups/Officer0") as Control).visible, "no officer groups while waiting")
	var officers: Array = [_officer(0, 1, 0), _officer(1, 0, 0), _officer(2, 0, 1), _officer(3, 0, 2), _officer(4, 0, 0)]
	var roles: Array[Dictionary] = [
		{"role": {"kind": Bp.ROLE_HELM, "index": 0}, "name": _msg("hud.role.helm"), "holder": 1, "action": &"take_helm"},
		{"role": {"kind": Bp.ROLE_WEAPON_GROUP, "index": 0}, "name": _msg("hud.group.name", [1]), "holder": 0, "action": &"take_group_1"},
		{"role": {"kind": Bp.ROLE_WEAPON_GROUP, "index": 4}, "name": _msg("hud.group.name", [5]), "holder": 0, "action": &""},
		{"role": {"kind": Bp.ROLE_SENSORS, "index": 0}, "name": _msg("hud.role.sensors"), "holder": 0, "action": &"take_sensors"},
	]
	board.call(&"set_part_names", {7: "Radar 1"})
	board.call(&"show_board", {"officers": officers, "radiating": PackedInt32Array([7]), "esm_warning": true, "emcon": Bp.EMCON_BURST, "held": []}, roles, {"emcon": Bp.EMCON_BURST, "overridden": true, "recommended": Bp.EMCON_SILENT})
	_expect(not (board.get_node("Scroll/Margin/Groups/Empty") as Control).visible, "a populated board hides the waiting line")
	var helm := board.get_node("Scroll/Margin/Groups/Officer0")
	_expect((helm.get_node("State") if helm.has_node("State") else helm.get_node("Head/State")).get(&"text") == tr(&"hud_board.state.player"), "the helm held by the player reads Player")
	_expect((helm.get_node("Advisory") as Label).visible, "a held role shows the officer's advisory")
	_expect(not (board.get_node("Scroll/Margin/Groups/Officer1/Advisory") as Label).visible, "an officer's own role shows no advisory")
	# The helm and the weapons groups are taken in the helm cluster and the weapons panel; the board
	# keeps one takeover, the sensors', so each role has one control on screen.
	_expect(not (helm.get_node("Roles") as Control).visible and helm.get_node("Roles").get_child_count() == 0, "the helm's takeover is not repeated on the board")
	var gunnery_roles := board.get_node("Scroll/Margin/Groups/Officer3/Roles")
	_expect(gunnery_roles.get_child_count() == 0, "the weapons groups' takeovers are not repeated under Gunnery (got %d)" % gunnery_roles.get_child_count())
	_expect((board.get_node("Scroll/Margin/Groups/Officer2/Roles") as Control).visible, "sensors' takeover sits under Sensors")
	var sensors_button: Button = board.get_node("Scroll/Margin/Groups/Officer2/Roles/Role_1_0").get_child(0)
	_expect(not sensors_button.button_pressed and sensors_button.text == tr(&"hud_board.role.take_sensors"), "the sensors toggle reads Take sensors while the officer has them")
	_expect(not (board.get_node("Scroll/Margin/Groups/Officer1/Roles") as Control).visible, "the engineer has no takeover")
	var esm: Label = board.get_node("Scroll/Margin/Groups/Officer2/Extra/Esm")
	_expect(esm.visible and esm.text == tr(&"hud_board.esm_warning"), "the ESM warning shows its sentence")
	var emitters: Label = board.get_node("Scroll/Margin/Groups/Officer2/Extra/Emitters")
	_expect(emitters.text.contains("Radar 1"), "radiating emitters are named: " + emitters.text)
	var overrides: Button = board.get_node("Scroll/Margin/Groups/Officer3/Overrides")
	_expect(overrides.visible and overrides.text.contains("2"), "the override count shows: " + overrides.text)
	_expect(not (board.get_node("Scroll/Margin/Groups/Officer0/Overrides") as Control).visible, "no count while nothing is overridden")
	board.call(&"show_diff", Bp.OFFICER_GUNNERY, [
		{"id": "slider/gunnery.aggression", "label": _msg("tuning.slider.gunnery.aggression"), "default_text": "0.5", "current_text": "0.9", "layer": 1},
		{"id": "preset", "label": _msg("tuning.preset.aggressive"), "default_text": "Cautious", "current_text": "Aggressive", "layer": 0},
	])
	var diff := board.get_node("Scroll/Margin/Groups/Officer3/Diff")
	_expect(diff.get_child_count() == 3 and diff.has_node("RevertAll"), "the diff lists each value and Revert all (children %d)" % diff.get_child_count())
	var targets: Array = board.call(&"focus_targets")
	_expect(targets.size() >= 5, "focus reaches the takeover, counts and reverts (got %d)" % targets.size())
	if targets.size() >= 2:
		var first: Control = targets[0]
		_expect(first.get_node_or_null(first.focus_neighbor_bottom) == targets[1], "Down walks the board in order")
	board.call(&"box_officer", Bp.OFFICER_HELM)
	_expect((board.get(&"_box_until") as Dictionary).has(Bp.OFFICER_HELM), "a control change boxes its officer")


# --- Why? ----------------------------------------------------------------------------------------

func _why_checks() -> void:
	var why: Control = _node("Frame/Overlays/WhyPopup")
	_expect(not bool(why.call(&"is_open")), "Why? starts closed")
	why.call(&"show_why", {"slot": 2, "chain": [
		{"text": _msg("hud_board.why.none"), "node": 5, "part": -1},
		{"text": _msg("hud_board.emitters.none"), "node": -1, "part": 4},
		{"text": _msg("hud_board.waiting"), "node": -1, "part": -1},
		{"text": {"key": &"", "args": []}, "node": -1, "part": -1},
	]})
	_expect(bool(why.call(&"is_open")), "a reply opens Why?")
	var chain := why.get_node("Panel/Margin/Column/Chain")
	_expect(chain.get_child_count() == 3, "every non-empty link is a line (got %d)" % chain.get_child_count())
	_expect(chain.get_child(0) is Button and chain.get_child(1) is Button and chain.get_child(2) is Label, "node and part links are buttons, plain causes are text")
	why.call(&"close")
	_expect(not bool(why.call(&"is_open")), "close hides Why?")
	why.call(&"show_why", {"slot": 2, "chain": []})
	_expect(chain.get_child_count() == 1, "an empty chain says the part works normally")


# --- control station -----------------------------------------------------------------------------

func _control_checks() -> void:
	var cp: Control = _node("Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Control")
	var script: GDScript = cp.get_script()
	_expect(script.call(&"_override_id", "preset") == "preset", "the ship preset's revert id")
	_expect(script.call(&"_override_id", "mount_preset", 3) == "mount_preset/3", "a mount preset's revert id")
	_expect(script.call(&"_override_id", "slider", "gunnery.aggression") == "slider/gunnery.aggression", "a slider's revert id")
	_expect(script.call(&"_override_id", "power_priority", 7) == "power_priority/7", "a power priority's revert id")
	_expect(script.call(&"_override_id", "emcon") == "emcon" and script.call(&"_override_id", "burst_trigger_range") == "burst_trigger_range", "the emission revert ids")
	var share := String(cp.call(&"_slider_text", {"key": "gunnery.aggression", "min": 0.0, "max": 1.0, "step": 0.01}, 0.85))
	_expect(share.contains("85") and not share.contains("."), "a share reads as a percentage: " + share)
	var metres := String(cp.call(&"_slider_text", {"key": "sensors.pd_radar_range_m", "min": 2000.0, "max": 12000.0, "step": 100.0}, 8000.0))
	_expect(metres.contains("8") and metres.contains("km"), "a long range reads in km: " + metres)
	var depth := String(cp.call(&"_slider_text", {"key": "dc.pump_threshold_m", "min": 0.0, "max": 0.5, "step": 0.01}, 0.05))
	_expect(depth.contains("0.05") and depth.contains("m"), "a short length keeps its decimals: " + depth)
	var secs := String(cp.call(&"_slider_text", {"key": "sensors.burst_repeat_s", "min": 10.0, "max": 60.0, "step": 1.0}, 30.0))
	_expect(secs.contains("30") and secs.contains("s"), "seconds read as seconds: " + secs)
	var gain := String(cp.call(&"_slider_text", {"key": "gunnery.pn_air", "min": 2.0, "max": 6.0, "step": 0.1}, 4.0))
	_expect(gain.begins_with("4") and not gain.contains("%"), "a plain gain reads as a number: " + gain)
	_expect((cp.get_node("Empty") as Label).visible or not cp.is_visible_in_tree(), "with no ship the station waits")


func _after_frame_checks() -> void:
	# The why popup lays itself out a frame later; it must stay inside the screen.
	var why: Control = _node("Frame/Overlays/WhyPopup")
	why.call(&"show_why", {"slot": 1, "chain": [{"text": _msg("hud_board.why.none"), "node": -1, "part": -1}]})
	why.set(&"_anchor", Vector2(100000.0, 100000.0))
	why.call(&"_place")
	var panel: Control = why.get_node("Panel")
	var view := why.get_viewport().get_visible_rect()
	_expect(view.grow(1.0).encloses(Rect2(panel.get_global_transform_with_canvas().origin, panel.size)), "Why? is kept inside the screen")
