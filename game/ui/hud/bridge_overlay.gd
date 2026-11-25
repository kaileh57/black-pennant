extends Control
## Track symbols over the bridge view with declutter.
## DESIGN.md L1660, L1670. Unit: ui-hud-tracks.
## Reads and calls: BpWorldView.track_markers, Hud.track, BpWorldView.pick.
##
## Each frame the world view hands over one marker per track (`track_markers()`: number, canvas
## position, on screen, range, declutter rank). The overlay draws, per track:
## - the stance symbol in the plot's geometry (Friend circle, Neutral capsule, Suspect shield,
##   Hostile triangle; air bar above, subsurface bar below, weapon centre mark; climb, dive or
##   steady glyph), tinted by stance on a dark outline so it reads over sky, sea and fire (rules 4
##   and 18). No 3D model is an icon (rule 13). Last-known marks are grey.
## - a label "0412 Hostile" on an opaque backing (HUD backing opacity, rule 18) with an outline,
##   at least 18 px (floor 1); the designated track adds "Designated", an inbound weapon "Inbound".
## - the white selection bracket on the selected track (rule 2).
## The overlay covers the whole frame; the HUD shell hands it the clusters drawn over it
## (`set_occluders`). Tracks off the view, or under a cluster, sit at the edge of the open sea
## with a chevron pointing out and their range in the label: off the view at the screen's edge, or
## beside the cluster that covers them. No label is placed on a cluster.
## Declutter (rule 14): labels are placed in rank order (selected track, designated, inbound
## weapons, Hostile in own weapon range by closest approach, Hostile outside, Suspect, Unknown rung,
## Neutral and Friend, then by range); a label that would overlap one already placed, or another
## track's symbol, is hidden. Hovering the pointer near symbols shows their hidden labels in a
## stack beside the pointer. Symbols are never hidden.
## Clicks pass through to the HUD shell, which picks through `BpWorldView.pick` (the symbol counts
## within 20 units). Track facts are pulled only on `Hud.tracks_changed` (10 Hz); per frame only
## the packed marker array is read.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Stance := preload("res://ui/widgets/stance_symbol.gd")

const THEME_TYPE := &"Plot"
const STRIDE := 6
const GRID := 24.0
## Edge markers sit this far inside the overlay.
const EDGE_INSET := 28.0
## Gap between a symbol and its label.
const LABEL_GAP := 6.0
## Hidden labels show for symbols within this distance of the pointer.
const HOVER_PX := 32.0
## Off-view markers closer than this to one already drawn with the same look are not redrawn.
const EDGE_MERGE_PX := 10.0
const STANCE_ITEMS: Array[StringName] = [&"stance_friend", &"stance_neutral", &"stance_suspect", &"stance_hostile"]

var _world_ref: WeakRef = null
var _backing := 1.0
## Per track number: { stance, domain, trend, selected, designated, last_known, inbound, lines:
## PackedStringArray, widths: PackedFloat32Array }.
var _info: Dictionary[int, Dictionary] = {}
var _markers := PackedFloat32Array()
var _font: Font
var _font_size := 20
var _outline_size := 4
var _symbol_px := 24.0
var _stroke := 2.0
var _line_h := 24.0
var _hover := Vector2(-1.0e9, -1.0e9)
var _searched_ms := -1000
## Clusters drawn over the overlay; labels and off-view markers keep clear of them.
var _occluders: Array[WeakRef] = []
## The open centre between the docks: off-view markers line its edges, not the docks' gaps.
var _edge_area: WeakRef = null


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	_load_theme()
	var hud := Kit.autoload(&"Hud")
	if hud != null and hud.has_signal(&"tracks_changed"):
		hud.connect(&"tracks_changed", _on_tracks)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: _on_tracks())
	_on_tracks()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			_load_theme()
			_on_tracks()
		NOTIFICATION_TRANSLATION_CHANGED:
			_on_tracks()
		NOTIFICATION_VISIBILITY_CHANGED:
			queue_redraw()


