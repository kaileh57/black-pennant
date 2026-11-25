extends MarginContainer
## Gauge strip along the top.
## DESIGN.md L153-187. Unit: ui-editor-gauges.
## Reads and calls: Editor.gauges, Editor.gauges_changed, Editor.gauge_latency.
##
## Eight gauges in one row under the top bar, left to right in the order of L157: Float, Speed, Power,
## Endurance, Punch, Sight, Resilience, Complexity. Each is a cell drawn in `_draw()` from the Gauge theme
## type (L1735): the gauge's name and its check (L1689: Pass is a check in text.primary, Caution a triangle
## on alert.caution, Fail a cross on alert.warning, each with its word), then its readouts. A readout with a
## band and limits is drawn against them (HUD rule 12: band, limits, needle; the value is the brightest
## thing); exact readouts with units are text. Float leads and is the widest: GM against the 4% to 15% of
## beam band (Fail at zero or below), displacement against the tonnage cap, and the GZ curve with its range
## of stability. Sight lists the radar horizon against the three reference targets (5 m, 20 m, 1,000 m)
## and draws coverage by 10-degree sector. Complexity shows its points against Caution at 400 and the PvP
## cap, or that it is measured on save.
##
## Stale values (L185): a gauge named in `stale` keeps its previous value, drawn in text.secondary, and its
## check gives way to the stale glyph and "Updating". A gauge must stay stale for the panel motion time
## (150 ms) before it is drawn so, so the fast gauges that refresh within 100 ms of an edit never flicker.
## Before the first reading every gauge shows "Updating" and says it is waiting.
##
## Each cell is a focusable button: click, Enter or the gamepad's accept opens its detail sheet under the
## strip (the full GZ curve with heel ticks, coverage by sector with bearings, each weapon's reach, the
## worst flooding cases, the split-strip state). The strip redraws on `gauges_changed`, and 150 ms after
## `design_changed` to pick up which gauges went stale; nothing is rebuilt per frame.
##
## Other dock panels reuse the check badge drawn here through `preload`: `Badges.draw(...)` and the
## `CheckBadge` control (`Bp.CHECK_*`, or `Badges.ADVISORY` and `Badges.UPDATING`).

signal gauge_opened(gauge: StringName)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Inspector := preload("res://ui/editor/part_inspector.gd")

## Gauge names in strip order (the keys of `GaugeStripDict`).
const GAUGES: Array[StringName] = [&"float", &"speed", &"power", &"endurance", &"punch", &"sight", &"resilience", &"complexity"]
const TITLE_KEYS: Dictionary[StringName, StringName] = {
	&"float": &"editor_gauges.float", &"speed": &"editor_gauges.speed", &"power": &"editor_gauges.power",
	&"endurance": &"editor_gauges.endurance", &"punch": &"editor_gauges.punch", &"sight": &"editor_gauges.sight",
	&"resilience": &"editor_gauges.resilience", &"complexity": &"editor_gauges.complexity",
}
const TIP_KEYS: Dictionary[StringName, StringName] = {
	&"float": &"editor_gauges.float.tip", &"speed": &"editor_gauges.speed.tip", &"power": &"editor_gauges.power.tip",
	&"endurance": &"editor_gauges.endurance.tip", &"punch": &"editor_gauges.punch.tip", &"sight": &"editor_gauges.sight.tip",
	&"resilience": &"editor_gauges.resilience.tip", &"complexity": &"editor_gauges.complexity.tip",
}
## Gauges that move to the background worker in the split strip (L185).
const SLOW: Array[StringName] = [&"endurance", &"sight", &"resilience"]
## A gauge must stay stale this long before it is drawn stale (motion token `panel`, L1649).
const STALE_SHOW_MS := 150
const TARGET_KEYS: Array[StringName] = [&"editor_gauges.sight.skimmer", &"editor_gauges.sight.mast", &"editor_gauges.sight.aircraft"]
## Width of the detail sheet's text column.
const DETAIL_W := 520.0
## Coverage share at which a 10-degree sector counts as clear.
const SECTOR_CLEAR := 0.5

var _cells: Dictionary[StringName, GaugeCell] = {}
var _row: HFlowContainer
var _detail: PopupPanel
var _detail_box: VBoxContainer
var _detail_scroll: ScrollContainer
var _detail_gauge: StringName = &""
var _data: Dictionary = {}
var _latency: Dictionary = {}
var _stale_since: Dictionary[StringName, int] = {}
var _stale_timer: Timer
var _design_timer: Timer
var _parts_names: Dictionary[int, String] = {}
## Name and check only, one short line per gauge (the dry dock sets it when the screen is both narrow and
## short, as at UI scale 200% on 1920 x 1080): the readouts stay one press away in the detail sheet, and the
## 3D view keeps the height. Abbreviation at 200% (L1713).
var headline := false:
	set(v):
		if v == headline:
			return
		headline = v
		if is_node_ready():
			_fit_width()


func _ready() -> void:
	accessibility_name = tr(&"editor_gauges.strip")
	# The docked panel look behind the strip, without the panel's own padding: the strip is the dock's
	# densest instrument and every pixel of height comes off the 3D view.
	var backing := Panel.new()
	backing.name = &"Backing"
	backing.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(backing)
	var margin := MarginContainer.new()
	margin.name = &"Margin"
	margin.mouse_filter = Control.MOUSE_FILTER_IGNORE
	margin.add_theme_constant_override(&"margin_left", Ui.spacing(1))
	margin.add_theme_constant_override(&"margin_right", Ui.spacing(1))
	margin.add_theme_constant_override(&"margin_top", 0)
	margin.add_theme_constant_override(&"margin_bottom", 0)
	add_child(margin)
	# A flow, so a strip too wide for the window even with short labels (UI scale 200%) wraps to a second
	# row and the dock scrolls on one axis only (L1716).
	_row = HFlowContainer.new()
	_row.name = &"Row"
	_row.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_row.add_theme_constant_override(&"h_separation", 0)
	_row.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	margin.add_child(_row)
	var focus_list: Array[Control] = []
	for i in GAUGES.size():
		var g := GAUGES[i]
		if i > 0:
			var rule := VSeparator.new()
			rule.mouse_filter = Control.MOUSE_FILTER_IGNORE
			rule.add_theme_constant_override(&"separation", Ui.spacing(0))
			_row.add_child(rule)
		var cell := GaugeCell.new()
		cell.name = StringName(String(g).capitalize().replace(" ", ""))
		cell.set_meta(&"control_id", "editor.gauge." + String(g))
		cell.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		cell.size_flags_stretch_ratio = 2.0 if g == &"float" else 1.0
		cell.pressed.connect(_open_detail.bind(g))
		# Gauges that update in the background keep room for "Updating", so the strip never reflows.
		cell.reserve_stale = SLOW.has(g) or g == &"complexity"
		_row.add_child(cell)
		_cells[g] = cell
		focus_list.append(cell)
	Kit.chain_horizontal(focus_list)
	_stale_timer = Timer.new()
	_stale_timer.one_shot = true
	_stale_timer.timeout.connect(_apply)
	add_child(_stale_timer)
	_design_timer = Timer.new()
	_design_timer.one_shot = true
	_design_timer.wait_time = STALE_SHOW_MS / 1000.0
	_design_timer.timeout.connect(_refresh)
	add_child(_design_timer)
	_build_detail()
	get_viewport().size_changed.connect(_fit_width)
	var editor := Kit.autoload(&"Editor")
	if editor != null and editor.has_signal(&"gauges_changed"):
		editor.connect(&"gauges_changed", _refresh)
		editor.connect(&"design_changed", _on_design_changed)
		editor.connect(&"job_changed", _on_job_changed)
	var tutorial := Kit.autoload(&"Tutorial")
	if tutorial != null and tutorial.has_signal(&"step_changed"):
		tutorial.connect(&"step_changed", func(_s: Dictionary) -> void: _apply_tutorial())
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		accessibility_name = tr(&"editor_gauges.strip")
		_apply()


