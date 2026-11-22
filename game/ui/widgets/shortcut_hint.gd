class_name ShortcutHint
extends Control
## Shortcut hint for an input action on the device in use.
## DESIGN.md L1673, L1609. Unit: ui-widgets.
##
## Shows the first binding of `action` on the device the player is using: a key cap with the key's
## name ("Tab", "Space") on keyboard and mouse, or the gamepad glyph from the icon set
## (res://ui/icons/pad_*.svg) on a gamepad, followed by `label` (a text key naming the effect).
## It follows rebinding (`Settings.bindings_changed`) and device changes
## (`Ui.input_device_changed`). Glyphs are icons, never Unicode (L1724 item 11). An action unbound on
## the keyboard shows "Not bound" so the hint never lies. On a gamepad an action with no pad binding
## draws nothing and takes no room: a key cap there ("X") would read as a pad button.
## `compact` drops the label for tight rows; the accessible name still has it.

const PAD_PREFIX := "pad_"
const CAP_PAD_X := 8.0
const GAP := 8.0
const GLYPH_PX := 24.0

var action: StringName = &"":
	set(v):
		if is_same(v, action):
			return
		action = v
		refresh()
var label: StringName = &"":
	set(v):
		if is_same(v, label):
			return
		label = v
		refresh()
var compact := false:
	set(v):
		if is_same(v, compact):
			return
		compact = v
		refresh()

var _binding := ""
var _glyph: Texture2D
## Nothing to show on the device in use (see the header).
var _empty := false
var _label_text := ""


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_PASS
	focus_mode = Control.FOCUS_NONE
	Ui.input_device_changed.connect(func(_pad: bool) -> void: refresh())
	var settings := get_node_or_null(^"/root/Settings")
	if settings != null and settings.has_signal(&"bindings_changed"):
		settings.connect(&"bindings_changed", refresh)
	refresh()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			update_minimum_size()
			queue_redraw()
		NOTIFICATION_TRANSLATION_CHANGED:
			refresh()


## Read the binding again.
func refresh() -> void:
	if not is_node_ready():
		return
	_glyph = null
	_binding = ""
	var stem := ""
	if action != &"":
		var settings := get_node_or_null(^"/root/Settings")
		# Ask for the device Ui reports: this redraw runs from Ui.input_device_changed, which can fire
		# before Settings has seen the same event.
		if settings != null and settings.has_method(&"binding_text_for"):
			_binding = String(settings.call(&"binding_text_for", action, Ui.using_gamepad))
			stem = String(settings.call(&"binding_glyph", action, Ui.using_gamepad))
		elif settings != null:
			_binding = String(settings.call(&"binding_text", action))
		if stem.begins_with(PAD_PREFIX) and ResourceLoader.exists("res://ui/icons/%s.svg" % stem):
			_glyph = Ui.icon(StringName(stem))
	_label_text = tr(label) if label != &"" else ""
	_empty = action != &"" and _binding.is_empty() and Ui.using_gamepad
	var shown := _binding if not _binding.is_empty() else tr(&"widgets.shortcut.unbound")
	if _glyph != null:
		# The glyph shows the position; the spoken name adds the button's own name ("A", "LT").
		shown = "%s, %s" % [tr(StringName("widgets.pad." + stem.trim_prefix(PAD_PREFIX))), _binding]
	accessibility_name = tr(&"widgets.shortcut.access").format([_label_text, shown]) if not _label_text.is_empty() else shown
	tooltip_text = accessibility_name if compact else ""
	update_minimum_size()
	queue_redraw()


func _font() -> Font:
	return get_theme_font(&"font", &"Code")


func _font_size() -> int:
	return get_theme_font_size(&"font_size", &"Code")


func _label_font() -> Font:
	return get_theme_font(&"font", &"Dense")


func _label_size() -> int:
	return get_theme_font_size(&"font_size", &"Dense")


func _cap_text() -> String:
	return _binding if not _binding.is_empty() else tr(&"widgets.shortcut.unbound")


func _cap_size() -> Vector2:
	if _glyph != null:
		return Vector2(GLYPH_PX, GLYPH_PX)
	var f := _font()
	var fs := _font_size()
	var w := f.get_string_size(_cap_text(), HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x if f != null else 24.0
	var h := f.get_height(fs) if f != null else 22.0
	return Vector2(maxf(GLYPH_PX, w + CAP_PAD_X * 2.0), maxf(GLYPH_PX, h + 4.0))


func _get_minimum_size() -> Vector2:
	if _empty:
		return Vector2.ZERO
	var cap := _cap_size()
	var w := cap.x
	var h := cap.y
	if not compact and not _label_text.is_empty():
		var lf := _label_font()
		var ls := _label_size()
		if lf != null:
			w += GAP + lf.get_string_size(_label_text, HORIZONTAL_ALIGNMENT_LEFT, -1, ls).x
			h = maxf(h, lf.get_height(ls))
	return Vector2(ceilf(w), ceilf(h))


func _draw() -> void:
	if _empty:
		return
	var cap := _cap_size()
	var top := (size.y - cap.y) * 0.5
	var cap_rect := Rect2(0.0, top, cap.x, cap.y)
	if _glyph != null:
		draw_texture_rect(_glyph, cap_rect, false, get_theme_color(&"font_color", &"Code"))
	else:
		draw_rect(cap_rect.grow(-0.5), get_theme_color(&"frame", &"Gauge"), false, 1.0)
		var f := _font()
		var fs := _font_size()
		if f != null:
			var baseline := top + (cap.y - f.get_height(fs)) * 0.5 + f.get_ascent(fs)
			var colour := get_theme_color(&"font_color", &"Code") if not _binding.is_empty() else get_theme_color(&"text_inactive", &"Gauge")
			draw_string(f, Vector2(CAP_PAD_X, baseline), _cap_text(), HORIZONTAL_ALIGNMENT_LEFT, -1, fs, colour)
	if not compact and not _label_text.is_empty():
		var lf := _label_font()
		var ls := _label_size()
		if lf != null:
			var baseline2 := (size.y - lf.get_height(ls)) * 0.5 + lf.get_ascent(ls)
			draw_string(lf, Vector2(cap.x + GAP, baseline2), _label_text, HORIZONTAL_ALIGNMENT_LEFT, -1, ls, get_theme_color(&"font_color", &"Dense"))
