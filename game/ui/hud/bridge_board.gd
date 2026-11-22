extends VBoxContainer
## Bridge Board: per officer task, reason, next action, control state, overrides.
## DESIGN.md L1664-1665. Unit: ui-hud-board.
## Reads and calls: Hud.board, Hud.board_changed, Hud.takeable_roles, Orders.take_control, Orders.query_overrides, Orders.revert.
##
## Docked at the right edge in both views (L106). One group per officer, separated by 1 px rules
## (no cards), in the order a fight uses them: Helm, Gunnery, Sensors, Engineer, Damage Control.
## Each group (HUD rule 8, L351):
## - The billet and its control state: "Officer" while the officer drives the role, "Player"
##   while the player holds it.
## - The current task in one line, its evidence-citing reason, and the next action. While the
##   player holds the role, the officer's suggested solution shows as an advisory line.
## - The sensors role the player can take (rule 9): a toggle that takes the role by hand or gives it
##   back, shows Officer or Player with the A chip or the hand glyph, and names its key on the
##   device in use (5 by default). The helm and the weapons groups are taken where their gauges
##   and mounts are, in the helm cluster and the weapons panel; the board still names who holds
##   them in each officer's control state, so one takeover control per role is on screen.
## - "N overridden" when values differ from the design: it opens the diff against the default
##   (`Orders.query_overrides`; the reply fills it and stays current while open), each value with
##   a one-click revert, and Revert all.
## - Sensors also shows the emission control state (A chip or hand, one-click revert), the
##   emitters radiating now, and the ESM warning "Enemy ESM has probably detected you".
## A takeover, a release or a mode change draws the 10 s box round the officer's group
## (`Hud.control_changed`, L1649). No spinners: an officer with nothing to report shows only its
## billet and state. States: no sortie or no hull yet (waiting), populated.
## Focus (L1738): `focus_targets()` lists the board's controls top to bottom and
## `focus_targets_changed` fires when that list changes; the sortie HUD links them between the
## track table and the station buttons. `show_board(board, roles, emcon)` draws hand-built
## dictionaries (tests and captures).

signal focus_targets_changed

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const ChipScene := preload("res://ui/widgets/auto_chip.tscn")
const HintScene := preload("res://ui/widgets/shortcut_hint.tscn")

## `Bp.OFFICER_*` in display order: Helm, Gunnery, Sensors, Engineer, Damage Control.
const ORDER: Array[int] = [0, 3, 2, 1, 4]
const OFFICER_HELM := 0
const OFFICER_SENSORS := 2
const OFFICER_GUNNERY := 3
## `Bp.ROLE_*`, `Bp.HOLDER_*`, `Bp.NONE`, kept here so the script parses before the extension loads.
const ROLE_HELM := 0
const ROLE_SENSORS := 1
const ROLE_MOUNT := 2
const ROLE_WEAPON_GROUP := 3
const HOLDER_PLAYER := 1
const NONE := -1
const ROLE_BUTTON_H := 40.0
## Role kinds whose takeover sits on the board; the helm cluster and weapons panel carry the rest.
const BOARD_ROLES: Array[int] = [ROLE_SENSORS]

@onready var _header_margin: MarginContainer = $Margin
@onready var _title: Label = $Margin/Header/Title
@onready var _scroll: ScrollContainer = $Scroll
@onready var _margin: MarginContainer = $Scroll/Margin
@onready var _groups_box: VBoxContainer = $Scroll/Margin/Groups
@onready var _empty: Label = $Scroll/Margin/Groups/Empty

## Per officer (`Bp.OFFICER_*`): the group's nodes.
var _groups: Dictionary[int, Dictionary] = {}
## Per role key ("kind:index"): `{ "button": Button, "hint": Control, "role": Dictionary }`.
var _role_rows: Dictionary[String, Dictionary] = {}
var _role_order: Array[String] = []
## Officers whose diff is open, and the last reply per officer.
var _diff_open: Dictionary[int, bool] = {}
var _diff_values: Dictionary[int, Array] = {}
## Box deadline per officer, msec.
var _box_until: Dictionary[int, int] = {}
var _box_timer: Timer
var _part_names: Dictionary[int, String] = {}
var _board: Dictionary = {}
var _roles: Array[Dictionary] = []
var _emcon: Dictionary = {}
var _live := false
var _targets: Array[Control] = []
## Height the HUD shell gives the board (`set_max_height`); below it the groups scroll. -1: no cap.
var _max_height := -1.0


