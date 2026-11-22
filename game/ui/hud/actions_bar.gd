extends Control
## Station actions with progress and why paused; prize decision; drone orders.
## DESIGN.md L784-797. Unit: ui-hud-ops.
## Reads and calls: Hud.available_actions, Hud.actions, Hud.offboard, Orders.start_action, Orders.cancel_action, Orders.prize, Orders.offboard.
##
## A docked HUD cluster (the HUD shell places it). Two sections on one opaque panel, each shown
## only when the hull has something for it, and the whole bar hidden when it has neither (HUD rule
## 1: quiet when normal; nothing here ever uses an alert colour):
## - Station actions (L786-795): one row per action the hull and operation allow
##   (`Hud.available_actions`). Idle, the row is a start button worded for what it targets: the
##   selected track (board and capture, photograph), the nearest landing (insert or extract), the
##   nearest survivors (recover), or a line drawn on the plot (lay mines). A disabled row says why
##   (`reason`). Under way, the row shows progress and, when paused, why (`ActionDict.block`, for
##   example "Too fast for the action"), with Cancel. The last finished attempt stays as one line.
## - Prize: when a boarding completes, the bar asks what to do with her (L790): sail her home under
##   her own Helm officer (the default if the player never answers) or strip her on the spot for
##   Salvage and blueprints. Stripping cannot be undone, so it asks again inline.
## - Offboard (L549, L553): one row per drone bay or helicopter deck with what is aboard, endurance
##   left while airborne or the recharge time, and its orders: launch, recall, shadow the selected
##   track, orbit a point or fly to a point picked on the plot.
##
## Plot picks (the mine line, an orbit centre, a waypoint) go through the sortie's BpPlotView: the
## HUD shell may hand it over with `set_plot`; otherwise the bar finds the one in the tree when it
## needs it. While a pick is open a hint row says what to click, with Cancel; the `cancel` action
## does the same.
##
## Data arrives by change signal (`actions_changed`, `offboard_changed`, `selection_changed`,
## `ship_static_changed`) and is pulled then; rows are rebuilt only when the set of rows changes and
## otherwise updated in place, so focus and layout hold while progress runs (L1449 budget).
## Every button is a 48 px target with explicit focus neighbours top to bottom (L1717, L1738).

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

## `Bp` values used here, kept so the script parses before the extension loads.
const NONE := -1
const STATION_BOARD_AND_CAPTURE := 0
const STATION_INSERT_OR_EXTRACT := 1
const STATION_RECOVER_SURVIVORS := 3
const STATION_LAY_MINES := 4
const STATION_PHOTOGRAPH := 5
const ACTION_RUNNING := 0
const ACTION_PAUSED := 1
const ACTION_ABORTED := 2
const ACTION_DONE := 3
const PRIZE_SAIL_HOME := 0
const PRIZE_STRIP_ON_SPOT := 1
const OFFBOARD_LAUNCH := 0
const OFFBOARD_RECALL := 1
const OFFBOARD_ORBIT := 2
const OFFBOARD_SHADOW := 3
const OFFBOARD_WAYPOINT := 4

## Orbit radius for a drone ordered to orbit a picked point. DECISION (the spec gives none): 2 km,
## wide enough to keep a drone clear of a ship's point defence, close enough to fix a contact by
## cross bearing (L553).
const ORBIT_RADIUS_M := 2000.0
## Endurance under this gets the recall advice in its line (no alert colour, rule 1).
const LOW_ENDURANCE_S := 180.0
const PANEL_W := 440.0

@onready var _panel: PanelContainer = $Panel
@onready var _scroll: ScrollContainer = $Panel/Margin/Scroll
@onready var _column: VBoxContainer = $Panel/Margin/Scroll/Column
@onready var _actions_title: Label = $Panel/Margin/Scroll/Column/ActionsTitle
@onready var _actions: VBoxContainer = $Panel/Margin/Scroll/Column/Actions
@onready var _off_rule: HSeparator = $Panel/Margin/Scroll/Column/OffboardRule
@onready var _off_title: Label = $Panel/Margin/Scroll/Column/OffboardTitle
@onready var _offboard: VBoxContainer = $Panel/Margin/Scroll/Column/Offboard
@onready var _pick_rule: HSeparator = $Panel/Margin/Scroll/Column/PickRule
@onready var _pick_text: Label = $Panel/Margin/Scroll/Column/Pick/Text
@onready var _pick_cancel: Button = $Panel/Margin/Scroll/Column/Pick/Cancel

