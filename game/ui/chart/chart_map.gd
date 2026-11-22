extends Control
## The theatre chart, a _draw() control of theme type Chart: nodes, lanes, regions, SEXTANT, contacts, weather.
## DESIGN.md L741-751, L1735. Unit: ui-chart.
## Reads and calls: Content.theatre_graph, Campaign.chart, Campaign.transit_preview, Campaign.hour_advanced, Campaign.theatre_event.
##
## The one light surface (L1561): every colour is a Chart theme item, the exact NOAA Chart 18649
## entries (L1643). Only ink and overlay.text carry text; overlay draws routes and lanes as lines;
## overlay.tint marks closed zones with a hatch, never a flat fill. Status is always a word, a
## symbol or a pattern, never hue alone (L1722). Focus and selection are drawn in ink, because
## ui.focus and select are white and vanish on water.deep.
##
## Layers: a sheet clipped to the neatline holds the chart itself, in two layers: a static one (water,
## land, shallows, region boundaries and the closed-region hatch), redrawn only on resize, theme change
## or a region opening, under the dynamic one (lanes, nodes, SEXTANT, contacts, offers, routes, labels,
## boxes), redrawn on data change, hover and the short contact easing. This control draws the neatline,
## the scale bar and the legend under it. Nothing is drawn per frame while the data is still. The sheet
## shows the whole theatre with a margin of open water, so no place sits on the neatline.
##
## Input: the mouse hovers and clicks nodes (double-click activates); the keyboard and gamepad move
## the selection with the focus directions to the nearest node that way (a direction with no node
## leaves focus to the neighbour control) and `ui_accept` activates. The table decides what
## activation does.
##
## Feed it through `bind(source)` and `refresh()`; the source is res://ui/chart/chart_source.gd or
## a fixture with the same readers.

signal node_selected(node: StringName)
signal node_activated(node: StringName)

const Kit := preload("res://ui/screens/menu_kit.gd")
const THEME_TYPE := &"Chart"

## Pixels around the neatline, and the gap between the neatline and the legend.
const MARGIN := 16.0
## A node under the pointer within this many pixels is hovered.
const PICK_PX := 16.0
## Half-size of a node symbol.
const SYMBOL := 7.0
## Ring radius around a node with an operation on offer.
const OFFER_RING := 14.0
## Spacing of the closed-region hatch: sparse, so a closed region reads as out of play without
## drowning the open one.
const HATCH_PX := 28.0
## Open water drawn around the theatre, km, so places at its edge are not on the neatline.
const VIEW_PAD_KM := 14.0
## Contacts last seen longer ago than this are drawn as outlines with their age (L1664).
const STALE_H := 6
## Kinds of node in legend order.
const KINDS: Array[StringName] = [&"cove", &"shipyard", &"depot", &"base", &"harbour", &"radar_site", &"esm_site", &"sea"]
## `Bp.PHASE_ADVANCING`, kept so the script parses before the extension loads.
const PHASE_ADVANCING := 2
## Label priority by kind (lower places first and wins overlaps).
const KIND_RANK: Dictionary[StringName, int] = {
	&"cove": 3, &"base": 5, &"shipyard": 5, &"depot": 5, &"harbour": 5, &"radar_site": 6, &"esm_site": 6, &"sea": 7,
}

var _source: RefCounted
var _graph: Dictionary = {}
var _chart: Dictionary = {}
var _nodes: Dictionary[StringName, Dictionary] = {}
var _node_ids: Array[StringName] = []
var _lanes: Array[Dictionary] = []
var _region_outlines: Dictionary[StringName, PackedVector2Array] = {}
var _region_anchor: Dictionary[StringName, Vector2] = {}
## Placed region label blocks in pixels, settled on layout and chart change.
var _region_rects: Dictionary[StringName, Rect2] = {}
var _regions_sig := ""
var _land: Array[PackedVector2Array] = []
var _shallows: Array[PackedVector2Array] = []
var _size_km := Vector2(600.0, 400.0)

var _selected: StringName = &""
var _hover: StringName = &""
var _preview: Array[StringName] = []
var _passage: Array[StringName] = []
var _passage_fraction := 0.0

## Contact easing between chart states: positions in km, and the eased fraction.
var _contact_from: Dictionary[int, Vector2] = {}
var _contact_to: Dictionary[int, Vector2] = {}
var _anim := 1.0
var _tween: Tween

## Mode-change boxes (L1649): key ("n:<node>", "l:<a>|<b>", "r:<region>") to expiry in ms.
var _boxes: Dictionary[String, int] = {}
var _box_timer: Timer

## Pixel transform: chart km to local pixels.
var _k := 1.0
var _origin := Vector2.ZERO
var _neat := Rect2()
## The chart's sheet (clipped to the neatline) and the canvas item the drawing helpers draw on now.
var _sheet: Control
var _ci: CanvasItem
var _static_layer: Control
var _closed_signature := ""
var _skipped_polygons := 0
## Cached pixel geometry of the static layer.
var _land_px: Array[PackedVector2Array] = []
var _coast_px: Array[PackedVector2Array] = []
var _shallows_px: Array[PackedVector2Array] = []
var _hatch_px: Array[PackedVector2Array] = []


func _ready() -> void:
	focus_mode = Control.FOCUS_ALL
	mouse_filter = Control.MOUSE_FILTER_STOP
	clip_contents = true
	auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	accessibility_name = tr(&"chart.map.name")
	tooltip_text = " "
	_ci = self
	_sheet = Control.new()
	_sheet.name = &"Sheet"
	_sheet.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_sheet.clip_contents = true
	_sheet.draw.connect(_draw_sheet)
	add_child(_sheet)
	_static_layer = Control.new()
	_static_layer.name = &"StaticLayer"
	_static_layer.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_static_layer.set_anchors_preset(Control.PRESET_FULL_RECT)
	_static_layer.show_behind_parent = true
	_static_layer.draw.connect(_draw_static)
	_sheet.add_child(_static_layer)
	_box_timer = Timer.new()
	_box_timer.one_shot = true
	_box_timer.timeout.connect(_on_box_timer)
	add_child(_box_timer)
	resized.connect(_relayout)
	focus_entered.connect(queue_redraw)
	focus_exited.connect(queue_redraw)
	_update_accessibility()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			if is_node_ready():
				_relayout()
		NOTIFICATION_TRANSLATION_CHANGED:
			if is_node_ready():
				accessibility_name = tr(&"chart.map.name")
				_update_accessibility()
				_redraw()
		NOTIFICATION_MOUSE_EXIT:
			if _hover != &"":
				_hover = &""
				_redraw()


## Read from `source` (res://ui/chart/chart_source.gd or a fixture) from now on.
func bind(source: RefCounted) -> void:
	_source = source
	_graph = {}


## Pull the graph (once) and the chart state, then redraw. Called on `Campaign.chart_changed`.
func refresh() -> void:
	if _source == null:
		return
	if _graph.is_empty():
		_set_graph(_source.call(&"graph"))
	_set_chart(_source.call(&"chart"))


## The node the selection bracket is on (`&""` for none).
func selected() -> StringName:
	return _selected


func has_node_id(node: StringName) -> bool:
	return _nodes.has(node)


## Put the selection on `node` (`&""` clears it). Emits `node_selected` only when `notify` is true.
func select(node: StringName, notify: bool = false) -> void:
	if node != &"" and not _nodes.has(node):
		node = &""
	if node == _selected:
		return
	_selected = node
	_place_regions()
	_update_accessibility()
	_redraw()
	if notify:
		node_selected.emit(node)


## The transit preview route (node ids from SEXTANT to the target); empty hides it.
func set_preview(path: Array) -> void:
	_preview = _ids(path)
	_redraw()


## A committed passage: the route and how far along it SEXTANT is (0..1). Empty clears it.
func set_passage(path: Array, fraction: float) -> void:
	_passage = _ids(path)
	_passage_fraction = clampf(fraction if is_finite(fraction) else 0.0, 0.0, 1.0)
	_redraw()