## The world view whose markers are drawn (the HUD shell hands it over).
func set_world(view: Node) -> void:
	_world_ref = weakref(view) if view != null else null


## The controls drawn over the overlay (the HUD shell's clusters). Their visible rects are read at
## each layout, so a cluster that grows or hides is followed without another call.
func set_occluders(controls: Array[Control]) -> void:
	_occluders.clear()
	for c: Control in controls:
		if c != null:
			_occluders.append(weakref(c))
	queue_redraw()


## The control whose rect off-view markers line (the HUD shell's centre between the docks); without
## one they line the overlay's own edges.
func set_edge_area(area: Control) -> void:
	_edge_area = weakref(area) if area != null else null
	queue_redraw()


## The visible occluder rects in overlay space.
func occluder_rects() -> Array[Rect2]:
	var out: Array[Rect2] = []
	var to_local := get_global_transform().affine_inverse()
	for ref: WeakRef in _occluders:
		var c := ref.get_ref() as Control
		if c == null or not is_instance_valid(c) or not c.is_visible_in_tree():
			continue
		var r := to_local * c.get_global_rect()
		if r.size.x >= 1.0 and r.size.y >= 1.0:
			out.append(r)
	return out


## Opacity of the label backing, 0..1 (the HUD shell passes `Ui.hud_backing_opacity`).
func set_backing(alpha: float) -> void:
	_backing = clampf(alpha, 0.0, 1.0)
	queue_redraw()


func _load_theme() -> void:
	_font = get_theme_font(&"label", THEME_TYPE) if has_theme_font(&"label", THEME_TYPE) else get_theme_default_font()
	_font_size = get_theme_font_size(&"label", THEME_TYPE) if has_theme_font_size(&"label", THEME_TYPE) else 20
	_outline_size = get_theme_constant(&"label_outline_size", THEME_TYPE) if has_theme_constant(&"label_outline_size", THEME_TYPE) else 4
	_symbol_px = float(get_theme_constant(&"symbol_size", THEME_TYPE)) if has_theme_constant(&"symbol_size", THEME_TYPE) else GRID
	_stroke = float(get_theme_constant(&"stroke_frame", THEME_TYPE)) if has_theme_constant(&"stroke_frame", THEME_TYPE) else 2.0
	_line_h = _font.get_height(_font_size) if _font != null else 24.0


func _world() -> Node:
	if _world_ref != null:
		var w: Object = _world_ref.get_ref()
		if w != null and is_instance_valid(w):
			return w as Node
	# With no world handed over, look for one at most once a second (captures and tests have none).
	var now := Time.get_ticks_msec()
	if is_inside_tree() and now - _searched_ms >= 1000:
		_searched_ms = now
		var all := get_tree().root.find_children("*", "BpWorldView", true, false)
		if not all.is_empty():
			_world_ref = weakref(all[0])
			return all[0]
	return null


# --- data ----------------------------------------------------------------------------------------

func _on_tracks() -> void:
	set_tracks(Kit.dicts(Kit.call_on(&"Hud", &"tracks")))


