class_name DamageHatch
extends Control
## Damage and flooding hatching over a rect, never hue.
## DESIGN.md L1651. Unit: ui-widgets.
##
## Damage reads by value and pattern alone (L1651, HUD rule 2), from the Damage theme type:
## - Intact (over 75%): clear.
## - Damaged (40 to 75%): 45 degree 1 px hatch every 6 px in text.secondary.
## - Heavy (1 to 39%): the same every 3 px in text.primary.
## - Destroyed: 45 and 135 degree hatch every 3 px on a surface.inset fill.
## - Flooding: horizontal lines every 4 px over the flooded part, rising from the bottom.
## Lay it over a part, compartment or cell; it ignores the mouse. Set `level` (`Bp.DAMAGE_*`),
## `flood` (0..1, the flooded fraction), `framed` (a 1 px rule.divider outline, for legends), or
## call `show_part(part)` with a `PartDict`. `level_for_health(health)` gives the level of a
## health fraction. Line geometry is rebuilt only when the size or the data changes.

const DAMAGE_INTACT := 0
const DAMAGE_DAMAGED := 1
const DAMAGE_HEAVY := 2
const DAMAGE_DESTROYED := 3
const LEVEL_NAMES: Array[String] = ["intact", "damaged", "heavy", "destroyed"]
const THEME_TYPE := &"Damage"

var level := DAMAGE_INTACT:
	set(v):
		if is_same(v, level):
			return
		level = clampi(v, DAMAGE_INTACT, DAMAGE_DESTROYED)
		_data_changed()
## Flooded fraction 0..1 of the rect, drawn from the bottom. 0 draws no flooding.
var flood := 0.0:
	set(v):
		if is_same(v, flood):
			return
		flood = clampf(v if is_finite(v) else 0.0, 0.0, 1.0)
		_data_changed()
var framed := false:
	set(v):
		if is_same(v, framed):
			return
		framed = v
		queue_redraw()

var _lines: PackedVector2Array = []
var _lines_2: PackedVector2Array = []
var _flood_lines: PackedVector2Array = []
var _built_for := Vector2(-1, -1)
var _has_data := false


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	resized.connect(_invalidate)


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			_invalidate()
		NOTIFICATION_TRANSLATION_CHANGED:
			_update_access()


## Show a `PartDict` (its `level`) with an optional flooded fraction.
func show_part(part: Dictionary, flooded: float = 0.0) -> void:
	level = int(part.get("level", DAMAGE_INTACT))
	flood = flooded


## The damage level of a health fraction (L1651 bands): over 0.75 intact, 0.40 to 0.75 damaged,
## above 0 to 0.39 heavy, 0 destroyed.
static func level_for_health(health: float) -> int:
	if not is_finite(health) or health <= 0.0:
		return DAMAGE_DESTROYED
	if health > 0.75:
		return DAMAGE_INTACT
	if health >= 0.40:
		return DAMAGE_DAMAGED
	return DAMAGE_HEAVY


## Segments (pairs of points) of parallel lines at `angle_deg` (0 = horizontal, 45 = rising to the
## right) every `spacing` px, clipped to `rect`. Lines sit on a lattice anchored at the rect origin.
static func hatch_lines(rect: Rect2, angle_deg: float, spacing: float) -> PackedVector2Array:
	var out := PackedVector2Array()
	if spacing <= 0.0 or rect.size.x <= 0.0 or rect.size.y <= 0.0:
		return out
	var a := deg_to_rad(angle_deg)
	var d := Vector2(cos(a), -sin(a))
	var n := Vector2(-d.y, d.x)
	var corners := [rect.position, Vector2(rect.end.x, rect.position.y), rect.end, Vector2(rect.position.x, rect.end.y)]
	var lo := INF
	var hi := -INF
	for c: Vector2 in corners:
		var t := (c - rect.position).dot(n)
		lo = minf(lo, t)
		hi = maxf(hi, t)
	var reach := rect.size.length()
	var t0 := ceilf(lo / spacing) * spacing
	var t := t0
	while t <= hi + 0.0001:
		var p := rect.position + n * t
		var seg := _clip(p - d * reach, p + d * reach, rect)
		if seg.size() == 2:
			out.append_array(seg)
		t += spacing
	return out