var _available: Array[Dictionary] = []
var _running: Array[Dictionary] = []
var _units: Array[Dictionary] = []
var _selected := NONE
var _signature := ""
## Per row key (`"action:4"`, `"prize:12"`, `"unit:7"`): the controls updated in place.
var _rows: Dictionary = {}
## Boarding action id to the prize vessel it took (entity id). The vessel comes from the boarded
## track's `TrackDict.vessel`, looked up on every refresh until it resolves: a boarding may start on
## approach, before the target is close enough to be drawn.
var _prize_vessel: Dictionary = {}
## Boarding action id to the track it was ordered on.
var _board_track: Dictionary = {}
## A boarding just ordered: its track, until its action id shows up.
var _pending_board_track := NONE
var _known_board_ids: Array[int] = []
## Boarding action id to `Bp.PRIZE_*` decided here.
var _prize_decided: Dictionary = {}
## Boarding action ids whose strip is waiting for the second press.
var _strip_armed: Array[int] = []
## Open plot pick: `{ "kind": &"line" }` or `{ "kind": &"point", "slot": int, "order": int }`.
var _pick: Dictionary = {}
var _plot_ref: WeakRef
## The plot an open pick is connected to.
var _pick_plot: WeakRef
## Last order refusal per row key, shown until the row changes.
var _refused: Dictionary = {}
var _live := false


func _ready() -> void:
	Ui.set_margins($Panel/Margin, 3)
	Ui.set_separation(_column, 2)
	Ui.set_separation(_actions, 3)
	Ui.set_separation(_offboard, 3)
	Ui.set_separation($Panel/Margin/Scroll/Column/Pick, 2)
	_panel.custom_minimum_size = Vector2(PANEL_W, 0.0)
	_panel.accessibility_name = tr(&"hud_ops.accessible_name")
	_column.minimum_size_changed.connect(_fit_root, CONNECT_DEFERRED)
	_panel.minimum_size_changed.connect(_fit_root, CONNECT_DEFERRED)
	get_viewport().size_changed.connect(_fit_root)
	_pick_text.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_pick_cancel.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_pick_cancel.pressed.connect(cancel_pick)
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		_live = true
		for sig: StringName in [&"actions_changed", &"offboard_changed", &"ship_static_changed"]:
			if hud.has_signal(sig):
				hud.connect(sig, refresh)
		if hud.has_signal(&"selection_changed"):
			hud.connect(&"selection_changed", _on_selection_changed)
	refresh()


func _exit_tree() -> void:
	if not _pick.is_empty():
		cancel_pick()


## Give the bar the sortie's plot for line and point picks (the HUD shell calls this).
func set_plot(plot: Control) -> void:
	_plot_ref = weakref(plot) if plot != null else null


func _unhandled_input(event: InputEvent) -> void:
	if not _pick.is_empty() and event.is_action_pressed(&"cancel", false, true):
		get_viewport().set_input_as_handled()
		cancel_pick()


## Pull everything from Hud and redraw.
func refresh() -> void:
	if not _live:
		show_ops([], [], [], NONE)
		return
	var sel: Variant = Kit.call_on(&"Hud", &"selected_track")
	show_ops(Kit.dicts(Kit.call_on(&"Hud", &"available_actions")), Kit.dicts(Kit.call_on(&"Hud", &"actions")),
		Kit.dicts(Kit.call_on(&"Hud", &"offboard")), int(sel) if sel is int else NONE)


func _on_selection_changed(track: int) -> void:
	_selected = track
	show_ops(_available, _running, _units, track)


