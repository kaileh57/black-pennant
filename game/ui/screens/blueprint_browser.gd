extends Control
## Blueprint browser with folders, stock and player designs.
## DESIGN.md L233, L2067-2087. Unit: ui-blueprints.
## Reads and calls: Editor.browse, Editor.file_info, Editor.rename_file, Editor.move_file, Editor.delete_file.
##
## Pushed from the main menu or the dry dock: `Screens.push(&"blueprint_browser", args)`. `args`:
## - `purpose: StringName`: `&"open"` (default; the main action opens the design in the dry dock, or
##   loads it into the open dock after asking about unsaved changes) or `&"pick"` (the main action
##   emits `picked(path)` and closes; for any screen that needs a design chosen).
## - `kind: StringName`: `&"ship"` (default) or `&"kit"`, the files the main action takes.
## - `folder: String`: the folder to open (`""` = My designs; `stock:starters` for the stock tree).
##
## Left: the folder tree. Stock designs (starter hulls, enemy classes) are read-only; My designs is
## `user://blueprints/` with its subfolders (L233), Imported among them (L2093). New folder adds one
## under the selected folder. Middle: the files of that folder, one row each with name, length,
## displacement and when it was saved. Right: the selected design's in-engine render when the dry
## dock has drawn one (cached by content hash under user://cache/thumbs; files carry no images,
## L2097), its facts and lineage, every load problem as a sentence (L2081-2083: a newer format names
## the release it needs, a migration says so, over 90 m is refused in player modes, L126), then the
## main action, Copy share string, Rename, Move, Duplicate, Delete (to the OS trash) and Export.
## Import file takes a .bp.ron or .kit.ron from anywhere, a file dropped on the window imports too,
## and pasting a share string anywhere here (Cmd+V or Ctrl+V) opens the import (L2093). The list is
## read again when the window regains focus, so a file placed in the folder appears (L2091).
## States: loading, empty folder, error (the folder does not read), populated.
## `show_folder(folder, entries)` and `show_tree(folders)` render plain data for tests and captures.

const Kit := preload("res://ui/screens/menu_kit.gd")

signal picked(path: String)

const ROOT_PATH := "user://blueprints/"
const IMPORT_FOLDER := "Imported"
const STOCK_FOLDERS: Array[String] = ["stock:starters", "stock:classes"]
const THUMB_DIR := "user://cache/thumbs"
const THUMB_SIZE := Vector2i(480, 270)
const MAX_PLAYER_LENGTH_M := 90.0
## Editor mode for opening a design from here: sandbox (every part, L963).
const DOCK_SANDBOX := 0
## Folder walk limits, so a huge or looping tree cannot stall the menu.
const MAX_DEPTH := 6
const MAX_FOLDERS := 200
const COL_NAME := 0
const COL_LENGTH := 1
const COL_TONNAGE := 2
const COL_SAVED := 3
const FOLDERS_W := 340.0
const DETAIL_W := 500.0

var _back: Button
var _import: Button
var _paste: Button
var _new_folder: Button
var _folders: Tree
var _files: Tree
var _list_state: Label
var _detail: VBoxContainer
var _thumb: TextureRect
var _name: Label
var _facts: Label
var _problems: VBoxContainer
var _main: Button
var _copy: Button
var _rename: Button
var _move: Button
var _duplicate: Button
var _delete: Button
var _export: Button
var _prompt_box: VBoxContainer
var _prompt_label: Label
var _prompt_field: LineEdit
var _prompt_choice: OptionButton
var _prompt_ok: Button
var _prompt_cancel: Button
var _note: Label
var _file_dialog: FileDialog

var _purpose: StringName = &"open"
var _kind: StringName = &"ship"
var _folder := ""
## `{ "path": String, "label": String, "stock": bool, "depth": int }` per folder, in tree order.
var _folder_rows: Array[Dictionary] = []
## The FolderEntryDicts of the open folder that are files.
var _entries: Array[Dictionary] = []
var _issue_cache: Dictionary[String, Array] = {}
var _prompt_action: Callable
var _dialog_action: StringName = &""
var _started := false


var _body: BoxContainer
var _body_scroll: ScrollContainer
var _body_step := 0
## `[control, width side by side, height when stacked]`, see `_fit_columns`.
var _fit: Array[Array] = []

