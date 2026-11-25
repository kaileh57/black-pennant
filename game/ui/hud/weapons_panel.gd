extends PanelContainer
## Mounts by group: engaging or holding with the reason, rounds, solution.
## DESIGN.md L407-408, L353, L518. Unit: ui-hud-weapons.
## Reads and calls: Hud.mounts, Hud.weapon_groups, Hud.mounts_changed, Orders.role_key, Orders.launch, Orders.fire_decoy.
##
## One block per weapons group, in takeover-key order, then the decoy launchers (mounts in no group):
## - Group line: the group's name, what it is doing (Engaging, Holding, Idle), who holds it
##   (Officer, or Player with the hand glyph, rule 9), its key, and a Take or Hand back button
##   (`Orders.take_control`; the key itself goes through the HUD shell's `Orders.role_key`).
## - Per mount, line one: its name, the track it is on, and its rounds ("24 of 120 rounds") or
##   cells and reload. Line two: the fire or hold reason whenever the mount is on a track (L353:
##   automation never holds fire without a visible reason); an idle mount with no track leaves it to
##   the tooltip and the Gunnery officer's line on the Bridge Board. Both lines keep to one line in the docked panel and end in an
##   ellipsis when the dock is narrow; the mount's tooltip reads them in full with its reach (the
##   effective range it engages at, L407). Line three only when it applies: local control ("own
##   sensor, 3x error", L394), a mount held by hand, and, with Solutions on, the reach and the
##   fire-control solution: flight time, impact error and iterations, or "unconverged" (L518; the
##   expert column of L408).
## - A launcher (missile or torpedo) has a Launch button that fires at the designated track, else
##   the selected one; with no track it is disabled and says why.
## - A decoy launcher has a Launch decoys button; C fires every one (the HUD shell's key).
## Rows update in place at the 10 Hz mount rate; the blocks are rebuilt only when the set of groups
## or mounts changes, so a click or focus is never lost. States: outside a sortie (hidden), waiting
## for the first picture, no weapons fitted, populated. `show_weapons(groups, mounts, statics,
## target)` renders plain data (tests and captures); `refresh()` pulls it.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const HintScene := preload("res://ui/widgets/shortcut_hint.tscn")

## The list scrolls past this share of the window height.
const MAX_HEIGHT_SHARE := 0.26
const GLYPH_PX := 24.0
## Narrower than this, group rows drop the holder word.
const HOLDER_W := 330.0
const STATUS_KEYS: Array[StringName] = [&"hud_weapons.status.idle", &"hud_weapons.status.engaging", &"hud_weapons.status.holding"]

## Solutions on or off, kept for the session (the expert view, L408).
static var solutions_on := false

@onready var _margin: MarginContainer = $Margin
@onready var _column: VBoxContainer = $Margin/Column
@onready var _header: HFlowContainer = $Margin/Column/Header
@onready var _solutions: Button = $Margin/Column/Header/Solutions
@onready var _scroll: ScrollContainer = $Margin/Column/Scroll
@onready var _groups_box: VBoxContainer = $Margin/Column/Scroll/Groups
@onready var _empty: Label = $Margin/Column/Empty

var _groups: Array[Dictionary] = []
var _mounts: Array[Dictionary] = []
var _statics: Dictionary[int, Dictionary] = {}
var _target := Bp.NONE
## Layout key (groups and mounts in order) of the rows built now.
var _layout_key: Array = []
## Group number to { "status", "holder", "hand", "take", "hint" }.
var _group_rows: Dictionary[int, Dictionary] = {}
## Mount slot to { "name", "rounds", "reason", "extra", "launch", "decoy" }.
var _mount_rows: Dictionary[int, Dictionary] = {}
var _group_held: Dictionary[int, bool] = {}
var _headless := false
## Height the HUD shell gives the panel (`set_max_height`); -1 falls back to the screen share.
var _max_height := -1.0
## A narrow dock: Take and Hand back shrink to the hand glyph (their words stay in the tooltip).
var _narrow := false


