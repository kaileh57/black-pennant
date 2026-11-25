extends Control
## Share dialog: copy share string, paste to import, save as file.
## DESIGN.md L2089-2097. Unit: ui-blueprints.
## Reads and calls: Editor.share_string, Editor.share_string_is_long, Editor.import_share_string, Editor.import_file, Editor.export_file.
##
## Pushed over the dry dock or the blueprint browser: `Screens.push(&"share_dialog", args)`.
## - Copy: `{ "mode": &"copy", "path": String, "name": String }`. `path` empty shares the design
##   under edit (`Editor.share_string`); a file path shares that file (`Editor.file_share_string`).
##   Shows the string, its length and Copy; above 4,000 characters it also offers Save as file,
##   because many chat clients cut long messages (L2095).
## - Import: `{ "mode": &"import", "text": String, "folder": String }`. A field to paste a share
##   string (whitespace and line breaks inside it are ignored, L2093), Import, and Import file for a
##   .bp.ron or .kit.ron. The result names what arrived and where, lists every problem as a
##   sentence, and offers Open in dry dock for a ship. Imported items land in Imported/ (L2091).
##   A string whose content does not match its hash is refused as damaged (L2079).
## Emits `imported(path)` after a successful import. Esc closes. `show_copy(text, name, long)` and
## `show_result(result)` render plain data for tests and captures.

const Kit := preload("res://ui/screens/menu_kit.gd")

signal imported(path: String)

const IMPORT_FOLDER := "Imported"
const DOCK_SANDBOX := 0
const PANEL_W := 860.0

var _title: Label
var _subtitle: Label
var _text: TextEdit
var _length: Label
var _primary: Button
var _save: Button
var _import_file: Button
var _open: Button
var _close_button: Button
var _result: Label
var _issues: Label
var _file_dialog: FileDialog

var _mode: StringName = &"copy"
var _path := ""
var _folder := IMPORT_FOLDER
var _share := ""
var _imported_path := ""
var _imported_kind := ""
var _dialog_action: StringName = &""


func _ready() -> void:
	accessibility_name = tr(&"share.title_copy")
	var scrim := ColorRect.new()
	scrim.set_anchors_preset(Control.PRESET_FULL_RECT)
	scrim.color = Ui.scrim_colour()
	scrim.mouse_filter = Control.MOUSE_FILTER_STOP
	add_child(scrim)
	var center := CenterContainer.new()
	center.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(center)
	var panel := PanelContainer.new()
	panel.custom_minimum_size = Vector2(PANEL_W, 0.0)
	center.add_child(panel)
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 4)
	panel.add_child(margin)
	var column := Kit.vbox(2)
	margin.add_child(column)
	_title = Kit.label("share.title_copy", &"PanelTitle")
	column.add_child(_title)
	_subtitle = Kit.label("", &"Body", true)
	column.add_child(_subtitle)
	_text = TextEdit.new()
	_text.custom_minimum_size = Vector2(0.0, 220.0)
	_text.wrap_mode = TextEdit.LINE_WRAPPING_BOUNDARY
	_text.autowrap_mode = TextServer.AUTOWRAP_ARBITRARY
	_text.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_text.accessibility_name = tr(&"share.string")
	_text.text_changed.connect(_on_text_changed)
	column.add_child(_text)
	_length = Kit.label("", &"Dense", true)
	column.add_child(_length)
	_result = Kit.label("", &"Body", true)
	_result.visible = false
	column.add_child(_result)
	_issues = Kit.label("", &"Body", true)
	_issues.visible = false
	column.add_child(_issues)
	var row := Kit.hbox(2)
	_primary = Kit.button("share.copy", true)
	_primary.pressed.connect(_on_primary)
	row.add_child(_primary)
	_save = Kit.button("share.save_file")
	_save.icon = Ui.icon(&"save")
	_save.pressed.connect(_on_save)
	row.add_child(_save)
	_import_file = Kit.button("share.import_file")
	_import_file.icon = Ui.icon(&"file")
	_import_file.pressed.connect(_on_import_file)
	row.add_child(_import_file)
	_open = Kit.button("share.open")
	_open.pressed.connect(_on_open)
	row.add_child(_open)
	_close_button = Kit.button("share.close")
	_close_button.pressed.connect(_close)
	row.add_child(_close_button)
	column.add_child(row)
	_file_dialog = FileDialog.new()
	_file_dialog.use_native_dialog = true
	_file_dialog.access = FileDialog.ACCESS_FILESYSTEM
	_file_dialog.file_selected.connect(_on_file_chosen)
	add_child(_file_dialog)
	show_copy("", "", false)


