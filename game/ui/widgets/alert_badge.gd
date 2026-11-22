class_name AlertBadge
extends Control
## Alert badge: priority glyph, word and colour; blink rules.
## DESIGN.md L1676-1687. Unit: ui-widgets.
##
## One alert as a badge: the priority's own glyph and word on its fill (Emergency and Alarm on
## alert.warning, Warning and Caution on alert.caution) in surface.inset, so the priority reads
## from shape and word as well as hue (L1717 item 4).
##
## Blinking (L1676-1687, HUD rule 11): only the glyph, a symbol-sized mark, blinks; the word stays.
## - Unacknowledged Emergency or Alarm: 2.5 Hz when `blink_fast` (the newest top one), else 1 Hz.
## - Unacknowledged Warning: 1 Hz. Caution: steady.
## - Acknowledged: steady until rectified. Cleared (rectified while unacknowledged): steady, with the
##   cleared glyph and the word "Cleared".
## - With blink-off set, nothing blinks; a 3 px outline marks what would blink.
## Every badge blinks on the shared `Ui.blink_changed` clock and redraws only when its phase flips.
##
## Use: `badge.show_alert(alert)` with an `AlertDict` (priority, state, word, blink_fast, text), or
## set the properties. The tooltip and accessible name carry the alert's sentence.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

## Bp.PRIORITY_* and Bp.ALERT_*, kept here so the script parses before the extension loads.
const PRIORITY_CAUTION := 0
const PRIORITY_WARNING := 1
const PRIORITY_ALARM := 2
const PRIORITY_EMERGENCY := 3
const ALERT_UNACKNOWLEDGED := 0
const ALERT_ACKNOWLEDGED := 1
const ALERT_CLEARED := 2
const GLYPHS: Array[StringName] = [&"alert_caution", &"alert_warning", &"alert_alarm", &"alert_emergency"]
const THEME_TYPE := &"Severity"
const OUTLINE_PX := 3.0

var priority := PRIORITY_CAUTION:
	set(v):
		if is_same(v, priority):
			return
		priority = clampi(v, PRIORITY_CAUTION, PRIORITY_EMERGENCY)
		_restyle()
var state := ALERT_UNACKNOWLEDGED:
	set(v):
		if is_same(v, state):
			return
		state = v
		_restyle()
## The priority word's text key (AlertDict `word`). Empty uses the built-in word for `priority`.
var word: StringName = &"":
	set(v):
		if is_same(v, word):
			return
		word = v
		_restyle()
var blink_fast := false:
	set(v):
		if is_same(v, blink_fast):
			return
		blink_fast = v
		_restyle()
## The alert's sentence (MsgDict), for the tooltip and screen readers.
var text: Variant = {}:
	set(v):
		if typeof(v) == typeof(text) and v == text:
			return
		text = v
		_text_cache = _Text.resolve(v)
		_restyle()

var _word_text := ""
var _text_cache := ""
var _glyph_on := true
var _listening := false

const PRIORITY_WORDS: Array[StringName] = [
	&"widgets.alert.caution",
	&"widgets.alert.warning",
	&"widgets.alert.alarm",
	&"widgets.alert.emergency",
]


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_PASS
	focus_mode = Control.FOCUS_NONE
	Ui.changed.connect(_on_ui_changed)
	_restyle()


func _exit_tree() -> void:
	_listen(false)


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			update_minimum_size()
			queue_redraw()
		NOTIFICATION_TRANSLATION_CHANGED:
			_text_cache = _Text.resolve(text)
			_restyle()


## Show one `AlertDict`.
func show_alert(alert: Dictionary) -> void:
	priority = int(alert.get("priority", PRIORITY_CAUTION))
	state = int(alert.get("state", ALERT_UNACKNOWLEDGED))
	word = StringName(alert.get("word", &""))
	blink_fast = bool(alert.get("blink_fast", false))
	text = alert.get("text", {})


## Whether this badge blinks now (before blink-off is applied), and how fast.
static func blink_rate(p: int, s: int, fast: bool) -> int:
	# 0 steady, 1 slow (1 Hz), 2 fast (2.5 Hz).
	if s != ALERT_UNACKNOWLEDGED:
		return 0
	if p >= PRIORITY_ALARM:
		return 2 if fast else 1
	if p == PRIORITY_WARNING:
		return 1
	return 0