func _ready() -> void:
	_headless = DisplayServer.get_name() == "headless"
	# Docked at the right edge: the panel's own 16 px margin is the dock's inset (one inset for
	# every cluster in a dock, so their text lines up).
	Ui.set_margins(_margin, 1)
	for side: StringName in [&"margin_left", &"margin_right", &"margin_top", &"margin_bottom"]:
		_margin.add_theme_constant_override(side, 0)
	Ui.set_separation(_column, 1)
	_header.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	_header.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	Ui.set_separation(_groups_box, 2)
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_solutions.focus_mode = Control.FOCUS_ALL
	_solutions.tooltip_text = tr(&"hud_weapons.solutions_tooltip")
	_solutions.set_pressed_no_signal(solutions_on)
	_solutions.toggled.connect(_on_solutions)
	_groups_box.minimum_size_changed.connect(_fit_scroll)
	resized.connect(_fit_scroll)
	get_viewport().size_changed.connect(_fit_scroll)
	accessibility_name = tr(&"hud_weapons.title")
	_on(&"Hud", &"mounts_changed", refresh)
	_on(&"Hud", &"ship_static_changed", refresh)
	_on(&"Hud", &"board_changed", refresh)
	_on(&"Hud", &"selection_changed", func(_t: int) -> void: refresh())
	_on(&"Hud", &"motion_changed", _on_motion)
	_on(&"Session", &"state_changed", func(_s: int) -> void: refresh())
	if Ui.has_signal(&"changed"):
		Ui.changed.connect(_on_ui_changed)
	_apply_backing()
	refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_layout_key = []
		show_weapons(_groups, _mounts, _statics.values(), _target)


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _on_ui_changed(what: StringName) -> void:
	if what == &"backing" or what == &"transparency":
		_apply_backing()
	elif what == &"palette":
		_layout_key = []
		show_weapons(_groups, _mounts, _statics.values(), _target)
	elif what == &"scale":
		_fit_scroll.call_deferred()


func _apply_backing() -> void:
	self_modulate.a = 1.0 if Ui.reduce_transparency else clampf(Ui.hud_backing_opacity, 0.0, 1.0)


## The panel takes the height its groups need, up to `height` (the HUD shell shares the dock's
## height out); past it the groups scroll (floor 2).
func set_max_height(height: float) -> void:
	if is_equal_approx(height, _max_height):
		return
	_max_height = height
	_fit_scroll()


func _fit_scroll() -> void:
	if not is_inside_tree():
		return
	# In a narrow dock (a large UI scale) the holder word gives way so each group keeps one row; the
	# Take or Hand back button and the hand glyph still say who has the group (floor 2).
	var wide := size.x <= 1.0 or size.x >= HOLDER_W
	var changed := wide == _narrow
	_narrow = not wide
	for n: int in _group_rows:
		(_group_rows[n]["holder"] as Control).visible = wide
	if changed:
		for g: Dictionary in _groups:
			_update_group(g)
	var cap := get_viewport().get_visible_rect().size.y * MAX_HEIGHT_SHARE
	if _max_height >= 0.0:
		# The cap is for the whole panel: take off the title row, the margins and the panel's own.
		var chrome := get_combined_minimum_size().y - _scroll.get_combined_minimum_size().y
		cap = maxf(_max_height - chrome, 0.0)
	var want := ceilf(minf(_groups_box.get_combined_minimum_size().y, cap))
	if not is_equal_approx(_scroll.custom_minimum_size.y, want):
		_scroll.custom_minimum_size = Vector2(0.0, want)


## Controls that take focus, top to bottom.
func focus_targets() -> Array:
	var out: Array = [_solutions]
	for g: Dictionary in _groups:
		var n := Kit.get_int(g, "group", -1)
		if _group_rows.has(n):
			out.append(_group_rows[n]["take"])
		for m: Dictionary in _mounts:
			var slot := Kit.get_int(m, "slot", -1)
			if Kit.get_int(m, "group", -1) == n and _mount_rows.has(slot) and _mount_rows[slot]["launch"] != null:
				out.append(_mount_rows[slot]["launch"])
	for m: Dictionary in _mounts:
		var slot := Kit.get_int(m, "slot", -1)
		if Kit.get_int(m, "group", -1) < 0 and _mount_rows.has(slot) and _mount_rows[slot]["decoy"] != null:
			out.append(_mount_rows[slot]["decoy"])
	return out


# --- data ----------------------------------------------------------------------------------------