func _ready() -> void:
	Ui.set_separation(self, 0)
	# 16 px from the dock's edges, as every docked cluster (their text lines up down the dock).
	Ui.set_margins(_header_margin, 3)
	_header_margin.add_theme_constant_override(&"margin_bottom", 0)
	Ui.set_margins(_margin, 3)
	_margin.add_theme_constant_override(&"margin_top", Ui.spacing(1))
	Ui.set_separation(_groups_box, 2)
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	accessibility_name = tr(&"hud_board.access")
	for officer: int in ORDER:
		_build_group(officer)
	_margin.minimum_size_changed.connect(_fit_height, CONNECT_DEFERRED)
	_box_timer = Timer.new()
	_box_timer.one_shot = true
	_box_timer.timeout.connect(_expire_boxes)
	add_child(_box_timer)
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		_live = true
		for sig: StringName in [&"board_changed", &"mounts_changed"]:
			if hud.has_signal(sig):
				hud.connect(sig, refresh)
		if hud.has_signal(&"ship_static_changed"):
			hud.connect(&"ship_static_changed", _on_ship_static)
		if hud.has_signal(&"control_changed"):
			hud.connect(&"control_changed", _on_control_changed)
		if hud.has_signal(&"overrides_reply"):
			hud.connect(&"overrides_reply", _on_overrides_reply)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: _on_ship_static())
	_on_ship_static()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		accessibility_name = tr(&"hud_board.access")
		_read_part_names()
		_render()


## The board takes the height its groups need, up to `height` (the HUD shell shares the dock's
## height out); past it the groups scroll (floor 2).
func set_max_height(height: float) -> void:
	if is_equal_approx(height, _max_height):
		return
	_max_height = height
	_fit_height()


func _fit_height() -> void:
	if not is_node_ready():
		return
	var want := _margin.get_combined_minimum_size().y
	if _max_height >= 0.0:
		# The cap is for the whole board: take off the title row and the gap under it.
		var chrome := get_combined_minimum_size().y - _scroll.get_combined_minimum_size().y
		want = clampf(_max_height - chrome, 0.0, want)
	want = ceilf(want)
	if not is_equal_approx(_scroll.custom_minimum_size.y, want):
		_scroll.custom_minimum_size = Vector2(0.0, want)


# --- building ------------------------------------------------------------------------------------

