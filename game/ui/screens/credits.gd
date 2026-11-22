extends Control
## Credits and licences: fonts under OFL, chart colour credit, engine and crates, the AI disclosure.
## DESIGN.md L1645, L1643, L1708. Unit: ui-menu.
## Reads and calls: App.licences, App.build_info.
##
## Pushed over the main menu. The left column says who made the game and how (copy rule 14, L1708:
## code, text and procedural assets written by an AI coding agent under owner direction; no
## generative image, audio, voice or music models), where the art and sound come from, and the
## build. The right column lists every third-party licence `App.licences()` returns (Godot, the
## Atkinson Hyperlegible fonts under the OFL, each Rust crate, the NOAA chart colours) with the
## chosen one's full text below. Keys and the gamepad move Up from the first licence to Close and
## Right into the text, where Up and Down scroll it and Left goes back.
## Licence texts are legal texts and are shown as written, untranslated.

const Kit := preload("res://ui/screens/menu_kit.gd")

@onready var _close: Button = $Margin/Column/Header/Close
@onready var _build: Label = $Margin/Column/Body/AboutScroll/About/Build
@onready var _list: ItemList = $Margin/Column/Body/Licences/List
@onready var _text: RichTextLabel = $Margin/Column/Body/Licences/Text

var _licences: Array[Dictionary] = []


func _ready() -> void:
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	Ui.set_separation($Margin/Column/Body, 4)
	Ui.set_separation($Margin/Column/Body/AboutScroll/About, 3)
	Ui.set_separation($Margin/Column/Body/Licences, 2)
	_build.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_build.text = ""
	_list.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_list.select_mode = ItemList.SELECT_SINGLE
	_list.focus_mode = Control.FOCUS_ALL
	_list.item_selected.connect(_show_licence)
	_text.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_text.bbcode_enabled = false
	_text.scroll_active = true
	_text.focus_mode = Control.FOCUS_ALL
	_close.pressed.connect(_leave)
	_close.focus_neighbor_bottom = _close.get_path_to(_list)
	_close.focus_neighbor_left = _close.get_path_to(_close)
	_close.focus_neighbor_right = _close.get_path_to(_close)
	_list.focus_neighbor_top = _list.get_path_to(_close)
	_list.focus_neighbor_right = _list.get_path_to(_text)
	_list.focus_neighbor_left = _list.get_path_to(_list)
	_list.focus_next = _list.get_path_to(_text)
	_list.gui_input.connect(_on_list_input)
	_text.focus_neighbor_top = _text.get_path_to(_list)
	_text.focus_neighbor_left = _text.get_path_to(_list)
	_text.focus_neighbor_right = _text.get_path_to(_text)
	_text.focus_neighbor_bottom = _text.get_path_to(_text)
	_text.gui_input.connect(_on_text_input)
	_text.focus_previous = _text.get_path_to(_list)
	_text.focus_next = _text.get_path_to(_close)
	accessibility_name = tr(&"credits.title")


func setup(_args: Dictionary) -> void:
	var info := Kit.as_dict(Kit.call_on(&"App", &"build_info"))
	show_data(Kit.dicts(Kit.call_on(&"App", &"licences")), info)


func default_focus() -> Control:
	return _list if _list.item_count > 0 else _close


func back_requested() -> void:
	_leave()


## Fill both columns from plain data (also the test entry point).
func show_data(licences: Array[Dictionary], build_info: Dictionary) -> void:
	_licences = licences
	_list.clear()
	for l: Dictionary in licences:
		var name_text := Kit.get_str(l, "name")
		var licence := Kit.get_str(l, "licence")
		var i := _list.add_item(tr(&"credits.licence_row").format([name_text, licence]) if not licence.is_empty() else name_text)
		_list.set_item_tooltip_enabled(i, false)
	_list.accessibility_name = tr(&"credits.licences")
	if licences.is_empty():
		_text.text = tr(&"credits.no_licences")
	else:
		_list.select(0)
		_show_licence(0)
	var version := Kit.get_str(build_info, "version")
	if version.is_empty():
		_build.text = ""
	else:
		_build.text = tr(&"credits.build").format([version, Kit.get_str(build_info, "build_id", "local"),
			Kit.get_int(build_info, "protocol"), Kit.get_int(build_info, "blueprint_format"), Kit.get_int(build_info, "save_format")])
	_build.visible = not _build.text.is_empty()


## The list keeps Up and Down for choosing a licence, so it would never let keys or the gamepad out:
## Up on the first row goes to Close, Right goes to the licence text to scroll it.
func _on_list_input(event: InputEvent) -> void:
	var target: Control = null
	if event.is_action_pressed(&"ui_right", false, true):
		target = _text
	elif event.is_action_pressed(&"ui_up", false, true) and (_list.get_selected_items().is_empty() or _list.get_selected_items()[0] == 0):
		target = _close
	if target != null:
		_list.accept_event()
		Ui.focus(target)


## Up and Down scroll the text; Left goes back to the list.
func _on_text_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_left", false, true):
		_text.accept_event()
		Ui.focus(_list)


func _show_licence(index: int) -> void:
	if index < 0 or index >= _licences.size():
		return
	var l := _licences[index]
	_text.text = Kit.get_str(l, "text")
	_text.scroll_to_line(0)
	_text.accessibility_name = Kit.get_str(l, "name")


func _leave() -> void:
	if Screens.top() == self and Screens.depth() > 1:
		Screens.pop()
	else:
		Screens.change(&"main_menu")