func refresh() -> void:
	if not is_node_ready():
		return
	var state: Variant = Kit.call_on(&"Session", &"state")
	var live := state is int and (int(state) == Bp.SESSION_IN_SORTIE or int(state) == Bp.SESSION_REPLAY)
	if not live:
		visible = false
		return
	var target := Kit.get_int(Kit.as_dict(Kit.call_on(&"Hud", &"motion")), "designated", Bp.NONE)
	if target <= 0:
		var sel: Variant = Kit.call_on(&"Hud", &"selected_track")
		target = int(sel) if sel is int else Bp.NONE
	show_weapons(Kit.dicts(Kit.call_on(&"Hud", &"weapon_groups")), Kit.dicts(Kit.call_on(&"Hud", &"mounts")), Kit.dicts(Kit.call_on(&"Hud", &"parts_static")), target)


func _on_motion() -> void:
	# The designated track arrives with the motion; the launch buttons follow it.
	var t := Kit.get_int(Kit.as_dict(Kit.call_on(&"Hud", &"motion")), "designated", Bp.NONE)
	if t > 0 and t != _target:
		refresh()


## Render `weapon_groups()`, `MountDict`s, `PartStaticDict`s and the launch target track (or
## `Bp.NONE`). Also the test entry point.
func show_weapons(groups: Array[Dictionary], mounts: Array[Dictionary], statics: Array, target: int) -> void:
	_groups = groups
	_mounts = mounts
	_target = target
	_statics.clear()
	for s: Variant in statics:
		if s is Dictionary:
			_statics[Kit.get_int(s, "slot", -1)] = s
	if not is_node_ready():
		return
	visible = true
	var key: Array = []
	for g: Dictionary in groups:
		key.append(["g", Kit.get_int(g, "group", -1)])
	for m: Dictionary in mounts:
		key.append(["m", Kit.get_int(m, "slot", -1), Kit.get_int(m, "group", -1)])
	if key != _layout_key:
		_build(groups, mounts)
		_layout_key = key
	var has_hull: bool = Kit.call_on(&"Hud", &"has_ship") == true or not mounts.is_empty()
	_empty.visible = mounts.is_empty()
	_empty.text = tr(&"hud_weapons.empty" if has_hull else &"hud_weapons.waiting")
	_scroll.visible = not mounts.is_empty()
	_solutions.visible = not mounts.is_empty()
	for g: Dictionary in groups:
		_update_group(g)
	for m: Dictionary in mounts:
		_update_mount(m)
	_fit_scroll.call_deferred()


func _build(groups: Array[Dictionary], mounts: Array[Dictionary]) -> void:
	Kit.clear(_groups_box)
	_group_rows.clear()
	_mount_rows.clear()
	var grouped: Dictionary[int, bool] = {}
	for g: Dictionary in groups:
		var n := Kit.get_int(g, "group", -1)
		grouped[n] = true
		var block := Kit.vbox(0)
		block.mouse_filter = Control.MOUSE_FILTER_IGNORE
		block.add_child(_group_header(g))
		for m: Dictionary in mounts:
			if Kit.get_int(m, "group", -1) == n:
				block.add_child(_mount_row(m, false))
		_groups_box.add_child(block)
	# Mounts outside any listed group: decoy launchers (group < 0), and any group the list lacks.
	var loose: Array[Dictionary] = []
	for m: Dictionary in mounts:
		if not grouped.has(Kit.get_int(m, "group", -1)):
			loose.append(m)
	if not loose.is_empty():
		var block := Kit.vbox(0)
		block.mouse_filter = Control.MOUSE_FILTER_IGNORE
		var head := Kit.hbox(1)
		head.mouse_filter = Control.MOUSE_FILTER_IGNORE
		var title := Kit.label(tr(&"hud_weapons.decoys.title"), &"Body", true)
		title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		head.add_child(title)
		if not _headless:
			var hint: Control = HintScene.instantiate()
			hint.set(&"action", &"decoys")
			hint.set(&"label", &"hud_weapons.decoys.all")
			hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			head.add_child(hint)
		block.add_child(head)
		for m: Dictionary in loose:
			block.add_child(_mount_row(m, Kit.get_int(m, "group", -1) < 0))
		_groups_box.add_child(block)
	Kit.chain_vertical(focus_targets())