func _build_group(officer: int) -> void:
	if not _groups.is_empty():
		var rule := HSeparator.new()
		rule.name = "Rule%d" % officer
		_groups_box.add_child(rule)
	var box := VBoxContainer.new()
	box.name = "Officer%d" % officer
	Ui.set_separation(box, 1)
	box.draw.connect(_draw_box.bind(officer))
	_groups_box.add_child(box)
	var head := HBoxContainer.new()
	head.name = "Head"
	Ui.set_separation(head, 2)
	box.add_child(head)
	var billet := _label(&"Body")
	billet.name = "Billet"
	billet.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	billet.autowrap_mode = TextServer.AUTOWRAP_OFF
	billet.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	head.add_child(billet)
	var state := _label(&"Dense")
	state.name = "State"
	state.autowrap_mode = TextServer.AUTOWRAP_OFF
	state.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	head.add_child(state)
	var task := _label(&"Body")
	task.name = "Task"
	box.add_child(task)
	var reason := _label(&"Dense")
	reason.name = "Reason"
	box.add_child(reason)
	var next := _label(&"Dense")
	next.name = "Next"
	box.add_child(next)
	var advisory := _label(&"SeverityAdvisory")
	advisory.name = "Advisory"
	box.add_child(advisory)
	var extra := VBoxContainer.new()
	extra.name = "Extra"
	Ui.set_separation(extra, 1)
	box.add_child(extra)
	var emcon_row: HFlowContainer = null
	var emcon_label: Label = null
	var emcon_chip: Button = null
	var emitters: Label = null
	var esm: Label = null
	if officer == OFFICER_SENSORS:
		# A flow: in a narrow dock (large UI scale) the cycle hint moves under the state.
		emcon_row = HFlowContainer.new()
		emcon_row.name = "Emcon"
		emcon_row.add_theme_constant_override(&"h_separation", Ui.spacing(1))
		emcon_row.add_theme_constant_override(&"v_separation", Ui.spacing(0))
		emcon_label = _label(&"Dense")
		emcon_label.autowrap_mode = TextServer.AUTOWRAP_OFF
		emcon_label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		emcon_row.add_child(emcon_label)
		emcon_chip = ChipScene.instantiate()
		emcon_chip.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		emcon_chip.connect(&"revert_requested", _revert.bind("emcon"))
		emcon_row.add_child(emcon_chip)
		var emcon_hint: Control = HintScene.instantiate()
		emcon_hint.name = "EmconHint"
		emcon_hint.set(&"label", &"hud_board.emcon.cycle")
		emcon_hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		emcon_hint.size_flags_horizontal = Control.SIZE_EXPAND | Control.SIZE_SHRINK_END
		emcon_row.add_child(emcon_hint)
		extra.add_child(emcon_row)
		emitters = _label(&"Dense")
		emitters.name = "Emitters"
		extra.add_child(emitters)
		esm = _label(&"SeverityCaution")
		esm.name = "Esm"
		extra.add_child(esm)
	var roles := HFlowContainer.new()
	roles.name = "Roles"
	roles.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	roles.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	box.add_child(roles)
	var overrides := Button.new()
	overrides.name = "Overrides"
	overrides.theme_type_variation = &"FlatButton"
	overrides.icon = Ui.icon(&"hand")
	overrides.alignment = HORIZONTAL_ALIGNMENT_LEFT
	overrides.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	overrides.custom_minimum_size = Vector2(0.0, 32.0)
	overrides.focus_mode = Control.FOCUS_ALL
	overrides.text_overrun_behavior = TextServer.OVERRUN_NO_TRIMMING
	overrides.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	overrides.pressed.connect(_on_overrides_pressed.bind(officer))
	box.add_child(overrides)
	var diff := VBoxContainer.new()
	diff.name = "Diff"
	Ui.set_separation(diff, 1)
	diff.visible = false
	box.add_child(diff)
	_groups[officer] = {
		"box": box, "billet": billet, "state": state, "task": task, "reason": reason, "next": next,
		"advisory": advisory, "roles": roles, "overrides": overrides, "diff": diff,
		"emcon_label": emcon_label, "emcon_chip": emcon_chip, "emitters": emitters, "esm": esm,
	}


func _label(variation: StringName) -> Label:
	var l := Label.new()
	l.theme_type_variation = variation
	l.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	l.visible = false
	return l


# --- data ----------------------------------------------------------------------------------------

## Pull the board, the takeable roles and the emission detail from Hud and draw them.
func refresh() -> void:
	if not is_node_ready():
		return
	var board: Dictionary = {}
	var roles: Array[Dictionary] = []
	var emcon: Dictionary = {}
	if _live and Kit.call_on(&"Hud", &"has_ship") == true:
		board = Kit.as_dict(Kit.call_on(&"Hud", &"board"))
		roles = Kit.dicts(Kit.call_on(&"Hud", &"takeable_roles"))
		emcon = Kit.as_dict(Kit.call_on(&"Hud", &"emcon"))
	# Mounts report at 10 Hz in a fight; the board redraws only when what it shows changed.
	if board == _board and roles == _roles and emcon == _emcon and not _board.is_empty():
		return
	show_board(board, roles, emcon)


## Draw a `BoardDict`, the `takeable_roles` list and the `emcon` detail. An empty board is the
## waiting state.
func show_board(board: Dictionary, roles: Array[Dictionary], emcon: Dictionary = {}) -> void:
	if not is_node_ready():
		return
	_board = board
	_roles = roles
	_emcon = emcon
	_render()


func _on_ship_static() -> void:
	_read_part_names()
	refresh()


func _read_part_names() -> void:
	_part_names.clear()
	if not _live:
		return
	for p: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"parts_static")):
		_part_names[Kit.get_int(p, "slot", NONE)] = _Text.resolve(p.get("name", {}))


## Name a part the way the HUD does, for the radiating-emitter line.
func set_part_names(names: Dictionary) -> void:
	_part_names.clear()
	for k: Variant in names:
		_part_names[int(k)] = String(names[k])
	if is_node_ready():
		_render()