func _on_design_changed(_rev: int) -> void:
	if _design_timer.is_inside_tree():
		_design_timer.start()


func _on_job_changed(job: StringName, _state: int) -> void:
	if job == &"trials":
		_refresh()


## Pull the strip from Editor and draw it.
func _refresh() -> void:
	var editor := Kit.autoload(&"Editor")
	var open := editor != null and editor.has_method(&"gauges") and bool(editor.call(&"is_open"))
	_data = Kit.as_dict(editor.call(&"gauges")) if open else {}
	_latency = Kit.as_dict(editor.call(&"gauge_latency")) if open else {}
	var now := Time.get_ticks_msec()
	var stale: Array[StringName] = []
	for s: Variant in _list(_data.get("stale", PackedStringArray())):
		stale.append(StringName(str(s)))
	for g: StringName in _stale_since.keys():
		if not stale.has(g):
			_stale_since.erase(g)
	for g: StringName in stale:
		if not _stale_since.has(g):
			_stale_since[g] = now
	_apply()


## Draw a hand-built `GaugeStripDict` (captures and tests). `stale_now` draws its `stale` gauges stale at
## once instead of after the 150 ms hold.
func show_strip(data: Dictionary, latency: Dictionary = {}, stale_now: bool = false) -> void:
	_data = data
	_latency = latency
	_stale_since.clear()
	var t := Time.get_ticks_msec() - (STALE_SHOW_MS if stale_now else 0)
	for s: Variant in _list(data.get("stale", PackedStringArray())):
		_stale_since[StringName(str(s))] = t
	_apply()


func _apply() -> void:
	var now := Time.get_ticks_msec()
	var wait := -1
	for g: StringName in GAUGES:
		var cell := _cells[g]
		var gd := Kit.as_dict(_data.get(String(g), {}))
		var stale := false
		if _stale_since.has(g):
			var age := now - _stale_since[g]
			stale = age >= STALE_SHOW_MS
			if not stale:
				wait = maxi(wait, STALE_SHOW_MS - age)
		var spec := _spec(g, gd) if not gd.is_empty() else _empty_spec()
		cell.show_spec(tr(TITLE_KEYS[g]), spec, stale or gd.is_empty())
		cell.tooltip_text = _tooltip(g)
	if wait > 0:
		_stale_timer.start(wait / 1000.0 + 0.001)
	_fit_width()
	_apply_tutorial()
	if _detail != null and _detail.visible and _detail_gauge != &"":
		_fill_detail(_detail_gauge)


## Short labels for every cell when the full strip is wider than the screen; when even the short strip does
## not fit (UI scale 200%), the row wraps.
func _fit_width() -> void:
	if not is_inside_tree():
		return
	for g: StringName in GAUGES:
		_cells[g].headline = headline
	if headline:
		for g: StringName in GAUGES:
			_cells[g].compact = true
			_cells[g].tight = 2
		return
	var room := get_viewport_rect().size.x - float(Ui.spacing(1) * 2)
	var rules := float(GAUGES.size() - 1) * _rule_w()
	# Try full labels, then short labels, then short labels without the coverage rose, then without the
	# GZ sparkline too (the detail sheet keeps both). The first that fits wins.
	for level: Vector2i in [Vector2i(0, 0), Vector2i(1, 0), Vector2i(1, 1), Vector2i(1, 2)]:
		var total := rules
		for g: StringName in GAUGES:
			_cells[g].compact = level.x == 1
			_cells[g].tight = level.y
			total += _cells[g].get_combined_minimum_size().x
		if total <= room:
			return


func _rule_w() -> float:
	for c: Node in _row.get_children():
		if c is VSeparator:
			return (c as Control).get_combined_minimum_size().x
	return 4.0


func _tooltip(g: StringName) -> String:
	var tip := tr(TIP_KEYS[g])
	if bool(_latency.get("split_strip", false)) and SLOW.has(g):
		tip += "\n" + tr(&"editor_gauges.split.tip")
	return tip


func _apply_tutorial() -> void:
	var t := Kit.autoload(&"Tutorial")
	for g: StringName in GAUGES:
		var cell := _cells[g]
		var ok := t == null or not t.has_method(&"is_allowed") or bool(t.call(&"is_allowed", String(cell.get_meta(&"control_id"))))
		cell.focus_mode = Control.FOCUS_ALL if ok else Control.FOCUS_NONE
		cell.enabled = ok


# --- what each gauge shows --------------------------------------------------------------------------

static func _f(d: Dictionary, key: String) -> float:
	var v: Variant = d.get(key, 0.0)
	if v is float or v is int:
		var x := float(v)
		return x if is_finite(x) else 0.0
	return 0.0


static func _num(value: float, unit: StringName, decimals: int) -> String:
	return Kit.number(value, unit, decimals)


## Any array or packed array as a plain Array (Rust hands over packed arrays).
static func _list(v: Variant) -> Array:
	if v is Array:
		return v
	if v is PackedStringArray or v is PackedFloat32Array or v is PackedFloat64Array or v is PackedInt32Array or v is PackedInt64Array:
		return Array(v)
	return []


static func _arr(v: Variant) -> PackedFloat32Array:
	if v is PackedFloat32Array:
		return v
	var out := PackedFloat32Array()
	if v is Array or v is PackedFloat64Array:
		for x: Variant in v:
			out.append(float(x))
	return out


## Weight of fire per minute: kilograms, or tonnes from 1,000 kg so the strip stays narrow.
func _fire(kg: float) -> String:
	if kg >= 1000.0:
		return tr(&"editor_gauges.unit.t_per_min").format([_num(kg / 1000.0, &"none", 1)])
	return tr(&"editor_gauges.unit.kg_per_min").format([_num(kg, &"none", 0)])


func _empty_spec() -> Dictionary:
	return {"check": Badges.UPDATING, "cols": [{"kind": "text", "rows": [[tr(&"editor_gauges.waiting"), ""]]}], "access": tr(&"editor_gauges.waiting")}


