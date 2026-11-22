class_name Gauge
extends Control
## Value with units against band and limits, with trend where rate matters. Theme type Gauge.
## DESIGN.md L1668, L1597. Unit: ui-widgets.
##
## Never a bare number (L1597): the value sits over a scale with its normal band and its limits,
## and a trend arrow where the rate matters. Drawn in `_draw()` from the Gauge theme type
## (L1735), redrawn only when data or the theme changes.
##
## Layout, top to bottom: the label (units colour) on the left and the value with its unit merged
## ("12 kn", L1701) on the right in the bold value font, the brightest thing on the gauge (rule 12);
## then the scale: an inset well, the band as an 8 px strip, a 2 px tick per limit and the needle.
## A limit tick turns alert colour only while the value is past it, so a normal gauge has no alert
## pixels (HUD rule 1). The trend arrow runs from the needle to where the value will be in
## `trend_s` seconds at the current rate.
##
## Use:
##   gauge.configure({ "label": &"hud.gauge.speed", "unit": &"kn", "decimals": 0, "min": 0.0,
##       "max": 40.0, "band_low": 5.0, "band_high": 32.0, "caution_high": 34.0,
##       "warning_high": 38.0, "trend_s": 30.0 })
##   gauge.set_reading(12.4, 0.2)   # value, and rate per second (NAN for no trend)
## Keys left out of `configure` keep their value; a limit or band edge of NAN is absent. `label` is
## a text key or MsgDict. A gauge with no trend says why in `trend_waiver` (a text key the
## gauge checklist reads, L1597): an exact readout or a value that does not change in play.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

signal state_changed(state: int)

## Where the value stands: in the band (or no band given), outside the band, past a caution limit,
## past a warning limit. NO_READING until the first `set_reading`.
enum State { NORMAL, OUTSIDE_BAND, CAUTION, WARNING, NO_READING }

const THEME_TYPE := &"Gauge"
const SCALE_HEIGHT := 16.0
const NEEDLE_OVERHANG := 4.0
const ARROW_HEAD := 6.0

var label: Variant = &"":
	set(v):
		if is_same(v, label):
			return
		label = v
		_label_text = _Text.resolve(v)
		_changed()
var unit: StringName = &"none":
	set(v):
		if is_same(v, unit):
			return
		unit = v
		_format_value()
var decimals := 0:
	set(v):
		if is_same(v, decimals):
			return
		decimals = clampi(v, 0, 6)
		_format_value()
var min_value := 0.0:
	set(v):
		if is_same(v, min_value):
			return
		min_value = v
		_changed()
var max_value := 1.0:
	set(v):
		if is_same(v, max_value):
			return
		max_value = v
		_changed()
var band_low := NAN:
	set(v):
		if _same_num(v, band_low):
			return
		band_low = v
		_changed()
var band_high := NAN:
	set(v):
		if _same_num(v, band_high):
			return
		band_high = v
		_changed()
var caution_low := NAN:
	set(v):
		if _same_num(v, caution_low):
			return
		caution_low = v
		_changed()
var caution_high := NAN:
	set(v):
		if _same_num(v, caution_high):
			return
		caution_high = v
		_changed()
var warning_low := NAN:
	set(v):
		if _same_num(v, warning_low):
			return
		warning_low = v
		_changed()
var warning_high := NAN:
	set(v):
		if _same_num(v, warning_high):
			return
		warning_high = v
		_changed()
## Seconds ahead the trend arrow points. 0 hides the trend.
var trend_s := 10.0:
	set(v):
		if is_same(v, trend_s):
			return
		trend_s = maxf(0.0, v)
		queue_redraw()
## Why this gauge has no trend, for the gauge checklist; `&""` when it has one.
var trend_waiver: StringName = &""
## Show the scale's end values under it.
var show_scale := false:
	set(v):
		if is_same(v, show_scale):
			return
		show_scale = v
		_format_scale()
		update_minimum_size()
		queue_redraw()

var value := NAN
var rate := NAN

var _label_text := ""
var _value_text := ""
var _min_text := ""
var _max_text := ""
var _state: State = State.NO_READING


func _ready() -> void:
	focus_mode = Control.FOCUS_NONE
	mouse_filter = Control.MOUSE_FILTER_PASS
	_update_access()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			update_minimum_size()
			queue_redraw()
		NOTIFICATION_TRANSLATION_CHANGED:
			_label_text = _Text.resolve(label)
			_format_value()
			_format_scale()


## Set several properties at once; see the file header for the keys.
func configure(spec: Dictionary) -> void:
	if spec.has("label"):
		label = spec["label"]
	if spec.has("unit"):
		unit = StringName(spec["unit"])
	if spec.has("decimals"):
		decimals = int(spec["decimals"])
	if spec.has("min"):
		min_value = float(spec["min"])
	if spec.has("max"):
		max_value = float(spec["max"])
	for key: String in ["band_low", "band_high", "caution_low", "caution_high", "warning_low", "warning_high"]:
		if spec.has(key):
			set(key, _num(spec[key]))
	if spec.has("trend_s"):
		trend_s = float(spec["trend_s"])
	if spec.has("trend_waiver"):
		trend_waiver = StringName(spec["trend_waiver"])
	if spec.has("show_scale"):
		show_scale = bool(spec["show_scale"])