## Draw from `available_actions`, `actions` (`ActionDict`s), `offboard` (`OffboardDict`s) and the
## selected track (`Bp.NONE` for none). Also the test and capture entry point.
func show_ops(available: Array[Dictionary], actions: Array[Dictionary], offboard: Array[Dictionary], selected: int) -> void:
	_available = available
	_running = actions
	_units = offboard
	_selected = selected
	_learn_board_ids(actions)
	var signature := _structure(available, actions, offboard)
	if signature != _signature:
		_signature = signature
		_rebuild()
	_update()
	var any_actions := not available.is_empty() or not _prizes().is_empty()
	var any_units := not offboard.is_empty()
	_actions_title.visible = any_actions
	_actions.visible = any_actions
	_off_rule.visible = any_actions and any_units
	_off_title.visible = any_units
	_offboard.visible = any_units
	visible = any_actions or any_units or not _pick.is_empty()


# --- model ---------------------------------------------------------------------------------------

## The latest ActionDict of each action type (highest id), by `Bp.STATION_*`.
static func latest_by_action(actions: Array[Dictionary]) -> Dictionary:
	var out := {}
	for a: Dictionary in actions:
		var kind := Kit.get_int(a, "action", NONE)
		if not out.has(kind) or Kit.get_int(a, "id") > Kit.get_int(out[kind], "id"):
			out[kind] = a
	return out


## Boardings that completed: each is a prize to decide (newest first).
func _prizes() -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	for a: Dictionary in _running:
		if Kit.get_int(a, "action") == STATION_BOARD_AND_CAPTURE and Kit.get_int(a, "state") == ACTION_DONE:
			out.append(a)
	out.sort_custom(func(x: Dictionary, y: Dictionary) -> bool: return Kit.get_int(x, "id") > Kit.get_int(y, "id"))
	return out


## A boarding that just appeared takes the vessel recorded when it was ordered.
func _learn_board_ids(actions: Array[Dictionary]) -> void:
	for a: Dictionary in actions:
		if Kit.get_int(a, "action") != STATION_BOARD_AND_CAPTURE:
			continue
		var id := Kit.get_int(a, "id")
		if _known_board_ids.has(id):
			continue
		_known_board_ids.append(id)
		if _pending_board_track != NONE:
			_board_track[id] = _pending_board_track
			_pending_board_track = NONE
	# Resolve the prize vessel while its track is still drawn.
	for id: Variant in _board_track:
		if not _prize_vessel.has(id):
			var track := Kit.as_dict(Kit.call_on(&"Hud", &"track", [int(_board_track[id])]))
			var vessel := Kit.get_int(track, "vessel", NONE)
			if vessel != NONE:
				_prize_vessel[id] = vessel


## What rows exist and which controls they have; a change rebuilds the rows.
func _structure(available: Array[Dictionary], actions: Array[Dictionary], offboard: Array[Dictionary]) -> String:
	var parts := PackedStringArray()
	var latest := latest_by_action(actions)
	for a: Dictionary in available:
		var kind := Kit.get_int(a, "action", NONE)
		var state := Kit.get_int(latest.get(kind, {}), "state", NONE)
		parts.append("a%d:%d" % [kind, 1 if state == ACTION_RUNNING or state == ACTION_PAUSED else 0])
	for p: Dictionary in _prizes():
		var id := Kit.get_int(p, "id")
		parts.append("p%d:%d:%d:%d" % [id, int(_prize_decided.get(id, NONE)), 1 if _strip_armed.has(id) else 0, 1 if _prize_vessel.has(id) else 0])
	for u: Dictionary in offboard:
		parts.append("u%d:%d" % [Kit.get_int(u, "slot"), unit_state(u)])
	parts.append("pick:%s" % str(_pick))
	return ",".join(parts)


## 0 none aboard, 1 ready, 2 airborne, 3 recharging.
static func unit_state(u: Dictionary) -> int:
	if Kit.get_int(u, "body", NONE) != NONE:
		return 2
	if Kit.get_float(u, "recharging_s") > 0.0:
		return 3
	if Kit.get_int(u, "aboard") > 0:
		return 1
	return 0


# --- building ------------------------------------------------------------------------------------