## Box a node, lane or region for the mode-change time (L1649): `kind` is `&"node"`, `&"lane"`
## or `&"region"`; a lane is `a` and `b`.
func flash(kind: StringName, a: StringName, b: StringName = &"") -> void:
	var key := ""
	match kind:
		&"node":
			if not _nodes.has(a):
				return
			key = "n:" + String(a)
		&"lane":
			if not (_nodes.has(a) and _nodes.has(b)):
				return
			key = "l:" + _lane_key(a, b)
		&"region":
			if not _region_anchor.has(a):
				return
			key = "r:" + String(a)
		_:
			return
	_boxes[key] = Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0)
	_arm_box_timer()
	_redraw()


## Where a node is drawn in this control's pixels; NAN when it is not on the chart.
func node_position(node: StringName) -> Vector2:
	if not _nodes.has(node) or _k <= 0.0:
		return Vector2(NAN, NAN)
	return _px(_node_km(node))


## How many mode-change boxes are up now (for tests).
func box_count() -> int:
	var now := Time.get_ticks_msec()
	var n := 0
	for key: String in _boxes:
		if _boxes[key] > now:
			n += 1
	return n


# --- data ----------------------------------------------------------------------------------------

func _set_graph(g: Variant) -> void:
	_graph = Kit.as_dict(g)
	_nodes.clear()
	_node_ids.clear()
	_lanes.clear()
	_region_outlines.clear()
	_region_anchor.clear()
	_land.clear()
	_shallows.clear()
	var sz: Variant = _graph.get("size_km")
	if sz is Vector2 and (sz as Vector2).x > 0.0 and (sz as Vector2).y > 0.0 and (sz as Vector2).is_finite():
		_size_km = sz
	for n: Dictionary in Kit.dicts(_graph.get("nodes")):
		var id := StringName(Kit.get_str(n, "id"))
		var pos: Variant = n.get("pos")
		if id == &"" or not pos is Vector2 or not (pos as Vector2).is_finite():
			continue
		_nodes[id] = n
		_node_ids.append(id)
	var seen: Dictionary[String, bool] = {}
	for l: Dictionary in Kit.dicts(_graph.get("lanes")):
		var a := StringName(Kit.get_str(l, "a"))
		var b := StringName(Kit.get_str(l, "b"))
		var key := _lane_key(a, b)
		if _nodes.has(a) and _nodes.has(b) and a != b and not seen.has(key):
			seen[key] = true
			_lanes.append({"a": a, "b": b})
	for r: Dictionary in Kit.dicts(_graph.get("regions")):
		var id := StringName(Kit.get_str(r, "id"))
		var outline: Variant = r.get("outline")
		if id == &"":
			continue
		var poly := PackedVector2Array(outline) if outline is PackedVector2Array else PackedVector2Array()
		_region_outlines[id] = poly
		_region_anchor[id] = _region_label_anchor(id, poly)
	_regions_sig = ""
	_land = _polys(_graph.get("land"))
	_shallows = _polys(_graph.get("shallows"))
	if _selected != &"" and not _nodes.has(_selected):
		_selected = &""
	_relayout()


func _set_chart(c: Variant) -> void:
	var old_display: Dictionary[int, Vector2] = {}
	for id: int in _contact_to:
		old_display[id] = _contact_display_km(id)
	_chart = Kit.as_dict(c)
	_contact_from.clear()
	_contact_to.clear()
	var moved := false
	for ct: Dictionary in Kit.dicts(_chart.get("contacts")):
		var id := Kit.get_int(ct, "id", -1)
		var node := StringName(Kit.get_str(ct, "node"))
		if not _nodes.has(node):
			continue
		var to := _node_km(node)
		_contact_to[id] = to
		var from: Vector2 = old_display.get(id, to)
		_contact_from[id] = from
		if from.distance_to(to) > 0.01:
			moved = true
	var closed := _closed_regions_signature()
	if closed != _closed_signature:
		_closed_signature = closed
		_rebuild_hatch()
		if _static_layer != null:
			_static_layer.queue_redraw()
	if moved and is_inside_tree() and Ui.motion_s(&"panel") > 0.0:
		if _tween != null and _tween.is_valid():
			_tween.kill()
		_anim = 0.0
		_tween = Ui.tween_for(self, &"Campaign.chart_changed")
		_tween.tween_method(_set_anim, 0.0, 1.0, Ui.motion_s(&"panel"))
	else:
		_anim = 1.0
	_place_regions()
	_update_accessibility()
	_redraw()


func _set_anim(v: float) -> void:
	_anim = v
	_redraw()


## Redraw this control (neatline, legend, focus) and the chart sheet.
func _redraw() -> void:
	queue_redraw()
	if _sheet != null:
		_sheet.queue_redraw()


func _polys(v: Variant) -> Array[PackedVector2Array]:
	var out: Array[PackedVector2Array] = []
	for item: Variant in Kit.as_array(v):
		if item is PackedVector2Array and (item as PackedVector2Array).size() >= 3:
			var ok := true
			for p: Vector2 in item as PackedVector2Array:
				if not p.is_finite():
					ok = false
					break
			if ok:
				out.append(item)
	return out


func _ids(path: Array) -> Array[StringName]:
	var out: Array[StringName] = []
	for item: Variant in path:
		if item is StringName or item is String:
			var id := StringName(item)
			if _nodes.has(id):
				out.append(id)
	return out


func _node_km(id: StringName) -> Vector2:
	return _nodes[id].get("pos", Vector2.ZERO)


func _region_label_anchor(id: StringName, poly: PackedVector2Array) -> Vector2:
	# The mean of the region's nodes sits inside the region and away from its edges; the outline's
	# vertex mean is the fallback for a region with no nodes.
	var sum := Vector2.ZERO
	var n := 0
	for node_id: StringName in _node_ids:
		if StringName(Kit.get_str(_nodes[node_id], "region")) == id:
			sum += _node_km(node_id)
			n += 1
	if n > 0:
		return sum / float(n)
	if poly.is_empty():
		return _size_km * 0.5
	var c := Vector2.ZERO
	for p: Vector2 in poly:
		c += p
	return c / float(poly.size())


func _closed_regions_signature() -> String:
	var parts := PackedStringArray()
	for r: Dictionary in Kit.dicts(_chart.get("regions")):
		if not Kit.get_bool(r, "open", true):
			parts.append(Kit.get_str(r, "id"))
	return ",".join(parts)


# --- layout --------------------------------------------------------------------------------------

func _legend_height(rows: int) -> float:
	return (_line_h() + _c(&"spacing_8")) * rows


## Fit the chart above its legend: more legend rows leave less height, a smaller chart leaves the
## legend less width, so settle the row count in a few rounds.
func _fit(rows: int) -> void:
	var avail := Vector2(size.x - MARGIN * 2.0, size.y - MARGIN * 2.0 - _c(&"spacing_8") - _legend_height(rows))
	var view := _size_km + Vector2.ONE * VIEW_PAD_KM * 2.0
	if avail.x <= 1.0 or avail.y <= 1.0:
		_k = 0.0
	else:
		_k = minf(avail.x / view.x, avail.y / view.y)
	var view_px := view * _k
	# Centred across, top-aligned: on a tall, narrow layout the chart stays in view at the top.
	var top_left := Vector2(floorf((size.x - view_px.x) * 0.5), MARGIN)
	_neat = Rect2(top_left, view_px)
	_origin = top_left + Vector2.ONE * VIEW_PAD_KM * _k