func _render() -> void:
	var officers := Kit.dicts(_board.get("officers", []))
	var waiting := officers.is_empty()
	_empty.visible = waiting
	if waiting:
		_empty.text = tr(&"hud_board.waiting")
	var by_role: Dictionary[int, Dictionary] = {}
	for o: Dictionary in officers:
		by_role[Kit.get_int(o, "role", NONE)] = o
	for officer: int in ORDER:
		var g := _groups[officer]
		var box: VBoxContainer = g["box"]
		box.visible = not waiting
		var rule := _groups_box.get_node_or_null(NodePath("Rule%d" % officer))
		if rule != null:
			(rule as Control).visible = not waiting
		if waiting:
			continue
		_render_officer(officer, by_role.get(officer, {}))
	_render_roles()
	_render_sensors()
	_relink()


func _render_officer(officer: int, o: Dictionary) -> void:
	var g := _groups[officer]
	var billet_key := StringName(o.get("name", &""))
	var billet := tr(billet_key) if billet_key != &"" else _word(&"officer", officer)
	(g["billet"] as Label).text = billet
	(g["billet"] as Label).visible = true
	var player := Kit.get_int(o, "holder", 0) == HOLDER_PLAYER
	var state_label: Label = g["state"]
	state_label.text = tr(&"hud_board.state.player") if player else tr(&"hud_board.state.officer")
	state_label.visible = true
	_set_line(g["task"], _Text.resolve(o.get("task", {})))
	_set_line(g["reason"], _Text.resolve(o.get("reason", {})))
	var next := _Text.resolve(o.get("next", {}))
	_set_line(g["next"], tr(&"hud_board.next").format([next]) if not next.is_empty() else "")
	# The advisory is already a sentence ("Suggested course 090, speed 18 kn"); the advisory
	# stripe sets it apart from the officer's own lines.
	_set_line(g["advisory"], _Text.resolve(o.get("advisory", {})) if player else "")
	var count := maxi(0, Kit.get_int(o, "overrides", 0))
	var button: Button = g["overrides"]
	button.visible = count > 0
	button.text = tr(&"hud_board.overrides.count").format([count])
	button.tooltip_text = tr(&"hud_board.overrides.tip")
	button.accessibility_name = tr(&"hud_board.overrides.access").format([billet, count])
	if count == 0 and _diff_open.get(officer, false):
		_close_diff(officer)
	var box: VBoxContainer = g["box"]
	box.accessibility_name = billet
	box.accessibility_description = "\n".join(PackedStringArray([state_label.text, (g["task"] as Label).text, (g["reason"] as Label).text, (g["next"] as Label).text]))


static func _set_line(label: Label, text: String) -> void:
	label.text = text
	label.visible = not text.is_empty()


func _render_sensors() -> void:
	var g := _groups[OFFICER_SENSORS]
	var emcon_label: Label = g["emcon_label"]
	var chip: Button = g["emcon_chip"]
	var emitters: Label = g["emitters"]
	var esm: Label = g["esm"]
	if _board.is_empty():
		for c: Control in [emcon_label.get_parent(), emitters, esm]:
			c.visible = false
		return
	(emcon_label.get_parent() as Control).visible = true
	emcon_label.visible = true
	# The key name is read once the board has data (no keyboard exists in a headless check).
	var hint := emcon_label.get_parent().get_node_or_null(^"EmconHint")
	if hint != null and StringName(hint.get(&"action")) != &"emcon_cycle":
		hint.set(&"action", &"emcon_cycle")
	var emcon := Kit.get_int(_emcon, "emcon", Kit.get_int(_board, "emcon", NONE))
	emcon_label.text = tr(&"hud_board.emcon.state").format([_word(&"emcon", emcon)])
	chip.set(&"overridden", Kit.get_bool(_emcon, "overridden"))
	var recommended := Kit.get_int(_emcon, "recommended", NONE)
	chip.set(&"default_text", _word(&"emcon", recommended) if recommended != NONE else "")
	var radiating: PackedInt32Array = _board.get("radiating", PackedInt32Array()) if _board.get("radiating") is PackedInt32Array else PackedInt32Array()
	if radiating.is_empty():
		emitters.text = tr(&"hud_board.emitters.none")
	else:
		var names: PackedStringArray = []
		for slot: int in radiating:
			names.append(_part_names.get(slot, tr(&"hud_board.emitters.part").format([slot + 1])))
		emitters.text = tr(&"hud_board.emitters.radiating").format([", ".join(names)])
	emitters.visible = true
	var warn := Kit.get_bool(_board, "esm_warning")
	esm.text = tr(&"hud_board.esm_warning") if warn else ""
	esm.visible = warn