## The cell content of one gauge: `{ check, cols, access }`. Columns are `scale` (a readout against band
## and limits), `text` (exact readouts), `gz` (the righting-arm curve) or `polar` (coverage by sector).
## Every limit comes from the dictionary; the check is the simulation's, never recomputed here.
func _spec(g: StringName, d: Dictionary) -> Dictionary:
	var check := int(d.get("check", Badges.NONE))
	var cols: Array[Dictionary] = []
	match g:
		&"float":
			var gm := _f(d, "gm_m")
			var lo := _f(d, "gm_caution_low")
			var hi := _f(d, "gm_caution_high")
			cols.append({"kind": "scale", "label": _two(&"editor_gauges.float.gm"), "value": _num(gm, &"m", 2),
				"v": gm, "min": minf(0.0, gm * 1.2), "max": maxf(maxf(hi * 1.4, gm * 1.1), 0.1), "band_lo": lo, "band_hi": hi,
				"caution_lo": lo, "caution_hi": hi, "warning_lo": 0.0, "warning_hi": NAN,
				"sub": _two(&"editor_gauges.float.band", [_num(lo, &"none", 2), _num(hi, &"m", 2)])})
			var disp := _f(d, "displacement_t")
			var cap := _f(d, "cap_t")
			var sub := _two(&"editor_gauges.float.cap", [_num(cap, &"t", 0)]) if cap > 0.0 else _two(&"editor_gauges.float.reserve", [Kit.percent(_f(d, "reserve"))])
			cols.append({"kind": "scale", "label": _two(&"editor_gauges.float.displacement"), "value": _num(disp, &"t", 1 if disp < 100.0 else 0),
				"v": disp, "min": 0.0, "max": maxf(maxf(cap, disp) * 1.2, 1.0), "band_lo": NAN, "band_hi": cap if cap > 0.0 else NAN,
				"caution_lo": NAN, "caution_hi": NAN, "warning_lo": NAN, "warning_hi": cap if cap > 0.0 else NAN, "sub": sub})
			cols.append({"kind": "gz", "label": _two(&"editor_gauges.float.range"), "value": _num(_f(d, "range_of_stability_deg"), &"deg", 0),
				"points": d.get("gz", PackedVector2Array()), "range": _f(d, "range_of_stability_deg")})
		&"speed":
			cols.append({"kind": "text", "rows": [
				[_two(&"editor_gauges.speed.top"), _num(_f(d, "top_speed_kn"), &"kn", 1)],
				[_two(&"editor_gauges.speed.turn"), _num(_f(d, "turning_circle_m"), &"m", 0)],
			]})
		&"power":
			var gen := _f(d, "generation_kw")
			var combat := _f(d, "combat_demand_kw")
			cols.append({"kind": "scale", "label": _two(&"editor_gauges.power.combat"), "value": _num(combat, &"kw", 0),
				"v": combat, "min": 0.0, "max": maxf(maxf(gen, combat) * 1.2, 1.0), "band_lo": 0.0, "band_hi": gen,
				"caution_lo": NAN, "caution_hi": NAN, "warning_lo": NAN, "warning_hi": gen, "minor": [_f(d, "cruise_demand_kw")],
				"sub": _two(&"editor_gauges.power.generation", [_num(gen, &"kw", 0)])})
		&"endurance":
			cols.append({"kind": "text", "rows": [
				[_two(&"editor_gauges.endurance.range"), _num(_f(d, "range_km"), &"km", 0)],
				[_two(&"editor_gauges.endurance.cruise"), _num(_f(d, "cruise_kn"), &"kn", 0)],
				[_two(&"editor_gauges.endurance.fuel"), _num(_f(d, "fuel_l"), &"l", 0)],
			]})
		&"punch":
			var longest := 0.0
			var ranges := Kit.dicts(d.get("ranges", []))
			for r: Dictionary in ranges:
				longest = maxf(longest, _f(r, "range_m"))
			cols.append({"kind": "text", "rows": [
				[_two(&"editor_gauges.punch.weight"), _fire(_f(d, "weight_of_fire_kg_per_min"))],
				[_two(&"editor_gauges.punch.reach"), _num(longest / 1000.0, &"km", 1)],
				[_two(&"editor_gauges.punch.weapons"), _num(float(ranges.size()), &"none", 0)],
			]})
		&"sight":
			var horizon := _arr(d.get("horizon_km", PackedFloat32Array()))
			var rows: Array = []
			for i in TARGET_KEYS.size():
				rows.append([_two(TARGET_KEYS[i]), _num(float(horizon[i]) if i < horizon.size() else 0.0, &"km", 0)])
			cols.append({"kind": "text", "rows": rows})
			var cov := _arr(d.get("coverage", PackedFloat32Array()))
			# The rose carries its share of clear sectors over it; the label is in the tooltip and the sheet.
			cols.append({"kind": "polar", "label": [tr(&"editor_gauges.sight.coverage"), ""], "value": Kit.percent(clear_share(cov)), "sectors": cov})
		&"resilience":
			cols.append({"kind": "text", "rows": [
				[_two(&"editor_gauges.resilience.one"), Kit.percent(_f(d, "one_flooded"))],
				[_two(&"editor_gauges.resilience.two"), Kit.percent(_f(d, "two_flooded"))],
				[_two(&"editor_gauges.resilience.hit"), Kit.percent(_f(d, "systems_surviving_hit"))],
			]})
		&"complexity":
			var caution := _f(d, "caution_at")
			var pvp := _f(d, "pvp_cap")
			var limits := _two(&"editor_gauges.complexity.limits", [_num(caution, &"none", 0), _num(pvp, &"none", 0)])
			if bool(d.get("measured", false)):
				var pts := _f(d, "points")
				cols.append({"kind": "scale", "label": _two(&"editor_gauges.complexity.points"), "value": _num(pts, &"none", 0),
					"v": pts, "min": 0.0, "max": maxf(maxf(pvp, caution) * 1.15, pts * 1.1), "band_lo": 0.0, "band_hi": caution,
					"caution_lo": NAN, "caution_hi": caution, "warning_lo": NAN, "warning_hi": NAN, "marks": [pvp], "sub": limits})
			else:
				# No reading yet (`measured` false): no check, and the points are not zero.
				check = Badges.NONE
				cols.append({"kind": "text", "rows": [
					[_two(&"editor_gauges.complexity.points"), _two(&"editor_gauges.complexity.unmeasured")],
					[limits, ""],
				]})
	return {"check": check, "cols": cols, "access": _access_text(cols)}


func _access_text(cols: Array[Dictionary]) -> String:
	var parts: PackedStringArray = []
	for c: Dictionary in cols:
		if String(c.get("kind", "")) == "text":
			for r: Array in c.get("rows", []):
				parts.append(("%s %s" % [pick(r[0], false), pick(r[1], false)]).strip_edges())
		else:
			parts.append("%s %s" % [pick(c.get("label", ""), false), pick(c.get("value", ""), false)])
			if c.has("sub"):
				parts.append(pick(c["sub"], false))
	return ", ".join(parts)


## A label in its full and short forms: `[full, short]`. The short form is the key with `.short` appended
## when the CSV has it (L1716 allows abbreviation where space runs out), else the full form again.
func _two(key: StringName, args: Array = []) -> Array:
	var full := tr(key)
	var short_key := StringName(String(key) + ".short")
	var short := tr(short_key)
	if short == String(short_key):
		short = full
	if not args.is_empty():
		full = full.format(args)
		short = short.format(args)
	return [full, short]


## The text of a label: a plain value, or the full or short form of a `_two` pair.
static func pick(v: Variant, short: bool) -> String:
	if v is Array:
		var a := v as Array
		if a.is_empty():
			return ""
		return str(a[1 if short and a.size() > 1 else 0])
	return str(v)


## Share of the sectors whose coverage is at least `SECTOR_CLEAR`.
static func clear_share(cov: PackedFloat32Array) -> float:
	if cov.is_empty():
		return 0.0
	var n := 0
	for v: float in cov:
		if v >= SECTOR_CLEAR:
			n += 1
	return float(n) / float(cov.size())


# --- detail sheet -----------------------------------------------------------------------------------

func _build_detail() -> void:
	_detail = PopupPanel.new()
	_detail.name = &"Detail"
	_detail.popup_hide.connect(_on_detail_hidden)
	add_child(_detail)
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 3)
	_detail.add_child(margin)
	# A long sheet (many weapons, many flooding cases) scrolls inside the room under the strip.
	_detail_scroll = ScrollContainer.new()
	_detail_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	margin.add_child(_detail_scroll)
	_detail_box = Kit.vbox(1)
	_detail_box.custom_minimum_size = Vector2(DETAIL_W, 0.0)
	_detail_scroll.add_child(_detail_box)


func _on_detail_hidden() -> void:
	var cell: GaugeCell = _cells.get(_detail_gauge)
	_detail_gauge = &""
	if cell != null and is_instance_valid(cell) and cell.is_visible_in_tree():
		Ui.focus(cell)