func _ready() -> void:
	accessibility_name = tr(&"blueprints.title")
	var backing := Panel.new()
	backing.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(backing)
	var margin := MarginContainer.new()
	margin.set_anchors_preset(Control.PRESET_FULL_RECT)
	Ui.set_margins(margin, 5)
	add_child(margin)
	var column := Kit.vbox(3)
	margin.add_child(column)

	var header := Kit.hbox(3)
	var title := Kit.label("blueprints.title", &"ScreenTitle")
	title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	header.add_child(title)
	_import = _tool_button("blueprints.import", &"file")
	_import.pressed.connect(_on_import_file)
	header.add_child(_import)
	_paste = _tool_button("blueprints.paste", &"copy")
	_paste.pressed.connect(_on_paste)
	header.add_child(_paste)
	_back = Kit.inline_button("blueprints.back")
	_back.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	_back.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_back.pressed.connect(_close)
	header.add_child(_back)
	column.add_child(header)
	column.add_child(HSeparator.new())

	var body := _responsive_body(column, 5)

	var left := Kit.vbox(2)
	left.custom_minimum_size = Vector2(FOLDERS_W, 0.0)
	body.add_child(left)
	left.add_child(Kit.label("blueprints.folders", &"PanelTitle"))
	_folders = Tree.new()
	_folders.hide_root = true
	_folders.select_mode = Tree.SELECT_SINGLE
	_folders.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_folders.focus_mode = Control.FOCUS_ALL
	_folders.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_folders.accessibility_name = tr(&"blueprints.folders")
	_folders.item_selected.connect(_on_folder_selected)
	_folders.gui_input.connect(_on_tree_side_input.bind(_folders))
	left.add_child(_folders)
	_new_folder = Kit.button("blueprints.new_folder")
	_new_folder.icon = Ui.icon(&"folder_add")
	_new_folder.pressed.connect(_ask_new_folder)
	left.add_child(_new_folder)

	var middle := Kit.vbox(2)
	middle.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	body.add_child(middle)
	middle.add_child(Kit.label("blueprints.designs", &"PanelTitle"))
	_files = Tree.new()
	_files.columns = 4
	_files.column_titles_visible = true
	_files.hide_root = true
	_files.select_mode = Tree.SELECT_ROW
	_files.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_files.focus_mode = Control.FOCUS_ALL
	_files.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_files.accessibility_name = tr(&"blueprints.designs")
	_files.set_column_expand(COL_NAME, true)
	_files.set_column_clip_content(COL_NAME, true)
	for c: int in [COL_LENGTH, COL_TONNAGE, COL_SAVED]:
		_files.set_column_expand(c, false)
	_files.set_column_custom_minimum_width(COL_NAME, 200)
	_files.set_column_custom_minimum_width(COL_LENGTH, 100)
	_files.set_column_custom_minimum_width(COL_TONNAGE, 140)
	_files.set_column_custom_minimum_width(COL_SAVED, 200)
	_files.item_selected.connect(_show_selected)
	_files.gui_input.connect(_on_tree_side_input.bind(_files))
	_files.item_activated.connect(_on_main)
	middle.add_child(_files)
	_list_state = Kit.label("", &"Body", true)
	middle.add_child(_list_state)

	var detail_scroll := ScrollContainer.new()
	detail_scroll.custom_minimum_size = Vector2(DETAIL_W, 0.0)
	detail_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	detail_scroll.follow_focus = true
	body.add_child(detail_scroll)
	_detail = Kit.vbox(2)
	_detail.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	detail_scroll.add_child(_detail)
	_thumb = TextureRect.new()
	_thumb.custom_minimum_size = Vector2(0.0, 240.0)
	_thumb.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	_thumb.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	_thumb.visible = false
	_detail.add_child(_thumb)
	_name = Kit.label("", &"PanelTitle", true)
	_detail.add_child(_name)
	_facts = Kit.label("", &"Body", true)
	_detail.add_child(_facts)
	_problems = Kit.vbox(1)
	_detail.add_child(_problems)
	_build_prompt()
	_main = Kit.button("blueprints.open", true)
	_main.pressed.connect(_on_main)
	_detail.add_child(_main)
	_copy = _detail_button("blueprints.copy_share", &"copy")
	_copy.pressed.connect(_on_copy)
	_detail.add_child(_copy)
	var row1 := Kit.hbox(2)
	_rename = _detail_button("blueprints.rename", &"")
	_rename.pressed.connect(_ask_rename)
	row1.add_child(_rename)
	_move = _detail_button("blueprints.move", &"folder")
	_move.pressed.connect(_ask_move)
	row1.add_child(_move)
	_detail.add_child(row1)
	var row2 := Kit.hbox(2)
	_duplicate = _detail_button("blueprints.duplicate", &"")
	_duplicate.pressed.connect(_on_duplicate)
	row2.add_child(_duplicate)
	_export = _detail_button("blueprints.export", &"save")
	_export.pressed.connect(_on_export)
	row2.add_child(_export)
	_detail.add_child(row2)
	_delete = _detail_button("blueprints.delete", &"delete")
	_delete.pressed.connect(_ask_delete)
	_detail.add_child(_delete)
	_note = Kit.label("", &"Body", true)
	_note.visible = false
	_detail.add_child(_note)

	_file_dialog = FileDialog.new()
	_file_dialog.use_native_dialog = true
	_file_dialog.access = FileDialog.ACCESS_FILESYSTEM
	_file_dialog.file_selected.connect(_on_file_chosen)
	add_child(_file_dialog)

	_fit = [[left, FOLDERS_W, 0.0], [_folders, 0.0, 300.0], [middle, 480.0, 0.0], [_files, 0.0, 420.0], [detail_scroll, DETAIL_W, 640.0]]
	_apply_columns()
	_show_list_state(&"loading")
	_show_selected()


func _tool_button(key: String, icon_stem: StringName) -> Button:
	var b := Kit.inline_button(key)
	b.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	b.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	if icon_stem != &"":
		b.icon = Ui.icon(icon_stem)
	return b


func _detail_button(key: String, icon_stem: StringName) -> Button:
	var b := Kit.button(key)
	if icon_stem != &"":
		b.icon = Ui.icon(icon_stem)
	return b


func _build_prompt() -> void:
	_prompt_box = Kit.vbox(1)
	_prompt_box.visible = false
	_prompt_label = Kit.label("", &"Body", true)
	_prompt_box.add_child(_prompt_label)
	_prompt_field = LineEdit.new()
	_prompt_field.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_prompt_field.max_length = 64
	_prompt_field.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_prompt_field.text_submitted.connect(func(_t: String) -> void: _on_prompt_ok())
	_prompt_box.add_child(_prompt_field)
	_prompt_choice = OptionButton.new()
	_prompt_choice.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_prompt_choice.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_prompt_box.add_child(_prompt_choice)
	var row := Kit.hbox(2)
	_prompt_ok = Kit.button("")
	_prompt_ok.pressed.connect(_on_prompt_ok)
	row.add_child(_prompt_ok)
	_prompt_cancel = Kit.button("blueprints.cancel")
	_prompt_cancel.pressed.connect(_close_prompt)
	row.add_child(_prompt_cancel)
	_prompt_box.add_child(row)
	_detail.add_child(_prompt_box)