## Take the facts the overlay draws from `TrackDict`s. Also the test entry point.
func set_tracks(tracks: Array[Dictionary]) -> void:
	var out: Dictionary[int, Dictionary] = {}
	var own := Kit.get_int(Kit.as_dict(Kit.call_on(&"Hud", &"motion")), "vessel", Bp.NONE)
	for t: Dictionary in tracks:
		var id := Kit.get_int(t, "id", -1)
		if id <= 0:
			continue
		# The own hull is drawn in 3D and is no contact of its own.
		if own != Bp.NONE and Kit.get_int(t, "vessel", Bp.NONE) == own:
			continue
		var stance := clampi(Kit.get_int(t, "stance", 2), 0, 3)
		var lines := PackedStringArray()
		lines.append(tr(&"hud_tracks.overlay.label").format([Kit.get_str(t, "label"), _word(&"stance", stance)]))
		if Kit.get_bool(t, "inbound"):
			lines.append(tr(&"hud_tracks.overlay.inbound"))
		elif Kit.get_bool(t, "designated"):
			lines.append(tr(&"hud_tracks.overlay.designated"))
		if Kit.get_bool(t, "last_known"):
			lines.append(tr(&"hud_tracks.overlay.last_known"))
		var widths := PackedFloat32Array()
		for l: String in lines:
			widths.append(_font.get_string_size(l, HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size).x if _font != null else float(l.length()) * 10.0)
		out[id] = {
			"stance": stance,
			"domain": clampi(Kit.get_int(t, "domain", 0), 0, 3),
			"trend": Kit.get_int(t, "trend", -1) if Kit.get_int(t, "domain", 0) != Bp.DOMAIN_SURFACE else -1,
			"selected": Kit.get_bool(t, "selected"),
			"designated": Kit.get_bool(t, "designated"),
			"last_known": Kit.get_bool(t, "last_known"),
			"inbound": Kit.get_bool(t, "inbound"),
			"range": Kit.get_float(t, "range_m", -1.0),
			"lines": lines,
			"widths": widths,
		}
	_info = out
	queue_redraw()


## Draw these markers instead of the world view's (tests and captures without a 3D view).
func set_markers(markers: PackedFloat32Array) -> void:
	_markers = markers
	queue_redraw()


func _process(_delta: float) -> void:
	if not is_visible_in_tree():
		return
	var w := _world()
	if w != null and w.has_method(&"track_markers"):
		var m: PackedFloat32Array = w.call(&"track_markers")
		if m != _markers:
			_markers = m
			queue_redraw()
	var mouse_now := get_local_mouse_position()
	if mouse_now.distance_to(_hover) >= 1.0:
		_hover = mouse_now
		queue_redraw()


# --- drawing -------------------------------------------------------------------------------------

func _draw() -> void:
	if _markers.size() < STRIDE or _info.is_empty():
		return
	var laid := layout_entries()
	var entries: Array[Dictionary] = laid[0]
	var shown: Array[bool] = laid[1]
	# Symbols first (lowest priority at the bottom), then labels, then the hover stack. Edge
	# markers that would sit on one of the same look are drawn once: the pile reads as one mark
	# and the hover stack lists them all.
	var drawn_edges: Array[Dictionary] = []
	for i in range(entries.size() - 1, -1, -1):
		var e := entries[i]
		if not bool(e["on"]) and not shown[i]:
			var dup := false
			for d: Dictionary in drawn_edges:
				if Vector2(d["pos"]).distance_to(Vector2(e["pos"])) < EDGE_MERGE_PX and d["look"] == _look(e):
					dup = true
					break
			if dup:
				continue
			drawn_edges.append({"pos": e["pos"], "look": _look(e)})
		_draw_symbol(e)
	for i in entries.size():
		if shown[i]:
			_draw_label(entries[i], Rect2(entries[i]["label"]).position)
	_draw_hover(entries, shown)


## The track whose marker or drawn label lies under a point in global canvas space, or 0. Edge
## markers count too: a contact off the view is designated by clicking its chevron or label.
func track_at(global_pos: Vector2) -> int:
	if _markers.size() < STRIDE or _info.is_empty():
		return 0
	var p := get_global_transform().affine_inverse() * global_pos
	var laid := layout_entries()
	var entries: Array[Dictionary] = laid[0]
	var shown: Array[bool] = laid[1]
	var reach := maxf(_symbol_px * 0.75, 12.0)
	for i in entries.size():
		var e := entries[i]
		if Vector2(e["pos"]).distance_to(p) <= reach or (shown[i] and Rect2(e["label"]).has_point(p)):
			return int(e["id"])
	return 0


