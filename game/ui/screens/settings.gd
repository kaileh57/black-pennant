extends Control
## Settings built from the schema, one page per section.
## DESIGN.md L1710-1723. Unit: ui-settings.
## Reads and calls: Settings.schema, Settings.set_value, Settings.reset_section, App.timing_log_text.
##
## Opened with `Screens.push(&"settings")` from the main menu and the pause menu; `args.section`
## (a section name such as `&"access"`) picks the first page. Every row comes from
## `Settings.schema()`: rows with `visible: false` (the other platform, internal flags) are left out,
## and a section with no visible row gets no page. Changes apply at once and are written by
## `Settings` 1 s later; nothing waits for a Save button.
##
## Layout: section list on the left (Up and Down move through it and switch the page), the page on
## the right (Right from the list enters it, Left goes back), a help line at the bottom that shows
## the focused or hovered row's tooltip, so gamepad players read what a pointer would hover.
## Rows by type: bool is one CheckButton; float a slider with its value in percent; enum an
## OptionButton (the gamepad's accept button steps it to the next choice); int and text a field
## that commits on Enter or when focus leaves.
##
## Page extras the schema cannot describe:
## - Accessibility: a preview of an officer line at the chosen size and backing opacity (L1723).
## - Controls: the bindings page (`settings_bindings`).
## - Graphics: what automatic quality picked and why (`Quality.detect_result`).
## - Privacy: what a crash report holds; Send and Don't send start or stop reporting at once (L1334).
## - Playtest: copy the timing log (L80) and the diagnostics to the clipboard.
## UI scale changes only when a slider drag ends, so the slider never moves under the pointer.

const Kit := preload("res://ui/screens/menu_kit.gd")
const ConsentScreen := preload("res://boot/consent.gd")

const UI_SCALE_KEY := &"access.ui_scale"
const PRIVACY_KEY := &"privacy.crash_reports"
const LINE_SCALE_KEY := &"access.officer_line_scale"
const LINE_OPACITY_KEY := &"access.officer_line_opacity"
const CONTROL_W := 360.0
const VALUE_W := 104.0
## Widest a page grows: past this a row's name and its control sit too far apart to read as a pair.
const PAGE_MAX_W := 880.0
## Pages whose extras lead the page instead of following the rows.
const EXTRAS_FIRST: Array[StringName] = [&"input"]

@onready var _title: Label = $Margin/Column/Header/Title
@onready var _close: Button = $Margin/Column/Header/Close
@onready var _sections_box: VBoxContainer = $Margin/Column/Body/SectionScroll/Sections
@onready var _page_title: Label = $Margin/Column/Body/PageScroll/Page/PageTitle
@onready var _rows_box: VBoxContainer = $Margin/Column/Body/PageScroll/Page/Rows
@onready var _extras: VBoxContainer = $Margin/Column/Body/PageScroll/Page/Extras
@onready var _reset: Button = $Margin/Column/Body/PageScroll/Page/Reset
@onready var _help: Label = $Margin/Column/Help

## Visible schema rows, in schema order.
var _schema: Array[Dictionary] = []
var _sections: Array[StringName] = []
var _section_buttons: Dictionary[StringName, Button] = {}
var _group := ButtonGroup.new()
var _current: StringName = &""
## Per key of the page on show: `{ "def": Dictionary, "control": Control, "value": Label or null }`.
var _rows: Dictionary[StringName, Dictionary] = {}
## The page's focusable controls in order (rows, extras, reset).
var _page_focus: Array[Control] = []
var _dragging: Dictionary[StringName, bool] = {}
var _status: Label
var _preview_panel: PanelContainer
var _preview_text: Label
var _loaded := false