func _relayout() -> void:
	if not is_node_ready():
		return
	# The legend runs the full width under the chart, so its height does not depend on the chart's
	# scale; the scale bar's room at the end of its first row is the most the bar can take.
	_fit(_legend_rows(_legend_width(), _scale_bar_reserve()))
	_land_px.clear()
	_coast_px.clear()
	_shallows_px.clear()
	_skipped_polygons = 0
	if _k > 0.0:
		for poly: PackedVector2Array in _land:
			var p := _px_poly(poly)
			var ring := p.duplicate()
			ring.append(p[0])
			_coast_px.append(ring)
			if _fillable(p):
				_land_px.append(p)
		for poly: PackedVector2Array in _shallows:
			var p := _px_poly(poly)
			if _fillable(p):
				_shallows_px.append(p)
	_rebuild_hatch()
	_place_regions()
	if _sheet != null:
		_sheet.position = _neat.position
		_sheet.size = _neat.size
		_sheet.queue_redraw()
	if _static_layer != null:
		_static_layer.queue_redraw()
	_redraw()


func _fillable(p: PackedVector2Array) -> bool:
	if Geometry2D.triangulate_polygon(p).is_empty():
		_skipped_polygons += 1
		return false
	return true


## Polygons that would not fill and are drawn as outlines only (for tests).
func skipped_polygons() -> int:
	return _skipped_polygons


func _rebuild_hatch() -> void:
	_hatch_px.clear()
	if _k <= 0.0:
		return
	for r: Dictionary in Kit.dicts(_chart.get("regions")):
		if Kit.get_bool(r, "open", true):
			continue
		var poly: PackedVector2Array = _region_outlines.get(StringName(Kit.get_str(r, "id")), PackedVector2Array())
		if poly.size() < 3:
			continue
		var px := _px_poly(poly)
		var bounds := Rect2(px[0], Vector2.ZERO)
		for p: Vector2 in px:
			bounds = bounds.expand(p)
		# 45 degree lines across the bounds, clipped to the region.
		var span := bounds.size.x + bounds.size.y
		var t := 0.0
		while t <= span:
			var a := Vector2(bounds.position.x + t, bounds.position.y)
			var b := a + Vector2(-span, span)
			for seg: PackedVector2Array in Geometry2D.intersect_polyline_with_polygon(PackedVector2Array([a, b]), px):
				_hatch_px.append(seg)
			t += HATCH_PX


func _px(km: Vector2) -> Vector2:
	return _origin + km * _k


func _px_poly(poly: PackedVector2Array) -> PackedVector2Array:
	var out := PackedVector2Array()
	out.resize(poly.size())
	for i in poly.size():
		out[i] = _px(poly[i])
	return out


# --- theme ---------------------------------------------------------------------------------------

func _col(item: StringName) -> Color:
	return get_theme_color(item, THEME_TYPE)


func _c(item: StringName) -> float:
	return float(get_theme_constant(item, THEME_TYPE))


func _font(bold: bool = false) -> Font:
	return get_theme_font(&"label_bold" if bold else &"label", THEME_TYPE)


func _font_size() -> int:
	return get_theme_font_size(&"label", THEME_TYPE)


func _line_h() -> float:
	var f := _font()
	return f.get_height(_font_size()) if f != null else 24.0


# --- static layer --------------------------------------------------------------------------------

func _draw_static() -> void:
	var layer := _static_layer
	var ink := _col(&"ink")
	# The layer sits on the sheet at the neatline; draw in this control's pixels.
	layer.draw_set_transform(-_neat.position)
	layer.draw_rect(_neat, _col(&"water_deep"))
	if _k <= 0.0:
		return
	for p: PackedVector2Array in _shallows_px:
		layer.draw_colored_polygon(p, _col(&"water_shallow"))
	for p: PackedVector2Array in _land_px:
		layer.draw_colored_polygon(p, _col(&"land"))
	# Coastlines in ink, every polygon including any that would not fill.
	for ring: PackedVector2Array in _coast_px:
		layer.draw_polyline(ring, ink, _c(&"stroke_rule"), true)
	for seg: PackedVector2Array in _hatch_px:
		if seg.size() >= 2:
			layer.draw_polyline(seg, _col(&"overlay_tint"), _c(&"stroke_rule"), true)
	# Region boundaries: dashed overlay lines (zones are lines and labels on a chart, L1643). Only the
	# borders between regions: an outline's stretches along the theatre's edge are open sea.
	for id: StringName in _region_outlines:
		var km := _region_outlines[id]
		for i in km.size():
			var a := km[i]
			var b := km[(i + 1) % km.size()]
			if _on_edge(a, b):
				continue
			layer.draw_dashed_line(_px(a), _px(b), _col(&"overlay"), _c(&"stroke_rule"), _c(&"spacing_8"), true)



## True when the outline edge `a`-`b` (chart km) runs along the theatre's edge.
func _on_edge(a: Vector2, b: Vector2) -> bool:
	var e := 0.01
	return (absf(a.x) < e and absf(b.x) < e) or (absf(a.y) < e and absf(b.y) < e) \
		or (absf(a.x - _size_km.x) < e and absf(b.x - _size_km.x) < e) or (absf(a.y - _size_km.y) < e and absf(b.y - _size_km.y) < e)


## The scale bar's length in km: the longest round length that fits a fifth of the chart.
func _scale_km() -> float:
	var km := 10.0
	for step: float in [10.0, 20.0, 25.0, 50.0, 100.0]:
		if step * _k <= _legend_width() * 0.2:
			km = step
	return km


func _legend_width() -> float:
	return maxf(0.0, size.x - MARGIN * 2.0)


## Room kept at the right end of the legend's first row for the scale bar: a fifth of the width
## (the bar's longest) and its label.
func _scale_bar_reserve() -> float:
	return _legend_width() * 0.2 + _text_w(Kit.number(100.0, &"km", 0)) + _c(&"spacing_12") + _c(&"spacing_24")


## The scale bar under the neatline's right end: a labelled reference (HUD rule 13).
func _draw_scale_bar(layer: Control) -> void:
	var font := _font()
	if font == null or _k <= 0.0:
		return
	var km := _scale_km()
	var ink := _col(&"ink")
	var y := _neat.end.y + MARGIN + _line_h() * 0.5 + 4.0
	var right := Vector2(size.x - MARGIN, y)
	var left := right - Vector2(km * _k, 0.0)
	var tick := _c(&"spacing_8")
	layer.draw_line(left, right, ink, _c(&"stroke_frame"))
	layer.draw_line(left, left - Vector2(0.0, tick), ink, _c(&"stroke_frame"))
	layer.draw_line(right, right - Vector2(0.0, tick), ink, _c(&"stroke_frame"))
	var text := Kit.number(km, &"km", 0)
	var fs := _font_size()
	var at := Vector2(left.x - _c(&"spacing_12") - _text_w(text), _neat.end.y + MARGIN + font.get_ascent(fs))
	layer.draw_string(font, at, text, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, ink)


# --- dynamic layer -------------------------------------------------------------------------------

func _draw() -> void:
	_ci = self
	# The whole control is the chart's paper: the legend and scale bar sit on it under the neatline.
	draw_rect(Rect2(Vector2.ZERO, size), _col(&"water_deep"))
	if _nodes.is_empty():
		_draw_empty()
		return
	if _k <= 0.0:
		return
	# Scale bar: a labelled reference (HUD rule 13). The neatline is drawn on the sheet.
	_draw_scale_bar(self)
	_draw_legend()
	if has_focus() and not Ui.using_mouse:
		var w := _c(&"stroke_focus")
		_ci.draw_rect(Rect2(Vector2.ONE * w, size - Vector2.ONE * w * 2.0), _col(&"ink"), false, w)


## The chart inside the neatline, drawn on the clipped sheet in this control's pixels.
func _draw_sheet() -> void:
	if _nodes.is_empty() or _k <= 0.0:
		return
	_ci = _sheet
	_sheet.draw_set_transform(-_neat.position)
	var labels: Array[Dictionary] = []
	_draw_lanes(labels)
	_draw_routes()
	_draw_nodes(labels)
	_draw_offers(labels)
	_draw_contacts(labels)
	_draw_sextant(labels)
	_draw_marks()
	_draw_regions(labels)
	_draw_labels(labels)
	# The neatline over everything on the sheet, just inside its clip.
	var w := _c(&"stroke_rule")
	_sheet.draw_rect(_neat.grow(-w * 0.5), _col(&"ink"), false, w)
	_ci = self


