extends VBoxContainer
## The design to field, for skirmish and sandbox setup: stock starter hulls and the player's own
## blueprints, with the chosen one's facts and anything that keeps it out of the sortie.
## DESIGN.md L131 (player hulls 10 to 90 m), L142-151 (six starters in sandbox and skirmish),
## L959-961, L2067-2087 (blueprint folders). Unit: ui-skirmish. Built in code: `DesignPicker.new()`.
##
## `load_entries()` reads `Content.starters(Bp.NONE)` (or the `stock:starters` folder when that
## list is empty) and walks `Editor.browse("")` through the player's folders. Rows are grouped,
## stock first; the group rows are not selectable. The last design fielded this run is chosen again,
## otherwise the 22 m fast attack craft, otherwise the first row.
## For the chosen design the panel lists its length, beam, displacement, cost and parts, then what
## stops it: over the tonnage cap of the chosen act, longer than 90 m, a newer format, or a fatal
## issue from `Editor.file_issues`. `blocked()` says whether Start should be refused.
## Empty state: no design at all offers the dry dock.

const Kit := preload("res://ui/screens/menu_kit.gd")

const NONE := -1
const DOCK_SANDBOX := 0
const MAX_LENGTH_M := 90.0
const FIRST_STARTER_M := 22.0
## Folder walk limits, so a huge or looping folder tree cannot stall the menu.
const MAX_DEPTH := 6
const MAX_ENTRIES := 400

signal selected(path: String)

## The design fielded last in this run, chosen again next time.
static var last_path := ""

var list: ItemList
var facts: Label
var issues_label: Label
var edit_button: Button
var empty_label: Label

## `{ "path": String, "stock": bool, "info": BlueprintInfoDict }` per selectable row, in list order.
var _entries: Array[Dictionary] = []
## List row index to `_entries` index; group rows map to -1.
var _row_entry: Array[int] = []
var _cap_t := 0.0
## Text key for a design over the cap: `{0}` its displacement, `{1}` the cap. The act's cap by default;
## a lobby passes its own wording, because a PvP cap is the lobby's, not an act's.
var _cap_key: StringName = &"skirmish.design.over_cap"
var _issue_cache: Dictionary[String, Array] = {}
var _blocked := false


func _init() -> void:
	Ui.set_separation(self, 2)
	size_flags_horizontal = Control.SIZE_EXPAND_FILL
	add_child(Kit.label("skirmish.design.title", &"PanelTitle"))
	list = ItemList.new()
	list.custom_minimum_size = Vector2(0.0, 300.0)
	list.size_flags_vertical = Control.SIZE_EXPAND_FILL
	list.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	list.select_mode = ItemList.SELECT_SINGLE
	list.focus_mode = Control.FOCUS_ALL
	list.accessibility_name = Kit.tr_key(&"skirmish.design.title")
	list.item_selected.connect(_on_item_selected)
	add_child(list)
	empty_label = Kit.label("skirmish.design.empty", &"Body")
	empty_label.visible = false
	add_child(empty_label)
	facts = Kit.label("", &"Body", true)
	add_child(facts)
	issues_label = Kit.label("", &"Body", true)
	issues_label.visible = false
	add_child(issues_label)
	edit_button = Kit.inline_button("skirmish.design.edit")
	edit_button.pressed.connect(_open_dock)
	add_child(edit_button)


## Read every stock starter and player design from the facades.
func load_entries() -> void:
	var entries: Array[Dictionary] = []
	var starters := Kit.dicts(Kit.call_on(&"Content", &"starters", [NONE]))
	for info: Dictionary in starters:
		entries.append({"path": Kit.get_str(info, "path"), "stock": true, "info": info})
	if starters.is_empty():
		_walk("stock:starters", 0, entries, true)
	_walk("", 0, entries, false)
	set_entries(entries)


func _walk(folder: String, depth: int, out: Array[Dictionary], stock: bool) -> void:
	if depth > MAX_DEPTH or out.size() >= MAX_ENTRIES:
		return
	for e: Dictionary in Kit.dicts(Kit.call_on(&"Editor", &"browse", [folder])):
		if out.size() >= MAX_ENTRIES:
			return
		var path := Kit.get_str(e, "path")
		if path.is_empty():
			continue
		if Kit.get_bool(e, "is_folder"):
			# Kits are not ships; their folder is skipped.
			if not stock and depth == 0 and Kit.get_str(e, "name") == "kits":
				continue
			_walk(child_folder(folder, e), depth + 1, out, stock)
			continue
		var info := Kit.as_dict(e.get("info"))
		if info.is_empty():
			continue
		var kind := Kit.get_str(info, "kind", "ship")
		if kind != "ship":
			continue
		out.append({"path": path, "stock": stock or Kit.get_bool(info, "stock"), "info": info})