func _ready() -> void:
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	Ui.set_separation($Margin/Column/Body, 4)
	Ui.set_separation(_sections_box, 1)
	Ui.set_separation($Margin/Column/Body/PageScroll/Page, 4)
	Ui.set_separation(_rows_box, 2)
	Ui.set_separation(_extras, 3)
	_help.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_help.text = ""
	_close.pressed.connect(_leave)
	_reset.pressed.connect(_confirm_reset)
	_reset.focus_entered.connect(_show_help_key.bind(&"settings_ui.reset_page.help"))
	_reset.mouse_entered.connect(_show_help_key.bind(&"settings_ui.reset_page.help"))
	accessibility_name = tr(&"settings_ui.title")
	var settings := Kit.autoload(&"Settings")
	if settings != null and settings.has_signal(&"changed"):
		settings.connect(&"changed", _on_setting_changed)
	Ui.changed.connect(_on_ui_changed)
	var page_scroll := $Margin/Column/Body/PageScroll as ScrollContainer
	# Show-never, not disabled: a disabled axis adds the page's width to the scroll's own minimum,
	# so a page sized for 100% would hold the layout wide after a switch to 200%.
	page_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_SHOW_NEVER
	($Margin/Column/Body/PageScroll/Page as Control).size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	page_scroll.resized.connect(_fit_page_width)
	_fit_page_width()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and _loaded:
		accessibility_name = tr(&"settings_ui.title")
		_show_section(_current)


## `args.section`: the page to open first.
func setup(args: Dictionary) -> void:
	_load(StringName(Kit.get_str(args, "section")))


func default_focus() -> Control:
	if _section_buttons.has(_current):
		return _section_buttons[_current]
	return _close


## Esc or the gamepad's back button. From a row of the page it first returns to the page's section
## button: Left adjusts a slider there, so without this a keyboard or gamepad player on a slider
## has no way back to the section list but closing settings. From the section list it closes.
func back_requested() -> void:
	var f := get_viewport().gui_get_focus_owner()
	var page := $Margin/Column/Body/PageScroll as Control
	if f != null and page.is_ancestor_of(f) and not Ui.using_mouse and _section_buttons.has(_current):
		Ui.focus(_section_buttons[_current])
		return
	_leave()


## Read the schema and build the section list. Also the test entry point.
func _load(first: StringName) -> void:
	_loaded = true
	_schema = []
	for d: Dictionary in Kit.dicts(Kit.call_on(&"Settings", &"schema")):
		if Kit.get_bool(d, "visible", false) and not Kit.get_str(d, "key").is_empty():
			_schema.append(d)
	_build_sections()
	if first == &"" or not _sections.has(first):
		first = _sections[0] if not _sections.is_empty() else &""
	_show_section(first)


func _build_sections() -> void:
	Kit.clear(_sections_box)
	_section_buttons.clear()
	_sections.clear()
	for d: Dictionary in _schema:
		var section := StringName(Kit.get_str(d, "section"))
		if section != &"" and not _sections.has(section):
			_sections.append(section)
	var buttons: Array[Control] = []
	for section: StringName in _sections:
		var b := Kit.button("settings.section." + String(section))
		b.toggle_mode = true
		b.button_group = _group
		b.pressed.connect(_show_section.bind(section))
		b.focus_entered.connect(_on_section_focus.bind(section))
		_sections_box.add_child(b)
		_section_buttons[section] = b
		buttons.append(b)
	Kit.chain_vertical(buttons)
	if not buttons.is_empty():
		buttons[0].focus_neighbor_top = buttons[0].get_path_to(_close)
		_close.focus_neighbor_bottom = _close.get_path_to(buttons[0])
	_close.focus_neighbor_left = _close.get_path_to(_close)
	_close.focus_neighbor_right = _close.get_path_to(_close)
	if _sections.is_empty():
		_page_title.text = "settings_ui.empty.title"
		_help.text = tr(&"settings_ui.empty.body")


## The page fills the scroll area up to PAGE_MAX_W, leaving room for the vertical scroll bar.
func _fit_page_width() -> void:
	var page_scroll := $Margin/Column/Body/PageScroll as ScrollContainer
	var bar := page_scroll.get_v_scroll_bar()
	var room := page_scroll.size.x - (bar.get_combined_minimum_size().x if bar != null else 0.0)
	($Margin/Column/Body/PageScroll/Page as Control).custom_minimum_size.x = clampf(room, 0.0, PAGE_MAX_W)