func _rebuild() -> void:
	var focused := get_viewport().gui_get_focus_owner() if is_inside_tree() else null
	var focus_key := String(focused.get_meta(&"ops_key", "")) if focused != null and is_ancestor_of(focused) else ""
	Kit.clear(_actions)
	Kit.clear(_offboard)
	_rows.clear()
	var latest := latest_by_action(_running)
	for a: Dictionary in _available:
		_separate(_actions)
		_build_action_row(a, latest.get(Kit.get_int(a, "action", NONE), {}))
	for p: Dictionary in _prizes():
		_separate(_actions)
		_build_prize_row(p)
	for u: Dictionary in _units:
		_separate(_offboard)
		_build_unit_row(u)
	_build_pick()
	_link_focus()
	if not focus_key.is_empty():
		for b: Control in _buttons():
			if String(b.get_meta(&"ops_key", "")) == focus_key:
				Ui.focus(b)
				break


## A 1 px rule between rows of one section (none before the first).
func _separate(section: VBoxContainer) -> void:
	if section.get_child_count() > 0:
		section.add_child(HSeparator.new())


func _button(key: String, text_key: String, on_press: Callable) -> Button:
	var b := Kit.inline_button(text_key)
	b.set_meta(&"ops_key", key)
	b.pressed.connect(on_press)
	return b


func _line(variation: StringName = &"Body") -> Label:
	var l := Kit.label("", variation, true)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	return l


func _build_action_row(avail: Dictionary, latest: Dictionary) -> void:
	var kind := Kit.get_int(avail, "action", NONE)
	var key := "action:%d" % kind
	var box := Kit.vbox(1)
	var head := Kit.hbox(2)
	var title := _line()
	title.text = _tx_str(&"word", [&"station", kind])
	head.add_child(title)
	var amount := Kit.label("", &"Body", true)
	amount.autowrap_mode = TextServer.AUTOWRAP_OFF
	head.add_child(amount)
	box.add_child(head)
	var bar := ProgressBar.new()
	bar.show_percentage = false
	bar.min_value = 0.0
	bar.max_value = 1.0
	bar.step = 0.0
	bar.custom_minimum_size = Vector2(0.0, Ui.spacing(1))
	bar.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	box.add_child(bar)
	var status := _line()
	box.add_child(status)
	var buttons := HFlowContainer.new()
	Ui.set_separation(buttons, 2)
	var under_way := _under_way(latest)
	var start: Button = null
	var cancel: Button = null
	if under_way:
		var id := Kit.get_int(latest, "id")
		cancel = _button(key + ":cancel", "hud_ops.action.cancel", func() -> void: _cancel_action(key, id))
		buttons.add_child(cancel)
	else:
		start = _button(key + ":start", start_key(kind, StringName(Kit.get_str(avail, "needs", "none"))), func() -> void: _start_action(key, kind, StringName(Kit.get_str(avail, "needs", "none"))))
		buttons.add_child(start)
	box.add_child(buttons)
	_actions.add_child(box)
	_rows[key] = {"amount": amount, "bar": bar, "status": status, "start": start, "cancel": cancel, "kind": kind}


static func _under_way(latest: Dictionary) -> bool:
	var s := Kit.get_int(latest, "state", NONE)
	return s == ACTION_RUNNING or s == ACTION_PAUSED


## The start button's text key for an action and what it targets.
static func start_key(kind: int, needs: StringName) -> String:
	match kind:
		STATION_BOARD_AND_CAPTURE:
			return "hud_ops.start.board"
		STATION_PHOTOGRAPH:
			return "hud_ops.start.photograph"
		STATION_INSERT_OR_EXTRACT:
			return "hud_ops.start.landing"
		STATION_RECOVER_SURVIVORS:
			return "hud_ops.start.survivors"
		STATION_LAY_MINES:
			return "hud_ops.start.mines"
	match needs:
		&"track":
			return "hud_ops.start.track"
		&"line":
			return "hud_ops.start.line"
	return "hud_ops.start.nearest"


