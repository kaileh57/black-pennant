extends Control
## Replay browser: 20 most recent plus pinned.
## DESIGN.md L1047. Unit: ui-replays.
## Reads and calls: Session.replays, Session.replay_open, Session.replay_pin, Session.replay_delete.
##
## Pushed over the main menu (`Screens.push(&"replay_browser")`). The game keeps the 20 most recent
## replays plus any the player pins (L1047), so the table has two groups: Pinned first, then Recent,
## each newest first. One row per replay (recorded time, mode, operation or players, length, size)
## and a detail column for the selected one, with Watch as the one prominent action, then Pin or
## Unpin and Delete. A replay of another format version stays listed, greyed, with the sentence that
## says why it cannot play (L1046), and Watch is refused for it.
## States: loading (until `setup` reads the store), empty, error (the folder does not read),
## populated. Esc and Back close the browser. `show_rows(rows)` renders plain data in the shape
## `Session.replays` returns and is the test and capture entry point; `setup` reads the facade.

const Kit := preload("res://ui/screens/menu_kit.gd")

const RECENT_KEPT := 20
const COL_RECORDED := 0
const COL_MODE := 1
const COL_WHAT := 2
const COL_LENGTH := 3
const COL_SIZE := 4
const DETAIL_W := 520.0

## The replay last opened from here, so the playback controls can pin it (the session does not say).
static var opened_path := ""

var _back: Button
var _tree: Tree
var _state_text: Label
var _count_text: Label
var _detail: VBoxContainer
var _detail_title: Label
var _detail_facts: Label
var _detail_problem: Label
var _watch: Button
var _pin: Button
var _delete: Button

## Every row as the facade gave it, in display order (pinned first).
var _rows: Array[Dictionary] = []
var _state: StringName = &"loading"
var _theme_started := false
var _busy := false


var _body: BoxContainer
var _body_scroll: ScrollContainer
var _body_step := 0
## `[control, width side by side, height when stacked]`, see `_fit_columns`.
var _fit: Array[Array] = []

func _ready() -> void:
	accessibility_name = tr(&"replays.title")
	var backing := Panel.new()
	backing.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(backing)
	var margin := MarginContainer.new()
	margin.set_anchors_preset(Control.PRESET_FULL_RECT)
	Ui.set_margins(margin, 5)
	add_child(margin)
	var column := Kit.vbox(3)
	margin.add_child(column)

	var header := Kit.hbox(4)
	var title := Kit.label("replays.title", &"ScreenTitle")
	title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	header.add_child(title)
	_back = Kit.inline_button("replays.back")
	_back.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	_back.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_back.pressed.connect(_close)
	header.add_child(_back)
	column.add_child(header)
	column.add_child(HSeparator.new())

	var body := _responsive_body(column, 6)
	var list_col := Kit.vbox(2)
	list_col.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	body.add_child(list_col)
	_tree = Tree.new()
	_tree.columns = 5
	_tree.column_titles_visible = true
	_tree.hide_root = true
	_tree.select_mode = Tree.SELECT_ROW
	_tree.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_tree.focus_mode = Control.FOCUS_ALL
	_tree.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_tree.accessibility_name = tr(&"replays.list")
	for c: int in [COL_RECORDED, COL_MODE, COL_WHAT, COL_LENGTH, COL_SIZE]:
		_tree.set_column_expand(c, c == COL_WHAT)
		_tree.set_column_clip_content(c, c == COL_WHAT)
	_tree.set_column_custom_minimum_width(COL_RECORDED, 250)
	_tree.set_column_custom_minimum_width(COL_MODE, 150)
	_tree.set_column_custom_minimum_width(COL_WHAT, 220)
	_tree.set_column_custom_minimum_width(COL_LENGTH, 110)
	_tree.set_column_custom_minimum_width(COL_SIZE, 120)
	_tree.item_selected.connect(_show_selected)
	_tree.item_activated.connect(_watch_selected)
	list_col.add_child(_tree)
	_state_text = Kit.label("", &"Body", true)
	list_col.add_child(_state_text)
	_count_text = Kit.label("", &"Dense", true)
	list_col.add_child(_count_text)

	_detail = Kit.vbox(3)
	_detail.custom_minimum_size = Vector2(DETAIL_W, 0.0)
	body.add_child(_detail)
	_detail_title = Kit.label("", &"PanelTitle", true)
	_detail.add_child(_detail_title)
	_detail_facts = Kit.label("", &"Body", true)
	_detail.add_child(_detail_facts)
	_detail_problem = Kit.label("", &"Body", true)
	_detail_problem.visible = false
	_detail.add_child(_detail_problem)
	_watch = Kit.button("replays.watch", true)
	_watch.pressed.connect(_watch_selected)
	_detail.add_child(_watch)
	var row := Kit.hbox(3)
	_pin = Kit.button("replays.pin")
	_pin.pressed.connect(_toggle_pin)
	row.add_child(_pin)
	_delete = Kit.button("replays.delete")
	_delete.pressed.connect(_ask_delete)
	row.add_child(_delete)
	_detail.add_child(row)
	_fit = [[list_col, 560.0, 0.0], [_tree, 0.0, 420.0], [_detail, DETAIL_W, 0.0]]
	_apply_columns()
	_show_state(&"loading")