func _apply_columns() -> void:
	var titles: Array[StringName] = [&"blueprints.col.name", &"blueprints.col.length", &"blueprints.col.tonnage", &"blueprints.col.saved"]
	for c in titles.size():
		_files.set_column_title(c, tr(titles[c]))
		_files.set_column_title_alignment(c, HORIZONTAL_ALIGNMENT_LEFT)


# --- facade -------------------------------------------------------------------------------------

func setup(args: Dictionary) -> void:
	_started = true
	_purpose = StringName(args.get("purpose", &"open"))
	_kind = StringName(args.get("kind", &"ship"))
	_main.text = "blueprints.choose" if _purpose == &"pick" else "blueprints.open"
	_capture_dock_thumbnail()
	refresh_tree()
	open_folder(String(args.get("folder", "")))
	var w := get_window()
	if w != null and not w.files_dropped.is_connected(_on_files_dropped):
		w.files_dropped.connect(_on_files_dropped)


func _exit_tree() -> void:
	var w := get_window()
	if _started and w != null and w.files_dropped.is_connected(_on_files_dropped):
		w.files_dropped.disconnect(_on_files_dropped)


func default_focus() -> Control:
	return _files if not _entries.is_empty() else _folders


func back_requested() -> void:
	if _prompt_box.visible:
		_close_prompt()
	else:
		_close()


func _notification(what: int) -> void:
	if what == NOTIFICATION_APPLICATION_FOCUS_IN and _started and is_inside_tree():
		# A file placed in the folder while the game was in the background shows up now (L2091).
		_issue_cache.clear()
		refresh_tree()
		open_folder(_folder)
	elif what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_apply_columns()


func _unhandled_input(event: InputEvent) -> void:
	if not _started or Screens.top() != self:
		return
	if event.is_action_pressed(&"ui_paste", false, true):
		var focus := get_viewport().gui_get_focus_owner()
		if focus is LineEdit or focus is TextEdit:
			return
		get_viewport().set_input_as_handled()
		_on_paste()


## Walk the folder trees and redraw the left column.
func refresh_tree() -> void:
	var rows: Array[Dictionary] = []
	rows.append({"path": "", "label": tr(&"blueprints.folder.mine"), "stock": false, "depth": 0})
	_walk_folders("", 1, rows)
	rows.append({"path": "stock:", "label": tr(&"blueprints.folder.stock"), "stock": true, "depth": 0, "group": true})
	for f: String in STOCK_FOLDERS:
		rows.append({"path": f, "label": tr(StringName("blueprints.folder." + f.trim_prefix("stock:"))), "stock": true, "depth": 1})
		# Enemy classes keep their designs one level down, by act; list those folders too, or the
		# folder reads as empty.
		for e: Dictionary in Kit.dicts(Kit.call_on(&"Editor", &"browse", [f])):
			if not Kit.get_bool(e, "is_folder"):
				continue
			var sub := relative_folder(Kit.get_str(e, "path"), f, Kit.get_str(e, "name"))
			rows.append({"path": sub, "label": stock_folder_label(sub), "stock": true, "depth": 2})
	show_tree(rows)


## What a stock sub-folder reads as: "act2" is "Act 2", anything else its own last segment.
static func stock_folder_label(path: String) -> String:
	var leaf := path.trim_suffix("/").get_file()
	if leaf.begins_with("act") and leaf.trim_prefix("act").is_valid_int():
		return TranslationServer.translate(StringName("units.word.act.%d" % leaf.trim_prefix("act").to_int()))
	return leaf


func _walk_folders(folder: String, depth: int, out: Array[Dictionary]) -> void:
	if depth > MAX_DEPTH or out.size() >= MAX_FOLDERS:
		return
	for e: Dictionary in Kit.dicts(Kit.call_on(&"Editor", &"browse", [folder])):
		if out.size() >= MAX_FOLDERS:
			return
		if not Kit.get_bool(e, "is_folder"):
			continue
		var name_text := Kit.get_str(e, "name")
		if name_text.is_empty() or name_text.begins_with("."):
			continue
		var child := relative_folder(Kit.get_str(e, "path"), folder, name_text)
		var label := tr(&"blueprints.folder.imported") if child == IMPORT_FOLDER else name_text
		out.append({"path": child, "label": label, "stock": false, "depth": depth})
		_walk_folders(child, depth + 1, out)


## The `Editor.browse` argument for a folder entry: relative to user://blueprints/.
static func relative_folder(path: String, parent: String, name_text: String) -> String:
	if path.begins_with(ROOT_PATH):
		return path.trim_prefix(ROOT_PATH).trim_suffix("/")
	if path.begins_with("stock:"):
		return path.trim_suffix("/")
	return parent.path_join(name_text) if not parent.is_empty() else name_text


## Draw the folder tree from plain rows (`path`, `label`, `stock`, `depth`, optional `group`).
func show_tree(rows: Array[Dictionary]) -> void:
	_folder_rows = rows
	_folders.clear()
	var root := _folders.create_item()
	var parents: Array[TreeItem] = [root]
	var select_item: TreeItem = null
	for r: Dictionary in rows:
		var depth := clampi(Kit.get_int(r, "depth"), 0, parents.size() - 1)
		var item := _folders.create_item(parents[depth])
		item.set_text(0, Kit.get_str(r, "label"))
		item.set_icon(0, Ui.icon(&"lock" if Kit.get_bool(r, "stock") and depth > 0 else &"folder"))
		item.set_metadata(0, Kit.get_str(r, "path"))
		if Kit.get_bool(r, "group"):
			item.set_selectable(0, false)
		parents.resize(depth + 1)
		parents.append(item)
		if Kit.get_str(r, "path") == _folder and not Kit.get_bool(r, "group"):
			select_item = item
	if select_item != null:
		select_item.select(0)
	_link_focus()