func _build_prize_row(p: Dictionary) -> void:
	var id := Kit.get_int(p, "id")
	var key := "prize:%d" % id
	var box := Kit.vbox(1)
	var title := _line()
	title.text = tr(&"hud_ops.prize.title")
	box.add_child(title)
	var status := _line()
	box.add_child(status)
	var buttons := HFlowContainer.new()
	Ui.set_separation(buttons, 2)
	var decided := int(_prize_decided.get(id, NONE))
	var vessel := int(_prize_vessel.get(id, NONE))
	if vessel != NONE and decided != PRIZE_STRIP_ON_SPOT:
		if _strip_armed.has(id):
			buttons.add_child(_button(key + ":strip_confirm", "hud_ops.prize.strip_confirm", func() -> void: _decide_prize(id, PRIZE_STRIP_ON_SPOT)))
			buttons.add_child(_button(key + ":strip_keep", "hud_ops.prize.strip_keep", func() -> void: _arm_strip(id, false)))
		else:
			if decided != PRIZE_SAIL_HOME:
				buttons.add_child(_button(key + ":home", "hud_ops.prize.home", func() -> void: _decide_prize(id, PRIZE_SAIL_HOME)))
			buttons.add_child(_button(key + ":strip", "hud_ops.prize.strip", func() -> void: _arm_strip(id, true)))
	if buttons.get_child_count() > 0:
		box.add_child(buttons)
	else:
		buttons.free()
	_actions.add_child(box)
	_rows[key] = {"status": status, "prize": id}


func _build_unit_row(u: Dictionary) -> void:
	var slot := Kit.get_int(u, "slot", NONE)
	var key := "unit:%d" % slot
	var box := Kit.vbox(1)
	var head := Kit.hbox(2)
	var title := _line()
	head.add_child(title)
	var count := Kit.label("", &"Body", true)
	count.autowrap_mode = TextServer.AUTOWRAP_OFF
	head.add_child(count)
	box.add_child(head)
	var status := _line()
	box.add_child(status)
	var buttons := HFlowContainer.new()
	Ui.set_separation(buttons, 2)
	var shadow: Button = null
	match unit_state(u):
		1:
			buttons.add_child(_button(key + ":launch", "hud_ops.unit.launch", func() -> void: _order(key, slot, {"kind": OFFBOARD_LAUNCH})))
		2:
			buttons.add_child(_button(key + ":recall", "hud_ops.unit.recall", func() -> void: _order(key, slot, {"kind": OFFBOARD_RECALL})))
			shadow = _button(key + ":shadow", "hud_ops.unit.shadow", func() -> void: _order(key, slot, {"kind": OFFBOARD_SHADOW, "track": _selected}))
			buttons.add_child(shadow)
			buttons.add_child(_button(key + ":orbit", "hud_ops.unit.orbit", func() -> void: begin_pick(slot, OFFBOARD_ORBIT)))
			buttons.add_child(_button(key + ":waypoint", "hud_ops.unit.waypoint", func() -> void: begin_pick(slot, OFFBOARD_WAYPOINT)))
	if buttons.get_child_count() > 0:
		box.add_child(buttons)
	else:
		buttons.free()
	_offboard.add_child(box)
	_rows[key] = {"title": title, "count": count, "status": status, "shadow": shadow, "slot": slot}


func _build_pick() -> void:
	var open := not _pick.is_empty()
	_pick_rule.visible = open
	$Panel/Margin/Scroll/Column/Pick.visible = open
	if not open:
		return
	if _pick.get("kind") == &"line":
		_pick_text.text = tr(&"hud_ops.pick.line")
	elif int(_pick.get("order", NONE)) == OFFBOARD_ORBIT:
		_pick_text.text = tr(&"hud_ops.pick.orbit").format([Kit.number(ORBIT_RADIUS_M / 1000.0, &"km", 0)])
	else:
		_pick_text.text = tr(&"hud_ops.pick.waypoint")


# --- updating in place ---------------------------------------------------------------------------

func _update() -> void:
	var latest := latest_by_action(_running)
	for a: Dictionary in _available:
		var kind := Kit.get_int(a, "action", NONE)
		var key := "action:%d" % kind
		if _rows.has(key):
			_update_action(key, _rows[key], a, latest.get(kind, {}))
	for p: Dictionary in _prizes():
		var key := "prize:%d" % Kit.get_int(p, "id")
		if _rows.has(key):
			_update_prize(_rows[key], Kit.get_int(p, "id"))
	for u: Dictionary in _units:
		var key := "unit:%d" % Kit.get_int(u, "slot", NONE)
		if _rows.has(key):
			_update_unit(key, _rows[key], u)