func _group_header(g: Dictionary) -> Control:
	var n := Kit.get_int(g, "group", -1)
	# One row: the name gives way (ellipsis) before the holder, key and button do.
	var row := Kit.hbox(1)
	row.mouse_filter = Control.MOUSE_FILTER_IGNORE
	var name := Kit.label(tr(&"hud_weapons.group.short").format([str(Kit.get_int(g, "key", n + 1))]), &"Body", true)
	name.tooltip_text = _Text.resolve(g.get("name", {}))
	name.mouse_filter = Control.MOUSE_FILTER_PASS
	name.autowrap_mode = TextServer.AUTOWRAP_OFF
	name.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	name.custom_minimum_size = Vector2(48, 0)
	name.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.add_child(name)
	# "Group 1" is short and never trimmed; the space after it pushes the holder to the right.
	name.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	var hand := TextureRect.new()
	hand.texture = Ui.icon(&"hand")
	hand.self_modulate = Ui.colour(&"text.primary")
	hand.custom_minimum_size = Vector2(GLYPH_PX, GLYPH_PX)
	hand.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	hand.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	hand.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	hand.mouse_filter = Control.MOUSE_FILTER_IGNORE
	row.add_child(hand)
	var holder := Kit.label("", &"Body", true)
	holder.autowrap_mode = TextServer.AUTOWRAP_OFF
	holder.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.add_child(holder)
	var hint: Control = null
	var key := Kit.get_int(g, "key", n + 1)
	var action := StringName("take_group_%d" % key)
	if not _headless and InputMap.has_action(action):
		hint = HintScene.instantiate()
		hint.set(&"compact", true)
		hint.set(&"action", action)
		hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		row.add_child(hint)
	var take := Kit.inline_button("")
	take.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	take.custom_minimum_size = Vector2(0, 32)
	take.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	take.pressed.connect(_on_take.bind(n))
	row.add_child(take)
	_group_rows[n] = {"name": name, "holder": holder, "hand": hand, "take": take, "hint": hint}
	return row


## One mount's lines: name, track and rounds; then the reason.
func _mount_row(m: Dictionary, decoy: bool) -> Control:
	var slot := Kit.get_int(m, "slot", -1)
	var box := Kit.vbox(0)
	box.mouse_filter = Control.MOUSE_FILTER_IGNORE
	# One line: the name and track give way (ellipsis) before the rounds do.
	var top := Kit.hbox(1)
	top.mouse_filter = Control.MOUSE_FILTER_IGNORE
	var name := Kit.label("", &"Body", true)
	name.autowrap_mode = TextServer.AUTOWRAP_OFF
	name.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	name.mouse_filter = Control.MOUSE_FILTER_PASS
	name.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	name.custom_minimum_size = Vector2(48, 0)
	var rounds := Kit.label("", &"Dense", true)
	rounds.autowrap_mode = TextServer.AUTOWRAP_OFF
	rounds.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
	rounds.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	var reason := Kit.label("", &"Dense", true)
	reason.autowrap_mode = TextServer.AUTOWRAP_OFF
	reason.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	reason.mouse_filter = Control.MOUSE_FILTER_PASS
	reason.custom_minimum_size = Vector2(80, 0)
	top.add_child(name)
	top.add_child(rounds)
	box.add_child(top)
	box.add_child(reason)
	var extra := Kit.label("", &"Dense", true)
	extra.visible = false
	box.add_child(extra)
	var launch: Button = null
	var decoy_button: Button = null
	if decoy:
		decoy_button = Kit.inline_button("")
		decoy_button.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		decoy_button.custom_minimum_size = Vector2(0, 36)
		decoy_button.pressed.connect(_on_decoy.bind(slot))
		box.add_child(decoy_button)
	elif is_launcher(_statics.get(slot, {})):
		launch = Kit.inline_button("")
		launch.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		launch.custom_minimum_size = Vector2(0, 36)
		launch.pressed.connect(_on_launch.bind(slot))
		box.add_child(launch)
	_mount_rows[slot] = {"name": name, "rounds": rounds, "reason": reason, "extra": extra, "launch": launch, "decoy": decoy_button}
	return box