func _on_folder_selected() -> void:
	var item := _folders.get_selected()
	if item == null:
		return
	var meta: Variant = item.get_metadata(0)
	if meta is String and meta != _folder:
		open_folder(meta)


## List a folder through `Editor.browse`.
func open_folder(folder: String) -> void:
	_folder = folder
	_close_prompt()
	var raw: Variant = Kit.call_on(&"Editor", &"browse", [folder])
	if not raw is Array:
		_entries = []
		_files.clear()
		_show_list_state(&"error")
		_show_selected()
		return
	show_folder(folder, Kit.dicts(raw))


func _is_stock_folder() -> bool:
	return _folder.begins_with("stock:")


## Render the files of a folder (`Array[FolderEntryDict]`; folders in it are shown in the tree).
func show_folder(folder: String, entries: Array[Dictionary]) -> void:
	_folder = folder
	var keep := selected_path()
	_entries = []
	for e: Dictionary in entries:
		if Kit.get_bool(e, "is_folder") or Kit.get_str(e, "path").is_empty():
			continue
		_entries.append(e)
	_files.clear()
	var root := _files.create_item()
	var select_item: TreeItem = null
	for e: Dictionary in _entries:
		var info := Kit.as_dict(e.get("info"))
		var item := _files.create_item(root)
		var name_text := Kit.get_str(info, "name", Kit.get_str(e, "name"))
		item.set_text(COL_NAME, name_text if not name_text.is_empty() else tr(&"blueprints.unnamed"))
		item.set_tooltip_text(COL_NAME, Kit.get_str(e, "name"))
		item.set_icon(COL_NAME, Ui.icon(&"part_hull" if Kit.get_str(info, "kind", "ship") != "kit" else &"part_control"))
		if not info.is_empty():
			item.set_text(COL_LENGTH, Kit.number(Kit.get_float(info, "length_m"), &"m", 1))
			item.set_text(COL_TONNAGE, Kit.number(Kit.get_float(info, "displacement_t"), &"t", 0))
			item.set_text(COL_SAVED, Kit.local_time(Kit.get_int(info, "modified_unix")))
		else:
			item.set_text(COL_LENGTH, "")
			item.set_text(COL_TONNAGE, "")
			item.set_text(COL_SAVED, tr(&"blueprints.unreadable"))
		item.set_text_alignment(COL_LENGTH, HORIZONTAL_ALIGNMENT_RIGHT)
		item.set_text_alignment(COL_TONNAGE, HORIZONTAL_ALIGNMENT_RIGHT)
		if info.is_empty() or Kit.get_bool(info, "newer_format"):
			for c in _files.columns:
				item.set_custom_color(c, Ui.colour(&"text.inactive"))
		item.set_metadata(0, Kit.get_str(e, "path"))
		if Kit.get_str(e, "path") == keep:
			select_item = item
	_show_list_state(&"empty" if _entries.is_empty() else &"populated")
	if select_item == null and root.get_child_count() > 0:
		select_item = root.get_child(0)
	if select_item != null:
		select_item.select(0)
		_files.scroll_to_item(select_item)
	_new_folder.disabled = _is_stock_folder()
	_show_selected()


func _show_list_state(state: StringName) -> void:
	match state:
		&"loading":
			_list_state.text = tr(&"blueprints.loading")
		&"empty":
			_list_state.text = tr(&"blueprints.empty_stock") if _is_stock_folder() else tr(&"blueprints.empty")
		&"error":
			_list_state.text = tr(&"blueprints.error")
		_:
			_list_state.text = ""
	_list_state.visible = not _list_state.text.is_empty()
	_files.visible = state == &"populated"
	_link_focus()


func selected_path() -> String:
	var item := _files.get_selected()
	if item == null:
		return ""
	var meta: Variant = item.get_metadata(0)
	return String(meta) if meta is String else ""


func _selected_entry() -> Dictionary:
	var path := selected_path()
	for e: Dictionary in _entries:
		if Kit.get_str(e, "path") == path:
			return e
	return {}


func _is_stock(entry: Dictionary) -> bool:
	return _is_stock_folder() or Kit.get_bool(Kit.as_dict(entry.get("info")), "stock") or not Kit.get_str(entry, "path").begins_with("user://")