func _draw_empty() -> void:
	var font := _font()
	if font == null:
		return
	var text := tr(&"chart.map.empty")
	_ci.draw_string(font, Vector2(MARGIN, size.y * 0.5), text, HORIZONTAL_ALIGNMENT_CENTER, maxf(1.0, size.x - MARGIN * 2.0), _font_size(), _col(&"ink"))


func _cut_lanes() -> Dictionary[String, int]:
	var out: Dictionary[String, int] = {}
	for c: Dictionary in Kit.dicts(_chart.get("cut_lanes")):
		out[_lane_key(StringName(Kit.get_str(c, "a")), StringName(Kit.get_str(c, "b")))] = Kit.get_int(c, "until_h")
	return out


static func _lane_key(a: StringName, b: StringName) -> String:
	return "%s|%s" % [a, b] if String(a) < String(b) else "%s|%s" % [b, a]


func _hour() -> int:
	return Kit.get_int(_chart, "hour")


func _draw_lanes(labels: Array[Dictionary]) -> void:
	var cut := _cut_lanes()
	var overlay := _col(&"overlay")
	var ink := _col(&"ink")
	for lane: Dictionary in _lanes:
		var a := _px(_node_km(lane["a"]))
		var b := _px(_node_km(lane["b"]))
		var key := _lane_key(lane["a"], lane["b"])
		if not cut.has(key):
			_ci.draw_line(a, b, overlay, _c(&"stroke_rule"), true)
			continue
		# A cut lane: dashed, crossed at its middle, and labelled with the days left (L749).
		_ci.draw_dashed_line(a, b, overlay, _c(&"stroke_frame"), _c(&"spacing_8"), true)
		var mid := (a + b) * 0.5
		var dir := (b - a).normalized() if a.distance_to(b) > 0.0 else Vector2.RIGHT
		var normal := Vector2(-dir.y, dir.x) * _c(&"spacing_8")
		for off: float in [-3.0, 3.0]:
			var o := dir * off
			_ci.draw_line(mid + o - normal, mid + o + normal, ink, _c(&"stroke_frame"), true)
		var days := maxi(0, ceili(float(cut[key] - _hour()) / 24.0))
		labels.append(_label(mid, [{"text": tr(&"chart.map.cut_lane").format([Kit.number(days, &"none", 0)]), "ink": true}], 4, false, _c(&"spacing_12")))


func _draw_routes() -> void:
	var overlay := _col(&"overlay")
	if _preview.size() >= 2:
		var pts := PackedVector2Array()
		for id: StringName in _preview:
			pts.append(_px(_node_km(id)))
		_ci.draw_polyline(pts, overlay, _c(&"stroke_selection"), true)
		_draw_arrow_head(pts[pts.size() - 2], pts[pts.size() - 1], overlay)
	if _passage.size() >= 2:
		var pts := _passage_points()
		var rest := PackedVector2Array([_passage_position()])
		for i in range(_passage_index_after(), pts.size()):
			rest.append(pts[i])
		for i in rest.size() - 1:
			_ci.draw_dashed_line(rest[i], rest[i + 1], overlay, _c(&"stroke_selection"), _c(&"spacing_12"), true)
		if rest.size() >= 2:
			_draw_arrow_head(rest[rest.size() - 2], rest[rest.size() - 1], overlay)
	for ct: Dictionary in Kit.dicts(_chart.get("contacts")):
		var dest := StringName(Kit.get_str(ct, "destination"))
		var id := Kit.get_int(ct, "id", -1)
		if dest == &"" or not _nodes.has(dest) or not _contact_to.has(id):
			continue
		var from := _px(_contact_display_km(id))
		var to := _px(_node_km(dest))
		if from.distance_to(to) < SYMBOL * 2.0:
			continue
		_ci.draw_dashed_line(from, to, overlay, _c(&"stroke_rule"), _c(&"spacing_4"), true)
		_draw_arrow_head(from, to, overlay)


func _draw_arrow_head(from: Vector2, to: Vector2, colour: Color) -> void:
	if from.distance_to(to) < SYMBOL * 2.0:
		return
	var dir := (to - from).normalized()
	var tip := to - dir * SYMBOL
	var back := tip - dir * _c(&"spacing_8")
	var side := Vector2(-dir.y, dir.x) * _c(&"spacing_4")
	_ci.draw_colored_polygon(PackedVector2Array([tip, back + side, back - side]), colour)


func _passage_points() -> PackedVector2Array:
	var pts := PackedVector2Array()
	for id: StringName in _passage:
		pts.append(_px(_node_km(id)))
	return pts


func _passage_length(pts: PackedVector2Array) -> float:
	var total := 0.0
	for i in pts.size() - 1:
		total += pts[i].distance_to(pts[i + 1])
	return total


## Where SEXTANT is along the committed route, by length.
func _passage_position() -> Vector2:
	var pts := _passage_points()
	if pts.is_empty():
		return Vector2(NAN, NAN)
	var want := _passage_length(pts) * _passage_fraction
	for i in pts.size() - 1:
		var seg := pts[i].distance_to(pts[i + 1])
		if want <= seg and seg > 0.0:
			return pts[i].lerp(pts[i + 1], want / seg)
		want -= seg
	return pts[pts.size() - 1]


## Index of the first route node still ahead of SEXTANT.
func _passage_index_after() -> int:
	var pts := _passage_points()
	var want := _passage_length(pts) * _passage_fraction
	var run := 0.0
	for i in pts.size():
		if i > 0:
			run += pts[i - 1].distance_to(pts[i])
		if run > want + 0.01:
			return i
	return pts.size()


func _open_coves() -> Array[StringName]:
	var out: Array[StringName] = []
	for c: Variant in Kit.as_array(_chart.get("coves")):
		if c is StringName or c is String:
			out.append(StringName(c))
	return out


func _draw_nodes(labels: Array[Dictionary]) -> void:
	var open := _open_coves()
	var closed_regions: Array[StringName] = []
	for r: Dictionary in Kit.dicts(_chart.get("regions")):
		if not Kit.get_bool(r, "open", true):
			closed_regions.append(StringName(Kit.get_str(r, "id")))
	var ink := _col(&"ink")
	for id: StringName in _node_ids:
		var n := _nodes[id]
		var kind := StringName(Kit.get_str(n, "kind"))
		var at := _px(_node_km(id))
		var closed := kind == &"cove" and not open.has(id) and not _chart.is_empty()
		draw_symbol(_ci, kind, at, ink, _c(&"stroke_frame"), closed)
		var rank: int = KIND_RANK.get(kind, 7)
		if id == _selected:
			rank = 0
		elif id == _hover:
			rank = 1
		# Sea areas and places in a closed region carry no name until pointed at; the overview
		# stays sparse (HUD rule 14).
		var region_closed := closed_regions.has(StringName(Kit.get_str(n, "region")))
		if (kind == &"sea" or region_closed) and rank > 1:
			continue
		var name_text := _source_name(n)
		if closed and not region_closed:
			name_text = tr(&"chart.map.cove_closed").format([name_text])
		labels.append(_label(at, [{"text": name_text, "ink": true}], rank, rank <= 1, OFFER_RING + _c(&"spacing_4")))


func _source_name(n: Dictionary) -> String:
	return String(TranslationServer.translate(StringName(Kit.get_str(n, "name", Kit.get_str(n, "id")))))


func _offers_by_node() -> Dictionary[StringName, Array]:
	var out: Dictionary[StringName, Array] = {}
	for o: Dictionary in Kit.dicts(_chart.get("offers")):
		var node := StringName(Kit.get_str(o, "node"))
		if not _nodes.has(node):
			continue
		if not out.has(node):
			out[node] = []
		out[node].append(o)
	return out