func _on_section_focus(section: StringName) -> void:
	# Keys and the gamepad move through the list and the page follows; the pointer clicks instead.
	if not Ui.using_mouse and section != _current:
		_show_section(section)


# --- one page ------------------------------------------------------------------------------------

func _show_section(section: StringName) -> void:
	_current = section
	_rows.clear()
	_page_focus.clear()
	_dragging.clear()
	_status = null
	_preview_panel = null
	_preview_text = null
	Kit.clear(_rows_box)
	Kit.clear(_extras)
	if section == &"":
		_reset.visible = false
		return
	# set_pressed_no_signal does not release the rest of a ButtonGroup, so every section is set:
	# only the page on show reads as pressed.
	for s: StringName in _section_buttons:
		_section_buttons[s].set_pressed_no_signal(s == section)
	_page_title.text = "settings.section." + String(section)
	_page_title.accessibility_name = tr(_page_title.text)
	# Rebinding is what most players open Controls for, so its button leads that page; every other
	# page's extras (previews, explanations) follow the rows they explain.
	var extras_first := section in EXTRAS_FIRST
	# Page order is title, then rows and extras in either order, then Reset.
	_rows_box.get_parent().move_child(_extras, _page_title.get_index() + (1 if extras_first else 2))
	if extras_first:
		_add_extras(section)
	for d: Dictionary in _schema:
		if StringName(Kit.get_str(d, "section")) == section:
			_add_row(d)
	if not extras_first:
		_add_extras(section)
	_reset.visible = true
	_page_focus.append(_reset)
	_link_page()
	_help.text = ""


func _add_row(d: Dictionary) -> void:
	var key := StringName(Kit.get_str(d, "key"))
	var type := StringName(Kit.get_str(d, "type"))
	var label_key := Kit.get_str(d, "label", "settings." + String(key))
	var row := Kit.hbox(3)
	row.mouse_filter = Control.MOUSE_FILTER_PASS
	var entry := {"def": d, "control": null, "value": null}
	var focus: Control = null
	match type:
		&"bool":
			# The name is a row label like every other type's, so all names share one left edge (a
			# CheckButton's own text sits inset by its stylebox); a click on the name flips the switch.
			var name_label := _row_label(label_key)
			var check := CheckButton.new()
			check.custom_minimum_size = Vector2(Kit.BUTTON_H, Kit.BUTTON_H)
			check.accessibility_name = tr(label_key)
			check.button_pressed = _bool_of(d.get("value"))
			check.toggled.connect(_on_bool.bind(key))
			name_label.mouse_filter = Control.MOUSE_FILTER_STOP
			name_label.gui_input.connect(_on_bool_label_input.bind(check))
			row.add_child(name_label)
			row.add_child(check)
			entry["control"] = check
			focus = check
		&"float":
			row.add_child(_row_label(label_key))
			var slider := preload("res://ui/widgets/focus_slider.gd").new()
			slider.custom_minimum_size = Vector2(CONTROL_W - VALUE_W, Kit.BUTTON_H)
			slider.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			slider.min_value = Kit.get_float(d, "min", 0.0)
			slider.max_value = Kit.get_float(d, "max", 1.0)
			slider.step = maxf(Kit.get_float(d, "step", 0.01), 0.0001)
			slider.value = _float_of(d.get("value"), slider.min_value)
			slider.accessibility_name = tr(label_key)
			var value := Kit.label(_float_text(slider.value), &"Body", true)
			value.custom_minimum_size = Vector2(VALUE_W, 0.0)
			value.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
			value.autowrap_mode = TextServer.AUTOWRAP_OFF
			slider.value_changed.connect(_on_float.bind(key))
			slider.drag_started.connect(func() -> void: _dragging[key] = true)
			slider.drag_ended.connect(_on_drag_ended.bind(key, slider))
			row.add_child(slider)
			row.add_child(value)
			entry["control"] = slider
			entry["value"] = value
			focus = slider
		&"enum":
			row.add_child(_row_label(label_key))
			var option := OptionButton.new()
			option.custom_minimum_size = Vector2(CONTROL_W, Kit.BUTTON_H)
			option.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			option.accessibility_name = tr(label_key)
			# The setting's value rides in each item's metadata: item ids cannot hold -1 (Automatic),
			# which OptionButton reads as "use the index".
			for o: Dictionary in Kit.dicts(d.get("options")):
				option.add_item(tr(StringName(Kit.get_str(o, "label"))))
				option.set_item_metadata(option.item_count - 1, Kit.get_int(o, "value"))
			_select_option(option, _int_of(d.get("value"), -1))
			option.item_selected.connect(_on_option.bind(key, option))
			option.gui_input.connect(_on_option_pad.bind(option))
			row.add_child(option)
			entry["control"] = option
			focus = option
		&"int", &"text":
			row.add_child(_row_label(label_key))
			var field := LineEdit.new()
			field.custom_minimum_size = Vector2(CONTROL_W, Kit.BUTTON_H)
			field.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			field.accessibility_name = tr(label_key)
			field.text = _field_text(d, d.get("value"))
			field.max_length = 256
			field.select_all_on_focus = true
			field.text_submitted.connect(func(_t: String) -> void: _commit_field(key, field))
			field.focus_exited.connect(_commit_field.bind(key, field))
			row.add_child(field)
			entry["control"] = field
			focus = field
		_:
			return
	_rows_box.add_child(row)
	_rows[key] = entry
	focus.tooltip_text = tr(StringName(Kit.get_str(d, "tooltip")))
	focus.focus_entered.connect(_show_help.bind(d))
	row.mouse_entered.connect(_show_help.bind(d))
	focus.mouse_entered.connect(_show_help.bind(d))
	# A switch's name takes clicks, so it also carries the hover help a pointer looks for there.
	if type == &"bool":
		var name_label := row.get_child(0) as Control
		name_label.tooltip_text = focus.tooltip_text
		name_label.mouse_entered.connect(_show_help.bind(d))
	_page_focus.append(focus)