func _update_action(key: String, row: Dictionary, avail: Dictionary, latest: Dictionary) -> void:
	var amount: Label = row["amount"]
	var bar: ProgressBar = row["bar"]
	var status: Label = row["status"]
	var start: Button = row["start"]
	var under_way := _under_way(latest)
	bar.visible = under_way
	amount.visible = under_way
	var text := ""
	if under_way:
		var progress := clampf(Kit.get_float(latest, "progress"), 0.0, 1.0)
		bar.value = progress
		amount.text = Kit.percent(progress)
		text = action_status(latest)
	else:
		var why := _Text.resolve(avail.get("reason"))
		var needs := StringName(Kit.get_str(avail, "needs", "none"))
		var enabled := Kit.get_bool(avail, "enabled")
		if enabled and needs == &"track" and _selected == NONE:
			enabled = false
			why = tr(&"hud_ops.action.select_track")
		if enabled and _pick.get("kind") == &"line" and Kit.get_int(row, "kind") == STATION_LAY_MINES:
			enabled = false
		if start != null:
			start.disabled = not enabled
		text = why if not why.is_empty() else last_attempt(latest)
	if _refused.has(key):
		text = String(_refused[key])
	status.text = text
	status.visible = not text.is_empty()


## "Under way", or "Paused: too fast for the action" with the sim's reason (L786: why paused).
static func action_status(a: Dictionary) -> String:
	var block := _Text.resolve(a.get("block"))
	match Kit.get_int(a, "state", NONE):
		ACTION_PAUSED:
			if block.is_empty():
				return String(TranslationServer.translate(&"hud_ops.state.paused"))
			return String(TranslationServer.translate(&"hud_ops.state.paused_why")).format([block])
		ACTION_RUNNING:
			return String(TranslationServer.translate(&"hud_ops.state.running"))
	return ""


## The outcome of the last finished attempt of an action, or "".
static func last_attempt(a: Dictionary) -> String:
	match Kit.get_int(a, "state", NONE):
		ACTION_DONE:
			if Kit.get_int(a, "action") == STATION_BOARD_AND_CAPTURE:
				return ""
			return String(TranslationServer.translate(&"hud_ops.state.done"))
		ACTION_ABORTED:
			var block := _Text.resolve(a.get("block"))
			if block.is_empty():
				return String(TranslationServer.translate(&"hud_ops.state.aborted"))
			return String(TranslationServer.translate(&"hud_ops.state.aborted_why")).format([block])
	return ""


func _update_prize(row: Dictionary, id: int) -> void:
	var status: Label = row["status"]
	var decided := int(_prize_decided.get(id, NONE))
	var key := &"hud_ops.prize.default"
	if not _prize_vessel.has(id):
		key = &"hud_ops.prize.unknown"
	elif decided == PRIZE_SAIL_HOME:
		key = &"hud_ops.prize.sailing"
	elif decided == PRIZE_STRIP_ON_SPOT:
		key = &"hud_ops.prize.stripping"
	elif _strip_armed.has(id):
		key = &"hud_ops.prize.strip_ask"
	var text := tr(key)
	var refused := "prize:%d" % id
	if _refused.has(refused):
		text = String(_refused[refused])
	status.text = text


func _update_unit(key: String, row: Dictionary, u: Dictionary) -> void:
	var title: Label = row["title"]
	var count: Label = row["count"]
	var status: Label = row["status"]
	title.text = _Text.resolve(u.get("name"))
	var aboard := Kit.get_int(u, "aboard")
	count.text = tr(&"hud_ops.unit.aboard").format([Kit.number(aboard, &"none", 0)])
	var text := unit_status(u)
	if _refused.has(key):
		text = String(_refused[key])
	status.text = text
	var shadow: Button = row["shadow"]
	if shadow != null:
		shadow.disabled = _selected == NONE
		shadow.tooltip_text = tr(&"hud_ops.action.select_track") if _selected == NONE else ""


## What a drone or helicopter is doing: airborne with endurance, recharging, ready or none aboard.
static func unit_status(u: Dictionary) -> String:
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	match unit_state(u):
		2:
			var left := Kit.get_float(u, "endurance_s")
			var key := &"hud_ops.unit.airborne_low" if left < LOW_ENDURANCE_S else &"hud_ops.unit.airborne"
			return t.call(key).format([_tx_str(&"duration", [left])])
		3:
			return t.call(&"hud_ops.unit.recharging").format([_tx_str(&"duration", [Kit.get_float(u, "recharging_s")])])
		1:
			return t.call(&"hud_ops.unit.ready")
	return t.call(&"hud_ops.unit.empty")