## A new reading: the value, and its rate per second (NAN for no trend arrow).
func set_reading(new_value: float, new_rate: float = NAN) -> void:
	var text_changes := not is_equal_approx(new_value, value) or is_nan(value)
	value = new_value if is_finite(new_value) else NAN
	rate = new_rate if is_finite(new_rate) else NAN
	if text_changes:
		_format_value()
	else:
		_changed()


## Drop the reading; the gauge shows its scale with no needle.
func clear_reading() -> void:
	value = NAN
	rate = NAN
	_format_value()


func state() -> State:
	return _state


## Where `v` stands against these limits and band. Static so tests and other draws can share it.
static func classify(v: float, band_lo: float, band_hi: float, caution_lo: float, caution_hi: float, warning_lo: float, warning_hi: float) -> State:
	if not is_finite(v):
		return State.NO_READING
	if (is_finite(warning_hi) and v >= warning_hi) or (is_finite(warning_lo) and v <= warning_lo):
		return State.WARNING
	if (is_finite(caution_hi) and v >= caution_hi) or (is_finite(caution_lo) and v <= caution_lo):
		return State.CAUTION
	if (is_finite(band_hi) and v > band_hi) or (is_finite(band_lo) and v < band_lo):
		return State.OUTSIDE_BAND
	return State.NORMAL


## The 0..1 position of `v` on a scale from `lo` to `hi`, clamped. A degenerate scale gives 0.5.
static func scale_fraction(v: float, lo: float, hi: float) -> float:
	if not is_finite(v) or not is_finite(lo) or not is_finite(hi) or is_equal_approx(lo, hi):
		return 0.5
	return clampf((v - lo) / (hi - lo), 0.0, 1.0)


func _get_minimum_size() -> Vector2:
	var vf := get_theme_font(&"value", THEME_TYPE)
	var vs := get_theme_font_size(&"value", THEME_TYPE)
	var h := vf.get_height(vs) if vf != null else 24.0
	h += float(get_theme_constant(&"spacing_4", THEME_TYPE)) + SCALE_HEIGHT + NEEDLE_OVERHANG
	if show_scale:
		var sf := get_theme_font(&"scale", THEME_TYPE)
		h += (sf.get_height(get_theme_font_size(&"scale", THEME_TYPE)) if sf != null else 22.0) + 2.0
	return Vector2(160.0, ceilf(h))