## Liang-Barsky clip of segment a-b to `r`. Returns two points, or none when it misses.
static func _clip(a: Vector2, b: Vector2, r: Rect2) -> PackedVector2Array:
	var d := b - a
	var u0 := 0.0
	var u1 := 1.0
	var p := [-d.x, d.x, -d.y, d.y]
	var q := [a.x - r.position.x, r.end.x - a.x, a.y - r.position.y, r.end.y - a.y]
	for i in 4:
		var pi_: float = p[i]
		var qi: float = q[i]
		if absf(pi_) < 1e-9:
			if qi < 0.0:
				return PackedVector2Array()
			continue
		var u := qi / pi_
		if pi_ < 0.0:
			u0 = maxf(u0, u)
		else:
			u1 = minf(u1, u)
		if u0 > u1:
			return PackedVector2Array()
	var s := a + d * u0
	var e := a + d * u1
	if s.distance_to(e) < 0.5:
		return PackedVector2Array()
	return PackedVector2Array([s, e])


func _invalidate() -> void:
	_built_for = Vector2(-1, -1)
	queue_redraw()


func _build() -> void:
	_built_for = size
	_lines.clear()
	_lines_2.clear()
	_flood_lines.clear()
	var rect := Rect2(Vector2.ZERO, size)
	if level != DAMAGE_INTACT:
		var name := LEVEL_NAMES[level]
		_lines = hatch_lines(rect, float(get_theme_constant(StringName("hatch_%s_angle" % name), THEME_TYPE)), float(get_theme_constant(StringName("hatch_%s_spacing" % name), THEME_TYPE)))
		if level == DAMAGE_DESTROYED:
			_lines_2 = hatch_lines(rect, float(get_theme_constant(&"hatch_destroyed_angle_2", THEME_TYPE)), float(get_theme_constant(&"hatch_destroyed_spacing", THEME_TYPE)))
	if flood > 0.0:
		var h := size.y * flood
		var wet := Rect2(0.0, size.y - h, size.x, h)
		_flood_lines = hatch_lines(wet, float(get_theme_constant(&"hatch_flooding_angle", THEME_TYPE)), float(get_theme_constant(&"hatch_flooding_spacing", THEME_TYPE)))


func _draw() -> void:
	if _built_for != size:
		_build()
	var width := float(get_theme_constant(&"hatch_width", THEME_TYPE))
	if level == DAMAGE_DESTROYED:
		draw_rect(Rect2(Vector2.ZERO, size), get_theme_color(&"hatch_destroyed_fill", THEME_TYPE), true)
	if level != DAMAGE_INTACT:
		var colour := get_theme_color(StringName("hatch_%s" % LEVEL_NAMES[level]), THEME_TYPE)
		if _lines.size() >= 2:
			draw_multiline(_lines, colour, width)
		if _lines_2.size() >= 2:
			draw_multiline(_lines_2, colour, width)
	if _flood_lines.size() >= 2:
		draw_multiline(_flood_lines, get_theme_color(&"hatch_flooding", THEME_TYPE), width)
	if framed:
		draw_rect(Rect2(Vector2.ONE * 0.5, size - Vector2.ONE), get_theme_color(&"frame", &"Gauge"), false, 1.0)


func _data_changed() -> void:
	_has_data = true
	_invalidate()
	_update_access()


func _update_access() -> void:
	if not is_inside_tree() or not _has_data:
		return
	var tx := get_node_or_null(^"/root/Tx")
	if tx == null:
		return
	var text := String(tx.call(&"word", &"damage", level))
	if flood > 0.0:
		text = tr(&"widgets.damage.flooding_access").format([text, String(tx.call(&"number", flood * 100.0, &"pct", 0))])
	accessibility_name = text