# --- roles ---------------------------------------------------------------------------------------

static func _role_key(role: Dictionary) -> String:
	return "%d:%d" % [Kit.get_int(role, "kind", NONE), Kit.get_int(role, "index", 0)]


static func _officer_of(role: Dictionary) -> int:
	match Kit.get_int(role, "kind", NONE):
		ROLE_HELM:
			return OFFICER_HELM
		ROLE_SENSORS:
			return OFFICER_SENSORS
		_:
			return OFFICER_GUNNERY


func _render_roles() -> void:
	var wanted: Array[String] = []
	var source: Array[Dictionary] = []
	if not _board.is_empty():
		source = _roles
	for r: Dictionary in source:
		var role := Kit.as_dict(r.get("role", {}))
		if role.is_empty() or not BOARD_ROLES.has(Kit.get_int(role, "kind", NONE)):
			continue
		var key := _role_key(role)
		wanted.append(key)
		var row: Dictionary = _role_rows.get(key, {})
		if row.is_empty():
			row = _make_role(key, role, _officer_of(role))
		row["role"] = role
		var button: Button = row["button"]
		var hint: Control = row["hint"]
		var name := _Text.resolve(r.get("name", {}))
		var player := Kit.get_int(r, "holder", 0) == HOLDER_PLAYER
		button.set_pressed_no_signal(player)
		button.icon = Ui.icon(&"hand" if player else &"chip_auto")
		button.text = _role_verb(role, name, player)
		var action := StringName(r.get("action", &""))
		hint.set(&"action", action)
		hint.visible = action != &""
		button.tooltip_text = tr(&"hud_board.role.release_tip" if player else &"hud_board.role.take_tip").format([name])
		button.accessibility_name = tr(&"hud_board.role.player" if player else &"hud_board.role.officer").format([name])
		button.accessibility_description = button.tooltip_text
	# Drop roles the design no longer has.
	for key: String in _role_rows.keys():
		if not wanted.has(key):
			var row: Dictionary = _role_rows[key]
			(row["cell"] as Node).queue_free()
			_role_rows.erase(key)
	_role_order = wanted
	# Visibility is set once per flow: hiding a flow for a moment would drop the focus it holds.
	var used: Dictionary[int, bool] = {}
	for key: String in wanted:
		var row: Dictionary = _role_rows[key]
		var cell: Control = row["cell"]
		var officer := _officer_of(row["role"])
		var flow: HFlowContainer = _groups[officer]["roles"]
		if cell.get_parent() != flow:
			if cell.get_parent() != null:
				cell.get_parent().remove_child(cell)
			flow.add_child(cell)
		if cell.get_index() != flow.get_child_count() - 1:
			flow.move_child(cell, -1)
		used[officer] = true
	for officer: int in ORDER:
		var flow: HFlowContainer = _groups[officer]["roles"]
		var on: bool = used.get(officer, false)
		if flow.visible != on:
			flow.visible = on


## The takeover button's text: "Take helm", "Release sensors", "Group 2".
func _role_verb(role: Dictionary, name: String, player: bool) -> String:
	match Kit.get_int(role, "kind", NONE):
		ROLE_HELM:
			return tr(&"hud_board.role.release_helm" if player else &"hud_board.role.take_helm")
		ROLE_SENSORS:
			return tr(&"hud_board.role.release_sensors" if player else &"hud_board.role.take_sensors")
		ROLE_WEAPON_GROUP:
			# A set of toggles under Gunnery: the group number, with the A chip or hand as its state,
			# keeps two to a row in the dock.
			return tr(&"hud_board.role.group").format([Kit.get_int(role, "index", 0) + 1])
	return tr(&"hud_board.role.release" if player else &"hud_board.role.take").format([name])


func _make_role(key: String, role: Dictionary, officer: int) -> Dictionary:
	var cell := HBoxContainer.new()
	cell.name = "Role_" + key.replace(":", "_")
	Ui.set_separation(cell, 1)
	var button := Button.new()
	button.toggle_mode = true
	button.focus_mode = Control.FOCUS_ALL
	button.custom_minimum_size = Vector2(0.0, ROLE_BUTTON_H)
	button.text_overrun_behavior = TextServer.OVERRUN_NO_TRIMMING
	button.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	button.toggled.connect(_on_role_toggled.bind(key))
	cell.add_child(button)
	var hint: Control = HintScene.instantiate()
	hint.set(&"compact", true)
	hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	hint.mouse_filter = Control.MOUSE_FILTER_IGNORE
	cell.add_child(hint)
	(_groups[officer]["roles"] as HFlowContainer).add_child(cell)
	var row := {"button": button, "hint": hint, "cell": cell, "role": role}
	_role_rows[key] = row
	return row