func _open_detail(g: StringName) -> void:
	if _detail.visible and _detail_gauge == g:
		_detail.hide()
		return
	_detail_gauge = g
	_parts_names.clear()
	_fill_detail(g)
	var r := _cells[g].get_global_rect()
	var vp := get_viewport_rect().size
	_detail_scroll.custom_minimum_size = Vector2(DETAIL_W + 16.0, minf(_detail_box.get_combined_minimum_size().y, maxf(160.0, vp.y - r.end.y - float(Ui.spacing(3) * 2) - 8.0)))
	_detail.reset_size()
	var size := Vector2(_detail.get_contents_minimum_size()).max(Vector2(DETAIL_W + 48.0, 120.0))
	size.y = minf(size.y, vp.y - r.end.y)
	var at := Vector2(clampf(r.position.x, 0.0, maxf(0.0, vp.x - size.x)), r.end.y)
	if not get_viewport().gui_embed_subwindows:
		at = get_viewport().get_screen_transform() * at + Vector2(get_window().position)
	_detail.popup(Rect2i(Vector2i(at), Vector2i(size)))
	# Wrapped labels know their height only once laid out at their width: fit the sheet a frame later.
	_settle_detail.call_deferred(Vector2i(at))
	gauge_opened.emit(g)


func _settle_detail(at: Vector2i) -> void:
	if not _detail.visible:
		return
	var vp := get_viewport_rect().size
	var pad := float(Ui.spacing(3) * 2)
	var room := maxf(160.0, vp.y - float(at.y) - pad - 8.0)
	_detail_scroll.custom_minimum_size = Vector2(DETAIL_W + 16.0, minf(_detail_box.get_combined_minimum_size().y, room))
	var want := Vector2(_detail.get_contents_minimum_size()).max(Vector2(DETAIL_W + 48.0, 120.0))
	want.y = minf(want.y, vp.y - float(at.y))
	_detail.size = Vector2i(want)
	_detail.position = at


func _fill_detail(g: StringName) -> void:
	Kit.clear(_detail_box)
	var d := Kit.as_dict(_data.get(String(g), {}))
	_detail_box.add_child(Kit.label(TITLE_KEYS[g], &"PanelTitle"))
	if d.is_empty():
		_detail_box.add_child(Kit.label(&"editor_gauges.waiting", &"Body"))
		return
	var badge := CheckBadge.new()
	badge.check = Badges.UPDATING if _stale_since.has(g) else int(d.get("check", Badges.NONE))
	if g == &"complexity" and not bool(d.get("measured", false)) and not _stale_since.has(g):
		badge.check = Badges.NONE
	if badge.check >= 0:
		_detail_box.add_child(badge)
	else:
		badge.free()
	_detail_box.add_child(Kit.label(TIP_KEYS[g], &"Dense"))
	var rows: Array = []
	match g:
		&"float":
			rows = [
				[&"editor_gauges.detail.gm", _num(_f(d, "gm_m"), &"m", 2)],
				[&"editor_gauges.detail.gm_beam", Kit.number(_f(d, "gm_over_beam") * 100.0, &"pct", 1)],
				[&"editor_gauges.detail.gm_band", tr(&"editor_gauges.float.band.short").format([_num(_f(d, "gm_caution_low"), &"none", 2), _num(_f(d, "gm_caution_high"), &"m", 2)])],
				[&"editor_gauges.detail.displacement", _num(_f(d, "displacement_t"), &"t", 1)],
				[&"editor_gauges.detail.cap", _num(_f(d, "cap_t"), &"t", 0) if _f(d, "cap_t") > 0.0 else tr(&"editor_gauges.detail.no_cap")],
				[&"editor_gauges.detail.reserve", Kit.percent(_f(d, "reserve"))],
				[&"editor_gauges.detail.draft", _num(_f(d, "draft_m"), &"m", 2)],
				[&"editor_gauges.detail.heel_trim", tr(&"editor_gauges.detail.heel_trim_value").format([_num(_f(d, "heel_deg"), &"deg", 1), _num(_f(d, "trim_deg"), &"deg", 1)])],
				[&"editor_gauges.detail.range", _num(_f(d, "range_of_stability_deg"), &"deg", 0)],
			]
		&"speed":
			rows = [[&"editor_gauges.speed.top", _num(_f(d, "top_speed_kn"), &"kn", 1)], [&"editor_gauges.speed.turn", _num(_f(d, "turning_circle_m"), &"m", 0)]]
		&"power":
			var gen := _f(d, "generation_kw")
			rows = [
				[&"editor_gauges.detail.generation", _num(gen, &"kw", 0)],
				[&"editor_gauges.detail.cruise", _num(_f(d, "cruise_demand_kw"), &"kw", 0)],
				[&"editor_gauges.detail.combat", _num(_f(d, "combat_demand_kw"), &"kw", 0)],
				[&"editor_gauges.detail.margin", _num(gen - _f(d, "combat_demand_kw"), &"kw", 0)],
				[&"editor_gauges.detail.battery", tr(&"editor_gauges.unit.kwh").format([_num(_f(d, "battery_kwh"), &"none", 0)])],
			]
		&"endurance":
			rows = [[&"editor_gauges.endurance.range", _num(_f(d, "range_km"), &"km", 0)], [&"editor_gauges.endurance.cruise", _num(_f(d, "cruise_kn"), &"kn", 1)], [&"editor_gauges.endurance.fuel", _num(_f(d, "fuel_l"), &"l", 0)]]
		&"punch":
			rows = [[&"editor_gauges.punch.weight", tr(&"editor_gauges.unit.kg_per_min").format([_num(_f(d, "weight_of_fire_kg_per_min"), &"none", 0)])]]
			var ranges := Kit.dicts(d.get("ranges", []))
			if ranges.is_empty():
				rows.append([&"editor_gauges.detail.no_weapons", ""])
			for r: Dictionary in ranges:
				rows.append([_part_name(int(r.get("slot", -1))), _num(_f(r, "range_m") / 1000.0, &"km", 1)])
		&"sight":
			_detail_box.add_child(_rows_grid([[&"editor_gauges.detail.antenna", _num(_f(d, "antenna_m"), &"m", 1)]]))
			var grid := GridContainer.new()
			grid.columns = 4
			grid.add_theme_constant_override(&"h_separation", Ui.spacing(3))
			grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
			for k: StringName in [&"editor_gauges.detail.target", &"editor_gauges.detail.horizon", &"editor_gauges.detail.radar", &"editor_gauges.detail.visual"]:
				var head := Kit.label(k, &"Dense")
				head.autowrap_mode = TextServer.AUTOWRAP_OFF
				head.horizontal_alignment = HORIZONTAL_ALIGNMENT_LEFT if k == &"editor_gauges.detail.target" else HORIZONTAL_ALIGNMENT_RIGHT
				grid.add_child(head)
			var series: Array[PackedFloat32Array] = [_arr(d.get("horizon_km", [])), _arr(d.get("radar_km", [])), _arr(d.get("visual_km", []))]
			for i in TARGET_KEYS.size():
				var target := Kit.label(TARGET_KEYS[i], &"Dense")
				target.autowrap_mode = TextServer.AUTOWRAP_OFF
				target.size_flags_horizontal = Control.SIZE_EXPAND_FILL
				grid.add_child(target)
				for arr: PackedFloat32Array in series:
					var v := Kit.label(_num(float(arr[i]) if i < arr.size() else 0.0, &"km", 0), &"Body", true)
					v.autowrap_mode = TextServer.AUTOWRAP_OFF
					v.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
					grid.add_child(v)
			_detail_box.add_child(grid)
			var plot := DetailPlot.new()
			plot.kind = &"polar"
			plot.sectors = _arr(d.get("coverage", []))
			plot.custom_minimum_size = Vector2(0.0, 220.0)
			_detail_box.add_child(plot)
			_detail_box.add_child(Kit.label(tr(&"editor_gauges.detail.coverage_legend").format([Kit.percent(SECTOR_CLEAR), Kit.percent(clear_share(plot.sectors))]), &"Dense", true))
		&"resilience":
			_detail_box.add_child(_rows_grid([
				[&"editor_gauges.resilience.one", Kit.percent(_f(d, "one_flooded"))],
				[&"editor_gauges.resilience.two", Kit.percent(_f(d, "two_flooded"))],
				[&"editor_gauges.resilience.hit", Kit.percent(_f(d, "systems_surviving_hit"))],
			]))
			var worst := Kit.dicts(d.get("worst", []))
			if not worst.is_empty():
				_detail_box.add_child(Kit.label(&"editor_gauges.detail.worst", &"Body"))
				for c: Dictionary in worst:
					var a := int(c.get("a", -1))
					var b := int(c.get("b", -1))
					var which := tr(&"editor_gauges.detail.one_comp").format([_num(float(a + 1), &"none", 0)]) if b < 0 else tr(&"editor_gauges.detail.two_comps").format([_num(float(a + 1), &"none", 0), _num(float(b + 1), &"none", 0)])
					var fate := tr(&"editor_gauges.detail.survives") if bool(c.get("survives", false)) else tr(&"editor_gauges.detail.lost")
					rows.append([which, tr(&"editor_gauges.detail.case").format([_num(_f(c, "heel_deg"), &"deg", 0), _num(_f(c, "trim_deg"), &"deg", 1), fate])])
		&"complexity":
			if bool(d.get("measured", false)):
				rows.append([&"editor_gauges.complexity.points", _num(_f(d, "points"), &"none", 0)])
			else:
				rows.append([&"editor_gauges.complexity.points", tr(&"editor_gauges.complexity.unmeasured")])
			rows.append([&"editor_gauges.detail.caution_at", _num(_f(d, "caution_at"), &"none", 0)])
			rows.append([&"editor_gauges.detail.pvp_cap", _num(_f(d, "pvp_cap"), &"none", 0)])
	if not rows.is_empty():
		_detail_box.add_child(_rows_grid(rows))
	if g == &"float":
		var plot := DetailPlot.new()
		plot.kind = &"gz"
		plot.points = d.get("gz", PackedVector2Array())
		plot.range_deg = _f(d, "range_of_stability_deg")
		plot.custom_minimum_size = Vector2(0.0, 200.0)
		_detail_box.add_child(plot)
		_detail_box.add_child(Kit.label(&"editor_gauges.detail.gz_legend", &"Dense"))
	if SLOW.has(g) and bool(_latency.get("split_strip", false)):
		_detail_box.add_child(Kit.label(tr(&"editor_gauges.split.detail").format([_num(float(_latency.get("p95_ms", 0.0)), &"none", 0)]), &"Dense", true))
	# Wrapped lines take the sheet's width, so the sheet measures its height before it opens.
	for n: Node in _detail_box.find_children("*", "Label", true, false):
		var l := n as Label
		if l.autowrap_mode != TextServer.AUTOWRAP_OFF and l.get_parent() == _detail_box:
			l.custom_minimum_size.x = DETAIL_W
			l.size = Vector2(DETAIL_W, l.size.y)


