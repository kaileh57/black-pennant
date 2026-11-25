class_name StanceSymbol
extends Control
## Track symbol drawn in code: frame by stance, bar by domain, trend glyph.
## DESIGN.md L1650, L1660-1661. Unit: ui-widgets.
##
## The same geometry as the stance icons in res://ui/icons (a 24 px grid with 2 px strokes), drawn
## at any size so lists, cards and legends match the plot (L1650):
## - Friend: circle. Neutral: 2:1 capsule. Suspect: downward shield (pentagon). Hostile: upward
##   triangle. Frames only take the stance tint (L1641).
## - Air: a 2 px bar above. Subsurface: a bar below. Weapon: a solid centre mark. Surface: frame only.
## - Trend (`Bp.TREND_*`, or -1 for none): climb, dive or steady glyph to the right (L1661 rule 5).
## - `selected` draws the 3 px white selection bracket (L1657 rule 2).
## Set `stance`, `domain`, `trend`, `selected` and `symbol_px`, or call `show_track(track)` with a
## `TrackDict`. Redraws only on change.

const STANCE_FRIEND := 0
const STANCE_NEUTRAL := 1
const STANCE_SUSPECT := 2
const STANCE_HOSTILE := 3
const DOMAIN_SURFACE := 0
const DOMAIN_SUBSURFACE := 1
const DOMAIN_AIR := 2
const DOMAIN_WEAPON := 3
const TREND_NONE := -1
const TREND_STEADY := 0
const TREND_CLIMBING := 1
const TREND_DIVING := 2
const COLOUR_ITEMS: Array[StringName] = [&"stance_friend", &"stance_neutral", &"stance_suspect", &"stance_hostile"]
const THEME_TYPE := &"Plot"
const GRID := 24.0

var stance := STANCE_SUSPECT:
	set(v):
		if is_same(v, stance):
			return
		stance = clampi(v, STANCE_FRIEND, STANCE_HOSTILE)
		_changed()
var domain := DOMAIN_SURFACE:
	set(v):
		if is_same(v, domain):
			return
		domain = clampi(v, DOMAIN_SURFACE, DOMAIN_WEAPON)
		_changed()
var trend := TREND_NONE:
	set(v):
		if is_same(v, trend):
			return
		trend = v if v >= TREND_STEADY and v <= TREND_DIVING else TREND_NONE
		_changed()
var selected := false:
	set(v):
		if is_same(v, selected):
			return
		selected = v
		queue_redraw()
## Symbol height and width in px at 100% UI scale; 0 uses the theme's Plot symbol size (24).
var symbol_px := 0.0:
	set(v):
		if is_same(v, symbol_px):
			return
		symbol_px = maxf(0.0, v)
		update_minimum_size()
		queue_redraw()


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_PASS


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			update_minimum_size()
			queue_redraw()
		NOTIFICATION_TRANSLATION_CHANGED:
			_update_access()


## Show a `TrackDict`: stance, domain, trend and selection.
func show_track(track: Dictionary) -> void:
	stance = int(track.get("stance", STANCE_SUSPECT))
	domain = int(track.get("domain", DOMAIN_SURFACE))
	trend = int(track.get("trend", TREND_NONE)) if domain != DOMAIN_SURFACE else TREND_NONE
	selected = bool(track.get("selected", false))


func _px() -> float:
	if symbol_px > 0.0:
		return symbol_px
	return float(get_theme_constant(&"symbol_size", THEME_TYPE)) if has_theme_constant(&"symbol_size", THEME_TYPE) else GRID


func _get_minimum_size() -> Vector2:
	var s := _px()
	return Vector2(s * (1.5 if trend != TREND_NONE else 1.0), s)