func _on_role_toggled(on: bool, key: String) -> void:
	var row: Dictionary = _role_rows.get(key, {})
	if row.is_empty():
		return
	var role: Dictionary = row["role"]
	var sent: Variant = Kit.call_on(&"Orders", &"take_control", [Kit.get_int(role, "kind", NONE), Kit.get_int(role, "index", 0), on])
	if sent != true:
		# Nothing went out (no live sortie): show the state the data still has.
		(row["button"] as Button).set_pressed_no_signal(not on)


func _on_control_changed(role: Dictionary, _holder: int) -> void:
	box_officer(_officer_of(role))


## Draw the mode-change box round one officer's group for 10 s (L1649).
func box_officer(officer: int) -> void:
	if not _groups.has(officer):
		return
	_box_until[officer] = Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0)
	(_groups[officer]["box"] as Control).queue_redraw()
	_schedule_boxes()


func _schedule_boxes() -> void:
	_box_timer.stop()
	var next := 0
	for officer: int in _box_until:
		var t := _box_until[officer]
		next = t if next == 0 else mini(next, t)
	if next > 0:
		_box_timer.start(maxf(0.05, (next - Time.get_ticks_msec()) / 1000.0))


func _expire_boxes() -> void:
	var now := Time.get_ticks_msec()
	for officer: int in _box_until.keys():
		if _box_until[officer] <= now:
			_box_until.erase(officer)
			(_groups[officer]["box"] as Control).queue_redraw()
	_schedule_boxes()


func _draw_box(officer: int) -> void:
	if not _box_until.has(officer):
		return
	var box: Control = _groups[officer]["box"]
	var w := 2.0
	# Outside the text by one spacing step, inside the dock's margin.
	var pad := Vector2(Ui.spacing(0) + w * 0.5, Ui.spacing(0) * 0.5 + w * 0.5)
	box.draw_rect(Rect2(-pad, box.size + pad * 2.0), Ui.colour(&"ui.box"), false, w)


# --- overrides -----------------------------------------------------------------------------------

## The count opens the diff, and closes it again.
func _on_overrides_pressed(officer: int) -> void:
	_set_diff(not _diff_open.get(officer, false), officer)


func _set_diff(on: bool, officer: int) -> void:
	if on:
		_diff_open[officer] = true
		(_groups[officer]["diff"] as Control).visible = true
		(_groups[officer]["overrides"] as Button).accessibility_description = tr(&"hud_board.overrides.open")
		if _diff_values.has(officer):
			_fill_diff(officer, _diff_values[officer])
		Kit.call_on(&"Orders", &"query_overrides", [officer])
	else:
		_close_diff(officer)
	_relink()


func _close_diff(officer: int) -> void:
	_diff_open.erase(officer)
	var g := _groups[officer]
	(g["overrides"] as Button).accessibility_description = tr(&"hud_board.overrides.closed")
	(g["diff"] as Control).visible = false


func _on_overrides_reply(officer: int, values: Array) -> void:
	_diff_values[officer] = values
	if _diff_open.get(officer, false):
		_fill_diff(officer, values)
		_relink()


## Fill one officer's diff with `Array[OverrideDict]` (also the test entry point).
func show_diff(officer: int, values: Array) -> void:
	if not _groups.has(officer):
		return
	_diff_open[officer] = true
	_diff_values[officer] = values
	var g := _groups[officer]
	(g["overrides"] as Button).accessibility_description = tr(&"hud_board.overrides.open")
	(g["diff"] as Control).visible = true
	_fill_diff(officer, values)
	_relink()