func _row_label(key: String) -> Label:
	var l := Kit.label(key)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	l.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	l.mouse_filter = Control.MOUSE_FILTER_PASS
	return l


func _link_page() -> void:
	var live := Kit.chain_vertical(_page_focus)
	var section_button: Control = _section_buttons.get(_current, _close)
	Kit.link_sides(live, section_button, null)
	for b: Button in _section_buttons.values():
		b.focus_neighbor_right = b.get_path_to(live[0]) if not live.is_empty() else NodePath()


# --- extras ---------------------------------------------------------------------------------------

func _add_extras(section: StringName) -> void:
	match section:
		&"access":
			_add_officer_preview()
		&"input":
			var bindings := Kit.inline_button("settings_ui.bindings.open")
			bindings.pressed.connect(_open_bindings)
			_extra_help(bindings, &"settings_ui.bindings.help")
			_extras.add_child(bindings)
			_page_focus.append(bindings)
		&"graphics":
			var line := _quality_line()
			if not line.is_empty():
				_extras.add_child(Kit.label(line, &"Body", true))
		&"privacy":
			# One item per line, as the consent screen lists them: joined into a sentence, their
			# leading capitals would sit mid-sentence.
			var held := Kit.vbox(0)
			held.add_child(Kit.label("settings_ui.privacy.holds", &"Body"))
			for key: StringName in ConsentScreen.REPORT_ITEMS:
				held.add_child(Kit.label(String(key), &"Body"))
			_extras.add_child(held)
		&"playtest":
			var row := Kit.hbox(3)
			var copy_log := Kit.inline_button("settings_ui.playtest.copy_log")
			copy_log.pressed.connect(_copy_text.bind(&"timing_log_text", &"settings_ui.playtest.log_copied"))
			_extra_help(copy_log, &"settings_ui.playtest.copy_log.help")
			var copy_diag := Kit.inline_button("settings_ui.playtest.copy_diagnostics")
			copy_diag.pressed.connect(_copy_text.bind(&"diagnostics_text", &"settings_ui.playtest.diagnostics_copied"))
			_extra_help(copy_diag, &"settings_ui.playtest.copy_diagnostics.help")
			row.add_child(copy_log)
			row.add_child(copy_diag)
			Kit.chain_horizontal([copy_log, copy_diag])
			_extras.add_child(row)
			_status = Kit.label("", &"Body", true)
			_status.visible = false
			_extras.add_child(_status)
			_page_focus.append(copy_log)
			_page_focus.append(copy_diag)