func _draw_offers(labels: Array[Dictionary]) -> void:
	var by_node := _offers_by_node()
	var overlay := _col(&"overlay")
	for node: StringName in by_node:
		var at := _px(_node_km(node))
		var offers: Array = by_node[node]
		var forced := false
		for o: Dictionary in offers:
			forced = forced or Kit.get_bool(o, "forced")
		_ci.draw_arc(at, OFFER_RING, 0.0, TAU, 32, overlay, _c(&"stroke_frame"), true)
		if forced:
			# A forced operation (cannot be declined, L749) carries a second ring.
			_ci.draw_arc(at, OFFER_RING + _c(&"spacing_4"), 0.0, TAU, 32, overlay, _c(&"stroke_frame"), true)
		var text := ""
		if offers.size() == 1:
			text = String(TranslationServer.translate(StringName(Kit.get_str(offers[0], "name"))))
		else:
			text = tr(&"chart.map.operations").format([Kit.number(offers.size(), &"none", 0)])
		if forced:
			text = tr(&"chart.map.forced").format([text])
		# A sea area carries no name of its own on the overview, so its offers name it.
		if StringName(Kit.get_str(_nodes[node], "kind")) == &"sea" and node != _selected and node != _hover:
			text = tr(&"chart.map.offers_at").format([text, _source_name(_nodes[node])])
		var rank := 0 if node == _selected else 2
		labels.append(_label(at + Vector2(0.0, _line_h()), [{"text": text, "ink": false}], rank, rank == 0, OFFER_RING + _c(&"spacing_4")))


func _contact_display_km(id: int) -> Vector2:
	var to: Vector2 = _contact_to.get(id, Vector2.ZERO)
	var from: Vector2 = _contact_from.get(id, to)
	return from.lerp(to, clampf(_anim, 0.0, 1.0))


func _draw_contacts(labels: Array[Dictionary]) -> void:
	var ink := _col(&"ink")
	var hour := _hour()
	# Contacts sit up and right of their node so the node symbol stays readable; several at one node
	# stack upward.
	var stacked: Dictionary[StringName, int] = {}
	for ct: Dictionary in Kit.dicts(_chart.get("contacts")):
		var id := Kit.get_int(ct, "id", -1)
		if not _contact_to.has(id):
			continue
		var node := StringName(Kit.get_str(ct, "node"))
		var nth: int = stacked.get(node, 0)
		stacked[node] = nth + 1
		var at := _px(_contact_display_km(id)) + Vector2(OFFER_RING + SYMBOL, -OFFER_RING - SYMBOL - nth * (SYMBOL * 2.0 + _c(&"spacing_8")))
		var convoy := Kit.get_bool(ct, "convoy")
		var age := maxi(0, hour - Kit.get_int(ct, "last_seen_h", hour))
		var stale := age > STALE_H
		_draw_contact_symbol(at, convoy, stale, ink)
		var hulls := Kit.get_int(ct, "hulls")
		var ace := StringName(Kit.get_str(ct, "ace"))
		var text := ""
		if convoy:
			text = tr(&"chart.map.convoy").format([Kit.number(hulls, &"none", 0)])
		elif ace != &"" and _source != null:
			text = tr(&"chart.map.group_ace").format([Kit.number(hulls, &"none", 0), _source.call(&"ace_name", ace)])
		else:
			text = tr(&"chart.map.group").format([Kit.number(hulls, &"none", 0)])
		var lines: Array[Dictionary] = [{"text": text, "ink": true}]
		if stale:
			lines.append({"text": tr(&"chart.map.seen_ago").format([Kit.number(age, &"h", 0)]), "ink": true})
		labels.append(_label(at, lines, 1, false, SYMBOL + _c(&"spacing_4")))


## A Combine contact: the Hostile upward triangle (L1650), filled when held, an outline when the
## sighting is old. A convoy carries a bar under the triangle.
func _draw_contact_symbol(at: Vector2, convoy: bool, stale: bool, ink: Color) -> void:
	var r := SYMBOL + 1.0
	var tri := PackedVector2Array([at + Vector2(0.0, -r), at + Vector2(r, r * 0.8), at + Vector2(-r, r * 0.8)])
	if stale:
		var ring := tri.duplicate()
		ring.append(tri[0])
		_ci.draw_polyline(ring, ink, _c(&"stroke_frame"), true)
	else:
		_ci.draw_colored_polygon(tri, ink)
	if convoy:
		_ci.draw_line(at + Vector2(-r, r * 0.8 + 4.0), at + Vector2(r, r * 0.8 + 4.0), ink, _c(&"stroke_frame"))


func _sextant_px() -> Vector2:
	if _passage.size() >= 2:
		return _passage_position()
	var id := StringName(Kit.get_str(_chart, "sextant"))
	if _nodes.has(id):
		return _px(_node_km(id))
	return Vector2(NAN, NAN)


func _draw_sextant(labels: Array[Dictionary]) -> void:
	var at := _sextant_px()
	if not at.is_finite():
		return
	draw_pennant(_ci, at, _col(&"ink"), _c(&"stroke_frame"))
	labels.append(_label(at + Vector2(0.0, -SYMBOL), [{"text": tr(&"chart.map.sextant"), "ink": true, "bold": true}], 0, true, SYMBOL * 2.0))


## SEXTANT's mark: a pole with a pennant flying from it, standing on a short base line.
static func draw_pennant(ci: CanvasItem, at: Vector2, ink: Color, width: float) -> void:
	var foot := at + Vector2(0.0, SYMBOL)
	var top := at + Vector2(0.0, -SYMBOL * 2.2)
	ci.draw_line(foot, top, ink, width)
	ci.draw_colored_polygon(PackedVector2Array([top, top + Vector2(SYMBOL * 2.2, SYMBOL * 0.7), top + Vector2(0.0, SYMBOL * 1.4)]), ink)
	ci.draw_line(foot + Vector2(-SYMBOL * 0.8, 0.0), foot + Vector2(SYMBOL * 0.8, 0.0), ink, width)


## A node's chart symbol by kind, in ink. A closed cove is crossed.
static func draw_symbol(ci: CanvasItem, kind: StringName, at: Vector2, ink: Color, width: float, closed: bool = false) -> void:
	var s := SYMBOL
	match kind:
		&"cove":
			ci.draw_arc(at, s, 0.0, TAU, 24, ink, width, true)
			if closed:
				ci.draw_line(at + Vector2(-s, -s), at + Vector2(s, s), ink, width, true)
				ci.draw_line(at + Vector2(-s, s), at + Vector2(s, -s), ink, width, true)
		&"shipyard":
			ci.draw_rect(Rect2(at - Vector2(s, s), Vector2(s, s) * 2.0), ink, false, width)
			ci.draw_line(at + Vector2(-s, 0.0), at + Vector2(s, 0.0), ink, width)
		&"depot":
			var d := PackedVector2Array([at + Vector2(0.0, -s), at + Vector2(s, 0.0), at + Vector2(0.0, s), at + Vector2(-s, 0.0), at + Vector2(0.0, -s)])
			ci.draw_polyline(d, ink, width, true)
		&"base":
			ci.draw_rect(Rect2(at - Vector2(s, s), Vector2(s, s) * 2.0), ink, true)
		&"harbour":
			ci.draw_rect(Rect2(at - Vector2(s, s), Vector2(s, s) * 2.0), ink, false, width)
			ci.draw_circle(at, 2.5, ink)
		&"radar_site":
			ci.draw_arc(at, s, 0.0, TAU, 24, ink, width, true)
			ci.draw_line(at + Vector2(0.0, -s), at + Vector2(0.0, s), ink, width)
		&"esm_site":
			ci.draw_arc(at, s, 0.0, TAU, 24, ink, width, true)
			ci.draw_line(at + Vector2(-s, 0.0), at + Vector2(s, 0.0), ink, width)
		_:
			ci.draw_circle(at, 3.0, ink)