func _draw() -> void:
	var w := size.x
	var vf := get_theme_font(&"value", THEME_TYPE)
	var vs := get_theme_font_size(&"value", THEME_TYPE)
	var uf := get_theme_font(&"units", THEME_TYPE)
	var us := get_theme_font_size(&"units", THEME_TYPE)
	var line_h := vf.get_height(vs) if vf != null else 24.0
	var ascent := vf.get_ascent(vs) if vf != null else 18.0
	# Text row.
	if uf != null and not _label_text.is_empty():
		var label_room := w - (vf.get_string_size(_value_text, HORIZONTAL_ALIGNMENT_LEFT, -1, vs).x if vf != null else 0.0) - 8.0
		draw_string(uf, Vector2(0.0, ascent), _label_text, HORIZONTAL_ALIGNMENT_LEFT, maxf(0.0, label_room), us, get_theme_color(&"units", THEME_TYPE), TextServer.JUSTIFICATION_NONE, TextServer.DIRECTION_AUTO, TextServer.ORIENTATION_HORIZONTAL)
	if vf != null and not _value_text.is_empty():
		draw_string(vf, Vector2(0.0, ascent), _value_text, HORIZONTAL_ALIGNMENT_RIGHT, w, vs, get_theme_color(&"value", THEME_TYPE))
	# Scale.
	var top := line_h + float(get_theme_constant(&"spacing_4", THEME_TYPE)) + NEEDLE_OVERHANG * 0.5
	var well := Rect2(0.0, top, w, SCALE_HEIGHT)
	draw_rect(well, get_theme_color(&"background", THEME_TYPE), true)
	var rule := float(get_theme_constant(&"stroke_rule", THEME_TYPE))
	draw_rect(well.grow(-rule * 0.5), get_theme_color(&"frame", THEME_TYPE), false, rule)
	var inner := well.grow(-rule)
	var band_px := float(get_theme_constant(&"band_thickness", THEME_TYPE))
	if is_finite(band_low) or is_finite(band_high):
		var x0 := _x(band_low if is_finite(band_low) else min_value, inner)
		var x1 := _x(band_high if is_finite(band_high) else max_value, inner)
		var bh := minf(band_px, inner.size.y)
		draw_rect(Rect2(x0, inner.position.y + (inner.size.y - bh) * 0.5, maxf(1.0, x1 - x0), bh), get_theme_color(&"band", THEME_TYPE), true)
	var frame := float(get_theme_constant(&"stroke_frame", THEME_TYPE))
	var tick := float(get_theme_constant(&"tick_length", THEME_TYPE))
	_draw_limit(caution_low, inner, frame, tick, _state == State.CAUTION and is_finite(caution_low) and value <= caution_low, &"limit_caution_active")
	_draw_limit(caution_high, inner, frame, tick, _state == State.CAUTION and is_finite(caution_high) and value >= caution_high, &"limit_caution_active")
	_draw_limit(warning_low, inner, frame, tick, _state == State.WARNING and is_finite(warning_low) and value <= warning_low, &"limit_warning_active")
	_draw_limit(warning_high, inner, frame, tick, _state == State.WARNING and is_finite(warning_high) and value >= warning_high, &"limit_warning_active")
	if is_finite(value):
		var nx := _x(value, inner)
		var needle := get_theme_color(&"needle", THEME_TYPE)
		draw_line(Vector2(nx, well.position.y - NEEDLE_OVERHANG * 0.5), Vector2(nx, well.end.y + NEEDLE_OVERHANG * 0.5), needle, frame + 1.0)
		if is_finite(rate) and trend_s > 0.0:
			var tx := _x(value + rate * trend_s, inner)
			if absf(tx - nx) >= ARROW_HEAD:
				var y := inner.get_center().y
				var trend := get_theme_color(&"trend", THEME_TYPE)
				var dir := signf(tx - nx)
				draw_line(Vector2(nx + dir * 2.0, y), Vector2(tx, y), trend, frame)
				draw_colored_polygon(PackedVector2Array([Vector2(tx + dir * 1.0, y), Vector2(tx - dir * ARROW_HEAD, y - ARROW_HEAD * 0.6), Vector2(tx - dir * ARROW_HEAD, y + ARROW_HEAD * 0.6)]), trend)
	if show_scale:
		var sf := get_theme_font(&"scale", THEME_TYPE)
		var ss := get_theme_font_size(&"scale", THEME_TYPE)
		if sf != null:
			var y2 := well.end.y + NEEDLE_OVERHANG * 0.5 + 2.0 + sf.get_ascent(ss)
			var sc := get_theme_color(&"scale", THEME_TYPE)
			draw_string(sf, Vector2(0.0, y2), _min_text, HORIZONTAL_ALIGNMENT_LEFT, w * 0.5, ss, sc)
			draw_string(sf, Vector2(w * 0.5, y2), _max_text, HORIZONTAL_ALIGNMENT_RIGHT, w * 0.5, ss, sc)


func _draw_limit(limit: float, inner: Rect2, width: float, tick: float, active: bool, active_colour: StringName) -> void:
	if not is_finite(limit):
		return
	var x := _x(limit, inner)
	var colour := get_theme_color(active_colour if active else &"limit", THEME_TYPE)
	var reach := maxf(0.0, (tick - inner.size.y) * 0.5)
	draw_line(Vector2(x, inner.position.y - reach), Vector2(x, inner.end.y + reach), colour, width)


func _x(v: float, inner: Rect2) -> float:
	return inner.position.x + scale_fraction(v, min_value, max_value) * inner.size.x


static func _same_num(a: float, b: float) -> bool:
	return (is_nan(a) and is_nan(b)) or a == b


static func _num(v: Variant) -> float:
	if v == null:
		return NAN
	return float(v)


func _format_value() -> void:
	_value_text = _number(value, unit, decimals) if is_finite(value) else ""
	_changed()


func _format_scale() -> void:
	if show_scale:
		_min_text = _number(min_value, unit, decimals)
		_max_text = _number(max_value, unit, decimals)


func _number(v: float, u: StringName, d: int) -> String:
	var tx := get_node_or_null(^"/root/Tx")
	if tx == null:
		return String.num(v, d)
	return String(tx.call(&"number", v, u, d))


func _changed() -> void:
	var s := classify(value, band_low, band_high, caution_low, caution_high, warning_low, warning_high)
	if s != _state:
		_state = s
		state_changed.emit(s)
	_update_access()
	queue_redraw()


const STATE_KEYS: Array[StringName] = [
	&"widgets.gauge.state.normal",
	&"widgets.gauge.state.outside_band",
	&"widgets.gauge.state.caution",
	&"widgets.gauge.state.warning",
	&"widgets.gauge.state.no_reading",
]


func _update_access() -> void:
	if not is_inside_tree():
		return
	accessibility_name = tr(&"widgets.gauge.access").format([_label_text, _value_text if not _value_text.is_empty() else tr(STATE_KEYS[State.NO_READING]), tr(STATE_KEYS[_state])])