func _extra_help(control: Control, key: StringName) -> void:
	control.focus_entered.connect(_show_help_key.bind(key))
	control.mouse_entered.connect(_show_help_key.bind(key))


## An officer line drawn at the chosen size over a backing at the chosen opacity (L1723 floor 10).
func _add_officer_preview() -> void:
	_extras.add_child(Kit.label("settings_ui.preview.heading", &"Body"))
	_preview_panel = PanelContainer.new()
	_preview_panel.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 2)
	var column := Kit.vbox(0)
	var speaker := Kit.label("settings_ui.preview.speaker", &"Body")
	_preview_text = Kit.label("settings_ui.preview.line", &"Body")
	_preview_text.autowrap_mode = TextServer.AUTOWRAP_OFF
	column.add_child(speaker)
	column.add_child(_preview_text)
	margin.add_child(column)
	_preview_panel.add_child(margin)
	_extras.add_child(_preview_panel)
	_update_preview()


func _update_preview() -> void:
	if _preview_panel == null or not is_instance_valid(_preview_panel):
		return
	var scale_value := clampf(_setting_float(LINE_SCALE_KEY, 1.0), 1.0, 2.0)
	var opacity := clampf(_setting_float(LINE_OPACITY_KEY, 1.0), 0.0, 1.0)
	# Only the backing fades; the text stays at full strength (self_modulate does not reach children).
	_preview_panel.self_modulate.a = opacity
	var settings := LabelSettings.new()
	settings.font = get_theme_font(&"font", &"Body")
	settings.font_size = roundi(Ui.font_size(&"Body") * scale_value)
	settings.font_color = Ui.colour(&"text.primary")
	settings.line_spacing = get_theme_constant(&"line_spacing", &"Body")
	for child: Node in _preview_panel.get_child(0).get_child(0).get_children():
		(child as Label).label_settings = settings


func _quality_line() -> String:
	var result := Kit.as_dict(Kit.call_on(&"Quality", &"detect_result"))
	var tier := Kit.get_int(result, "tier", -1)
	if tier < 0:
		return tr(&"settings_ui.graphics.not_detected")
	var word := String(Kit.call_on(&"Tx", &"word", [&"quality", tier]) if Kit.autoload(&"Tx") != null else "")
	var reason := Kit._Text.resolve(result.get("reason", {}))
	var line := tr(&"settings_ui.graphics.detected").format([word])
	if not reason.is_empty():
		line += " " + reason
	if Kit.get_bool(result, "below_minimum"):
		line += " " + tr(&"settings_ui.graphics.below_minimum")
	return line


func _copy_text(method: StringName, done_key: StringName) -> void:
	var text: Variant = Kit.call_on(&"App", method)
	var ok := text is String and not (text as String).is_empty()
	if ok:
		DisplayServer.clipboard_set(text)
	if _status != null and is_instance_valid(_status):
		_status.text = tr(done_key) if ok else tr(&"settings_ui.playtest.nothing_to_copy")
		_status.visible = true


func _open_bindings() -> void:
	Screens.push(&"settings_bindings")


# --- committing ------------------------------------------------------------------------------------

func _on_bool(pressed: bool, key: StringName) -> void:
	_commit(key, pressed)