func _draw_marks() -> void:
	var ink := _col(&"ink")
	if _hover != &"" and _hover != _selected and _nodes.has(_hover):
		_ci.draw_arc(_px(_node_km(_hover)), SYMBOL + _c(&"spacing_4"), 0.0, TAU, 32, ink, _c(&"stroke_rule"), true)
	if _selected != &"" and _nodes.has(_selected):
		_draw_bracket(_px(_node_km(_selected)), OFFER_RING + _c(&"spacing_4"), ink)
	var now := Time.get_ticks_msec()
	for key: String in _boxes:
		if _boxes[key] <= now:
			continue
		var r := _box_rect(key)
		if r.has_area():
			_ci.draw_rect(r, ink, false, _c(&"stroke_frame"))


## The selection bracket: four corners, 3 px, in ink on the light surface.
func _draw_bracket(at: Vector2, half: float, ink: Color) -> void:
	var w := _c(&"stroke_selection")
	var arm := half * 0.5
	for sx: float in [-1.0, 1.0]:
		for sy: float in [-1.0, 1.0]:
			var corner := at + Vector2(sx * half, sy * half)
			_ci.draw_line(corner, corner - Vector2(sx * arm, 0.0), ink, w)
			_ci.draw_line(corner, corner - Vector2(0.0, sy * arm), ink, w)


func _box_rect(key: String) -> Rect2:
	var pad := OFFER_RING + _c(&"spacing_8")
	if key.begins_with("n:"):
		var id := StringName(key.substr(2))
		if _nodes.has(id):
			var at := _px(_node_km(id))
			return Rect2(at - Vector2(pad, pad), Vector2(pad, pad) * 2.0)
	elif key.begins_with("l:"):
		var ab := key.substr(2).split("|")
		if ab.size() == 2 and _nodes.has(StringName(ab[0])) and _nodes.has(StringName(ab[1])):
			var mid := (_px(_node_km(StringName(ab[0]))) + _px(_node_km(StringName(ab[1])))) * 0.5
			return Rect2(mid - Vector2(pad, pad), Vector2(pad, pad) * 2.0)
	elif key.begins_with("r:"):
		var id := StringName(key.substr(2))
		if _region_anchor.has(id):
			return _region_block(id).grow(_c(&"spacing_4"))
	return Rect2()


func _region_dict(id: StringName) -> Dictionary:
	for r: Dictionary in Kit.dicts(_chart.get("regions")):
		if StringName(Kit.get_str(r, "id")) == id:
			return r
	return {}


## The lines of a region's label block: name, Alert and Supply, weather (L743, L749).
func _region_lines(id: StringName) -> Array[Dictionary]:
	var name_key := StringName(id)
	for r: Dictionary in Kit.dicts(_graph.get("regions")):
		if StringName(Kit.get_str(r, "id")) == id:
			name_key = StringName(Kit.get_str(r, "name", String(id)))
	var lines: Array[Dictionary] = [{"text": String(TranslationServer.translate(name_key)), "ink": false, "bold": true}]
	var r := _region_dict(id)
	if r.is_empty():
		return lines
	if not Kit.get_bool(r, "open", true):
		lines.append({"text": tr(&"chart.map.region_closed"), "ink": true})
		return lines
	lines.append({"text": tr(&"chart.map.region_state").format([Kit.number(Kit.get_int(r, "alert"), &"none", 0), Kit.number(roundf(Kit.get_float(r, "supply")), &"pct", 0)]), "ink": true})
	if r.has("patrols"):
		lines.append({"text": tr(&"chart.map.region_patrols").format([Kit.number(Kit.get_int(r, "patrols"), &"none", 0)]), "ink": true})
	var w := Kit.as_dict(r.get("weather"))
	if not w.is_empty():
		lines.append({"text": tr(&"chart.map.weather").format([word(&"weather", Kit.get_int(w, "kind")), Kit.number(Kit.get_int(w, "sea_state"), &"none", 0)]), "ink": true})
	if Kit.get_bool(r, "combine_control"):
		lines.append({"text": tr(&"chart.map.combine_control"), "ink": true})
	return lines


func _region_block(id: StringName) -> Rect2:
	if _region_rects.has(id):
		return _region_rects[id]
	return _region_block_at(id, _px(_region_anchor[id]))


func _region_block_at(id: StringName, centre: Vector2) -> Rect2:
	var lines := _region_lines(id)
	var w := 0.0
	for l: Dictionary in lines:
		w = maxf(w, _text_w(l["text"], l.get("bold", false)))
	var h := _line_h() * lines.size()
	return Rect2(centre - Vector2(w * 0.5, h * 0.5), Vector2(w, h))


## Place each region's label block near the middle of its places where it covers the fewest place
## symbols, lanes and other labelled marks (contacts, offers, SEXTANT, cut lanes), inside the neatline
## and clear of the other regions' blocks; outside its own region only when nothing inside is clear.
func _place_regions() -> void:
	if _k <= 0.0:
		_region_rects.clear()
		return
	var sizes: Dictionary[StringName, Vector2] = {}
	for id: StringName in _region_anchor:
		sizes[id] = _region_block_at(id, Vector2.ZERO).size
	# Only the layout, the blocks' sizes and the marks they avoid move them; an hour of supply drift
	# usually changes none of these. While theatre time runs the marks may move every hour, so the
	# blocks hold still until it stops (about 6 ms a search) unless the layout or a size changed.
	var hard := str(_neat) + str(sizes) + str(_open_coves())
	var soft := str(_contact_to) + str(_offers_by_node().keys()) + Kit.get_str(_chart, "sextant") + str(_cut_lanes().keys())
	if hard + soft == _regions_sig:
		return
	if not _regions_sig.is_empty() and _regions_sig.begins_with(hard) and Kit.get_int(_chart, "phase", -1) == PHASE_ADVANCING and _region_rects.size() == sizes.size():
		return
	_regions_sig = hard + soft
	_region_rects.clear()
	var symbols: Array[Vector2] = []
	for id: StringName in _node_ids:
		symbols.append(_px(_node_km(id)))
	var obstacles := _label_obstacles()
	var segments: Array[PackedVector2Array] = []
	for lane: Dictionary in _lanes:
		segments.append(PackedVector2Array([_px(_node_km(lane["a"])), _px(_node_km(lane["b"]))]))
	var step := _c(&"spacing_24")
	var gap := _c(&"spacing_8")
	for id: StringName in _region_anchor:
		var home := _px(_region_anchor[id])
		var sz := sizes[id]
		var best := Rect2(home - sz * 0.5, sz)
		var best_score := INF
		var outline := _px_poly(_region_outlines.get(id, PackedVector2Array()))
		for gy in range(-10, 11):
			for gx in range(-10, 11):
				var r := Rect2(home + Vector2(gx, gy) * step - sz * 0.5, sz)
				if not _neat.encloses(r):
					continue
				var score := Vector2(gx, gy).length() * 1.5
				if outline.size() >= 3 and not Geometry2D.is_point_in_polygon(r.get_center(), outline):
					score += 150.0
				for other: StringName in _region_rects:
					if _region_rects[other].grow(gap).intersects(r):
						score += 1000.0
				var padded := r.grow(gap)
				for o: Rect2 in obstacles:
					if score >= best_score:
						break
					if padded.intersects(o):
						score += 600.0
				var grown := r.grow(SYMBOL + 2.0)
				for p: Vector2 in symbols:
					if score >= best_score:
						break
					if grown.has_point(p):
						score += 100.0
				if score >= best_score:
					continue
				for seg: PackedVector2Array in segments:
					if _segment_hits_rect(seg[0], seg[1], r):
						score += 10.0
				if score < best_score:
					best_score = score
					best = r
		_region_rects[id] = best