func _rows_grid(rows: Array) -> GridContainer:
	var grid := GridContainer.new()
	grid.columns = 2
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(3))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	for r: Array in rows:
		var key: Variant = r[0]
		var l := Kit.label(String(key), &"Dense", not (key is StringName))
		l.autowrap_mode = TextServer.AUTOWRAP_OFF
		grid.add_child(l)
		var v := Kit.label(String(r[1]), &"Body", true)
		v.autowrap_mode = TextServer.AUTOWRAP_OFF
		v.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		v.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(v)
	return grid


## A weapon's name as the dock names it: its label, else its generated name ("Gun 2").
func _part_name(slot: int) -> String:
	if _parts_names.is_empty():
		var parts := Kit.dicts(Kit.call_on(&"Editor", &"parts"))
		for p: Dictionary in parts:
			var s := int(p.get("slot", -1))
			var label := Kit.get_str(p, "label").strip_edges()
			_parts_names[s] = label if not label.is_empty() else Inspector.generated_name(s, parts)
	var n: String = _parts_names.get(slot, "")
	return n if not n.is_empty() else tr(&"editor_gauges.detail.slot").format([_num(float(slot + 1), &"none", 0)])


# --- check badge ------------------------------------------------------------------------------------

## Editor states as glyph, word and hue (L1689), drawn on any Control.
class Badges:
	const Kit := preload("res://ui/screens/menu_kit.gd")
	## `Bp.CHECK_*` are 0 Pass, 1 Caution, 2 Fail; these extend them for the badge.
	const ADVISORY := 3
	const UPDATING := 4
	## Pass as the check glyph alone (L1689: "check or in-band value"), for the strip's quiet normal state.
	const PASS_GLYPH := 5
	const NONE := -1
	const PAD := 6.0
	const GAP := 4.0

	static func word(check: int) -> String:
		match check:
			PASS_GLYPH:
				return word(0)
			ADVISORY:
				return String(TranslationServer.translate(&"editor_gauges.check.advisory"))
			UPDATING:
				return String(TranslationServer.translate(&"editor_gauges.check.updating"))
		var tx := Kit.autoload(&"Tx")
		if tx != null and check >= 0 and check <= 2:
			var w := String(tx.call(&"word", &"check", check))
			if not w.is_empty():
				return w
		var keys: Array[StringName] = [&"editor_gauges.check.pass", &"editor_gauges.check.caution", &"editor_gauges.check.fail"]
		return String(TranslationServer.translate(keys[clampi(check, 0, 2)]))

	static func icon(check: int) -> StringName:
		match check:
			0, PASS_GLYPH:
				return &"check_pass"
			1:
				return &"check_caution"
			2:
				return &"check_fail"
			UPDATING:
				return &"reload"
		return &""

	static func filled(check: int) -> bool:
		return check == 1 or check == 2

	## Width of a badge of `check` in `font` at `size`.
	static func width(check: int, font: Font, size: int) -> float:
		if check < 0 or font == null:
			return 0.0
		var h := font.get_height(size)
		if check == PASS_GLYPH:
			return h
		var icon_w := (h + GAP) if icon(check) != &"" else 0.0
		return (PAD * 2.0 if filled(check) else 0.0) + icon_w + font.get_string_size(word(check), HORIZONTAL_ALIGNMENT_LEFT, -1, size).x

	## Draw a badge with its top-left at `pos`; returns its size. Pass: check glyph and word in
	## text.primary. Caution: triangle and word on the alert.caution badge. Fail: cross and word on the
	## alert.warning badge. Badge text is the Severity type's surface.inset. Advisory: the word in
	## text.secondary. Updating: the stale glyph and word in text.secondary.
	static func draw(c: Control, pos: Vector2, check: int, font: Font, size: int) -> Vector2:
		if check < 0 or font == null:
			return Vector2.ZERO
		var h := font.get_height(size)
		var w := width(check, font, size)
		var colour := c.get_theme_color(&"text_primary", &"Gauge")
		var x := pos.x
		if filled(check):
			var box := c.get_theme_stylebox(&"badge_caution" if check == 1 else &"badge_warning", &"Severity")
			if box != null:
				c.draw_style_box(box, Rect2(pos, Vector2(w, h)))
			colour = c.get_theme_color(&"badge_text" if check == 1 else &"badge_text_warning", &"Severity")
			x += PAD
		elif check == ADVISORY or check == UPDATING:
			colour = c.get_theme_color(&"text_secondary", &"Gauge")
		var stem := icon(check)
		if stem != &"":
			var tex := Ui.icon(stem)
			if tex != null:
				c.draw_texture_rect(tex, Rect2(Vector2(x, pos.y + 1.0), Vector2(h - 2.0, h - 2.0)), false, colour)
			x += h + GAP
		if check != PASS_GLYPH:
			c.draw_string(font, Vector2(x, pos.y + font.get_ascent(size)), word(check), HORIZONTAL_ALIGNMENT_LEFT, -1, size, colour)
		return Vector2(w, h)