## Markers in overlay space, in declutter order, and which labels fit: `[entries, shown]`, where
## each entry is `{ id, pos, on, edge, rank, range, info }` and `shown[i]` says whether entry i's
## label is drawn (rule 14). Also the test entry point.
func layout_entries() -> Array:
	var rect := Rect2(Vector2.ZERO, size)
	var inner := rect.grow(-EDGE_INSET)
	var blocked := occluder_rects()
	var to_local := get_global_transform().affine_inverse()
	var edges := inner
	var area := _edge_area.get_ref() as Control if _edge_area != null else null
	if area != null and is_instance_valid(area) and area.is_visible_in_tree():
		var a := (to_local * area.get_global_rect()).grow(-EDGE_INSET).intersection(inner)
		if a.size.x > _symbol_px and a.size.y > _symbol_px:
			edges = a
	var entries: Array[Dictionary] = []
	var n := _markers.size() / STRIDE
	for i in n:
		var id := int(_markers[i * STRIDE])
		if not _info.has(id):
			continue
		var p := to_local * Vector2(_markers[i * STRIDE + 1], _markers[i * STRIDE + 2])
		if not p.is_finite():
			continue
		var on := _markers[i * STRIDE + 3] > 0.5 and inner.has_point(p) and not _inside_any(p, blocked)
		var edge := Vector2.ZERO
		if not on:
			var c := edges.get_center()
			var clamped := Vector2(clampf(p.x, edges.position.x, edges.end.x), clampf(p.y, edges.position.y, edges.end.y))
			edge = (p - c).normalized() if (p - c).length() > 0.5 else Vector2.UP
			p = _clear_of(clamped, blocked, c)
		var info: Dictionary = _info[id]
		var rank := float(_markers[i * STRIDE + 5])
		if bool(info["selected"]):
			rank = -1.0
		entries.append({"id": id, "pos": p, "on": on, "edge": edge, "rank": rank, "range": float(_markers[i * STRIDE + 4]), "info": info})
	entries.sort_custom(func(a: Dictionary, b: Dictionary) -> bool:
		if a["rank"] != b["rank"]:
			return float(a["rank"]) < float(b["rank"])
		return float(a["range"]) < float(b["range"]))
	var half := _symbol_px * 0.5
	var symbols: Array[Rect2] = []
	for e: Dictionary in entries:
		symbols.append(Rect2(Vector2(e["pos"]) - Vector2(half, half), Vector2(_symbol_px, _symbol_px)))
	var placed: Array[Rect2] = []
	var shown: Array[bool] = []
	for i in entries.size():
		var r := _label_rect(entries[i])
		var ok := rect.encloses(r) and not _touches_any(r, blocked)
		if not ok:
			# Beside a cluster on its right, the label flips to the symbol's left.
			var flipped := _label_rect(entries[i], true)
			if rect.encloses(flipped) and not _touches_any(flipped, blocked):
				r = flipped
				ok = true
		entries[i]["label"] = r
		if ok:
			for q: Rect2 in placed:
				if q.intersects(r):
					ok = false
					break
		if ok:
			for j in symbols.size():
				if j != i and symbols[j].intersects(r):
					ok = false
					break
		shown.append(ok)
		if ok:
			placed.append(r)
	return [entries, shown]


static func _inside_any(p: Vector2, rects: Array[Rect2]) -> bool:
	for r: Rect2 in rects:
		if r.has_point(p):
			return true
	return false


static func _touches_any(box: Rect2, rects: Array[Rect2]) -> bool:
	for r: Rect2 in rects:
		if r.intersects(box):
			return true
	return false


## Move an edge marker that a cluster covers to that cluster's side facing the open sea: across
## to its inner edge for a dock at the side of the screen, else above or below it.
func _clear_of(p: Vector2, rects: Array[Rect2], centre: Vector2) -> Vector2:
	var out := p
	for pass_i in 3:
		var moved := false
		for r: Rect2 in rects:
			if not r.grow(_symbol_px * 0.5).has_point(out):
				continue
			var g := r.grow(_symbol_px * 0.5 + float(Ui.spacing(1)))
			var dx := g.end.x - out.x if centre.x > r.get_center().x else g.position.x - out.x
			var dy := g.end.y - out.y if centre.y > r.get_center().y else g.position.y - out.y
			if absf(dx) <= absf(dy):
				out.x += dx
			else:
				out.y += dy
			moved = true
		if not moved:
			break
	return out