## A released left click on a switch's name flips the switch (actions fire on release, L1717).
func _on_bool_label_input(event: InputEvent, check: CheckButton) -> void:
	var click := event as InputEventMouseButton
	if click != null and click.button_index == MOUSE_BUTTON_LEFT and not click.pressed and is_instance_valid(check):
		check.button_pressed = not check.button_pressed
		Ui.focus(check)


func _on_float(value: float, key: StringName) -> void:
	var entry: Dictionary = _rows.get(key, {})
	var value_label: Variant = entry.get("value")
	if value_label is Label:
		(value_label as Label).text = _float_text(value)
	# UI scale waits for the drag to end so the slider does not jump under the pointer.
	if key == UI_SCALE_KEY and bool(_dragging.get(key, false)):
		return
	_commit(key, value)


func _on_drag_ended(changed: bool, key: StringName, slider: HSlider) -> void:
	_dragging[key] = false
	if changed:
		_commit(key, slider.value)


func _on_option(index: int, key: StringName, option: OptionButton) -> void:
	if index < 0 or index >= option.item_count:
		return
	_commit(key, int(option.get_item_metadata(index)))


## The gamepad's accept button steps a list to its next choice instead of opening it: the open
## list loses gamepad input to the page behind it (the d-pad closes it and moves the focus on), so
## a pad player could never change one. Keys and the pointer still open the list.
func _on_option_pad(event: InputEvent, option: OptionButton) -> void:
	if not event is InputEventJoypadButton or not event.is_action(&"ui_accept"):
		return
	option.accept_event()
	if not event.is_pressed() or option.item_count == 0:
		return
	var next := (option.selected + 1) % option.item_count
	option.select(next)
	option.item_selected.emit(next)


func _commit_field(key: StringName, field: LineEdit) -> void:
	var entry: Dictionary = _rows.get(key, {})
	if entry.is_empty() or not is_instance_valid(field):
		return
	var d: Dictionary = entry["def"]
	if StringName(Kit.get_str(d, "type")) == &"int":
		var text := field.text.strip_edges()
		if not text.is_valid_int():
			# Not a number: show the stored value again rather than guessing.
			field.text = _field_text(d, _setting(key))
			return
		var lo := int(Kit.get_float(d, "min", -INF))
		var hi := int(Kit.get_float(d, "max", INF))
		var v := clampi(text.to_int(), lo, hi)
		field.text = str(v)
		_commit(key, v)
	else:
		_commit(key, field.text.strip_edges())


func _commit(key: StringName, value: Variant) -> void:
	var previous: Variant = _setting(key)
	var settings := Kit.autoload(&"Settings")
	if settings == null:
		return
	if not bool(settings.call(&"set_value", key, value)):
		push_warning("settings: %s refused %s" % [key, str(value)])
		_sync_row(key)
		return
	if key == PRIVACY_KEY:
		_apply_privacy(_int_of(previous, -1), _int_of(_setting(key), -1))


## Consent changes take effect at once (L1334): Send starts both SDKs, Don't send closes them and
## deletes what they stored.
func _apply_privacy(before: int, after: int) -> void:
	if after == before:
		return
	if after == 1:
		ConsentScreen.start_reporting()
	elif before == 1:
		ConsentScreen.stop_reporting()


# --- reset -----------------------------------------------------------------------------------------

func _confirm_reset() -> void:
	if _current == &"":
		return
	var section_word := {"key": StringName("settings.section." + String(_current))}
	var title := {"key": &"settings_ui.reset.title", "args": [section_word]}
	var dialog := Screens.open_dialog(title, &"settings_ui.reset.body", [
		{"id": &"reset", "label": &"settings_ui.reset.confirm"},
		{"id": &"keep", "label": &"settings_ui.reset.keep"},
	], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_reset_chosen.bind(_current), CONNECT_ONE_SHOT)