## The `Editor.browse` argument for a folder entry: relative to user://blueprints/ for the
## player's tree, `stock:<path>` for the stock tree, whichever form the entry's path is in.
static func child_folder(parent: String, entry: Dictionary) -> String:
	const ROOT := "user://blueprints/"
	var path := Kit.get_str(entry, "path")
	if path.begins_with(ROOT):
		return path.trim_prefix(ROOT).trim_suffix("/")
	if path.begins_with("stock:") or not path.contains("://"):
		return path.trim_suffix("/")
	return parent.path_join(Kit.get_str(entry, "name"))


## Fill the list from plain rows (also the test entry point).
func set_entries(entries: Array[Dictionary]) -> void:
	_entries = []
	for e: Dictionary in entries:
		if not Kit.get_str(e, "path").is_empty():
			_entries.append(e)
	list.clear()
	_row_entry.clear()
	var stock_rows: Array[int] = []
	var own_rows: Array[int] = []
	for i in _entries.size():
		(stock_rows if Kit.get_bool(_entries[i], "stock") else own_rows).append(i)
	for group: Array in [[&"skirmish.design.group.stock", stock_rows], [&"skirmish.design.group.own", own_rows]]:
		var rows: Array = group[1]
		if rows.is_empty():
			continue
		var header := list.add_item(Kit.tr_key(group[0]), null, false)
		list.set_item_disabled(header, true)
		list.set_item_tooltip_enabled(header, false)
		_row_entry.append(-1)
		for i: int in rows:
			var row := list.add_item(row_text(Kit.as_dict(_entries[i].get("info"))))
			list.set_item_tooltip_enabled(row, false)
			_row_entry.append(i)
	var empty := _entries.is_empty()
	empty_label.visible = empty
	list.visible = not empty
	edit_button.text = "skirmish.design.build" if empty else "skirmish.design.edit"
	_choose_default()


func _choose_default() -> void:
	var want := -1
	var best_gap := INF
	for i in _entries.size():
		if Kit.get_str(_entries[i], "path") == last_path and not last_path.is_empty():
			want = i
			break
	if want < 0:
		for i in _entries.size():
			if not Kit.get_bool(_entries[i], "stock"):
				continue
			var gap := absf(Kit.get_float(Kit.as_dict(_entries[i].get("info")), "length_m") - FIRST_STARTER_M)
			if gap < best_gap:
				best_gap = gap
				want = i
	if want < 0 and not _entries.is_empty():
		want = 0
	select_entry(want)


## Select the design at `_entries[index]` (or nothing for -1).
func select_entry(index: int) -> void:
	var row := _row_entry.find(index) if index >= 0 else -1
	if row >= 0:
		list.select(row)
		list.ensure_current_is_visible()
	else:
		list.deselect_all()
	_show_selected()


func _on_item_selected(row: int) -> void:
	if row < 0 or row >= _row_entry.size() or _row_entry[row] < 0:
		_show_selected()
		return
	_show_selected()
	selected.emit(selected_path())


func _selected_index() -> int:
	var rows := list.get_selected_items()
	if rows.is_empty():
		return -1
	var row := rows[0]
	return _row_entry[row] if row < _row_entry.size() else -1


func selected_path() -> String:
	var i := _selected_index()
	return Kit.get_str(_entries[i], "path") if i >= 0 else ""


func selected_info() -> Dictionary:
	var i := _selected_index()
	return Kit.as_dict(_entries[i].get("info")) if i >= 0 else {}


## The tonnage cap the design must meet (0 = none, as in the sandbox), and optionally the text key that
## says a design is over it.
func set_cap(cap_t: float, over_key: StringName = &"") -> void:
	_cap_t = maxf(cap_t, 0.0)
	if not over_key.is_empty():
		_cap_key = over_key
	_show_selected()


## True when the chosen design cannot be fielded (or nothing is chosen).
func blocked() -> bool:
	return _blocked