func setup(args: Dictionary) -> void:
	_mode = StringName(args.get("mode", &"copy"))
	_path = String(args.get("path", ""))
	_folder = String(args.get("folder", IMPORT_FOLDER))
	if _mode == &"import":
		show_import(String(args.get("text", "")))
		return
	var text: Variant = Kit.call_on(&"Editor", &"file_share_string", [_path]) if not _path.is_empty() else Kit.call_on(&"Editor", &"share_string")
	var share := String(text) if text is String else ""
	var long_v: Variant = Kit.call_on(&"Editor", &"share_string_is_long") if _path.is_empty() else null
	var long := bool(long_v) if long_v is bool else share.length() > long_threshold()
	show_copy(share, String(args.get("name", "")), long)


func default_focus() -> Control:
	if _mode == &"import" and _imported_path.is_empty():
		return _text if _text.text.is_empty() else _primary
	return _primary if not _primary.disabled else _close_button


func back_requested() -> void:
	_close()


## Characters above which Save as file is offered (L2095).
static func long_threshold() -> int:
	return 4000


## Copy mode with a share string (`""` = it could not be made).
func show_copy(text: String, design_name: String, long: bool) -> void:
	_mode = &"copy"
	_share = text
	_title.text = tr(&"share.title_copy")
	_subtitle.text = tr(&"share.copy_named").format([design_name]) if not design_name.is_empty() else tr(&"share.copy_help")
	_text.text = text
	_text.editable = false
	_text.placeholder_text = ""
	_length.text = tr(&"share.length").format([Kit.number(float(text.length()), &"none", 0)]) if not text.is_empty() else tr(&"share.unavailable")
	_length.visible = true
	if long and not text.is_empty():
		_length.text += " " + tr(&"share.long")
	_result.visible = false
	_issues.visible = false
	_primary.text = "share.copy"
	_primary.disabled = text.is_empty()
	_save.visible = long and not text.is_empty()
	_import_file.visible = false
	_open.visible = false
	_close_button.text = "share.close"
	_link_focus()


## Import mode, with text already pasted or empty.
func show_import(text: String) -> void:
	_mode = &"import"
	_imported_path = ""
	_title.text = tr(&"share.title_import")
	_subtitle.text = tr(&"share.import_help")
	_text.editable = true
	_text.text = text
	_text.placeholder_text = tr(&"share.paste_here")
	_result.visible = false
	_issues.visible = false
	_primary.text = "share.import"
	_save.visible = false
	_import_file.visible = true
	_open.visible = false
	_close_button.text = "share.cancel"
	_on_text_changed()


func _on_text_changed() -> void:
	if _mode != &"import":
		return
	var t := _text.text.strip_edges()
	_length.visible = not t.is_empty()
	var looks: Variant = Kit.call_on(&"Editor", &"is_share_string", [t])
	var ok: bool = looks == true or (looks == null and t.begins_with("BPN"))
	_length.text = tr(&"share.length").format([Kit.number(float(compact(t).length()), &"none", 0)]) if ok else tr(&"share.not_a_string")
	_primary.disabled = t.is_empty() or not ok
	_link_focus()


## A pasted string without the whitespace chat clients add (the decoder ignores it too).
static func compact(text: String) -> String:
	var out := ""
	for part: String in text.split("\n", false):
		out += part.strip_edges().replace(" ", "").replace("\t", "").replace("\r", "")
	return out


## Show the `import_share_string` / `import_file` result Dictionary.
func show_result(result: Dictionary) -> void:
	var ok := Kit.get_bool(result, "ok")
	var lines := PackedStringArray()
	for issue: Dictionary in Kit.dicts(result.get("issues", [])):
		var t := Kit._Text.resolve(issue.get("text", {}))
		if not t.is_empty():
			lines.append(t)
	_issues.text = "\n".join(lines)
	_issues.visible = not lines.is_empty()
	if ok:
		_imported_path = Kit.get_str(result, "path")
		_imported_kind = Kit.get_str(result, "kind", "ship")
		var what := tr(&"share.kit") if _imported_kind == "kit" else tr(&"share.ship")
		_result.text = tr(&"share.imported").format([Kit.get_str(result, "name"), what, _imported_path.get_base_dir().get_file()])
		_text.editable = false
		_primary.visible = false
		_import_file.visible = false
		_open.visible = _imported_kind != "kit"
		_close_button.text = "share.done"
		imported.emit(_imported_path)
	else:
		_result.text = tr(&"share.import_failed")
	_result.visible = true
	_link_focus()
	Ui.focus(_open if _open.visible else _close_button if ok else _text)


