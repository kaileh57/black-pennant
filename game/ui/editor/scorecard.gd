extends VBoxContainer
## Trial scorecard and Compare view.
## DESIGN.md L227-229. Unit: ui-editor-trials.
## Reads and calls: Editor.scorecard, Editor.scorecard_history, Editor.compare, Editor.run_trials.
##
## The right dock's Trials tab. On each save the background worker runs the trial battery (L227: turning
## circle, acceleration, heel recovery and a one-against-three fight over 3 seeds); this tab shows the
## newest result as a spec sheet, label over value (copy rule 7 allows that on spec sheets):
## - the fight first, because it is the battery's verdict: its score (100 x enemy tonnage share disabled,
##   less 50 x own cells share lost, averaged over seeds) with each seed's score, fights won, hull left,
##   rounds per kill, first-salvo hit rate and the three Combine classes it met;
## - handling: turning circle, time to 90% of top speed, top speed, heel recovery and peak heel;
## - complexity points, with the Caution triangle above the gauge's Caution level (L976);
## - "Short trial" when the on-save fight was cut to 1 seed of 90 s (the L227 fallback);
## - drift: a gauge whose prediction is more than 10% off the simulation (L229), each with Caution.
## Compare: pick an earlier version of this design file; each metric shows before, after and whether the
## change is better, worse or the same, as a word with a direction glyph (never hue alone).
##
## States: dock closed; design never saved (trials run on save); running (the worker's `trials` job, with
## the earlier result kept on screen); failed; a result; no earlier versions to compare.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Strip := preload("res://ui/editor/gauge_strip.gd")

## A gauge prediction this far off the simulation is drift (L229).
const DRIFT_LIMIT := 0.10
const DRIFT_KEYS: Dictionary[String, StringName] = {
	"float": &"editor_gauges.float", "speed": &"editor_gauges.speed", "power": &"editor_gauges.power",
	"endurance": &"editor_gauges.endurance", "punch": &"editor_gauges.punch", "sight": &"editor_gauges.sight",
	"resilience": &"editor_gauges.resilience", "complexity": &"editor_gauges.complexity",
}

var _status: Label
var _run: Button
var _body: VBoxContainer
var _compare_pick: OptionButton
var _compare_box: VBoxContainer
var _card: Dictionary = {}
var _history: Array[Dictionary] = []
var _job := -1
var _open := false
var _saved := false
var _caution_at := NAN
var _compare_hash := ""
var _refresh_queued := false


func _ready() -> void:
	Ui.set_separation(self, 0)
	accessibility_name = tr(&"editor_trials.name")
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 2)
	add_child(margin)
	var head := Kit.hbox(2)
	margin.add_child(head)
	_status = Kit.label("", &"Body", true)
	_status.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	head.add_child(_status)
	_run = Kit.inline_button("editor_trials.run")
	_run.tooltip_text = tr(&"editor_trials.run.tip")
	_run.set_meta(&"control_id", "editor.trials.run")
	_run.pressed.connect(_on_run)
	head.add_child(_run)
	var scroll := ScrollContainer.new()
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	add_child(scroll)
	var inner := MarginContainer.new()
	inner.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	inner.add_theme_constant_override(&"margin_left", Ui.spacing(2))
	inner.add_theme_constant_override(&"margin_right", Ui.spacing(2))
	inner.add_theme_constant_override(&"margin_top", 0)
	inner.add_theme_constant_override(&"margin_bottom", Ui.spacing(2))
	scroll.add_child(inner)
	var column := Kit.vbox(3)
	column.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	inner.add_child(column)
	_body = Kit.vbox(2)
	column.add_child(_body)
	column.add_child(HSeparator.new())
	# The label sits over the list, so a version's name ("No earlier version yet", a dated save) has the
	# dock's whole width and is never cut.
	var pick_row := Kit.vbox(1)
	column.add_child(pick_row)
	var pick_label := Kit.label("editor_trials.compare", &"Body")
	pick_label.autowrap_mode = TextServer.AUTOWRAP_OFF
	pick_row.add_child(pick_label)
	_compare_pick = OptionButton.new()
	_compare_pick.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_compare_pick.custom_minimum_size.y = Kit.BUTTON_H
	_compare_pick.fit_to_longest_item = false
	_compare_pick.clip_text = true
	_compare_pick.accessibility_name = tr(&"editor_trials.compare")
	_compare_pick.set_meta(&"control_id", "editor.trials.compare")
	_compare_pick.item_selected.connect(_on_compare_picked)
	pick_row.add_child(_compare_pick)
	_compare_box = Kit.vbox(1)
	column.add_child(_compare_box)
	var editor := Kit.autoload(&"Editor")
	if editor != null and editor.has_signal(&"scorecard_changed"):
		editor.connect(&"scorecard_changed", _queue_refresh)
		editor.connect(&"job_changed", _on_job_changed)
		editor.connect(&"saved", func(_p: String) -> void: _queue_refresh())
		editor.connect(&"design_changed", func(_r: int) -> void: _queue_refresh())
	var tutorial := Kit.autoload(&"Tutorial")
	if tutorial != null and tutorial.has_signal(&"step_changed"):
		tutorial.connect(&"step_changed", func(_s: Dictionary) -> void: _queue_refresh())
	Kit.chain_vertical([_run, _compare_pick])
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		accessibility_name = tr(&"editor_trials.name")
		_queue_refresh()