func _show_selected() -> void:
	_note.visible = false
	var e := _selected_entry()
	var none := e.is_empty()
	for b: Button in [_main, _copy, _rename, _move, _duplicate, _delete, _export]:
		b.visible = not none
	if none:
		_thumb.visible = false
		_name.text = tr(&"blueprints.none_selected") if not _entries.is_empty() else ""
		_facts.text = ""
		Kit.clear(_problems)
		_link_focus()
		return
	var info := Kit.as_dict(e.get("info"))
	var stock := _is_stock(e)
	var name_text := Kit.get_str(info, "name", Kit.get_str(e, "name"))
	_name.text = name_text if not name_text.is_empty() else tr(&"blueprints.unnamed")
	_facts.text = facts_text(info, stock)
	_show_thumb(Kit.get_str(info, "hash"))
	var problems := problems_for(info)
	var blocked := info.is_empty() or Kit.get_bool(info, "newer_format")
	for issue: Dictionary in _file_issues(Kit.get_str(e, "path")):
		var text := Kit._Text.resolve(issue.get("text", {}))
		if text.is_empty():
			continue
		var fatal := Kit.get_bool(issue, "fatal")
		blocked = blocked or fatal
		problems.append({"text": text, "fatal": fatal})
	_show_problems(problems)
	var kind := Kit.get_str(info, "kind", "ship")
	var right_kind := kind == String(_kind) or (_kind == &"ship" and kind == "enemy_class")
	_main.disabled = blocked or not right_kind
	_copy.disabled = info.is_empty()
	_rename.disabled = stock
	_move.disabled = stock
	_delete.disabled = stock
	_duplicate.text = "blueprints.duplicate_stock" if stock else "blueprints.duplicate"
	_duplicate.disabled = info.is_empty()
	_export.disabled = info.is_empty()
	_link_focus()


static func facts_text(info: Dictionary, stock: bool) -> String:
	if info.is_empty():
		return Kit.tr_key(&"blueprints.no_info")
	var lines := PackedStringArray()
	if Kit.get_str(info, "kind") == "kit":
		lines.append(Kit.tr_key(&"blueprints.fact.kit"))
	lines.append(Kit.tr_key(&"blueprints.fact.size").format([Kit.number(Kit.get_float(info, "length_m"), &"m", 1),
		Kit.number(Kit.get_float(info, "beam_m"), &"m", 1), Kit.number(Kit.get_float(info, "displacement_t"), &"t", 0)]))
	lines.append(Kit.tr_key(&"blueprints.fact.cost").format([Kit.number(float(Kit.get_int(info, "cost")), &"salvage", 0), Kit.get_int(info, "parts")]))
	var complexity := Kit.get_float(info, "complexity", -1.0)
	if complexity >= 0.0:
		lines.append(Kit.tr_key(&"blueprints.fact.complexity").format([Kit.number(complexity, &"none", 0)]))
	var author := Kit.get_str(info, "author")
	if not author.is_empty():
		lines.append(Kit.tr_key(&"blueprints.fact.author").format([author]))
	var lineage := Kit.as_array(info.get("lineage", []))
	if not lineage.is_empty():
		var first := Kit.as_dict(lineage[-1])
		var from := Kit.get_str(first, "author")
		lines.append(Kit.tr_key(&"blueprints.fact.lineage").format([lineage.size(), from if not from.is_empty() else Kit.tr_key(&"blueprints.unknown_author")]))
	var saved := Kit.local_time(Kit.get_int(info, "modified_unix"))
	if not saved.is_empty() and not stock:
		lines.append(Kit.tr_key(&"blueprints.fact.saved").format([saved]))
	lines.append(Kit.tr_key(&"blueprints.fact.format").format([Kit.get_int(info, "format"), Kit.get_str(info, "game_version")]))
	if stock:
		lines.append(Kit.tr_key(&"blueprints.fact.stock"))
	return "\n".join(lines)


## What the info alone says about loading: newer format, migration, too long for player modes.
static func problems_for(info: Dictionary) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	if info.is_empty():
		out.append({"text": Kit.tr_key(&"blueprints.problem.unreadable"), "fatal": true})
		return out
	if Kit.get_bool(info, "newer_format"):
		out.append({"text": Kit.tr_key(&"blueprints.problem.newer").format([Kit.get_str(info, "game_version")]), "fatal": true})
	if Kit.get_bool(info, "needs_migration"):
		out.append({"text": Kit.tr_key(&"blueprints.problem.migrate"), "fatal": false})
	# Over 90 m opens in the dock (enemy-class mode allows 150 m) but no player mode fields it (L126).
	var length := Kit.get_float(info, "length_m")
	if length > MAX_PLAYER_LENGTH_M + 0.001:
		out.append({"text": Kit.tr_key(&"blueprints.problem.too_long").format([Kit.number(length, &"m", 1), Kit.number(MAX_PLAYER_LENGTH_M, &"m", 0)]), "fatal": false})
	return out


func _file_issues(path: String) -> Array:
	if path.is_empty():
		return []
	if not _issue_cache.has(path):
		_issue_cache[path] = Kit.dicts(Kit.call_on(&"Editor", &"file_issues", [path, ""]))
	return _issue_cache[path]