func _update_group(g: Dictionary) -> void:
	var n := Kit.get_int(g, "group", -1)
	if not _group_rows.has(n):
		return
	var row: Dictionary = _group_rows[n]
	var player := Kit.get_int(g, "holder", Bp.HOLDER_OFFICER) == Bp.HOLDER_PLAYER
	_group_held[n] = player
	# What the group does is in its mounts' reasons; the header names it for the screen reader.
	var status := clampi(Kit.get_int(g, "status", 0), 0, STATUS_KEYS.size() - 1)
	(row["name"] as Label).accessibility_description = tr(STATUS_KEYS[status])
	(row["holder"] as Label).text = tr(&"hud_weapons.holder.player" if player else &"hud_weapons.holder.officer")
	(row["hand"] as TextureRect).visible = player
	var take: Button = row["take"]
	take.text = "" if _narrow else tr(&"hud_weapons.group.release" if player else &"hud_weapons.group.take")
	take.icon = Ui.icon(&"hand") if _narrow else null
	take.toggle_mode = _narrow
	take.set_pressed_no_signal(_narrow and player)
	take.tooltip_text = tr(&"hud_weapons.group.release_tooltip" if player else &"hud_weapons.group.take_tooltip")
	take.accessibility_name = "%s, %s" % [_Text.resolve(g.get("name", {})), tr(&"hud_weapons.group.release" if player else &"hud_weapons.group.take")]


func _update_mount(m: Dictionary) -> void:
	var slot := Kit.get_int(m, "slot", -1)
	if not _mount_rows.has(slot):
		return
	var row: Dictionary = _mount_rows[slot]
	var name := _Text.resolve(m.get("name", {}))
	var track := Kit.get_int(m, "track", Bp.NONE)
	var status := Kit.get_int(m, "status", Bp.ENGAGE_IDLE)
	var name_text := name
	if track > 0 and status != Bp.ENGAGE_IDLE:
		name_text = tr(&"hud_weapons.mount.on_track").format([name, _track(track)])
	(row["name"] as Label).text = name_text
	(row["rounds"] as Label).text = rounds_text(m)
	var reason := _Text.resolve(m.get("reason", {}))
	(row["reason"] as Label).text = reason
	# An idle mount with no track has nothing to explain beyond its name line (no "on 0002"); its
	# reason reads in the tooltip, and the Gunnery officer's line on the board says why.
	(row["reason"] as Label).visible = not reason.is_empty() and not (status == Bp.ENGAGE_IDLE and track <= 0)
	var extra := extra_text(m, solutions_on)
	var extra_label: Label = row["extra"]
	extra_label.text = extra
	extra_label.visible = not extra.is_empty()
	# The tooltip reads the trimmed lines in full, with the reach.
	var tip := PackedStringArray([name_text, rounds_text(m), reason])
	var reach := reach_text(m)
	if not reach.is_empty():
		tip.append(reach)
	(row["name"] as Label).tooltip_text = "\n".join(tip)
	(row["reason"] as Label).tooltip_text = (row["name"] as Label).tooltip_text
	var ready := Kit.get_int(m, "ready", 0)
	var launch: Button = row["launch"]
	if launch != null:
		if _target > 0:
			launch.text = tr(&"hud_weapons.launch.at").format([_track(_target)])
			launch.disabled = ready <= 0
			launch.tooltip_text = tr(&"hud_weapons.launch.tooltip" if ready > 0 else &"hud_weapons.launch.empty")
		else:
			launch.text = tr(&"hud_weapons.launch.plain")
			launch.disabled = true
			launch.tooltip_text = tr(&"hud_weapons.launch.no_track")
		launch.accessibility_name = "%s, %s" % [name, launch.text]
	var decoy: Button = row["decoy"]
	if decoy != null:
		decoy.text = tr(&"hud_weapons.decoys.launch")
		decoy.disabled = ready <= 0
		decoy.tooltip_text = tr(&"hud_weapons.decoys.tooltip" if ready > 0 else &"hud_weapons.decoys.empty")
		decoy.accessibility_name = "%s, %s" % [name, decoy.text]


static func _track(track: int) -> String:
	var v: Variant = Kit.call_on(&"Tx", &"track_label", [track])
	if v is String and not (v as String).is_empty():
		return v
	return "%04d" % track