func _on_primary() -> void:
	if _primary.disabled:
		return
	if _mode == &"copy":
		DisplayServer.clipboard_set(_share)
		_result.text = tr(&"share.copied")
		_result.visible = true
		return
	var raw: Variant = Kit.call_on(&"Editor", &"import_share_string", [_text.text, _folder])
	var result := Kit.as_dict(raw)
	if raw == null:
		result = {"ok": false, "issues": [{"text": {"key": &"share.import_failed", "args": []}, "fatal": true}]}
	show_result(result)


func _on_save() -> void:
	_dialog_action = &"save"
	_file_dialog.file_mode = FileDialog.FILE_MODE_SAVE_FILE
	var kit := _path.ends_with(".kit.ron")
	_file_dialog.filters = PackedStringArray(["*.kit.ron ; " + tr(&"share.filter")]) if kit else PackedStringArray(["*.bp.ron ; " + tr(&"share.filter")])
	_file_dialog.current_file = _path.get_file() if not _path.is_empty() else "design.bp.ron"
	_file_dialog.title = tr(&"share.save_file")
	_file_dialog.popup_centered()


func _on_import_file() -> void:
	_dialog_action = &"import"
	_file_dialog.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	_file_dialog.filters = PackedStringArray(["*.bp.ron, *.kit.ron ; " + tr(&"share.filter")])
	_file_dialog.title = tr(&"share.import_file")
	_file_dialog.popup_centered()


func _on_file_chosen(os_path: String) -> void:
	var action := _dialog_action
	_dialog_action = &""
	if action == &"import":
		var raw: Variant = Kit.call_on(&"Editor", &"import_file", [os_path, _folder])
		var result := Kit.as_dict(raw)
		if raw == null:
			result = {"ok": false}
		show_result(result)
	elif action == &"save":
		var ok := false
		if _path.is_empty():
			ok = Kit.call_on(&"Editor", &"export_file", [os_path]) == true
		elif _path.begins_with("user://") or _path.begins_with("res://"):
			ok = DirAccess.copy_absolute(ProjectSettings.globalize_path(_path), os_path) == OK
		_result.text = tr(&"share.saved").format([os_path.get_file()]) if ok else tr(&"share.save_failed")
		_result.visible = true


func _on_open() -> void:
	if _imported_path.is_empty():
		return
	var path := _imported_path
	if Kit.call_on(&"Editor", &"is_open") == true:
		if Kit.call_on(&"Editor", &"is_dirty") == true:
			var dialog := Screens.open_dialog(&"blueprints.discard_title", &"blueprints.discard_body",
				[{"id": &"keep", "label": &"blueprints.keep_editing"}, {"id": &"open", "label": &"blueprints.discard_open"}], &"keep", &"keep")
			if dialog != null:
				dialog.connect(&"chosen", _on_discard_choice.bind(path))
			return
		_open_in_dock(path)
		return
	_close()
	Kit.call_on(&"Screens", &"open_dry_dock", [DOCK_SANDBOX, {"blueprint": path}])


func _on_discard_choice(id: StringName, path: String) -> void:
	if id == &"open" and is_inside_tree():
		_open_in_dock(path)


func _open_in_dock(path: String) -> void:
	var issues: Variant = Kit.call_on(&"Editor", &"load_design", [path])
	var problems := Kit.problem_texts(issues, &"share.open_failed")
	var fatal := not issues is Array
	for i: Dictionary in Kit.dicts(issues):
		fatal = fatal or Kit.get_bool(i, "fatal")
	if fatal:
		_result.text = Kit.problems_body(problems)
		_result.visible = true
		return
	# Close every browser and dialog over the dock: the design is open now.
	Screens.pop_to_base()


func _close() -> void:
	if Screens.top() == self:
		Screens.pop()


func _link_focus() -> void:
	var row := [_primary, _save, _import_file, _open, _close_button]
	Kit.chain_horizontal(row)
	for c: Control in row:
		c.focus_neighbor_top = c.get_path_to(_text)
	_text.focus_neighbor_bottom = _text.get_path_to(_primary if _primary.visible and not _primary.disabled else _close_button)