func setup(_args: Dictionary) -> void:
	_theme_started = true
	Kit.start_menu_theme()
	refresh()


func _exit_tree() -> void:
	Kit.leave_menu_theme(_theme_started)


func default_focus() -> Control:
	return _tree if _state == &"populated" else _back


func back_requested() -> void:
	_close()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_apply_columns()
		if _state == &"populated":
			show_rows(_rows.duplicate())


## Read the replay store again and redraw.
func refresh() -> void:
	var raw: Variant = Kit.call_on(&"Session", &"replays")
	if not raw is Array:
		_rows = []
		_tree.clear()
		_show_state(&"error")
		return
	show_rows(Kit.dicts(raw))


## Render plain replay rows (the shape `Session.replays` returns).
func show_rows(rows: Array[Dictionary]) -> void:
	var keep := _selected_path()
	var pinned: Array[Dictionary] = []
	var recent: Array[Dictionary] = []
	for r: Dictionary in rows:
		if Kit.get_str(r, "path").is_empty():
			continue
		(pinned if Kit.get_bool(r, "pinned") else recent).append(r)
	_rows = []
	_rows.append_array(pinned)
	_rows.append_array(recent)
	_tree.clear()
	var root := _tree.create_item()
	var select_item: TreeItem = null
	for group: Array in [[&"replays.group.pinned", pinned], [&"replays.group.recent", recent]]:
		var members: Array = group[1]
		if members.is_empty():
			continue
		var head := _tree.create_item(root)
		head.set_text(0, tr(group[0]))
		for c in _tree.columns:
			head.set_selectable(c, false)
		for r: Dictionary in members:
			var item := _tree.create_item(head)
			item.set_text(COL_RECORDED, recorded_text(r))
			item.set_text(COL_MODE, mode_text(Kit.get_int(r, "mode", -1)))
			item.set_text(COL_WHAT, what_text(r))
			item.set_text(COL_LENGTH, clock_text(Kit.get_float(r, "duration_s")))
			item.set_text(COL_SIZE, size_text(Kit.get_float(r, "size_mb")))
			item.set_text_alignment(COL_LENGTH, HORIZONTAL_ALIGNMENT_RIGHT)
			item.set_text_alignment(COL_SIZE, HORIZONTAL_ALIGNMENT_RIGHT)
			if Kit.get_bool(r, "pinned"):
				item.set_icon(COL_RECORDED, Ui.icon(&"pin"))
			if not Kit.get_bool(r, "playable", true):
				for c in _tree.columns:
					item.set_custom_color(c, Ui.colour(&"text.inactive"))
			var path := Kit.get_str(r, "path")
			item.set_metadata(0, path)
			if path == keep:
				select_item = item
	_show_state(&"empty" if _rows.is_empty() else &"populated")
	if select_item == null:
		select_item = _first_row_item()
	if select_item != null:
		select_item.select(0)
		_tree.scroll_to_item(select_item)
	_count_text.text = count_text(pinned.size(), recent.size())
	_show_selected()


func _apply_columns() -> void:
	var titles: Array[StringName] = [&"replays.col.recorded", &"replays.col.mode", &"replays.col.what", &"replays.col.length", &"replays.col.size"]
	for c in titles.size():
		_tree.set_column_title(c, tr(titles[c]))
		_tree.set_column_title_alignment(c, HORIZONTAL_ALIGNMENT_LEFT)


func _show_state(state: StringName) -> void:
	_state = state
	match state:
		&"loading":
			_state_text.text = tr(&"replays.loading")
		&"empty":
			_state_text.text = tr(&"replays.empty")
		&"error":
			_state_text.text = tr(&"replays.error")
		_:
			_state_text.text = ""
	_state_text.visible = not _state_text.text.is_empty()
	_tree.visible = state == &"populated"
	_detail.visible = state == &"populated"
	_count_text.visible = state == &"populated"
	_link_focus()