func _on_job_changed(job: StringName, state: int) -> void:
	if job == &"trials":
		_job = state
		_queue_refresh()


func _queue_refresh() -> void:
	if _refresh_queued:
		return
	_refresh_queued = true
	_refresh.call_deferred()


func _refresh() -> void:
	_refresh_queued = false
	var e := Kit.autoload(&"Editor")
	_open = e != null and e.has_method(&"scorecard") and bool(e.call(&"is_open"))
	var card: Variant = e.call(&"scorecard") if _open else null
	_card = card if card is Dictionary else {}
	_history = Kit.dicts(e.call(&"scorecard_history") if _open else [])
	_saved = _open and not String(e.call(&"design_path")).is_empty()
	var g := Kit.as_dict(e.call(&"gauges")) if _open else {}
	_caution_at = float(Kit.as_dict(g.get("complexity", {})).get("caution_at", NAN))
	render()


## Draw `_card` and `_history` (captures fill them by hand).
func render() -> void:
	var running := _job == Bp.JOB_RUNNING
	_run.disabled = not _open or running or not _saved
	if not _open:
		_status.text = tr(&"editor_trials.closed")
	elif running:
		_status.text = tr(&"editor_trials.running_short" if bool(_card.get("short_trial", false)) else &"editor_trials.running")
	elif _job == Bp.JOB_FAILED:
		_status.text = tr(&"editor_trials.failed")
	elif _card.is_empty():
		_status.text = tr(&"editor_trials.unsaved" if not _saved else &"editor_trials.none")
	else:
		_status.text = tr(&"editor_trials.last").format([Kit.local_time(int(_card.get("saved_unix", 0)))])
	Kit.clear(_body)
	if not _card.is_empty():
		_render_card()
	_fill_compare_pick()
	_render_compare()
	_apply_tutorial()


func _f(d: Dictionary, key: String) -> float:
	var v: Variant = d.get(key, 0.0)
	return float(v) if (v is float or v is int) and is_finite(float(v)) else 0.0