func _restyle() -> void:
	if not is_inside_tree():
		return
	if state == ALERT_CLEARED:
		_word_text = tr(&"widgets.alert.cleared")
	elif word != &"":
		_word_text = tr(word)
	else:
		_word_text = tr(PRIORITY_WORDS[priority])
	var rate := blink_rate(priority, state, blink_fast)
	_listen(rate > 0 and not Ui.blink_off)
	_glyph_on = Ui.blink_on(rate == 2) if rate > 0 else true
	tooltip_text = _text_cache
	accessibility_name = ("%s. %s" % [_word_text, _text_cache]) if not _text_cache.is_empty() else _word_text
	update_minimum_size()
	queue_redraw()


func _listen(on: bool) -> void:
	if on == _listening:
		return
	_listening = on
	if on:
		Ui.blink_changed.connect(_on_blink)
	elif Ui.blink_changed.is_connected(_on_blink):
		Ui.blink_changed.disconnect(_on_blink)


func _on_blink(slow_on: bool, fast_on: bool) -> void:
	var rate := blink_rate(priority, state, blink_fast)
	var on := true
	if rate == 1:
		on = slow_on
	elif rate == 2:
		on = fast_on
	if on != _glyph_on:
		_glyph_on = on
		queue_redraw()


func _on_ui_changed(what: StringName) -> void:
	if what == &"blink" or what == &"palette":
		_restyle()


func _style() -> StyleBox:
	return get_theme_stylebox(&"badge_warning" if priority >= PRIORITY_ALARM else &"badge_caution", THEME_TYPE)


func _ink() -> Color:
	return get_theme_color(&"badge_text_warning" if priority >= PRIORITY_ALARM else &"badge_text", THEME_TYPE)


func _glyph_px() -> float:
	var font := get_theme_font(&"font", THEME_TYPE)
	var fs := get_theme_font_size(&"font_size", THEME_TYPE)
	return ceilf(font.get_height(fs)) if font != null else 24.0


func _get_minimum_size() -> Vector2:
	var style := _style()
	var font := get_theme_font(&"font", THEME_TYPE)
	var fs := get_theme_font_size(&"font_size", THEME_TYPE)
	var g := _glyph_px()
	var tw := font.get_string_size(_word_text, HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x if font != null else 0.0
	var gap := float(get_theme_constant(&"spacing_4", &"Gauge"))
	var inner := Vector2(g + gap + tw, g)
	var outline := OUTLINE_PX * 2.0
	if style != null:
		inner += style.get_minimum_size()
	return Vector2(ceilf(inner.x + outline), ceilf(maxf(inner.y, 24.0) + outline))


func _draw() -> void:
	var rect := Rect2(Vector2.ONE * OUTLINE_PX, size - Vector2.ONE * OUTLINE_PX * 2.0)
	var style := _style()
	if style != null:
		draw_style_box(style, rect)
	var rate := blink_rate(priority, state, blink_fast)
	if rate > 0 and Ui.blink_off:
		var outline := get_theme_stylebox(&"badge_blink_off", THEME_TYPE)
		if outline != null:
			draw_style_box(outline, rect.grow(OUTLINE_PX))
	var content := rect
	if style != null:
		content = Rect2(rect.position + Vector2(style.get_margin(SIDE_LEFT), style.get_margin(SIDE_TOP)), rect.size - style.get_minimum_size())
	var g := _glyph_px()
	var ink := _ink()
	var glyph_rect := Rect2(content.position.x, content.position.y + (content.size.y - g) * 0.5, g, g)
	if _glyph_on:
		var tex := Ui.icon(&"alert_cleared" if state == ALERT_CLEARED else GLYPHS[priority])
		if tex != null:
			draw_texture_rect(tex, glyph_rect, false, ink)
	var font := get_theme_font(&"font", THEME_TYPE)
	var fs := get_theme_font_size(&"font_size", THEME_TYPE)
	if font != null:
		var gap := float(get_theme_constant(&"spacing_4", &"Gauge"))
		var baseline := content.position.y + (content.size.y - font.get_height(fs)) * 0.5 + font.get_ascent(fs)
		draw_string(font, Vector2(glyph_rect.end.x + gap, baseline), _word_text, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, ink)