func _show_problems(problems: Array) -> void:
	Kit.clear(_problems)
	for p: Variant in problems:
		var d := Kit.as_dict(p)
		var row := Kit.hbox(2)
		var glyph := TextureRect.new()
		glyph.texture = Ui.icon(&"check_fail" if Kit.get_bool(d, "fatal") else &"check_caution")
		glyph.stretch_mode = TextureRect.STRETCH_KEEP_CENTERED
		glyph.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
		row.add_child(glyph)
		var l := Kit.label(Kit.get_str(d, "text"), &"Body", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		_problems.add_child(row)


# --- thumbnails ---------------------------------------------------------------------------------

func _show_thumb(hash_hex: String) -> void:
	_thumb.texture = null
	if hash_hex.is_empty() or not hash_hex.is_valid_hex_number():
		_thumb.visible = false
		return
	var path := THUMB_DIR.path_join(hash_hex + ".png")
	if FileAccess.file_exists(path):
		var img := Image.load_from_file(ProjectSettings.globalize_path(path))
		if img != null and not img.is_empty():
			_thumb.texture = ImageTexture.create_from_image(img)
	_thumb.visible = _thumb.texture != null


## When the dry dock is open under this browser, keep its in-engine render of the open design
## (L1238), so the design shows a picture here from now on.
func _capture_dock_thumbnail() -> void:
	if Kit.call_on(&"Editor", &"is_open") != true:
		return
	var path: Variant = Kit.call_on(&"Editor", &"design_path")
	if not path is String or (path as String).is_empty():
		return
	var info := Kit.as_dict(Kit.call_on(&"Editor", &"file_info", [path]))
	var hash_hex := Kit.get_str(info, "hash")
	if hash_hex.is_empty() or not hash_hex.is_valid_hex_number():
		return
	var views := get_tree().root.find_children("*", "BpEditorView", true, false)
	if views.is_empty():
		return
	var img: Variant = views[0].call(&"thumbnail", THUMB_SIZE)
	if img is Image and not (img as Image).is_empty():
		DirAccess.make_dir_recursive_absolute(ProjectSettings.globalize_path(THUMB_DIR))
		(img as Image).save_png(ProjectSettings.globalize_path(THUMB_DIR.path_join(hash_hex + ".png")))


# --- actions -------------------------------------------------------------------------------------

func _say(text: String) -> void:
	_note.text = text
	_note.visible = not text.is_empty()


func _on_main() -> void:
	var path := selected_path()
	if path.is_empty() or _main.disabled:
		return
	if _purpose == &"pick":
		picked.emit(path)
		_close()
		return
	if Kit.call_on(&"Editor", &"is_open") == true:
		if Kit.call_on(&"Editor", &"is_dirty") == true:
			var dialog := Screens.open_dialog(&"blueprints.discard_title", &"blueprints.discard_body",
				[{"id": &"keep", "label": &"blueprints.keep_editing"}, {"id": &"open", "label": &"blueprints.discard_open"}], &"keep", &"keep")
			if dialog != null:
				dialog.connect(&"chosen", _on_discard_choice.bind(path))
			return
		_load_into_dock(path)
		return
	var issues: Variant = Kit.call_on(&"Screens", &"open_dry_dock", [DOCK_SANDBOX, {"blueprint": path}])
	_report_issues(issues, &"blueprints.open_failed")


func _on_discard_choice(id: StringName, path: String) -> void:
	if id == &"open" and is_inside_tree():
		_load_into_dock(path)


func _load_into_dock(path: String) -> void:
	var issues: Variant = Kit.call_on(&"Editor", &"load_design", [path])
	if _report_issues(issues, &"blueprints.open_failed"):
		_close()


## Show a facade's IssueDicts; true when nothing fatal stopped the action.
func _report_issues(issues: Variant, fallback: StringName) -> bool:
	if not issues is Array:
		_say(tr(fallback))
		return false
	var fatal := false
	for issue: Dictionary in Kit.dicts(issues):
		fatal = fatal or Kit.get_bool(issue, "fatal")
	if fatal:
		Screens.open_message(&"blueprints.open_failed_title", Kit.problems_body(Kit.problem_texts(issues, fallback)))
		return false
	return true


func _on_copy() -> void:
	var e := _selected_entry()
	if e.is_empty():
		return
	Screens.push(&"share_dialog", {"mode": &"copy", "path": Kit.get_str(e, "path"), "name": _name.text})


func _on_paste() -> void:
	var text := DisplayServer.clipboard_get() if DisplayServer.has_feature(DisplayServer.FEATURE_CLIPBOARD) else ""
	var looks: Variant = Kit.call_on(&"Editor", &"is_share_string", [text])
	var screen := Screens.push(&"share_dialog", {"mode": &"import", "text": text if looks == true else "", "folder": IMPORT_FOLDER})
	if screen != null and screen.has_signal(&"imported"):
		screen.connect(&"imported", _on_imported)


func _on_imported(path: String) -> void:
	if not is_inside_tree():
		return
	refresh_tree()
	open_folder(path.get_base_dir().trim_prefix(ROOT_PATH).trim_suffix("/") if path.begins_with(ROOT_PATH) else IMPORT_FOLDER)


func _on_import_file() -> void:
	_dialog_action = &"import"
	_file_dialog.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	_file_dialog.filters = PackedStringArray(["*.bp.ron, *.kit.ron ; " + tr(&"blueprints.filter")])
	_file_dialog.title = tr(&"blueprints.import")
	_file_dialog.popup_centered()


func _on_export() -> void:
	var e := _selected_entry()
	if e.is_empty():
		return
	_dialog_action = &"export"
	_file_dialog.file_mode = FileDialog.FILE_MODE_SAVE_FILE
	var file := Kit.get_str(e, "path").get_file()
	_file_dialog.filters = PackedStringArray(["*.kit.ron ; " + tr(&"blueprints.filter")]) if file.ends_with(".kit.ron") else PackedStringArray(["*.bp.ron ; " + tr(&"blueprints.filter")])
	_file_dialog.current_file = file if not file.is_empty() else "design.bp.ron"
	_file_dialog.title = tr(&"blueprints.export")
	_file_dialog.popup_centered()


func _on_file_chosen(os_path: String) -> void:
	match _dialog_action:
		&"import":
			_import_os_file(os_path)
		&"export":
			_export_to(os_path)
	_dialog_action = &""


func _import_os_file(os_path: String) -> void:
	var result := Kit.as_dict(Kit.call_on(&"Editor", &"import_file", [os_path, IMPORT_FOLDER]))
	if Kit.get_bool(result, "ok"):
		_on_imported(Kit.get_str(result, "path"))
		_say(tr(&"blueprints.imported").format([Kit.get_str(result, "name")]))
	else:
		var texts := Kit.problem_texts(result.get("issues", null), &"blueprints.import_failed")
		Screens.open_message(&"blueprints.import_failed_title", Kit.problems_body(texts))


func _export_to(os_path: String) -> void:
	var e := _selected_entry()
	if e.is_empty():
		return
	var src := Kit.get_str(e, "path")
	var err := ERR_FILE_NOT_FOUND
	if src.begins_with("user://") or src.begins_with("res://"):
		err = DirAccess.copy_absolute(ProjectSettings.globalize_path(src), os_path)
	else:
		# A stock design has no file of its own: write its canonical text through the share string.
		var share: Variant = Kit.call_on(&"Editor", &"file_share_string", [src])
		if share is String and not (share as String).is_empty():
			var result := Kit.as_dict(Kit.call_on(&"Editor", &"import_share_string", [share, IMPORT_FOLDER]))
			if Kit.get_bool(result, "ok"):
				err = DirAccess.copy_absolute(ProjectSettings.globalize_path(Kit.get_str(result, "path")), os_path)
	_say(tr(&"blueprints.exported").format([os_path.get_file()]) if err == OK else tr(&"blueprints.export_failed"))


func _on_files_dropped(files: PackedStringArray) -> void:
	if not is_inside_tree() or Screens.top() != self:
		return
	var done := 0
	var failed := PackedStringArray()
	for f: String in files:
		var lower := f.to_lower()
		if not (lower.ends_with(".bp.ron") or lower.ends_with(".kit.ron")):
			failed.append(tr(&"blueprints.drop_not_blueprint").format([f.get_file()]))
			continue
		var result := Kit.as_dict(Kit.call_on(&"Editor", &"import_file", [f, IMPORT_FOLDER]))
		if Kit.get_bool(result, "ok"):
			done += 1
		else:
			for t: String in Kit.problem_texts(result.get("issues", null), &"blueprints.import_failed"):
				failed.append(tr(&"blueprints.drop_failed").format([f.get_file(), t]))
	refresh_tree()
	open_folder(IMPORT_FOLDER if done > 0 else _folder)
	if not failed.is_empty():
		Screens.open_message(&"blueprints.import_failed_title", "\n".join(failed))
	elif done > 0:
		_say(tr(&"blueprints.dropped").format([done]))


func _on_duplicate() -> void:
	var path := selected_path()
	if path.is_empty():
		return
	var copy: Variant = Kit.call_on(&"Editor", &"duplicate_file", [path])
	if copy is String and not (copy as String).is_empty():
		_issue_cache.erase(copy)
		var folder := (copy as String).get_base_dir()
		open_folder(folder.trim_prefix(ROOT_PATH).trim_suffix("/") if folder.begins_with(ROOT_PATH) else _folder)
		_select_path(copy)
		_say(tr(&"blueprints.duplicated"))
	else:
		_say(tr(&"blueprints.duplicate_failed"))


func _select_path(path: String) -> void:
	var root := _files.get_root()
	if root == null:
		return
	for item: TreeItem in root.get_children():
		if item.get_metadata(0) == path:
			item.select(0)
			_files.scroll_to_item(item)
			return


func _ask_delete() -> void:
	var e := _selected_entry()
	if e.is_empty() or _is_stock(e):
		return
	var dialog := Screens.open_dialog(&"blueprints.delete_title", tr(&"blueprints.delete_body").format([_name.text]),
		[{"id": &"keep", "label": &"blueprints.delete_keep"}, {"id": &"delete", "label": &"blueprints.delete_confirm"}], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_delete_choice.bind(Kit.get_str(e, "path")))


func _on_delete_choice(id: StringName, path: String) -> void:
	if id != &"delete" or not is_inside_tree():
		return
	if Kit.call_on(&"Editor", &"delete_file", [path]) == true:
		_issue_cache.erase(path)
		open_folder(_folder)
		_say(tr(&"blueprints.deleted"))
	else:
		_say(tr(&"blueprints.delete_failed"))


# --- inline prompts (rename, move, new folder) ---------------------------------------------------

func _open_prompt(label_key: StringName, ok_key: String, initial: String, choices: Array[Dictionary], action: Callable) -> void:
	_prompt_label.text = tr(label_key)
	_prompt_ok.text = ok_key
	_prompt_field.visible = choices.is_empty()
	_prompt_field.text = initial
	_prompt_field.accessibility_name = tr(label_key)
	_prompt_choice.visible = not choices.is_empty()
	_prompt_choice.clear()
	for i in choices.size():
		_prompt_choice.add_item(Kit.get_str(choices[i], "label"), i)
		_prompt_choice.set_item_metadata(i, Kit.get_str(choices[i], "path"))
	_prompt_choice.accessibility_name = tr(label_key)
	_prompt_action = action
	_prompt_box.visible = true
	_link_focus()
	if _prompt_field.visible:
		Ui.focus(_prompt_field)
		_prompt_field.select_all()
	else:
		Ui.focus(_prompt_choice)


func _close_prompt() -> void:
	if _prompt_box == null:
		return
	_prompt_box.visible = false
	_prompt_action = Callable()
	_link_focus()


func _on_prompt_ok() -> void:
	if not _prompt_box.visible or not _prompt_action.is_valid():
		return
	var value: String = _prompt_field.text.strip_edges() if _prompt_field.visible else String(_prompt_choice.get_selected_metadata())
	var action := _prompt_action
	action.call(value)


func _ask_rename() -> void:
	var e := _selected_entry()
	if e.is_empty() or _is_stock(e):
		return
	var stem := file_stem(Kit.get_str(e, "path"))
	_open_prompt(&"blueprints.rename_label", "blueprints.rename_confirm", stem, [], _do_rename.bind(Kit.get_str(e, "path")))


func _do_rename(new_name: String, path: String) -> void:
	var problem := name_problem(new_name)
	if not problem.is_empty():
		_prompt_label.text = problem
		return
	if Kit.call_on(&"Editor", &"rename_file", [path, new_name]) == true:
		_close_prompt()
		_issue_cache.clear()
		open_folder(_folder)
		_say(tr(&"blueprints.renamed").format([new_name]))
	else:
		_prompt_label.text = tr(&"blueprints.rename_failed")


func _ask_move() -> void:
	var e := _selected_entry()
	if e.is_empty() or _is_stock(e):
		return
	var choices: Array[Dictionary] = []
	for r: Dictionary in _folder_rows:
		if Kit.get_bool(r, "stock") or Kit.get_str(r, "path") == _folder:
			continue
		choices.append({"path": Kit.get_str(r, "path"), "label": "  ".repeat(maxi(Kit.get_int(r, "depth"), 0)) + Kit.get_str(r, "label")})
	if choices.is_empty():
		_say(tr(&"blueprints.move_nowhere"))
		return
	_open_prompt(&"blueprints.move_label", "blueprints.move_confirm", "", choices, _do_move.bind(Kit.get_str(e, "path")))


func _do_move(folder: String, path: String) -> void:
	if Kit.call_on(&"Editor", &"move_file", [path, folder]) == true:
		_close_prompt()
		_issue_cache.erase(path)
		open_folder(_folder)
		_say(tr(&"blueprints.moved"))
	else:
		_prompt_label.text = tr(&"blueprints.move_failed")


func _ask_new_folder() -> void:
	if _is_stock_folder():
		return
	_open_prompt(&"blueprints.new_folder_label", "blueprints.new_folder_confirm", "", [], _do_new_folder)


func _do_new_folder(folder_name: String) -> void:
	var problem := name_problem(folder_name)
	if not problem.is_empty():
		_prompt_label.text = problem
		return
	var full := _folder.path_join(folder_name) if not _folder.is_empty() else folder_name
	if Kit.call_on(&"Editor", &"make_folder", [full]) == true:
		_close_prompt()
		_folder = full
		refresh_tree()
		open_folder(full)
	else:
		_prompt_label.text = tr(&"blueprints.new_folder_failed")


## A file's name without the folder and the `.bp.ron` or `.kit.ron` ending.
static func file_stem(path: String) -> String:
	var f := path.get_file()
	for ext: String in [".bp.ron", ".kit.ron"]:
		if f.ends_with(ext):
			return f.trim_suffix(ext)
	return f.get_basename()


## Why a typed file or folder name cannot be used, or "" when it can.
static func name_problem(text: String) -> String:
	var t := text.strip_edges()
	if t.is_empty():
		return Kit.tr_key(&"blueprints.name_empty")
	if t.begins_with(".") or not t.is_valid_filename() or t.contains("/") or t.contains("\\"):
		return Kit.tr_key(&"blueprints.name_bad")
	return ""


func _close() -> void:
	if Screens.top() == self:
		Screens.pop()


## A Tree keeps Left and Right for folding, so keys and the gamepad could never leave it. Left or
## Right on a row with nothing to fold moves to that side's neighbour instead.
func _on_tree_side_input(event: InputEvent, tree: Tree) -> void:
	var right := event.is_action_pressed(&"ui_right", false, true)
	var left := event.is_action_pressed(&"ui_left", false, true)
	if not right and not left:
		return
	var item := tree.get_selected()
	if item != null and item.get_child_count() > 0 and item.collapsed == right:
		return
	var path := tree.focus_neighbor_right if right else tree.focus_neighbor_left
	var target := tree.get_node_or_null(path) as Control if not path.is_empty() else null
	if target != null and target != tree and target.is_visible_in_tree():
		tree.accept_event()
		Ui.focus(target)


func _link_focus() -> void:
	if _delete == null:
		return
	var detail: Array = []
	if _prompt_box.visible:
		detail.append_array([_prompt_field, _prompt_choice, _prompt_ok, _prompt_cancel])
	detail.append_array([_main, _copy, _rename, _move, _duplicate, _export, _delete])
	var right := Kit.chain_vertical(detail)
	Kit.chain_horizontal([_rename, _move])
	Kit.chain_horizontal([_duplicate, _export])
	Kit.chain_horizontal([_prompt_ok, _prompt_cancel])
	var left := Kit.chain_vertical([_folders, _new_folder])
	_folders.focus_neighbor_right = _folders.get_path_to(_files if _files.visible else (right[0] if not right.is_empty() else _folders))
	_new_folder.focus_neighbor_right = _new_folder.get_path_to(_files if _files.visible else _new_folder)
	_files.focus_neighbor_left = _files.get_path_to(_folders)
	_files.focus_neighbor_right = _files.get_path_to(right[0] if not right.is_empty() else _files)
	_files.focus_neighbor_top = _files.get_path_to(_back)
	for c: Control in right:
		if c != _move and c != _export and c != _prompt_cancel:
			c.focus_neighbor_left = c.get_path_to(_files if _files.visible else _folders)
		if c == _move or c == _export or c == _prompt_cancel:
			c.focus_neighbor_right = c.get_path_to(c)
	for c: Control in left:
		c.focus_neighbor_left = c.get_path_to(c)
	_folders.focus_neighbor_top = _folders.get_path_to(_back)
	if not right.is_empty():
		right[0].focus_neighbor_top = right[0].get_path_to(_back)
	Kit.chain_horizontal([_import, _paste, _back])
	for c: Control in [_import, _paste, _back]:
		c.focus_neighbor_bottom = c.get_path_to(_files if _files.visible else _folders)


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