func _fill_diff(officer: int, values: Array) -> void:
	var diff: VBoxContainer = _groups[officer]["diff"]
	# A reply rebuilds the list; keyboard focus inside it moves to the same place in the new one.
	var focused := -1
	var owner := get_viewport().gui_get_focus_owner() if is_inside_tree() else null
	if owner != null and diff.is_ancestor_of(owner):
		focused = diff.find_children("*", "Button", true, false).find(owner)
	Kit.clear(diff)
	if focused >= 0:
		_refocus_diff.call_deferred(officer, focused)
	var rows := Kit.dicts(values)
	if rows.is_empty():
		var none := Kit.label("hud_board.diff.none", &"Dense")
		diff.add_child(none)
		return
	for o: Dictionary in rows:
		var row := HBoxContainer.new()
		Ui.set_separation(row, 2)
		var text := Kit.vbox(0)
		text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		var label := Kit.label(_Text.resolve(o.get("label", {})), &"Body", true)
		text.add_child(label)
		var values_text := tr(&"hud_board.diff.values").format([Kit.get_str(o, "default_text"), Kit.get_str(o, "current_text")])
		text.add_child(Kit.label(values_text, &"Dense", true))
		row.add_child(text)
		var revert := Button.new()
		revert.icon = Ui.icon(&"arrow_revert")
		revert.text = tr(&"hud_board.diff.revert")
		revert.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		revert.custom_minimum_size = Vector2(0.0, ROLE_BUTTON_H)
		revert.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		revert.focus_mode = Control.FOCUS_ALL
		revert.tooltip_text = tr(&"hud_board.diff.revert_tip").format([Kit.get_str(o, "default_text")])
		revert.accessibility_name = tr(&"hud_board.diff.revert_access").format([label.text, Kit.get_str(o, "default_text")])
		revert.pressed.connect(_revert.bind(Kit.get_str(o, "id")))
		row.add_child(revert)
		diff.add_child(row)
	var all := Button.new()
	all.name = "RevertAll"
	all.text = tr(&"hud_board.diff.revert_all")
	all.icon = Ui.icon(&"arrow_revert")
	all.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	all.custom_minimum_size = Vector2(0.0, ROLE_BUTTON_H)
	all.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	all.focus_mode = Control.FOCUS_ALL
	all.tooltip_text = tr(&"hud_board.diff.revert_all_tip")
	all.pressed.connect(_revert_all.bind(officer))
	diff.add_child(all)


func _refocus_diff(officer: int, index: int) -> void:
	var buttons := (_groups[officer]["diff"] as Node).find_children("*", "Button", true, false)
	if not buttons.is_empty():
		(buttons[clampi(index, 0, buttons.size() - 1)] as Control).grab_focus()
	else:
		(_groups[officer]["overrides"] as Control).grab_focus()


func _revert(id: String) -> void:
	if not id.is_empty():
		Kit.call_on(&"Orders", &"revert", [id])


func _revert_all(officer: int) -> void:
	if Kit.call_on(&"Orders", &"revert_all", [officer]) == true:
		# Revert-all announces no single value, so the list is asked for again; its reply clears the
		# overridden marks everywhere.
		Kit.call_on(&"Orders", &"query_overrides", [officer])


# --- focus ---------------------------------------------------------------------------------------

## The board's focusable controls, top to bottom.
func focus_targets() -> Array:
	return _targets


func _relink() -> void:
	var list: Array[Control] = []
	for officer: int in ORDER:
		var g := _groups[officer]
		if not (g["box"] as Control).visible:
			continue
		for key: String in _role_order:
			var row: Dictionary = _role_rows[key]
			if _officer_of(row["role"]) == officer:
				list.append(row["button"])
		var chip: Button = g["emcon_chip"]
		if chip != null and chip.focus_mode != Control.FOCUS_NONE and chip.is_visible_in_tree():
			list.append(chip)
		var overrides: Button = g["overrides"]
		if overrides.visible:
			list.append(overrides)
		var diff: Control = g["diff"]
		if diff.visible:
			for b: Node in diff.find_children("*", "Button", true, false):
				list.append(b as Control)
	# Neighbours are rewritten only when the list changes, so the links the sortie HUD adds at the
	# board's edges hold between refreshes.
	var shown: Array[Control] = []
	for c: Control in list:
		if c.focus_mode != Control.FOCUS_NONE and c.visible and not (c is BaseButton and (c as BaseButton).disabled):
			shown.append(c)
	if shown == _targets:
		return
	_targets = Kit.chain_vertical(shown)
	focus_targets_changed.emit()


# --- text ----------------------------------------------------------------------------------------

func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"word") and value >= 0:
		return String(tx.call(&"word", kind, value))
	return ""