## A check badge as a control, for rows in the other dock panels. `big` uses the Body size.
class CheckBadge extends Control:
	var check := 0:
		set(v):
			check = v
			accessibility_name = Badges.word(v) if v >= 0 else ""
			update_minimum_size()
			queue_redraw()
	var big := false:
		set(v):
			big = v
			update_minimum_size()
			queue_redraw()

	func _init() -> void:
		mouse_filter = Control.MOUSE_FILTER_IGNORE
		size_flags_vertical = Control.SIZE_SHRINK_CENTER
		size_flags_horizontal = Control.SIZE_SHRINK_BEGIN

	func _notification(what: int) -> void:
		if what == NOTIFICATION_THEME_CHANGED or what == NOTIFICATION_TRANSLATION_CHANGED:
			update_minimum_size()
			queue_redraw()

	func _font() -> Font:
		return get_theme_font(&"value", &"Gauge") if big else get_theme_font(&"units", &"Gauge")

	func _size() -> int:
		return get_theme_font_size(&"value", &"Gauge") if big else get_theme_font_size(&"units", &"Gauge")

	func _get_minimum_size() -> Vector2:
		var f := _font()
		if f == null or check < 0:
			return Vector2.ZERO
		return Vector2(ceilf(Badges.width(check, f, _size())), ceilf(f.get_height(_size())))

	func _draw() -> void:
		Badges.draw(self, Vector2.ZERO, check, _font(), _size())


# --- one gauge cell ---------------------------------------------------------------------------------