# --- orders --------------------------------------------------------------------------------------

func _start_action(key: String, kind: int, needs: StringName) -> void:
	_refused.erase(key)
	match needs:
		&"track":
			if _selected == NONE:
				return
			if _send(key, &"start_action", [kind, {"kind": &"track", "track": _selected}]) and kind == STATION_BOARD_AND_CAPTURE:
				_pending_board_track = _selected
		&"line":
			begin_line()
		_:
			# Insert, extract and recovery take the nearest landing or survivor site (L788-789).
			_send(key, &"start_action", [kind, {"kind": &"none"}])
	_redraw()


func _cancel_action(key: String, id: int) -> void:
	_refused.erase(key)
	_send(key, &"cancel_action", [id])
	_redraw()


func _arm_strip(id: int, on: bool) -> void:
	if on and not _strip_armed.has(id):
		_strip_armed.append(id)
	elif not on:
		_strip_armed.erase(id)
	_redraw()


func _decide_prize(id: int, decision: int) -> void:
	var key := "prize:%d" % id
	_refused.erase(key)
	_strip_armed.erase(id)
	var vessel := int(_prize_vessel.get(id, NONE))
	if vessel != NONE and _send(key, &"prize", [vessel, decision]):
		_prize_decided[id] = decision
	_redraw()


func _order(key: String, slot: int, order: Dictionary) -> void:
	_refused.erase(key)
	if int(order.get("kind", NONE)) == OFFBOARD_SHADOW and _selected == NONE:
		return
	_send(key, &"offboard", [slot, order])
	_redraw()


## Send an order through Orders; a refusal shows on the row. Returns true when sent.
func _send(key: String, method: StringName, args: Array) -> bool:
	var ok: Variant = Kit.call_on(&"Orders", method, args)
	if ok == true:
		return true
	_refused[key] = tr(&"hud_ops.refused")
	return false


func _redraw() -> void:
	show_ops(_available, _running, _units, _selected)


# --- plot picks ----------------------------------------------------------------------------------

## The plot for picks: the one handed over by `set_plot`, else a live BpPlotView in the tree, the
## visible one first (the plot table may be hidden behind the bridge view while a pick starts).
func _find_plot() -> Control:
	if _plot_ref != null and _plot_ref.get_ref() != null:
		return _plot_ref.get_ref() as Control
	if not is_inside_tree():
		return null
	var fallback: Control = null
	for n: Node in get_tree().root.find_children("*", "BpPlotView", true, false):
		if not n is Control or _doomed(n):
			continue
		if (n as Control).is_visible_in_tree():
			return n as Control
		if fallback == null:
			fallback = n as Control
	return fallback


## True when the node or an ancestor is about to be freed (a screen closing around its plot).
static func _doomed(n: Node) -> bool:
	while n != null:
		if n.is_queued_for_deletion():
			return true
		n = n.get_parent()
	return false


## Ask the plot for a mine line; the action starts when the line is drawn.
func begin_line() -> void:
	cancel_pick()
	_pick = {"kind": &"line"}
	_redraw()
	Ui.focus(_pick_cancel)
	var plot := _find_plot()
	if plot != null and plot.has_signal(&"line_drawn"):
		_pick_plot = weakref(plot)
		plot.connect(&"line_drawn", _on_line_drawn, CONNECT_ONE_SHOT)
		plot.call(&"set_tool", &"line")


## Ask the plot for a point for a drone order (`Bp.OFFBOARD_ORBIT` or `Bp.OFFBOARD_WAYPOINT`).
func begin_pick(slot: int, order: int) -> void:
	cancel_pick()
	_pick = {"kind": &"point", "slot": slot, "order": order}
	_redraw()
	Ui.focus(_pick_cancel)
	var plot := _find_plot()
	if plot != null and plot.has_signal(&"sea_clicked"):
		_pick_plot = weakref(plot)
		plot.connect(&"sea_clicked", _on_sea_clicked)