func _on_reset_chosen(id: StringName, section: StringName) -> void:
	if id != &"reset":
		return
	var crash_before := _int_of(_setting(PRIVACY_KEY), -1)
	Kit.call_on(&"Settings", &"reset_section", [section])
	if section == &"privacy":
		_apply_privacy(crash_before, _int_of(_setting(PRIVACY_KEY), -1))
	_reload_values()
	_show_section(section)
	Ui.focus.call_deferred(_reset)


# --- keeping rows in step ----------------------------------------------------------------------------

func _on_setting_changed(key: StringName) -> void:
	if not _loaded or not is_inside_tree():
		return
	if not String(key).contains("."):
		_reload_values()
		_show_section(_current)
		return
	for d: Dictionary in _schema:
		if StringName(Kit.get_str(d, "key")) == key:
			d["value"] = _setting(key)
	_sync_row(key)
	if key == LINE_SCALE_KEY or key == LINE_OPACITY_KEY:
		_update_preview()


func _on_ui_changed(what: StringName) -> void:
	if what == &"palette":
		_update_preview()


func _reload_values() -> void:
	for d: Dictionary in _schema:
		d["value"] = _setting(StringName(Kit.get_str(d, "key")))


## Show the stored value in its row without firing the row's own signals.
func _sync_row(key: StringName) -> void:
	var entry: Dictionary = _rows.get(key, {})
	if entry.is_empty():
		return
	var control: Variant = entry["control"]
	if not is_instance_valid(control):
		return
	var value: Variant = _setting(key)
	if control is CheckButton:
		(control as CheckButton).set_pressed_no_signal(_bool_of(value))
	elif control is HSlider:
		var slider := control as HSlider
		if not bool(_dragging.get(key, false)):
			slider.set_value_no_signal(_float_of(value, slider.value))
		var value_label: Variant = entry.get("value")
		if value_label is Label:
			(value_label as Label).text = _float_text(slider.value)
	elif control is OptionButton:
		_select_option(control as OptionButton, _int_of(value, -1))
	elif control is LineEdit:
		var field := control as LineEdit
		if not field.has_focus():
			field.text = _field_text(entry["def"], value)


func _show_help(d: Dictionary) -> void:
	var text := tr(StringName(Kit.get_str(d, "tooltip")))
	if Kit.get_bool(d, "needs_restart"):
		text += " " + tr(&"settings_ui.needs_restart")
	_help.text = text


func _show_help_key(key: StringName) -> void:
	_help.text = tr(key)


func _leave() -> void:
	if Screens.top() == self and Screens.depth() > 1:
		Screens.pop()
	else:
		Screens.change(&"main_menu")


# --- values ------------------------------------------------------------------------------------------

func _setting(key: StringName) -> Variant:
	return Kit.call_on(&"Settings", &"get_value", [key])


func _setting_float(key: StringName, fallback: float) -> float:
	return _float_of(_setting(key), fallback)


static func _bool_of(v: Variant) -> bool:
	if v is bool:
		return v
	if v is int:
		return v != 0
	return false


static func _float_of(v: Variant, fallback: float) -> float:
	if (v is float or v is int) and is_finite(float(v)):
		return float(v)
	return fallback


static func _int_of(v: Variant, fallback: int) -> int:
	if v is int:
		return v
	if v is float and is_finite(v):
		return int(v)
	return fallback


## Every float in the schema is a fraction or a multiplier of a default, so it reads as a percent.
static func _float_text(v: float) -> String:
	return Kit.percent(v)


static func _field_text(d: Dictionary, v: Variant) -> String:
	if StringName(Kit.get_str(d, "type")) == &"int":
		return str(_int_of(v, Kit.get_int(d, "default", 0)))
	return String(v) if v is String else ""


## Select the item holding `value`. A value no option has (privacy before it is asked) selects none
## and the closed list says so in words rather than showing an empty box.
static func _select_option(option: OptionButton, value: int) -> void:
	var index := -1
	for i in option.item_count:
		if int(option.get_item_metadata(i)) == value:
			index = i
	option.select(index)
	if index < 0:
		option.text = Kit.tr_key(&"settings_ui.option.unset")