func _first_row_item() -> TreeItem:
	var root := _tree.get_root()
	if root == null:
		return null
	for head: TreeItem in root.get_children():
		if head.get_child_count() > 0:
			return head.get_child(0)
	return null


func _selected_path() -> String:
	var item := _tree.get_selected()
	if item == null:
		return ""
	var meta: Variant = item.get_metadata(0)
	return String(meta) if meta is String else ""


func _selected_row() -> Dictionary:
	var path := _selected_path()
	for r: Dictionary in _rows:
		if Kit.get_str(r, "path") == path:
			return r
	return {}


func _show_selected() -> void:
	var r := _selected_row()
	if r.is_empty():
		_detail_title.text = tr(&"replays.none_selected")
		_detail_facts.text = ""
		_detail_problem.visible = false
		_watch.disabled = true
		_pin.disabled = true
		_delete.disabled = true
		_link_focus()
		return
	_detail_title.text = title_text(r)
	_detail_facts.text = facts_text(r)
	var playable := Kit.get_bool(r, "playable", true)
	var problem := Kit._Text.resolve(r.get("problem", {}))
	_detail_problem.text = problem if not problem.is_empty() else tr(&"replays.unplayable")
	_detail_problem.visible = not playable
	_watch.disabled = not playable or _busy
	_pin.disabled = _busy
	_delete.disabled = _busy
	_pin.text = "replays.unpin" if Kit.get_bool(r, "pinned") else "replays.pin"
	_link_focus()


# --- text ----------------------------------------------------------------------------------------

static func mode_text(mode: int) -> String:
	if mode >= 0:
		var w: Variant = Kit.call_on(&"Tx", &"word", [&"mode", mode])
		if w is String and not (w as String).is_empty():
			return w
	return Kit.tr_key(&"replays.mode_unknown")


static func clock_text(seconds: float) -> String:
	var s: Variant = Kit.call_on(&"Tx", &"clock", [maxf(seconds, 0.0)])
	if s is String and not (s as String).is_empty():
		return s
	var whole := maxi(int(seconds), 0)
	return "%d:%02d" % [whole / 60, whole % 60]


static func duration_text(seconds: float) -> String:
	var s: Variant = Kit.call_on(&"Tx", &"duration", [maxf(seconds, 0.0)])
	if s is String and not (s as String).is_empty():
		return s
	return clock_text(seconds)


static func size_text(mb: float) -> String:
	return Kit.tr_key(&"replays.size_mb").format([Kit.number(maxf(mb, 0.0), &"none", 1)])


static func recorded_text(r: Dictionary) -> String:
	var t := Kit.local_time(Kit.get_int(r, "recorded_unix"))
	return t if not t.is_empty() else Kit.tr_key(&"replays.recorded_unknown")


## The operation's name when the replay is of one, else the players.
static func what_text(r: Dictionary) -> String:
	var op := Kit.get_str(r, "operation")
	if not op.is_empty():
		var key := StringName(op + ".name")
		var name_text := Kit.tr_key(key)
		if name_text != String(key):
			return name_text
	return players_text(r)


static func players_text(r: Dictionary) -> String:
	var names := PackedStringArray()
	var raw: Variant = r.get("players", PackedStringArray())
	var list: Array = Array(raw) if raw is PackedStringArray else Kit.as_array(raw)
	for n: Variant in list:
		var s := String(n).strip_edges() if (n is String or n is StringName) else ""
		if not s.is_empty():
			names.append(s)
	if names.is_empty():
		return Kit.tr_key(&"replays.players_none")
	return ", ".join(names)


static func title_text(r: Dictionary) -> String:
	return Kit.tr_key(&"replays.detail_title").format([mode_text(Kit.get_int(r, "mode", -1)), what_text(r)])


static func facts_text(r: Dictionary) -> String:
	var lines := PackedStringArray()
	lines.append(Kit.tr_key(&"replays.fact.recorded").format([recorded_text(r)]))
	lines.append(Kit.tr_key(&"replays.fact.length").format([duration_text(Kit.get_float(r, "duration_s"))]))
	lines.append(Kit.tr_key(&"replays.fact.players").format([players_text(r)]))
	lines.append(Kit.tr_key(&"replays.fact.size").format([size_text(Kit.get_float(r, "size_mb"))]))
	lines.append(Kit.tr_key(&"replays.fact.pinned") if Kit.get_bool(r, "pinned") else Kit.tr_key(&"replays.fact.recent"))
	return "\n".join(lines)