func _show_selected() -> void:
	var i := _selected_index()
	if i < 0:
		facts.text = Kit.tr_key(&"skirmish.design.none") if not _entries.is_empty() else ""
		facts.visible = not facts.text.is_empty()
		issues_label.visible = false
		_blocked = true
		return
	var info := Kit.as_dict(_entries[i].get("info"))
	facts.text = facts_text(info)
	facts.visible = true
	var problems := problems_for(info, _cap_t, _cap_key)
	if not Kit.get_bool(_entries[i], "stock"):
		problems.append_array(_file_problems(Kit.get_str(_entries[i], "path")))
	_blocked = not problems.is_empty()
	issues_label.text = "\n".join(problems)
	issues_label.visible = _blocked


## One list row: the design's name with its length and displacement.
static func row_text(info: Dictionary) -> String:
	var name_text := Kit.get_str(info, "name")
	if name_text.is_empty():
		name_text = Kit.tr_key(&"skirmish.design.unnamed")
	return Kit.tr_key(&"skirmish.design.row").format([name_text,
		Kit.number(Kit.get_float(info, "length_m"), &"m", 0), Kit.number(Kit.get_float(info, "displacement_t"), &"t", 0)])


static func facts_text(info: Dictionary) -> String:
	var line := Kit.tr_key(&"skirmish.design.facts").format([
		Kit.number(Kit.get_float(info, "length_m"), &"m", 1),
		Kit.number(Kit.get_float(info, "beam_m"), &"m", 1),
		Kit.number(Kit.get_float(info, "displacement_t"), &"t", 0),
		Kit.number(float(Kit.get_int(info, "cost")), &"salvage", 0),
		Kit.get_int(info, "parts"),
	])
	var author := Kit.get_str(info, "author")
	if not author.is_empty():
		line += " " + Kit.tr_key(&"skirmish.design.author").format([author])
	return line


## What keeps a design out, from its info alone: format, length and the act's tonnage cap.
static func problems_for(info: Dictionary, cap_t: float, over_key: StringName = &"skirmish.design.over_cap") -> PackedStringArray:
	var out := PackedStringArray()
	if Kit.get_bool(info, "newer_format"):
		out.append(Kit.tr_key(&"skirmish.design.newer_format"))
	var length := Kit.get_float(info, "length_m")
	if length > MAX_LENGTH_M + 0.001:
		out.append(Kit.tr_key(&"skirmish.design.too_long").format([Kit.number(length, &"m", 1), Kit.number(MAX_LENGTH_M, &"m", 0)]))
	var t := Kit.get_float(info, "displacement_t")
	if cap_t > 0.0 and t > cap_t + 0.001:
		out.append(Kit.tr_key(over_key).format([Kit.number(t, &"t", 0), Kit.number(cap_t, &"t", 0)]))
	return out


## Fatal issues of a player file, read once per path (`Editor.file_issues` with no ruleset).
func _file_problems(path: String) -> PackedStringArray:
	if not _issue_cache.has(path):
		_issue_cache[path] = Kit.dicts(Kit.call_on(&"Editor", &"file_issues", [path, ""]))
	var out := PackedStringArray()
	for issue: Dictionary in _issue_cache[path]:
		if Kit.get_bool(issue, "fatal"):
			var text := Kit._Text.resolve(issue.get("text", {}))
			if not text.is_empty():
				out.append(text)
	return out


func _open_dock() -> void:
	var opts := {}
	var path := selected_path()
	if not path.is_empty():
		opts["blueprint"] = path
	var issues: Variant = Kit.call_on(&"Screens", &"open_dry_dock", [DOCK_SANDBOX, opts])
	if not issues is Array:
		Screens.open_message(&"menu.dry_dock.failed_title", &"menu.dry_dock.failed_body")
		return
	for issue: Dictionary in Kit.dicts(issues):
		if Kit.get_bool(issue, "fatal"):
			Screens.open_message(&"menu.dry_dock.failed_title", Kit.problems_body(Kit.problem_texts(issues, &"menu.dry_dock.failed_body")))
			return


## Every selectable row: `{ "path", "stock", "info" }`, stock first as listed.
func entries() -> Array[Dictionary]:
	return _entries


## The focusable controls top to bottom.
func focus_controls() -> Array[Control]:
	return [list, edit_button]