# --- text (static so tests share them) -----------------------------------------------------------

## True for a missile or torpedo launcher (`PartStaticDict`).
static func is_launcher(part: Dictionary) -> bool:
	var c := Kit.get_int(part, "category", -1)
	return c == Bp.CATEGORY_MISSILE or c == Bp.CATEGORY_UNDERWATER


## "24 of 120"; "2 ready, reload 14 s"; "Empty".
static func rounds_text(m: Dictionary) -> String:
	var ready := maxi(Kit.get_int(m, "ready", 0), 0)
	var magazine := maxi(Kit.get_int(m, "magazine", 0), 0)
	var reload_s := Kit.get_float(m, "reload_s", 0.0)
	var text := ""
	if reload_s > 0.05 and is_finite(reload_s):
		text = TranslationServer.translate(&"hud_weapons.rounds.reload").format([str(ready), Kit.number(reload_s, &"s", 0)])
	elif magazine > ready:
		text = TranslationServer.translate(&"hud_weapons.rounds.of").format([str(ready), str(magazine)])
	elif ready > 0:
		text = TranslationServer.translate(&"hud_weapons.rounds.ready").format([str(ready)])
	else:
		text = TranslationServer.translate(&"hud_weapons.rounds.empty")
	return text


## "Reach 8.0 km."; "" when the mount reports none.
static func reach_text(m: Dictionary) -> String:
	var reach := Kit.get_float(m, "effective_range_m", 0.0)
	if not (reach > 0.0 and is_finite(reach)):
		return ""
	var v: Variant = Kit.call_on(&"Tx", &"range", [reach])
	return TranslationServer.translate(&"hud_weapons.reach").format([v if v is String and not (v as String).is_empty() else Kit.number(reach / 1000.0, &"km", 1)])


## Line three: local control (L394), a mount held by hand and, with solutions on, the reach and the
## solution (L518). "" when none applies.
static func extra_text(m: Dictionary, solutions: bool) -> String:
	var parts: PackedStringArray = []
	if solutions:
		var reach := reach_text(m)
		if not reach.is_empty():
			parts.append(reach)
	if Kit.get_bool(m, "local_control"):
		parts.append(TranslationServer.translate(&"hud_weapons.local_control"))
	if Kit.get_int(m, "holder", Bp.HOLDER_OFFICER) == Bp.HOLDER_PLAYER:
		parts.append(TranslationServer.translate(&"hud_weapons.mount.by_hand"))
	if solutions:
		var s: Variant = m.get("solution")
		if s is Dictionary and not (s as Dictionary).is_empty():
			parts.append(solution_text(s))
		elif Kit.get_int(m, "status", Bp.ENGAGE_IDLE) != Bp.ENGAGE_IDLE:
			parts.append(TranslationServer.translate(&"hud_weapons.solution.none"))
	return " ".join(parts)


## "Solution: 12.3 s flight, 0.4 m error, 3 iterations" or "Solution unconverged after 6
## iterations, 1.2 m error".
static func solution_text(s: Dictionary) -> String:
	var iterations := str(maxi(Kit.get_int(s, "iterations", 0), 0))
	var error := Kit.number(maxf(Kit.get_float(s, "error_m", 0.0), 0.0), &"m", 1)
	if not Kit.get_bool(s, "converged"):
		return TranslationServer.translate(&"hud_weapons.solution.unconverged").format([iterations, error])
	var flight := Kit.number(maxf(Kit.get_float(s, "time_of_flight_s", 0.0), 0.0), &"s", 1)
	return TranslationServer.translate(&"hud_weapons.solution.converged").format([flight, error, iterations])


# --- orders --------------------------------------------------------------------------------------

func _on_take(group: int) -> void:
	Kit.call_on(&"Orders", &"take_control", [Bp.ROLE_WEAPON_GROUP, group, not _group_held.get(group, false)])


func _on_launch(slot: int) -> void:
	if _target > 0:
		Kit.call_on(&"Orders", &"launch", [slot, _target, -1.0, -1.0])


func _on_decoy(slot: int) -> void:
	Kit.call_on(&"Orders", &"fire_decoy", [slot])


func _on_solutions(on: bool) -> void:
	solutions_on = on
	for m: Dictionary in _mounts:
		_update_mount(m)