## Where the labelled marks and their labels sit (their first-choice spot): contacts, operations on
## offer, SEXTANT and cut lanes. Region blocks keep clear of them.
func _label_obstacles() -> Array[Rect2]:
	var out: Array[Rect2] = []
	var lh := _line_h()
	var stacked: Dictionary[StringName, int] = {}
	for ct: Dictionary in Kit.dicts(_chart.get("contacts")):
		var id := Kit.get_int(ct, "id", -1)
		if not _contact_to.has(id):
			continue
		var node := StringName(Kit.get_str(ct, "node"))
		var nth: int = stacked.get(node, 0)
		stacked[node] = nth + 1
		var at := _px(_contact_to[id]) + Vector2(OFFER_RING + SYMBOL, -OFFER_RING - SYMBOL - nth * (SYMBOL * 2.0 + _c(&"spacing_8")))
		out.append(Rect2(at - Vector2(SYMBOL, lh), Vector2(SYMBOL * 2.0 + _c(&"spacing_4") + 360.0, lh * 2.0)))
	for node: StringName in _offers_by_node():
		var at := _px(_node_km(node))
		out.append(Rect2(at - Vector2(OFFER_RING, OFFER_RING), Vector2(OFFER_RING * 2.0 + 200.0, OFFER_RING + lh * 1.5)))
	# SEXTANT at its chart node (not along a passage, which moves every hour).
	var home := StringName(Kit.get_str(_chart, "sextant"))
	if _nodes.has(home):
		var sx := _px(_node_km(home))
		out.append(Rect2(sx - Vector2(SYMBOL * 2.0, SYMBOL * 3.0), Vector2(SYMBOL * 4.0 + _text_w(tr(&"chart.map.sextant"), true), SYMBOL * 4.0 + lh * 0.5)))
	# Open coves are where SEXTANT can go: their names stay readable.
	for cove: StringName in _open_coves():
		if _nodes.has(cove):
			var at := _px(_node_km(cove))
			var w := _text_w(_source_name(_nodes[cove]))
			out.append(Rect2(at + Vector2(OFFER_RING, -lh * 0.5), Vector2(w + _c(&"spacing_4"), lh)))
	if _selected != &"" and _nodes.has(_selected):
		var at := _px(_node_km(_selected))
		var w := _text_w(_source_name(_nodes[_selected]))
		out.append(Rect2(at - Vector2(OFFER_RING + w, lh * 0.5), Vector2(OFFER_RING * 2.0 + w * 2.0, lh)))
	for key: String in _cut_lanes():
		var ab := key.split("|")
		if ab.size() == 2 and _nodes.has(StringName(ab[0])) and _nodes.has(StringName(ab[1])):
			var mid := (_px(_node_km(StringName(ab[0]))) + _px(_node_km(StringName(ab[1])))) * 0.5
			out.append(Rect2(mid - Vector2(_c(&"spacing_12"), lh * 0.5), Vector2(_c(&"spacing_24") + 100.0, lh)))
	return out


static func _segment_hits_rect(a: Vector2, b: Vector2, r: Rect2) -> bool:
	if r.has_point(a) or r.has_point(b):
		return true
	var corners := [r.position, Vector2(r.end.x, r.position.y), r.end, Vector2(r.position.x, r.end.y)]
	for i in 4:
		if Geometry2D.segment_intersects_segment(a, b, corners[i], corners[(i + 1) % 4]) != null:
			return true
	return false


func _draw_regions(labels: Array[Dictionary]) -> void:
	for id: StringName in _region_anchor:
		labels.append({"rect": _region_block(id), "lines": _region_lines(id), "rank": 2, "force": true, "align": HORIZONTAL_ALIGNMENT_CENTER})


## The word for an enumeration value through `Tx.word` ("" without Tx).
static func word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"word"):
		return String(tx.call(&"word", kind, value))
	return ""


# --- labels --------------------------------------------------------------------------------------

func _text_w(text: String, bold: bool = false) -> float:
	var f := _font(bold)
	if f == null:
		return 0.0
	return f.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size()).x


## A label candidate beside a point: tried right of it, then left, above and below.
func _label(at: Vector2, lines: Array[Dictionary], rank: int, force: bool, gap: float = 0.0) -> Dictionary:
	var w := 0.0
	for l: Dictionary in lines:
		w = maxf(w, _text_w(l["text"], l.get("bold", false)))
	var h := _line_h() * lines.size()
	var spots: Array[Rect2] = [
		Rect2(at + Vector2(gap, -h * 0.5), Vector2(w, h)),
		Rect2(at + Vector2(-gap - w, -h * 0.5), Vector2(w, h)),
		Rect2(at + Vector2(-w * 0.5, -gap - h), Vector2(w, h)),
		Rect2(at + Vector2(-w * 0.5, gap), Vector2(w, h)),
	]
	return {"spots": spots, "lines": lines, "rank": rank, "force": force, "align": HORIZONTAL_ALIGNMENT_LEFT}


## Place labels by rank: each takes its first free spot inside the neatline; one with none is hidden
## unless forced (selected, hovered, SEXTANT, regions), which takes its first spot regardless
## (HUD rule 14). Hover shows any hidden label.
func _draw_labels(labels: Array[Dictionary]) -> void:
	labels.sort_custom(func(a: Dictionary, b: Dictionary) -> bool: return int(a["rank"]) < int(b["rank"]))
	var placed: Array[Rect2] = []
	# Every place symbol counts as taken, so a name never prints across a neighbour's mark (its own
	# mark sits OFFER_RING away, outside its spots); a forced label still takes its first spot.
	for id: StringName in _node_ids:
		placed.append(Rect2(_px(_node_km(id)) - Vector2(SYMBOL, SYMBOL), Vector2(SYMBOL, SYMBOL) * 2.0))
	var bounds := _neat.grow(-2.0)
	for l: Dictionary in labels:
		var spots: Array[Rect2] = []
		if l.has("rect"):
			spots.append(l["rect"])
		else:
			spots.assign(l["spots"])
		var chosen := Rect2()
		var found := false
		for r: Rect2 in spots:
			if not bounds.encloses(r):
				continue
			var free := true
			for p: Rect2 in placed:
				if p.intersects(r):
					free = false
					break
			if free:
				chosen = r
				found = true
				break
		if not found and bool(l["force"]) and not spots.is_empty():
			chosen = spots[0]
			for r: Rect2 in spots:
				if bounds.encloses(r):
					chosen = r
					break
			found = true
		if not found:
			continue
		placed.append(chosen)
		_draw_text_block(chosen, l["lines"], l.get("align", HORIZONTAL_ALIGNMENT_LEFT))


func _draw_text_block(r: Rect2, lines: Array, align: int) -> void:
	var fs := _font_size()
	var outline := int(_c(&"label_outline_size"))
	var y := r.position.y
	for l: Dictionary in lines:
		var font := _font(l.get("bold", false))
		if font == null:
			return
		var colour := _col(&"text") if l.get("ink", true) else _col(&"label_overlay")
		var at := Vector2(r.position.x, y + font.get_ascent(fs))
		_ci.draw_string_outline(font, at, l["text"], align, r.size.x, fs, outline, _col(&"label_outline"))
		_ci.draw_string(font, at, l["text"], align, r.size.x, fs, colour)
		y += _line_h()


# --- legend --------------------------------------------------------------------------------------