## One gauge in the strip, drawn from the Gauge theme type. A button: `pressed` on click release inside
## it, or on accept while focused.
class GaugeCell extends Control:
	signal pressed

	const PAD_X := 8.0
	const PAD_Y := 4.0
	const COL_GAP := 10.0
	const SCALE_H := 12.0
	const PLOT_W := 92.0
	const POLAR_W := 52.0
	const MIN_SCALE_W := 120.0

	## Room for the "Updating" badge whatever the check, so the strip does not reflow when it goes stale.
	var reserve_stale := false
	## 1 drops the coverage rose, 2 the GZ sparkline too, when even the short strip does not fit.
	var tight := 0:
		set(v):
			if v == tight:
				return
			tight = v
			update_minimum_size()
			queue_redraw()
	## Name and check only (the strip's `headline`).
	var headline := false:
		set(v):
			if v == headline:
				return
			headline = v
			update_minimum_size()
			queue_redraw()
	## Short labels, when the full strip does not fit the screen.
	var compact := false:
		set(v):
			if v == compact:
				return
			compact = v
			update_minimum_size()
			queue_redraw()
	var enabled := true:
		set(v):
			enabled = v
			mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND if v else Control.CURSOR_ARROW
			queue_redraw()
	var _title := ""
	var _spec: Dictionary = {}
	var _stale := false
	var _hover := false
	var _press := false

	func _init() -> void:
		focus_mode = Control.FOCUS_ALL
		mouse_filter = Control.MOUSE_FILTER_STOP
		mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND
		mouse_entered.connect(_on_hover.bind(true))
		mouse_exited.connect(_on_hover.bind(false))
		focus_entered.connect(queue_redraw)
		focus_exited.connect(queue_redraw)

	func _on_hover(on: bool) -> void:
		_hover = on
		if not on:
			_press = false
		queue_redraw()

	func _t(v: Variant) -> String:
		if v is Array:
			var a := v as Array
			return "" if a.is_empty() else str(a[1 if compact and a.size() > 1 else 0])
		return str(v)

	func _check() -> int:
		if _stale:
			return Badges.UPDATING
		var c := int(_spec.get("check", Badges.NONE))
		return Badges.PASS_GLYPH if c == 0 else c

	func show_spec(title: String, spec: Dictionary, stale: bool) -> void:
		_title = title
		_spec = spec
		_stale = stale
		var check := _check()
		accessibility_name = ", ".join(PackedStringArray([title, Badges.word(check) if check >= 0 else "", String(spec.get("access", ""))])).replace(", , ", ", ")
		update_minimum_size()
		queue_redraw()

	func _gui_input(event: InputEvent) -> void:
		if not enabled:
			return
		if event is InputEventMouseButton and (event as InputEventMouseButton).button_index == MOUSE_BUTTON_LEFT:
			var mb := event as InputEventMouseButton
			if mb.pressed:
				_press = true
			elif _press:
				_press = false
				if Rect2(Vector2.ZERO, size).has_point(mb.position):
					pressed.emit()
			queue_redraw()
			accept_event()
		elif event.is_action_pressed(&"ui_accept", false, true):
			pressed.emit()
			accept_event()

	func _label_font() -> Font:
		return get_theme_font(&"units", &"Gauge")

	func _label_size() -> int:
		return get_theme_font_size(&"units", &"Gauge")

	func _value_font() -> Font:
		return get_theme_font(&"value", &"Gauge")

	func _value_size() -> int:
		return get_theme_font_size(&"value", &"Gauge")

	## Height of a readout row: the value's ascent and a 4 px gap (tabular rows, not running text).
	func _line_h() -> float:
		var lf := _label_font()
		var vf := _value_font()
		return ceilf(maxf(lf.get_ascent(_label_size()) if lf != null else 20.0, vf.get_ascent(_value_size()) if vf != null else 22.0) + 4.0)

	func _title_h() -> float:
		var lf := _label_font()
		return lf.get_height(_label_size()) if lf != null else 27.0

	func _text_w(f: Font, s: int, t: String) -> float:
		return f.get_string_size(t, HORIZONTAL_ALIGNMENT_LEFT, -1, s).x if f != null else 0.0

	func _col_width(c: Dictionary) -> float:
		var lf := _label_font()
		var vf := _value_font()
		var ls := _label_size()
		var vs := _value_size()
		match String(c.get("kind", "")):
			"text":
				var w := 0.0
				for r: Array in c.get("rows", []):
					w = maxf(w, _text_w(lf, ls, _t(r[0])) + 12.0 + _text_w(vf, vs, _t(r[1])))
				return w
			"scale":
				var top := _text_w(lf, ls, _t(c.get("label", ""))) + 12.0 + _text_w(vf, vs, _t(c.get("value", "")))
				return maxf(MIN_SCALE_W, maxf(top, _text_w(lf, ls, _t(c.get("sub", "")))))
			"gz":
				return maxf(PLOT_W, _text_w(lf, ls, _t(c.get("label", ""))) + 8.0 + _text_w(vf, vs, _t(c.get("value", ""))))
			"polar":
				return maxf(POLAR_W, _text_w(lf, ls, _t(c.get("label", ""))) + 8.0 + _text_w(vf, vs, _t(c.get("value", ""))))
		return 120.0

	func _shown_cols() -> Array:
		var out: Array = []
		for c: Dictionary in _spec.get("cols", []):
			var kind := String(c.get("kind", ""))
			if (tight >= 1 and kind == "polar") or (tight >= 2 and kind == "gz"):
				continue
			out.append(c)
		return out

	func _cols_width() -> float:
		var w := 0.0
		var cols := _shown_cols()
		for i in cols.size():
			w += _col_width(cols[i]) + (COL_GAP if i > 0 else 0.0)
		return w

	func _get_minimum_size() -> Vector2:
		var badge_w := Badges.width(_check(), _label_font(), _label_size())
		if reserve_stale:
			badge_w = maxf(badge_w, Badges.width(Badges.UPDATING, _label_font(), _label_size()))
		var title_w := _text_w(_label_font(), _label_size(), _title) + 12.0 + badge_w
		if headline:
			return Vector2(ceilf(title_w + PAD_X * 2.0), ceilf(PAD_Y * 2.0 + _title_h()))
		return Vector2(ceilf(maxf(_cols_width(), title_w) + PAD_X * 2.0), ceilf(PAD_Y * 2.0 + _title_h() + 2.0 + _line_h() * 3.0))

	func _draw() -> void:
		var lf := _label_font()
		var vf := _value_font()
		if lf == null or vf == null:
			return
		var ls := _label_size()
		var vs := _value_size()
		var line := _line_h()
		var rule := float(get_theme_constant(&"stroke_rule", &"Gauge"))
		if _press and enabled:
			draw_rect(Rect2(Vector2.ZERO, size), get_theme_color(&"surface_inset", &"Gauge"), true)
		elif _hover and enabled:
			draw_rect(Rect2(Vector2.ZERO, size).grow(-rule * 0.5), get_theme_color(&"text_secondary", &"Gauge"), false, rule)
		var units := get_theme_color(&"units", &"Gauge")
		var value_col := get_theme_color(&"text_secondary", &"Gauge") if _stale else get_theme_color(&"value", &"Gauge")
		# Title row: the gauge's name on the left, its check on the right.
		var y := PAD_Y
		draw_string(lf, Vector2(PAD_X, y + lf.get_ascent(ls)), _title, HORIZONTAL_ALIGNMENT_LEFT, -1, ls, get_theme_color(&"text_primary", &"Gauge"))
		var check := _check()
		if check >= 0:
			var bw := Badges.width(check, lf, ls)
			Badges.draw(self, Vector2(size.x - PAD_X - bw, y), check, lf, ls)
		y += _title_h() + 2.0
		var cols := _shown_cols() if not headline else []
		var x := PAD_X
		var extra := maxf(0.0, size.x - PAD_X * 2.0 - _cols_width())
		var stretchers := 0
		for c: Dictionary in cols:
			if String(c.get("kind", "")) != "polar":
				stretchers += 1
		for i in cols.size():
			var c: Dictionary = cols[i]
			var w := _col_width(c)
			if String(c.get("kind", "")) != "polar" and stretchers > 0:
				w += extra / stretchers
			_draw_col(c, Rect2(x, y, w, line * 3.0), line, units, value_col)
			x += w + COL_GAP
		if has_focus():
			var focus := get_theme_stylebox(&"focus", &"Button")
			if focus != null:
				draw_style_box(focus, Rect2(Vector2.ZERO, size))

	func _draw_col(c: Dictionary, r: Rect2, line: float, units: Color, value_col: Color) -> void:
		var lf := _label_font()
		var vf := _value_font()
		var ls := _label_size()
		var vs := _value_size()
		var kind := String(c.get("kind", ""))
		# Label and value share a baseline near the foot of the row.
		var la := line - 4.0
		var va := line - 4.0
		match kind:
			"text":
				var y := r.position.y
				for row: Array in c.get("rows", []):
					draw_string(lf, Vector2(r.position.x, y + la), _t(row[0]), HORIZONTAL_ALIGNMENT_LEFT, r.size.x, ls, units)
					draw_string(vf, Vector2(r.position.x, y + va), _t(row[1]), HORIZONTAL_ALIGNMENT_RIGHT, r.size.x, vs, value_col)
					y += line
			"scale":
				draw_string(lf, Vector2(r.position.x, r.position.y + la), _t(c.get("label", "")), HORIZONTAL_ALIGNMENT_LEFT, r.size.x, ls, units)
				draw_string(vf, Vector2(r.position.x, r.position.y + va), _t(c.get("value", "")), HORIZONTAL_ALIGNMENT_RIGHT, r.size.x, vs, value_col)
				_draw_scale(c, Rect2(r.position.x, r.position.y + line + (line - SCALE_H) * 0.5, r.size.x, SCALE_H))
				draw_string(lf, Vector2(r.position.x, r.position.y + line * 2.0 + la), _t(c.get("sub", "")), HORIZONTAL_ALIGNMENT_LEFT, r.size.x, ls, units)
			"gz", "polar":
				draw_string(lf, Vector2(r.position.x, r.position.y + la), _t(c.get("label", "")), HORIZONTAL_ALIGNMENT_LEFT, r.size.x, ls, units)
				draw_string(vf, Vector2(r.position.x, r.position.y + va), _t(c.get("value", "")), HORIZONTAL_ALIGNMENT_RIGHT, r.size.x, vs, value_col)
				var area := Rect2(r.position.x, r.position.y + line + 4.0, r.size.x, line * 2.0 - 6.0)
				if kind == "gz":
					Plots.gz(self, area, c.get("points", PackedVector2Array()), float(c.get("range", 0.0)), false, _stale)
				else:
					var side := minf(area.size.x, area.size.y)
					Plots.polar(self, Rect2(area.end.x - side, area.position.y, side, side), c.get("sectors", PackedFloat32Array()), false, _stale)

	## A readout against its band and limits, as the Gauge widget draws one (L1597): inset well, band
	## strip, limit ticks (alert colour only while the value is past them, HUD rule 1), the needle.
	func _draw_scale(c: Dictionary, well: Rect2) -> void:
		var lo := float(c.get("min", 0.0))
		var hi := float(c.get("max", 1.0))
		var v := float(c.get("v", NAN))
		var rule := float(get_theme_constant(&"stroke_rule", &"Gauge"))
		var frame := float(get_theme_constant(&"stroke_frame", &"Gauge"))
		draw_rect(well, get_theme_color(&"background", &"Gauge"), true)
		draw_rect(well.grow(-rule * 0.5), get_theme_color(&"frame", &"Gauge"), false, rule)
		var inner := well.grow(-rule)
		var blo := float(c.get("band_lo", NAN))
		var bhi := float(c.get("band_hi", NAN))
		if is_finite(blo) or is_finite(bhi):
			var x0 := _px(blo if is_finite(blo) else lo, lo, hi, inner)
			var x1 := _px(bhi if is_finite(bhi) else hi, lo, hi, inner)
			var bh := minf(float(get_theme_constant(&"band_thickness", &"Gauge")) * 0.5, inner.size.y)
			draw_rect(Rect2(x0, inner.position.y + (inner.size.y - bh) * 0.5, maxf(1.0, x1 - x0), bh), get_theme_color(&"band", &"Gauge"), true)
		var minor := float(get_theme_constant(&"tick_length_minor", &"Gauge"))
		for m: Variant in c.get("minor", []):
			var mx := _px(float(m), lo, hi, inner)
			draw_line(Vector2(mx, inner.end.y - minor), Vector2(mx, inner.end.y), get_theme_color(&"scale", &"Gauge"), rule)
		var tick := float(get_theme_constant(&"tick_length", &"Gauge"))
		var reach := maxf(0.0, (tick - inner.size.y) * 0.5)
		var limits: Array = [
			[float(c.get("caution_lo", NAN)), is_finite(v) and v <= float(c.get("caution_lo", NAN)), &"limit_caution_active"],
			[float(c.get("caution_hi", NAN)), is_finite(v) and v >= float(c.get("caution_hi", NAN)), &"limit_caution_active"],
			[float(c.get("warning_lo", NAN)), is_finite(v) and v <= float(c.get("warning_lo", NAN)), &"limit_warning_active"],
			[float(c.get("warning_hi", NAN)), is_finite(v) and v > float(c.get("warning_hi", NAN)), &"limit_warning_active"],
		]
		for m: Variant in c.get("marks", []):
			limits.append([float(m), false, &"limit"])
		# A limit lights only while the gauge's own check agrees it is past it.
		var check := _check()
		for l: Array in limits:
			var lv: float = l[0]
			if not is_finite(lv):
				continue
			var lx := _px(lv, lo, hi, inner)
			var active: bool = l[1] and (check == 1 or check == 2)
			draw_line(Vector2(lx, inner.position.y - reach), Vector2(lx, inner.end.y + reach), get_theme_color(l[2] if active else &"limit", &"Gauge"), frame)
		if is_finite(v):
			var nx := _px(v, lo, hi, inner)
			var needle := get_theme_color(&"text_secondary", &"Gauge") if _stale else get_theme_color(&"needle", &"Gauge")
			draw_line(Vector2(nx, well.position.y - 2.0), Vector2(nx, well.end.y + 2.0), needle, frame + 1.0)

	static func _px(x: float, lo: float, hi: float, inner: Rect2) -> float:
		if not is_finite(x) or is_equal_approx(hi, lo):
			return inner.position.x
		return inner.position.x + clampf((x - lo) / (hi - lo), 0.0, 1.0) * inner.size.x