## What a symbol looks like: stance, domain, trend and greyed.
static func _look(e: Dictionary) -> int:
	var info: Dictionary = e["info"]
	return int(info["stance"]) * 1000 + int(info["domain"]) * 100 + (int(info["trend"]) + 1) * 10 + (1 if bool(info["last_known"]) else 0)


func _label_lines(e: Dictionary) -> PackedStringArray:
	var info: Dictionary = e["info"]
	var lines: PackedStringArray = (info["lines"] as PackedStringArray).duplicate()
	if not bool(e["on"]) and float(e["range"]) >= 0.0:
		lines[0] = tr(&"hud_tracks.overlay.label_range").format([lines[0], _tx(&"range", [float(e["range"])])])
	return lines


func _label_size(e: Dictionary) -> Vector2:
	var info: Dictionary = e["info"]
	var widths: PackedFloat32Array = info["widths"]
	var w := 0.0
	for x: float in widths:
		w = maxf(w, x)
	if not bool(e["on"]) and float(e["range"]) >= 0.0:
		var lines := _label_lines(e)
		w = maxf(w, _font.get_string_size(lines[0], HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size).x if _font != null else w)
	var pad := float(Ui.spacing(0))
	return Vector2(w + pad * 2.0, _line_h * float(widths.size()) + pad)


## Labels sit right of the symbol; at the right edge they flip left.
func _label_rect(e: Dictionary, flip: bool = false) -> Rect2:
	var p: Vector2 = e["pos"]
	var s := _label_size(e)
	var half := _symbol_px * 0.5
	var at := Vector2(p.x + half + LABEL_GAP, p.y - s.y * 0.5)
	if flip or at.x + s.x > size.x:
		at.x = p.x - half - LABEL_GAP - s.x
	return Rect2(at, s)


func _draw_symbol(e: Dictionary) -> void:
	var info: Dictionary = e["info"]
	var stance := int(info["stance"])
	var domain := int(info["domain"])
	var colour := get_theme_color(&"text_secondary", THEME_TYPE) if bool(info["last_known"]) else get_theme_color(STANCE_ITEMS[stance], THEME_TYPE)
	var outline := get_theme_color(&"label_outline", THEME_TYPE)
	var k := _symbol_px / GRID
	var origin: Vector2 = Vector2(e["pos"]) - Vector2(_symbol_px, _symbol_px) * 0.5
	var pts := _Stance.frame_points(stance)
	for i in pts.size():
		pts[i] = origin + pts[i] * k
	var under := _stroke + float(_outline_size)
	draw_polyline(pts, outline, under, true)
	draw_polyline(pts, colour, _stroke, true)
	match domain:
		Bp.DOMAIN_AIR:
			_line_pair(origin + Vector2(7, 2) * k, origin + Vector2(17, 2) * k, colour, outline, under)
		Bp.DOMAIN_SUBSURFACE:
			_line_pair(origin + Vector2(7, 22) * k, origin + Vector2(17, 22) * k, colour, outline, under)
		Bp.DOMAIN_WEAPON:
			var c := origin + _Stance.centre_mark(stance) * k
			draw_circle(c, 2.0 * k + float(_outline_size) * 0.5, outline, true, -1.0, true)
			draw_circle(c, 2.0 * k, colour, true, -1.0, true)
	var trend := int(info["trend"])
	if trend >= 0:
		var t := origin + Vector2(GRID, 0.0) * k
		var glyph: PackedVector2Array
		match trend:
			Bp.TREND_CLIMBING:
				glyph = PackedVector2Array([Vector2(7, 15), Vector2(12, 9), Vector2(17, 15)])
			Bp.TREND_DIVING:
				glyph = PackedVector2Array([Vector2(7, 9), Vector2(12, 15), Vector2(17, 9)])
			_:
				glyph = PackedVector2Array([Vector2(7, 12), Vector2(17, 12)])
		for i in glyph.size():
			glyph[i] = t + (glyph[i] - Vector2(12, 12)) * 0.5 * k + Vector2(6, 12) * k
		draw_polyline(glyph, outline, under, true)
		draw_polyline(glyph, colour, _stroke, true)
	if not bool(e["on"]):
		_draw_chevron(Vector2(e["pos"]), Vector2(e["edge"]), colour, outline, under)
	if bool(info["selected"]):
		_draw_bracket(Rect2(origin, Vector2(_symbol_px, _symbol_px)).grow(4.0))