## Close an open pick and give the plot back its orders tool.
func cancel_pick() -> void:
	if _pick.is_empty():
		return
	_pick = {}
	_release_plot()
	if is_node_ready():
		_redraw()


## Disconnect from the plot the pick went to (not whichever plot is visible now).
func _release_plot() -> void:
	var plot: Control = _pick_plot.get_ref() as Control if _pick_plot != null else null
	_pick_plot = null
	if plot == null:
		return
	if plot.is_connected(&"sea_clicked", _on_sea_clicked):
		plot.disconnect(&"sea_clicked", _on_sea_clicked)
	if plot.is_connected(&"line_drawn", _on_line_drawn):
		plot.disconnect(&"line_drawn", _on_line_drawn)
	plot.call(&"set_tool", &"orders")


func _on_line_drawn(a: Vector2, b: Vector2) -> void:
	_pick = {}
	var key := "action:%d" % STATION_LAY_MINES
	# The order goes first, so nothing on the plot side can lose it.
	if a.is_finite() and b.is_finite() and a != b:
		_send(key, &"start_action", [STATION_LAY_MINES, {"kind": &"line", "a": a, "b": b}])
	_redraw()
	_release_plot()


func _on_sea_clicked(map: Vector2, button: int) -> void:
	if _pick.get("kind") != &"point" or button != MOUSE_BUTTON_LEFT or not map.is_finite():
		return
	var slot := int(_pick.get("slot", NONE))
	var order := int(_pick.get("order", NONE))
	_pick = {}
	var key := "unit:%d" % slot
	if order == OFFBOARD_ORBIT:
		_send(key, &"offboard", [slot, {"kind": OFFBOARD_ORBIT, "centre": map, "radius_m": ORBIT_RADIUS_M}])
	else:
		_send(key, &"offboard", [slot, {"kind": OFFBOARD_WAYPOINT, "point": map}])
	_redraw()
	_release_plot()


# --- layout and focus ----------------------------------------------------------------------------

## The bar is as tall as its rows, up to this share of the screen; past that its rows scroll (and
## follow focus), so a 200% UI scale never pushes buttons off screen (L1714).
const MAX_SCREEN_SHARE := 0.7


## The bar's width: the HUD shell gives it its dock's width, so it never scrolls sideways there.
func set_panel_width(width: float) -> void:
	var w := maxf(floorf(width), 0.0)
	if is_equal_approx(_panel.custom_minimum_size.x, w):
		return
	_panel.custom_minimum_size = Vector2(w, 0.0)
	_fit_root()


func _fit_root() -> void:
	if not is_inside_tree():
		return
	var cap := maxf(get_viewport_rect().size.y * MAX_SCREEN_SHARE, Kit.BUTTON_H * 4.0)
	_scroll.custom_minimum_size = Vector2(0.0, minf(_column.get_combined_minimum_size().y, cap))
	custom_minimum_size = _panel.get_combined_minimum_size()
	size = custom_minimum_size
	_panel.size = custom_minimum_size


func _buttons() -> Array[Control]:
	var out: Array[Control] = []
	for c: Node in _panel.find_children("*", "Button", true, false):
		if (c as Button).is_visible_in_tree():
			out.append(c as Control)
	return out


func _link_focus() -> void:
	var list: Array[Control] = []
	for c: Node in _actions.find_children("*", "Button", true, false):
		list.append(c as Control)
	for c: Node in _offboard.find_children("*", "Button", true, false):
		list.append(c as Control)
	if not _pick.is_empty():
		list.append(_pick_cancel)
	for i in list.size():
		var c := list[i]
		var up := list[i - 1] if i > 0 else c
		var down := list[i + 1] if i < list.size() - 1 else c
		c.focus_neighbor_top = c.get_path_to(up)
		c.focus_neighbor_bottom = c.get_path_to(down)
		c.focus_neighbor_left = c.get_path_to(up)
		c.focus_neighbor_right = c.get_path_to(down)
		c.focus_previous = c.get_path_to(up)
		c.focus_next = c.get_path_to(down)


static func _tx_str(method: StringName, args: Array) -> String:
	var v: Variant = Kit.call_on(&"Tx", method, args)
	return String(v) if v is String or v is StringName else ""