## The frame outline in 24-unit grid space, as a closed polyline (first point repeated last).
## Circles and capsules are approximated with enough segments to look round at 48 px.
static func frame_points(which: int) -> PackedVector2Array:
	var pts := PackedVector2Array()
	match which:
		STANCE_FRIEND:
			for i in 33:
				var a := TAU * float(i) / 32.0
				pts.append(Vector2(12.0, 12.0) + Vector2(cos(a), sin(a)) * 6.0)
		STANCE_NEUTRAL:
			# rect x 4..20, y 8..16, end radius 4: two half circles joined by straight sides.
			for i in 17:
				var a := -PI * 0.5 + PI * float(i) / 16.0
				pts.append(Vector2(16.0, 12.0) + Vector2(cos(a), sin(a)) * 4.0)
			for i in 17:
				var a := PI * 0.5 + PI * float(i) / 16.0
				pts.append(Vector2(8.0, 12.0) + Vector2(cos(a), sin(a)) * 4.0)
			pts.append(pts[0])
		STANCE_SUSPECT:
			pts = PackedVector2Array([Vector2(5, 6), Vector2(19, 6), Vector2(19, 12), Vector2(12, 18), Vector2(5, 12), Vector2(5, 6)])
		_:
			pts = PackedVector2Array([Vector2(12, 6), Vector2(19, 18), Vector2(5, 18), Vector2(12, 6)])
	return pts


## Where the weapon mark sits inside each frame (grid units).
static func centre_mark(which: int) -> Vector2:
	match which:
		STANCE_FRIEND:
			return Vector2(12.0, 12.5)
		STANCE_SUSPECT:
			return Vector2(12.0, 10.5)
		STANCE_HOSTILE:
			return Vector2(12.0, 14.0)
		_:
			return Vector2(12.0, 12.0)


func _draw() -> void:
	var s := _px()
	var k := s / GRID
	var origin := Vector2(0.0, (size.y - s) * 0.5)
	var colour := get_theme_color(COLOUR_ITEMS[stance], THEME_TYPE)
	var stroke := maxf(1.0, float(get_theme_constant(&"stroke_frame", THEME_TYPE)) * k)
	var pts := frame_points(stance)
	for i in pts.size():
		pts[i] = origin + pts[i] * k
	draw_polyline(pts, colour, stroke, true)
	match domain:
		DOMAIN_AIR:
			draw_line(origin + Vector2(7, 2) * k, origin + Vector2(17, 2) * k, colour, stroke, true)
		DOMAIN_SUBSURFACE:
			draw_line(origin + Vector2(7, 22) * k, origin + Vector2(17, 22) * k, colour, stroke, true)
		DOMAIN_WEAPON:
			draw_circle(origin + centre_mark(stance) * k, 2.0 * k, colour, true, -1.0, true)
	if trend != TREND_NONE:
		var t := origin + Vector2(GRID, 0.0) * k
		var h := 0.5
		var glyph: PackedVector2Array
		match trend:
			TREND_CLIMBING:
				glyph = PackedVector2Array([Vector2(7, 15), Vector2(12, 9), Vector2(17, 15)])
			TREND_DIVING:
				glyph = PackedVector2Array([Vector2(7, 9), Vector2(12, 15), Vector2(17, 9)])
			_:
				glyph = PackedVector2Array([Vector2(7, 12), Vector2(17, 12)])
		for i in glyph.size():
			glyph[i] = t + (glyph[i] - Vector2(12, 12)) * h * k + Vector2(6, 12) * k
		draw_polyline(glyph, colour, stroke, true)
	if selected:
		_draw_bracket(Rect2(origin, Vector2(s, s)))


func _draw_bracket(r: Rect2) -> void:
	var c := get_theme_color(&"select", THEME_TYPE)
	var w := float(get_theme_constant(&"stroke_selection", THEME_TYPE))
	var arm := r.size.x * 0.3
	var a := r.grow(-w * 0.5)
	for corner: Vector2 in [a.position, Vector2(a.end.x, a.position.y), a.end, Vector2(a.position.x, a.end.y)]:
		var sx := 1.0 if corner.x <= a.get_center().x else -1.0
		var sy := 1.0 if corner.y <= a.get_center().y else -1.0
		draw_polyline(PackedVector2Array([corner + Vector2(0, sy * arm), corner, corner + Vector2(sx * arm, 0)]), c, w)


var _has_data := false


func _changed() -> void:
	_has_data = true
	update_minimum_size()
	_update_access()
	queue_redraw()


func _update_access() -> void:
	if not is_inside_tree() or not _has_data:
		return
	var tx := get_node_or_null(^"/root/Tx")
	if tx == null:
		return
	accessibility_name = tr(&"widgets.stance.access").format([String(tx.call(&"word", &"stance", stance)), String(tx.call(&"word", &"domain", domain))])