func _line_pair(a: Vector2, b: Vector2, colour: Color, outline: Color, under: float) -> void:
	draw_line(a, b, outline, under, true)
	draw_line(a, b, colour, _stroke, true)


## A chevron just outside the symbol, pointing at where the track is.
func _draw_chevron(p: Vector2, dir: Vector2, colour: Color, outline: Color, under: float) -> void:
	var d := dir.normalized()
	var n := Vector2(-d.y, d.x)
	var tip := p + d * (_symbol_px * 0.5 + 10.0)
	var back := tip - d * 7.0
	var pts := PackedVector2Array([back + n * 6.0, tip, back - n * 6.0])
	draw_polyline(pts, outline, under, true)
	draw_polyline(pts, colour, _stroke, true)


func _draw_bracket(r: Rect2) -> void:
	var c := get_theme_color(&"select", THEME_TYPE)
	var w := float(get_theme_constant(&"stroke_selection", THEME_TYPE)) if has_theme_constant(&"stroke_selection", THEME_TYPE) else 3.0
	var arm := r.size.x * 0.3
	var a := r.grow(-w * 0.5)
	for corner: Vector2 in [a.position, Vector2(a.end.x, a.position.y), a.end, Vector2(a.position.x, a.end.y)]:
		var sx := 1.0 if corner.x <= a.get_center().x else -1.0
		var sy := 1.0 if corner.y <= a.get_center().y else -1.0
		draw_polyline(PackedVector2Array([corner + Vector2(0, sy * arm), corner, corner + Vector2(sx * arm, 0)]), c, w)


func _draw_label(e: Dictionary, at: Vector2) -> void:
	if _font == null:
		return
	var lines := _label_lines(e)
	var s := _label_size(e)
	var back := get_theme_color(&"surface_base", THEME_TYPE)
	back.a *= _backing
	if back.a > 0.0:
		draw_rect(Rect2(at, s), back, true)
	var info: Dictionary = e["info"]
	var ink := get_theme_color(&"text_secondary", THEME_TYPE) if bool(info["last_known"]) else get_theme_color(&"label", THEME_TYPE)
	var outline := get_theme_color(&"label_outline", THEME_TYPE)
	var pad := float(Ui.spacing(0))
	var ascent := _font.get_ascent(_font_size)
	for i in lines.size():
		var p := at + Vector2(pad, pad * 0.5 + ascent + _line_h * float(i))
		if _outline_size > 0:
			draw_string_outline(_font, p, lines[i], HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size, _outline_size, outline)
		draw_string(_font, p, lines[i], HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size, ink)


## Hidden labels of the symbols under the pointer, stacked beside it (rule 14: hover shows them).
func _draw_hover(entries: Array[Dictionary], shown: Array[bool]) -> void:
	if not Rect2(Vector2.ZERO, size).has_point(_hover):
		return
	var at := _hover + Vector2(16.0, 16.0)
	for i in entries.size():
		if shown[i] or Vector2(entries[i]["pos"]).distance_to(_hover) > HOVER_PX:
			continue
		var s := _label_size(entries[i])
		if at.x + s.x > size.x:
			at.x = maxf(0.0, _hover.x - 16.0 - s.x)
		_draw_label(entries[i], at)
		at.y += s.y + 2.0


func _word(kind: StringName, value: int) -> String:
	var v: Variant = Kit.call_on(&"Tx", &"word", [kind, value])
	return String(v) if v is String or v is StringName else ""


func _tx(method: StringName, args: Array) -> String:
	var v: Variant = Kit.call_on(&"Tx", method, args)
	return String(v) if v is String or v is StringName else ""