func _render_card() -> void:
	var c := _card
	if bool(c.get("short_trial", false)):
		_body.add_child(Kit.label(&"editor_trials.short", &"Body"))
	_body.add_child(Kit.label(&"editor_trials.fight", &"Body"))
	var seeds: PackedStringArray = []
	for s: Variant in Strip._list(c.get("per_seed", PackedFloat32Array())):
		seeds.append(Kit.number(float(s), &"none", 0))
	var n_seeds := seeds.size()
	# Each Combine class once, with how many of it the fights met.
	var counts: Dictionary[String, int] = {}
	var order: PackedStringArray = []
	for id: Variant in Strip._list(c.get("opponents", [])):
		var key := str(id)
		if not counts.has(key):
			counts[key] = 0
			order.append(key)
		counts[key] += 1
	var opponents: PackedStringArray = []
	for key: String in order:
		var info := Kit.as_dict(Kit.call_on(&"Content", &"class_info", [StringName(key)]))
		var nm := _Text.resolve(StringName(str(info.get("name", ""))))
		nm = nm if not nm.is_empty() else key
		opponents.append(nm if counts[key] == 1 else tr(&"editor_trials.opponent_count").format([nm, Kit.number(float(counts[key]), &"none", 0)]))
	var fight_rows: Array = [
		[&"editor_trials.score", Kit.number(_f(c, "score"), &"none", 0)],
		[&"editor_trials.per_seed", ", ".join(seeds)],
		[&"editor_trials.wins", tr(&"editor_trials.wins_value").format([Kit.number(_f(c, "wins"), &"none", 0), Kit.number(float(n_seeds), &"none", 0)])],
		[&"editor_trials.hull_left", Kit.percent(_f(c, "hull_left"))],
		[&"editor_trials.rounds_per_kill", Kit.number(_f(c, "rounds_per_kill"), &"rounds", 0) if _f(c, "rounds_per_kill") > 0.0 else tr(&"editor_trials.no_kills")],
		[&"editor_trials.first_salvo", Kit.percent(_f(c, "first_salvo_hit_rate"))],
	]
	_body.add_child(_grid(fight_rows))
	if not opponents.is_empty():
		# A list, not a cell: class names are long and the dock is narrow.
		_body.add_child(Kit.label(tr(&"editor_trials.opponents_line").format(["; ".join(opponents)]), &"Dense", true))
	_body.add_child(Kit.label(&"editor_trials.handling", &"Body"))
	_body.add_child(_grid([
		[&"dock.metric.turning_circle_m", Kit.number(_f(c, "turning_circle_m"), &"m", 0)],
		[&"dock.metric.time_to_90pct_speed_s", Kit.number(_f(c, "time_to_90pct_speed_s"), &"s", 1)],
		[&"dock.metric.top_speed_kn", Kit.number(_f(c, "top_speed_kn"), &"kn", 1)],
		[&"dock.metric.heel_recovery_s", Kit.number(_f(c, "heel_recovery_s"), &"s", 1)],
		[&"dock.metric.peak_heel_deg", Kit.number(_f(c, "peak_heel_deg"), &"deg", 1)],
	]))
	var pts := _f(c, "complexity_points")
	var crow := Kit.hbox(2)
	var clabel := Kit.label(&"dock.metric.complexity_points", &"Dense")
	clabel.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	crow.add_child(clabel)
	if is_finite(_caution_at) and _caution_at > 0.0 and pts > _caution_at:
		var badge := Strip.CheckBadge.new()
		badge.check = 1
		crow.add_child(badge)
	var cval := Kit.label(Kit.number(pts, &"none", 0), &"Body", true)
	cval.autowrap_mode = TextServer.AUTOWRAP_OFF
	crow.add_child(cval)
	_body.add_child(crow)
	var drift := Kit.dicts(c.get("drift", []))
	for d: Dictionary in drift:
		var predicted := _f(d, "predicted")
		var simulated := _f(d, "simulated")
		var off := absf(predicted - simulated) / maxf(absf(simulated), 1e-6)
		var row := Kit.hbox(2)
		var badge := Strip.CheckBadge.new()
		badge.check = 1 if off > DRIFT_LIMIT else Strip.Badges.ADVISORY
		row.add_child(badge)
		var gauge_name: String = tr(DRIFT_KEYS.get(String(d.get("gauge", "")), &"")) if DRIFT_KEYS.has(String(d.get("gauge", ""))) else String(d.get("gauge", ""))
		var text := Kit.label(tr(&"editor_trials.drift").format([gauge_name, Kit.number(predicted, &"none", 1), Kit.number(simulated, &"none", 1), Kit.number(off * 100.0, &"pct", 0)]), &"Dense", true)
		text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(text)
		_body.add_child(row)


func _grid(rows: Array) -> GridContainer:
	var grid := GridContainer.new()
	grid.columns = 2
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(3))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	for r: Array in rows:
		var l := Kit.label(String(r[0]), &"Dense")
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(l)
		var v := Kit.label(String(r[1]), &"Body", true)
		v.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		v.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(v)
	return grid


# --- compare ----------------------------------------------------------------------------------------