## Drawing of the GZ curve and the coverage rose, shared by the strip and the detail sheet.
class Plots:
	const Kit := preload("res://ui/screens/menu_kit.gd")

	## The righting arm (metres) against heel (degrees), with the zero line and the range of stability.
	## `axes` adds heel ticks and the peak arm for the detail sheet.
	static func gz(c: Control, area: Rect2, pts: PackedVector2Array, range_deg: float, axes: bool, stale: bool) -> void:
		var rule := float(c.get_theme_constant(&"stroke_rule", &"Gauge"))
		var frame := float(c.get_theme_constant(&"stroke_frame", &"Gauge"))
		c.draw_rect(area, c.get_theme_color(&"background", &"Gauge"), true)
		if pts.size() < 2:
			return
		var max_h := 10.0
		var max_a := 0.0
		var min_a := 0.0
		for p: Vector2 in pts:
			if is_finite(p.x) and is_finite(p.y):
				max_h = maxf(max_h, p.x)
				max_a = maxf(max_a, p.y)
				min_a = minf(min_a, p.y)
		var span := maxf(max_a - min_a, 0.01)
		var top := max_a + span * 0.12
		var bottom := min_a - span * 0.12
		var pad := Vector2(4.0, 2.0)
		var inner := area.grow_individual(-pad.x, -pad.y, -pad.x, -pad.y)
		var zy := inner.position.y + (top / (top - bottom)) * inner.size.y
		c.draw_line(Vector2(inner.position.x, zy), Vector2(inner.end.x, zy), c.get_theme_color(&"rule_reference", &"Gauge"), rule)
		if range_deg > 0.0:
			var rx := inner.position.x + clampf(range_deg / max_h, 0.0, 1.0) * inner.size.x
			c.draw_line(Vector2(rx, inner.position.y), Vector2(rx, inner.end.y), c.get_theme_color(&"limit", &"Gauge"), rule)
		var line := PackedVector2Array()
		for p: Vector2 in pts:
			if is_finite(p.x) and is_finite(p.y):
				line.append(Vector2(inner.position.x + p.x / max_h * inner.size.x, inner.position.y + (top - p.y) / (top - bottom) * inner.size.y))
		if line.size() >= 2:
			c.draw_polyline(line, c.get_theme_color(&"text_secondary" if stale else &"value", &"Gauge"), frame, true)
		if axes:
			var f := c.get_theme_font(&"scale", &"Gauge")
			var s := c.get_theme_font_size(&"scale", &"Gauge")
			var col := c.get_theme_color(&"scale", &"Gauge")
			if f != null:
				var h := 0.0
				while h <= max_h + 0.01:
					var tx := inner.position.x + h / max_h * inner.size.x
					c.draw_line(Vector2(tx, inner.end.y), Vector2(tx, inner.end.y - 6.0), col, rule)
					var txt := Kit.number(h, &"deg", 0)
					if int(roundf(h)) % 30 == 0 and tx + 3.0 + f.get_string_size(txt, HORIZONTAL_ALIGNMENT_LEFT, -1, s).x <= inner.end.x:
						c.draw_string(f, Vector2(tx + 3.0, inner.end.y - 8.0), txt, HORIZONTAL_ALIGNMENT_LEFT, -1, s, col)
					h += 10.0
				c.draw_string(f, Vector2(inner.position.x + 4.0, inner.position.y + f.get_ascent(s)), Kit.number(max_a, &"m", 2), HORIZONTAL_ALIGNMENT_LEFT, -1, s, col)

	## Coverage of the sectors (36 of 10 degrees from the bow, bow up): each is filled out to its share in
	## text.secondary; the outer ring and the bow line are references.
	static func polar(c: Control, area: Rect2, cov: PackedFloat32Array, labels: bool, stale: bool) -> void:
		var centre := area.get_center()
		var radius := minf(area.size.x, area.size.y) * 0.5 - (26.0 if labels else 1.0)
		if radius <= 2.0:
			return
		var rule := float(c.get_theme_constant(&"stroke_rule", &"Gauge"))
		c.draw_circle(centre, radius, c.get_theme_color(&"background", &"Gauge"), true)
		var fill := c.get_theme_color(&"rule_divider" if stale else &"text_secondary", &"Gauge")
		var n := cov.size()
		for i in n:
			var share := clampf(cov[i], 0.0, 1.0) if is_finite(cov[i]) else 0.0
			if share <= 0.0:
				continue
			var a0 := deg_to_rad(float(i) * 360.0 / n - 90.0)
			var a1 := deg_to_rad(float(i + 1) * 360.0 / n - 90.0)
			var poly := PackedVector2Array([centre])
			for k in 5:
				var a := lerpf(a0, a1, k / 4.0)
				poly.append(centre + Vector2(cos(a), sin(a)) * radius * share)
			c.draw_colored_polygon(poly, fill)
		c.draw_arc(centre, radius, 0.0, TAU, 64, c.get_theme_color(&"rule_reference", &"Gauge"), rule, true)
		c.draw_line(centre, centre + Vector2(0.0, -radius), c.get_theme_color(&"value", &"Gauge"), rule)
		if labels:
			var f := c.get_theme_font(&"scale", &"Gauge")
			var s := c.get_theme_font_size(&"scale", &"Gauge")
			var col := c.get_theme_color(&"scale", &"Gauge")
			if f != null:
				for deg: int in [0, 90, 180, 270]:
					var a := deg_to_rad(float(deg) - 90.0)
					var txt := String(Kit.call_on(&"Tx", &"bearing", [float(deg)])) if Kit.autoload(&"Tx") != null else str(deg)
					var tw := f.get_string_size(txt, HORIZONTAL_ALIGNMENT_LEFT, -1, s).x
					var p := centre + Vector2(cos(a), sin(a)) * (radius + 14.0)
					c.draw_string(f, p + Vector2(-tw * 0.5, f.get_ascent(s) * 0.4), txt, HORIZONTAL_ALIGNMENT_LEFT, -1, s, col)


## A labelled plot in the detail sheet.
class DetailPlot extends Control:
	var kind: StringName = &"gz"
	var points := PackedVector2Array()
	var sectors := PackedFloat32Array()
	var range_deg := 0.0

	func _init() -> void:
		mouse_filter = Control.MOUSE_FILTER_IGNORE
		size_flags_horizontal = Control.SIZE_EXPAND_FILL

	func _draw() -> void:
		if kind == &"gz":
			Plots.gz(self, Rect2(Vector2.ZERO, size), points, range_deg, true, false)
		else:
			Plots.polar(self, Rect2(Vector2.ZERO, size), sectors, true, false)