## The legend under the neatline: every symbol and line the chart draws, with its word (L1691).
func _draw_legend() -> void:
	var font := _font()
	if font == null:
		return
	var ink := _col(&"ink")
	var overlay := _col(&"overlay")
	var fs := _font_size()
	var entries := _legend_entries()
	var row_h := _line_h() + _c(&"spacing_8")
	var sym_w := SYMBOL * 2.0 + _c(&"spacing_12")
	var half := SYMBOL + 2.0
	for item: Dictionary in _legend_place(entries, _legend_width(), _scale_bar_reserve()):
		var e: Dictionary = item["entry"]
		var x: float = MARGIN + float(item["x"])
		var y: float = _neat.end.y + MARGIN + row_h * int(item["row"])
		var mid := Vector2(x + half, y + _line_h() * 0.5)
		match e["kind"]:
			&"sextant":
				draw_pennant(_ci, mid + Vector2(-half * 0.5, 4.0), ink, _c(&"stroke_frame"))
			&"group":
				_draw_contact_symbol(mid, false, false, ink)
			&"convoy":
				_draw_contact_symbol(mid + Vector2(0.0, -2.0), true, false, ink)
			&"offer":
				_ci.draw_arc(mid, half, 0.0, TAU, 24, overlay, _c(&"stroke_frame"), true)
			&"lane":
				_ci.draw_line(mid - Vector2(half, 0.0), mid + Vector2(half, 0.0), overlay, _c(&"stroke_rule"), true)
			&"cut":
				_ci.draw_dashed_line(mid - Vector2(half, 0.0), mid + Vector2(half, 0.0), overlay, _c(&"stroke_frame"), 4.0, true)
				_ci.draw_line(mid + Vector2(0.0, -SYMBOL), mid + Vector2(0.0, SYMBOL), ink, _c(&"stroke_frame"))
			&"route":
				_ci.draw_line(mid - Vector2(half, 0.0), mid + Vector2(half, 0.0), overlay, _c(&"stroke_selection"), true)
			&"closed":
				var box := Rect2(mid - Vector2(SYMBOL, SYMBOL), Vector2(SYMBOL, SYMBOL) * 2.0)
				for seg: PackedVector2Array in _legend_hatch(box):
					_ci.draw_polyline(seg, _col(&"overlay_tint"), _c(&"stroke_rule"), true)
				_ci.draw_rect(box, overlay, false, _c(&"stroke_rule"))
			_:
				draw_symbol(_ci, e["kind"], mid, ink, _c(&"stroke_frame"))
		_ci.draw_string(font, Vector2(x + sym_w, y + font.get_ascent(fs)), e["text"], HORIZONTAL_ALIGNMENT_LEFT, -1, fs, ink)


func _legend_entries() -> Array[Dictionary]:
	var entries: Array[Dictionary] = []
	for kind: StringName in KINDS:
		entries.append({"kind": kind, "text": tr(StringName("chart.legend." + String(kind)))})
	entries.append({"kind": &"sextant", "text": tr(&"chart.map.sextant")})
	for kind: StringName in [&"group", &"convoy", &"offer", &"lane", &"cut", &"route", &"closed"]:
		entries.append({"kind": kind, "text": tr(StringName("chart.legend." + String(kind)))})
	return entries


## Flow the legend entries into rows `width` wide, the first row leaving `reserve` at its right end
## for the scale bar. Each item has `entry`, `x` (from the row start) and `row`.
func _legend_place(entries: Array[Dictionary], width: float, reserve: float) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	var sym_w := SYMBOL * 2.0 + _c(&"spacing_12")
	var x := 0.0
	var row := 0
	for e: Dictionary in entries:
		var w := sym_w + _text_w(e["text"]) + _c(&"spacing_24")
		var limit := width - (reserve if row == 0 else 0.0)
		if x + w > limit and x > 0.0:
			x = 0.0
			row += 1
		out.append({"entry": e, "x": x, "row": row})
		x += w
	return out


func _legend_rows(width: float, reserve: float) -> int:
	var placed := _legend_place(_legend_entries(), width, reserve)
	return (int(placed[-1]["row"]) + 1) if not placed.is_empty() else 0


func _legend_hatch(box: Rect2) -> Array[PackedVector2Array]:
	var out: Array[PackedVector2Array] = []
	var poly := PackedVector2Array([box.position, Vector2(box.end.x, box.position.y), box.end, Vector2(box.position.x, box.end.y)])
	var span := box.size.x + box.size.y
	var t := 0.0
	while t <= span:
		var a := Vector2(box.position.x + t, box.position.y)
		for seg: PackedVector2Array in Geometry2D.intersect_polyline_with_polygon(PackedVector2Array([a, a + Vector2(-span, span)]), poly):
			out.append(seg)
		t += HATCH_PX * 0.5
	return out


# --- boxes ---------------------------------------------------------------------------------------

func _arm_box_timer() -> void:
	var now := Time.get_ticks_msec()
	var soonest := -1
	for key: String in _boxes.keys():
		var until: int = _boxes[key]
		if until <= now:
			_boxes.erase(key)
		elif soonest < 0 or until < soonest:
			soonest = until
	if soonest > 0 and _box_timer.is_inside_tree():
		_box_timer.start(maxf(0.05, float(soonest - now) / 1000.0))


func _on_box_timer() -> void:
	_arm_box_timer()
	_redraw()


# --- input ---------------------------------------------------------------------------------------

func _nearest(at: Vector2, within: float) -> StringName:
	var best := &""
	var best_d := within
	for id: StringName in _node_ids:
		var d := _px(_node_km(id)).distance_to(at)
		if d <= best_d:
			best_d = d
			best = id
	return best


func _gui_input(event: InputEvent) -> void:
	if event is InputEventMouseMotion:
		var h := _nearest((event as InputEventMouseMotion).position, PICK_PX)
		if h != _hover:
			_hover = h
			_redraw()
		return
	if event is InputEventMouseButton:
		var mb := event as InputEventMouseButton
		if mb.button_index == MOUSE_BUTTON_LEFT and mb.pressed:
			var hit := _nearest(mb.position, PICK_PX)
			grab_focus(true)
			if hit != &"":
				select(hit, true)
				if mb.double_click:
					node_activated.emit(hit)
			accept_event()
		return
	var dir := Vector2.ZERO
	if event.is_action_pressed(&"ui_left", true):
		dir = Vector2.LEFT
	elif event.is_action_pressed(&"ui_right", true):
		dir = Vector2.RIGHT
	elif event.is_action_pressed(&"ui_up", true):
		dir = Vector2.UP
	elif event.is_action_pressed(&"ui_down", true):
		dir = Vector2.DOWN
	if dir != Vector2.ZERO:
		var next := step(dir)
		if next != &"":
			select(next, true)
			accept_event()
		return
	if event.is_action_pressed(&"ui_accept") and _selected != &"":
		node_activated.emit(_selected)
		accept_event()


## The nearest node in a 120 degree cone that way from the selection (or from SEXTANT, or the
## chart's middle, when nothing is selected); `&""` when there is none, so focus can leave.
func step(dir: Vector2) -> StringName:
	var from := Vector2(NAN, NAN)
	if _selected != &"":
		from = _px(_node_km(_selected))
	else:
		from = _sextant_px()
	if not from.is_finite():
		from = _neat.get_center()
	var best := &""
	var best_score := INF
	for id: StringName in _node_ids:
		if id == _selected:
			continue
		var d := _px(_node_km(id)) - from
		var dist := d.length()
		if dist < 1.0:
			continue
		var cos_a := d.dot(dir) / dist
		if cos_a < 0.5:
			continue
		# Favour nodes straight ahead: distance grows as the angle opens.
		var score := dist * (2.0 - cos_a)
		if score < best_score:
			best_score = score
			best = id
	return best


func _get_tooltip(at_position: Vector2) -> String:
	var hit := _nearest(at_position, PICK_PX)
	if hit == &"":
		return ""
	return describe(hit)


## A sentence naming a node, its kind and region, for the tooltip and the screen reader.
func describe(node: StringName) -> String:
	if not _nodes.has(node):
		return ""
	var n := _nodes[node]
	var kind := StringName(Kit.get_str(n, "kind"))
	var region_key := StringName(Kit.get_str(n, "region"))
	for r: Dictionary in Kit.dicts(_graph.get("regions")):
		if StringName(Kit.get_str(r, "id")) == region_key:
			region_key = StringName(Kit.get_str(r, "name", String(region_key)))
	return tr(&"chart.map.describe").format([_source_name(n), tr(StringName("chart.legend." + String(kind))), String(TranslationServer.translate(region_key))])


func _update_accessibility() -> void:
	accessibility_description = describe(_selected) if _selected != &"" else tr(&"chart.map.none_selected")