func _fill_compare_pick() -> void:
	_compare_pick.clear()
	if _history.is_empty():
		_compare_pick.add_item(tr(&"editor_trials.compare.none"))
		_compare_pick.set_item_disabled(0, true)
		_compare_pick.disabled = true
		_compare_hash = ""
		return
	_compare_pick.disabled = _card.is_empty()
	var keep := -1
	for i in _history.size():
		var h := _history[i]
		var label := String(h.get("label", ""))
		var text := tr(&"editor_trials.compare.item").format([Kit.local_time(int(h.get("saved_unix", 0))), label if not label.is_empty() else tr(&"editor_trials.compare.unnamed"), Kit.number(_f(h, "score"), &"none", 0)])
		_compare_pick.add_item(text, i)
		_compare_pick.set_item_metadata(i, String(h.get("hash", "")))
		if String(h.get("hash", "")) == _compare_hash:
			keep = i
	if keep < 0:
		keep = 0
	_compare_pick.select(keep)
	_compare_hash = String(_compare_pick.get_item_metadata(keep))


func _on_compare_picked(index: int) -> void:
	_compare_hash = String(_compare_pick.get_item_metadata(index)) if index >= 0 else ""
	_render_compare()


func _render_compare() -> void:
	Kit.clear(_compare_box)
	if _card.is_empty() or _compare_hash.is_empty():
		if _open and not _card.is_empty():
			_compare_box.add_child(Kit.label(&"editor_trials.compare.empty", &"Dense"))
		return
	var rows := Kit.dicts(Kit.call_on(&"Editor", &"compare", [_compare_hash, String(_card.get("hash", ""))]))
	render_compare_rows(rows)


## The Compare table from `Editor.compare` rows (captures pass them by hand).
func render_compare_rows(rows: Array[Dictionary]) -> void:
	Kit.clear(_compare_box)
	if rows.is_empty():
		_compare_box.add_child(Kit.label(&"editor_trials.compare.empty", &"Dense"))
		return
	# Two lines a measure, so it fits the narrow dock: its name with the change on the right, then the values.
	for r: Dictionary in rows:
		var unit := StringName(str(r.get("unit", "none")))
		var before := _f(r, "before")
		var after := _f(r, "after")
		var entry := Kit.vbox(0)
		var head := Kit.hbox(2)
		var name_label := Kit.label(String(r.get("metric", "")), &"Dense")
		name_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		head.add_child(name_label)
		head.add_child(_change_cell(int(r.get("better", 0)), before, after))
		entry.add_child(head)
		var values := Kit.label(tr(&"editor_trials.compare.from_to").format([_value_text(before, unit), _value_text(after, unit)]), &"Body", true)
		entry.add_child(values)
		entry.accessibility_name = "%s: %s" % [name_label.text, values.text]
		_compare_box.add_child(entry)


func _value_text(v: float, unit: StringName) -> String:
	if unit == &"pct":
		return Kit.percent(v)
	return Kit.number(v, unit, 0 if absf(v) >= 100.0 or unit == &"rounds" or (unit == &"none" and absf(v) >= 10.0) else 1)


## Better, worse or same as a word with the direction of the number as a glyph, in grey (L1606).
func _change_cell(better: int, before: float, after: float) -> Control:
	var row := Kit.hbox(0)
	var icon := TextureRect.new()
	icon.custom_minimum_size = Vector2(20.0, 20.0)
	icon.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	icon.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	icon.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	icon.mouse_filter = Control.MOUSE_FILTER_IGNORE
	if better != 0 and not is_equal_approx(before, after):
		icon.texture = Ui.icon(&"arrow_up" if after > before else &"arrow_down")
		icon.modulate = Ui.colour(&"text.secondary")
	row.add_child(icon)
	var word: StringName = &"editor_trials.compare.same"
	if better > 0:
		word = &"editor_trials.compare.better"
	elif better < 0:
		word = &"editor_trials.compare.worse"
	var l := Kit.label(word, &"Body")
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	row.add_child(l)
	return row


## Run the battery on the saved design. `job_changed` reports it running. When nothing was saved in this
## session the battery has no saved design to run on, so the design is saved, which runs it (L227).
func _on_run() -> void:
	var e := Kit.autoload(&"Editor")
	if e == null:
		return
	e.call(&"run_trials")
	if _job != Bp.JOB_RUNNING and _saved:
		e.call(&"save_design")
	_queue_refresh()


func _apply_tutorial() -> void:
	var t := Kit.autoload(&"Tutorial")
	if t == null or not t.has_method(&"is_allowed"):
		return
	for c: Control in [_run, _compare_pick]:
		if not bool(t.call(&"is_allowed", String(c.get_meta(&"control_id")))):
			(c as BaseButton).disabled = true