static func count_text(pinned: int, recent: int) -> String:
	return Kit.tr_key(&"replays.count").format([recent, RECENT_KEPT, pinned])


# --- actions -------------------------------------------------------------------------------------

func _watch_selected() -> void:
	var r := _selected_row()
	if r.is_empty() or _busy or not Kit.get_bool(r, "playable", true):
		return
	_busy = true
	_show_selected()
	# The session's loading state moves the screens to the replay root (which clears this browser);
	# a refusal arrives through Session.failed, which Screens shows, and the browser stays open.
	opened_path = Kit.get_str(r, "path")
	var ok: Variant = Kit.call_on(&"Session", &"replay_open", [opened_path])
	_busy = false
	if not is_inside_tree():
		return
	_show_selected()
	if ok != true:
		_state_text.text = tr(&"replays.open_failed")
		_state_text.visible = true


func _toggle_pin() -> void:
	var r := _selected_row()
	if r.is_empty():
		return
	var ok: Variant = Kit.call_on(&"Session", &"replay_pin", [Kit.get_str(r, "path"), not Kit.get_bool(r, "pinned")])
	if ok != true:
		Screens.open_message(&"replays.pin_failed_title", &"replays.pin_failed_body")
		return
	refresh()
	Ui.focus(_pin)


func _ask_delete() -> void:
	var r := _selected_row()
	if r.is_empty():
		return
	var path := Kit.get_str(r, "path")
	var dialog := Screens.open_dialog(&"replays.delete_title", Kit.tr_key(&"replays.delete_body").format([title_text(r)]),
		[{"id": &"keep", "label": &"replays.delete_keep"}, {"id": &"delete", "label": &"replays.delete_confirm"}], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", func(id: StringName) -> void: _on_delete_choice(id, path))


func _on_delete_choice(id: StringName, path: String) -> void:
	if id != &"delete" or not is_inside_tree():
		return
	var ok: Variant = Kit.call_on(&"Session", &"replay_delete", [path])
	if ok != true:
		Screens.open_message(&"replays.delete_failed_title", &"replays.delete_failed_body")
	refresh()


func _close() -> void:
	if Screens.top() == self:
		Screens.pop()


func _link_focus() -> void:
	if _tree == null:
		return
	var list_on := _tree.visible
	var detail := Kit.chain_vertical([_watch, _pin])
	Kit.chain_horizontal([_pin, _delete])
	_delete.focus_neighbor_top = _delete.get_path_to(_watch if not _watch.disabled else _delete)
	_delete.focus_neighbor_bottom = _delete.get_path_to(_delete)
	_tree.focus_neighbor_top = _tree.get_path_to(_back)
	_tree.focus_neighbor_left = _tree.get_path_to(_tree)
	var first: Control = detail[0] if not detail.is_empty() else _tree
	_tree.focus_neighbor_right = _tree.get_path_to(first)
	_tree.focus_next = _tree.get_path_to(first)
	for c: Control in detail:
		c.focus_neighbor_left = c.get_path_to(_tree)
	if not detail.is_empty():
		detail[0].focus_neighbor_top = detail[0].get_path_to(_back)
	_back.focus_neighbor_bottom = _back.get_path_to(_tree if list_on else _back)
	_back.focus_neighbor_left = _back.get_path_to(_tree if list_on else _back)
	_back.focus_neighbor_right = _back.get_path_to(_back)


# --- layout that never clips (L1714) ------------------------------------------------------------

func _responsive_body(parent: Control, step: int) -> BoxContainer:
	_body_scroll = ScrollContainer.new()
	_body_scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_body_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	_body_scroll.follow_focus = true
	parent.add_child(_body_scroll)
	_body = BoxContainer.new()
	_body.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_body.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_body_step = step
	Ui.set_separation(_body, step)
	_body_scroll.add_child(_body)
	_body_scroll.resized.connect(_fit_columns)
	return _body


## Columns side by side while they fit; one scrolling column when they do not, so a UI scale up to
## 200% never clips (L1714). `_fit` rows: [control, width side by side, height when stacked].
func _fit_columns() -> void:
	if _body_scroll == null:
		return
	var need := 0.0
	var count := 0
	for f: Array in _fit:
		if float(f[1]) > 0.0:
			need += float(f[1])
			count += 1
	need += Ui.spacing(_body_step) * maxi(count - 1, 0)
	var stack := _body_scroll.size.x + 1.0 < need
	_body.vertical = stack
	for f: Array in _fit:
		var c: Control = f[0]
		c.custom_minimum_size = Vector2(0.0 if stack else float(f[1]), float(f[2]) if stack else 0.0)
