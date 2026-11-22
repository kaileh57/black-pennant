extends VBoxContainer
## Fit Check, Quartermaster proposal and refit cost.
## DESIGN.md L799-807. Unit: ui-editor-fit.
## Reads and calls: Editor.fit_check, Editor.fit_result, Editor.quartermaster, Editor.quartermaster_result, Editor.refit_diff, Campaign.refit_quote.
##
## The right dock's Fit Check tab, shown in a campaign dock (L67, L801). Top to bottom, in the order the
## player works:
## - Fit Check: a 30 s headless run in the briefing's forecast sea (a full-power acceleration run, then a
##   180 degree turn) scores each constraint tag Pass or Fail, as glyph and word, with its value against
##   its limit, then top speed, time to turn 180 degrees, range and detection range against each known
##   threat (ours on them, theirs on us). Under the tags, the seakeeping sweep's verdict as glyph and
##   words: rides out the sea, can roll over (the sea state and where the sea came from), or did not finish
##   (a sweep the 20 s ceiling cut short is never read as safe). It runs by itself when the tab is shown and the result
##   is missing or older than the design, and again on each save while the tab is up; "Check again" runs
##   it by hand.
## - Quartermaster (L803): asks for the cheapest legal refit. It answers that the design already fits, or
##   offers a refit as a diff of every changed part with its cost, to preview as a ghost and accept as one
##   undo step, or says "No legal refit found" and names the blocking tag. It never changes the hull.
## - Refit cost (L805): what the design under edit costs against the fielded hull, in Salvage and theatre
##   hours (two hours a module swap, one hour per 10 t of structure), the parts taken free from the parts
##   store, and Caution when the Salvage on hand does not cover it (Launch then stays blocked).
## `Editor.refit_diff` prices the design under edit, so the cost follows every edit; `Campaign.refit_quote`
## prices the saved design and is shown only when the dock has nothing to price.
##
## States: outside a campaign (an explanation, nothing to run); running; failed; a stale result kept on
## screen with a note; a current result; the Quartermaster's three answers and its running state.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Strip := preload("res://ui/editor/gauge_strip.gd")

var _intro: Label
var _check: Button
var _fit_box: VBoxContainer
var _qm: Button
var _qm_box: VBoxContainer
var _cost_box: VBoxContainer
var _fit: Dictionary = {}
var _qm_result: Dictionary = {}
var _diff: Dictionary = {}
var _budget := -1
var _campaign := false
var _qm_preview := false
var _refresh_queued := false
var _auto_rev := -1


func _ready() -> void:
	Ui.set_separation(self, 0)
	accessibility_name = tr(&"editor_fit.name")
	var scroll := ScrollContainer.new()
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	add_child(scroll)
	var margin := MarginContainer.new()
	margin.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	Ui.set_margins(margin, 2)
	scroll.add_child(margin)
	var column := Kit.vbox(3)
	column.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	margin.add_child(column)
	_intro = Kit.label("", &"Body", true)
	column.add_child(_intro)
	_check = Kit.inline_button("editor_fit.check")
	_check.tooltip_text = tr(&"editor_fit.check.tip")
	_check.set_meta(&"control_id", "editor.fit.check")
	_check.pressed.connect(_on_check)
	column.add_child(_check)
	_fit_box = Kit.vbox(1)
	column.add_child(_fit_box)
	column.add_child(HSeparator.new())
	_qm = Kit.inline_button("editor_fit.qm.ask")
	_qm.tooltip_text = tr(&"editor_fit.qm.ask.tip")
	_qm.set_meta(&"control_id", "editor.fit.quartermaster")
	_qm.pressed.connect(_on_ask_qm)
	column.add_child(_qm)
	_qm_box = Kit.vbox(1)
	column.add_child(_qm_box)
	column.add_child(HSeparator.new())
	_cost_box = Kit.vbox(1)
	column.add_child(_cost_box)
	var e := Kit.autoload(&"Editor")
	if e != null and e.has_signal(&"fit_changed"):
		e.connect(&"fit_changed", _queue_refresh)
		e.connect(&"quartermaster_changed", _queue_refresh)
		e.connect(&"design_changed", _on_design_changed)
		e.connect(&"job_changed", _on_job_changed)
		e.connect(&"saved", _on_saved)
	var tutorial := Kit.autoload(&"Tutorial")
	if tutorial != null and tutorial.has_signal(&"step_changed"):
		tutorial.connect(&"step_changed", func(_s: Dictionary) -> void: _queue_refresh())
	visibility_changed.connect(_on_visibility_changed)
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		accessibility_name = tr(&"editor_fit.name")
		_queue_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


func _on_design_changed(_rev: int) -> void:
	_qm_preview = false
	_queue_refresh()


func _on_job_changed(job: StringName, _state: int) -> void:
	if job == &"fit" or job == &"quartermaster":
		_queue_refresh()


func _on_saved(_path: String) -> void:
	if is_visible_in_tree():
		_auto_check()


func _on_visibility_changed() -> void:
	if is_visible_in_tree():
		_refresh()
		_auto_check()
	elif _qm_preview:
		_qm_preview = false
		Kit.call_on(&"Editor", &"preview_clear")


## Run Fit Check when the tab is up and the result is missing or older than the design, once a revision.
func _auto_check() -> void:
	var e := _editor()
	if e == null or not _campaign:
		return
	var state := int(_fit.get("state", Bp.JOB_IDLE))
	var rev := int(e.call(&"rev"))
	if (state == Bp.JOB_IDLE or state == Bp.JOB_STALE) and rev != _auto_rev:
		_auto_rev = rev
		e.call(&"fit_check")
		_queue_refresh()


func _queue_refresh() -> void:
	if _refresh_queued:
		return
	_refresh_queued = true
	_refresh.call_deferred()


func _refresh() -> void:
	_refresh_queued = false
	var e := _editor()
	var open := e != null and e.has_method(&"fit_result") and bool(e.call(&"is_open"))
	_campaign = open and int(e.call(&"dock_mode")) == Bp.DOCK_CAMPAIGN
	_fit = Kit.as_dict(e.call(&"fit_result")) if _campaign else {}
	_qm_result = Kit.as_dict(e.call(&"quartermaster_result")) if _campaign else {}
	_diff = Kit.as_dict(e.call(&"refit_diff")) if _campaign else {}
	if _campaign and Kit.dicts(_diff.get("changes", [])).is_empty() and _f(_diff, "salvage") <= 0.0:
		var quote := Kit.as_dict(Kit.call_on(&"Campaign", &"refit_quote"))
		if not Kit.dicts(quote.get("changes", [])).is_empty():
			_diff = quote
	_budget = int(Kit.as_dict(e.call(&"limits")).get("budget_salvage", -1)) if _campaign else -1
	render(open)


func _f(d: Dictionary, key: String) -> float:
	var v: Variant = d.get(key, 0.0)
	return float(v) if (v is float or v is int) and is_finite(float(v)) else 0.0


## Draw `_fit`, `_qm_result` and `_diff` (captures fill them by hand).
func render(open: bool) -> void:
	if not open:
		_intro.text = tr(&"editor_fit.closed")
	elif not _campaign:
		_intro.text = tr(&"editor_fit.outside")
	else:
		_intro.text = tr(&"editor_fit.intro")
	var fit_state := int(_fit.get("state", Bp.JOB_IDLE))
	_check.visible = _campaign
	_check.disabled = fit_state == Bp.JOB_RUNNING
	_check.text = tr(&"editor_fit.check_again" if fit_state == Bp.JOB_DONE or fit_state == Bp.JOB_STALE else &"editor_fit.check")
	_render_fit(fit_state)
	var qm_state := int(_qm_result.get("state", Bp.JOB_IDLE))
	_qm.visible = _campaign
	_qm.disabled = qm_state == Bp.JOB_RUNNING
	_render_qm(qm_state)
	_render_cost()
	var chain: Array = [_check]
	chain.append_array(_fit_box.find_children("*", "BaseButton", true, false))
	chain.append(_qm)
	chain.append_array(_qm_box.find_children("*", "BaseButton", true, false))
	Kit.chain_vertical(chain)
	_apply_tutorial()


func _state_line(state: int, running: StringName, stale: StringName, failed: StringName) -> String:
	match state:
		Bp.JOB_RUNNING:
			return tr(running)
		Bp.JOB_STALE:
			return tr(stale)
		Bp.JOB_FAILED:
			return tr(failed)
	return ""


func _render_fit(state: int) -> void:
	Kit.clear(_fit_box)
	if not _campaign:
		return
	var line := _state_line(state, &"editor_fit.running", &"editor_fit.stale", &"editor_fit.failed")
	if not line.is_empty():
		_fit_box.add_child(Kit.label(line, &"Dense", true))
	var tags := Kit.dicts(_fit.get("tags", []))
	var have := state == Bp.JOB_DONE or (state == Bp.JOB_STALE or state == Bp.JOB_RUNNING) and (not tags.is_empty() or _f(_fit, "top_speed_kn") > 0.0)
	if not have:
		if state == Bp.JOB_IDLE:
			_fit_box.add_child(Kit.label(&"editor_fit.not_run", &"Dense"))
		return
	_fit_box.add_child(_verdict_row(tags))
	for t: Dictionary in tags:
		_fit_box.add_child(_tag_row(t))
	var sea := seakeeping_verdict(_fit)
	if not sea.is_empty():
		var row := Kit.hbox(2)
		var badge := Strip.CheckBadge.new()
		badge.check = int(sea["check"])
		row.add_child(badge)
		var l := Kit.label(String(sea["text"]), &"Body", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		row.accessibility_name = "%s: %s" % [Strip.Badges.word(badge.check), sea["text"]]
		_fit_box.add_child(row)
	var rows: Array = [
		[&"editor_fit.top_speed", Kit.number(_f(_fit, "top_speed_kn"), &"kn", 1)],
		[&"editor_fit.turn", Kit.number(_f(_fit, "time_to_turn_180_s"), &"s", 0)],
		[&"editor_fit.range", Kit.number(_f(_fit, "range_km"), &"km", 0)],
	]
	_fit_box.add_child(_grid(rows))
	var detect := Kit.dicts(_fit.get("detect", []))
	if not detect.is_empty():
		_fit_box.add_child(Kit.label(&"editor_fit.detect", &"Body"))
		# A threat a line: its class, then both detection ranges (class names are long; the dock is narrow).
		for d: Dictionary in detect:
			var info := Kit.as_dict(Kit.call_on(&"Content", &"class_info", [StringName(str(d.get("class_id", "")))]))
			var nm := _Text.resolve(StringName(str(info.get("name", ""))))
			var entry := Kit.vbox(0)
			entry.add_child(Kit.label(nm if not nm.is_empty() else str(d.get("class_id", "")), &"Dense", true))
			var ranges := Kit.label(tr(&"editor_fit.detect.ranges").format([Kit.number(_f(d, "we_detect_km"), &"km", 0), Kit.number(_f(d, "they_detect_km"), &"km", 0)]), &"Body", true)
			entry.add_child(ranges)
			_fit_box.add_child(entry)
	if not bool(_fit.get("simulated", true)):
		_fit_box.add_child(Kit.label(&"editor_fit.estimated", &"Dense"))


## Fit Check's seakeeping sweep in plain words, from `FitResultDict.seakeeping`: `{ "check": 0 safe, 1 not
## finished, 2 can roll over; "text": String }`, or empty when the sweep did not run. A sweep cut short (the
## editor's 20 s ceiling) that saw no run go over says it did not finish, never that the hull is safe.
## The briefing shows the same line.
static func seakeeping_verdict(fit: Dictionary) -> Dictionary:
	var s := Kit.as_dict(fit.get("seakeeping"))
	if s.is_empty():
		return {}
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	var sea := Kit.number(float(Kit.get_int(s, "sea_state")), &"none", 0)
	if Kit.get_bool(s, "rolls_over"):
		if Kit.get_bool(s, "worst_rolled_over") or not Kit.get_bool(s, "short_range"):
			return { "check": 2, "text": t.call(&"editor_fit.sea.rolls").format([sea, sea_bearing(Kit.get_float(s, "worst_from_deg"))]) }
		return { "check": 2, "text": t.call(&"editor_fit.sea.short").format([sea, Kit.number(Kit.get_float(s, "range_deg"), &"deg", 0)]) }
	if Kit.get_bool(s, "safe") and Kit.get_bool(s, "complete"):
		return { "check": 0, "text": t.call(&"editor_fit.sea.safe").format([sea, Kit.number(Kit.get_float(s, "worst_heel_deg"), &"deg", 0)]) }
	var due := Kit.get_int(s, "directions", 6)
	return { "check": 1, "text": t.call(&"editor_fit.sea.unfinished").format([Kit.number(float(mini(Kit.get_int(s, "runs"), due)), &"none", 0), Kit.number(float(due), &"none", 0), sea]) }


## Where the sea came from, degrees clockwise off the bow, as words that complete "with the sea ...": the
## sweep's six directions, from ahead and every 60 degrees round.
static func sea_bearing(from_deg: float) -> String:
	const FROM: Array[StringName] = [
		&"editor_fit.sea.from.0", &"editor_fit.sea.from.60", &"editor_fit.sea.from.120",
		&"editor_fit.sea.from.180", &"editor_fit.sea.from.240", &"editor_fit.sea.from.300",
	]
	return String(TranslationServer.translate(FROM[posmod(int(roundf(from_deg / 60.0)), 6)]))


## One line: the design passes every tag, or how many fail.
func _verdict_row(tags: Array[Dictionary]) -> Control:
	var fails := 0
	for t: Dictionary in tags:
		if not bool(t.get("pass", false)):
			fails += 1
	var row := Kit.hbox(2)
	var badge := Strip.CheckBadge.new()
	badge.check = 0 if bool(_fit.get("passes", false)) and fails == 0 else 2
	row.add_child(badge)
	var text := ""
	if tags.is_empty():
		text = tr(&"editor_fit.no_tags")
	elif fails == 0:
		text = tr(&"editor_fit.all_pass")
	else:
		text = tr(&"editor_fit.fails_one" if fails == 1 else &"editor_fit.fails").format([Kit.number(float(fails), &"none", 0), Kit.number(float(tags.size()), &"none", 0)])
	var l := Kit.label(text, &"Body", true)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(l)
	return row


## A tag: Pass or Fail as glyph and word, the tag as a sentence, its value against its limit.
func _tag_row(t: Dictionary) -> Control:
	var box := Kit.vbox(0)
	var head := Kit.hbox(2)
	box.add_child(head)
	var badge := Strip.CheckBadge.new()
	var passes := bool(t.get("pass", false))
	badge.check = 0 if passes else 2
	head.add_child(badge)
	var sentence := _Text.resolve(t.get("text", {}))
	var l := Kit.label(sentence, &"Body", true)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	head.add_child(l)
	var unit := StringName(str(t.get("unit", "none")))
	var dec := 1 if unit == &"m" or unit == &"none" else 0
	var value_text := tr(&"editor_fit.tag_value").format([Kit.number(_f(t, "value"), unit, dec), Kit.number(_f(t, "limit"), unit, dec)])
	box.add_child(Kit.label(value_text, &"Dense", true))
	box.accessibility_name = "%s: %s, %s" % [Strip.Badges.word(badge.check), sentence, value_text]
	return box


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
		v.autowrap_mode = TextServer.AUTOWRAP_OFF
		grid.add_child(v)
	return grid


func _render_qm(state: int) -> void:
	Kit.clear(_qm_box)
	if not _campaign:
		return
	var line := _state_line(state, &"editor_fit.qm.running", &"editor_fit.qm.stale", &"editor_fit.qm.failed")
	if not line.is_empty():
		_qm_box.add_child(Kit.label(line, &"Dense", true))
	if state == Bp.JOB_RUNNING:
		return
	match int(_qm_result.get("kind", -1)):
		Bp.QM_ALREADY_FITS:
			var row := Kit.hbox(2)
			var badge := Strip.CheckBadge.new()
			badge.check = 0
			row.add_child(badge)
			var fits := Kit.label(&"editor_fit.qm.fits", &"Body")
			fits.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			row.add_child(fits)
			_qm_box.add_child(row)
		Bp.QM_NO_LEGAL_REFIT:
			var row := Kit.hbox(2)
			var badge := Strip.CheckBadge.new()
			badge.check = 2
			row.add_child(badge)
			var none := Kit.label(&"editor_fit.qm.none", &"Body")
			none.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			row.add_child(none)
			_qm_box.add_child(row)
			var blocking := _Text.resolve(_qm_result.get("blocking", {}))
			if not blocking.is_empty():
				_qm_box.add_child(Kit.label(tr(&"editor_fit.qm.blocking").format([blocking]), &"Body", true))
		Bp.QM_REFIT:
			var diff := Kit.as_dict(_qm_result.get("diff", {}))
			_qm_box.add_child(Kit.label(&"editor_fit.qm.proposal", &"Body"))
			_qm_box.add_child(_changes_list(diff))
			_qm_box.add_child(_cost_line(diff))
			var fit := Kit.as_dict(_qm_result.get("fit", {}))
			if not fit.is_empty():
				_qm_box.add_child(Kit.label(&"editor_fit.qm.passes", &"Dense"))
			if state == Bp.JOB_STALE:
				return
			var actions := Kit.hbox(1)
			if _qm_preview:
				var accept := Kit.inline_button("editor_fit.qm.accept")
				accept.set_meta(&"control_id", "editor.fit.quartermaster")
				accept.pressed.connect(_on_accept_qm)
				actions.add_child(accept)
				var cancel := Kit.inline_button("editor_fit.qm.cancel")
				cancel.set_meta(&"control_id", "editor.fit.quartermaster")
				cancel.pressed.connect(_on_cancel_qm)
				actions.add_child(cancel)
			else:
				var preview := Kit.inline_button("editor_fit.qm.preview")
				preview.tooltip_text = tr(&"editor_fit.qm.preview.tip")
				preview.set_meta(&"control_id", "editor.fit.quartermaster")
				preview.pressed.connect(_on_preview_qm)
				actions.add_child(preview)
			_qm_box.add_child(actions)
		_:
			if state == Bp.JOB_IDLE:
				_qm_box.add_child(Kit.label(&"editor_fit.qm.idle", &"Dense"))


## Every changed part: added, removed or moved, with its cell.
func _changes_list(diff: Dictionary) -> Control:
	var box := Kit.vbox(0)
	var changes := Kit.dicts(diff.get("changes", []))
	if changes.is_empty():
		box.add_child(Kit.label(&"editor_fit.cost.no_changes", &"Dense"))
		return box
	for c: Dictionary in changes:
		var kind := StringName(str(c.get("kind", "")))
		var part_name := _part_name(c)
		var cell: Variant = c.get("cell", Vector3i.ZERO)
		var at := _cell_text(cell)
		var text := ""
		match kind:
			&"added":
				text = tr(&"editor_fit.change.added").format([part_name, at])
			&"removed":
				text = tr(&"editor_fit.change.removed").format([part_name, at])
			_:
				text = tr(&"editor_fit.change.moved").format([part_name, _cell_text(c.get("from_cell", Vector3i.ZERO)), at])
		var row := Kit.hbox(1)
		var icon := TextureRect.new()
		icon.custom_minimum_size = Vector2(20.0, 20.0)
		icon.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
		icon.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
		icon.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		icon.texture = Ui.icon({&"added": &"add", &"removed": &"remove"}.get(kind, &"arrow_right"))
		icon.modulate = Ui.colour(&"text.secondary")
		row.add_child(icon)
		var l := Kit.label(text, &"Dense", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		box.add_child(row)
	return box


func _part_name(c: Dictionary) -> String:
	var n := String(c.get("name", ""))
	if n.is_empty():
		return String(c.get("part", ""))
	# Catalogue names are text keys; a translation that misses gives the key back.
	return tr(n)


func _cell_text(cell: Variant) -> String:
	if not cell is Vector3i:
		return ""
	var v := cell as Vector3i
	return tr(&"editor_fit.cell").format([Kit.number(float(v.x), &"none", 0), Kit.number(float(v.y), &"none", 0), Kit.number(float(v.z), &"none", 0)])


## Salvage and theatre hours, parts from the store, and Caution when the Salvage on hand falls short.
func _cost_line(diff: Dictionary) -> Control:
	var box := Kit.vbox(0)
	var salvage := _f(diff, "salvage")
	var hours := _f(diff, "hours")
	box.add_child(Kit.label(tr(&"editor_fit.cost.line").format([Kit.number(salvage, &"salvage", 0), Kit.number(hours, &"h", 0)]), &"Body", true))
	var store := 0
	var from_store := Kit.as_dict(diff.get("from_store", {}))
	for k: Variant in from_store:
		store += int(from_store[k])
	if store > 0:
		box.add_child(Kit.label(tr(&"editor_fit.cost.store_one" if store == 1 else &"editor_fit.cost.store").format([Kit.number(float(store), &"none", 0)]), &"Dense", true))
	var dt := _f(diff, "structure_delta_t")
	if absf(dt) >= 0.05:
		box.add_child(Kit.label(tr(&"editor_fit.cost.structure").format([Kit.number(dt, &"t", 1)]), &"Dense", true))
	if not bool(diff.get("affordable", true)):
		var row := Kit.hbox(2)
		var badge := Strip.CheckBadge.new()
		badge.check = 1
		row.add_child(badge)
		var text := tr(&"editor_fit.cost.short").format([Kit.number(float(maxi(_budget, 0)), &"salvage", 0)]) if _budget >= 0 else tr(&"editor_fit.cost.short_unknown")
		var l := Kit.label(text, &"Body", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		box.add_child(row)
	return box


func _render_cost() -> void:
	Kit.clear(_cost_box)
	if not _campaign:
		return
	_cost_box.add_child(Kit.label(&"editor_fit.cost.title", &"Body"))
	var changes := Kit.dicts(_diff.get("changes", []))
	if changes.is_empty() and _f(_diff, "salvage") <= 0.0 and _f(_diff, "hours") <= 0.0:
		_cost_box.add_child(Kit.label(&"editor_fit.cost.no_changes", &"Dense"))
		return
	_cost_box.add_child(_cost_line(_diff))
	_cost_box.add_child(Kit.label(tr(&"editor_fit.cost.count_one" if changes.size() == 1 else &"editor_fit.cost.count").format([Kit.number(float(changes.size()), &"none", 0)]), &"Dense", true))


# --- actions ----------------------------------------------------------------------------------------

func _on_check() -> void:
	var e := _editor()
	if e != null and bool(e.call(&"fit_check")):
		_auto_rev = int(e.call(&"rev"))
	_queue_refresh()


func _on_ask_qm() -> void:
	_qm_preview = false
	Kit.call_on(&"Editor", &"quartermaster")
	_queue_refresh()


func _on_preview_qm() -> void:
	_qm_preview = bool(Kit.call_on(&"Editor", &"quartermaster_preview"))
	_refresh()


func _on_accept_qm() -> void:
	_qm_preview = false
	Kit.call_on(&"Editor", &"quartermaster_accept")
	_queue_refresh()


func _on_cancel_qm() -> void:
	_qm_preview = false
	Kit.call_on(&"Editor", &"preview_clear")
	_refresh()
	Ui.focus(_qm)


func _apply_tutorial() -> void:
	var t := Kit.autoload(&"Tutorial")
	if t == null or not t.has_method(&"is_allowed"):
		return
	for n: Node in find_children("*", "BaseButton", true, false):
		var b := n as BaseButton
		if b.has_meta(&"control_id") and not bool(t.call(&"is_allowed", String(b.get_meta(&"control_id")))):
			b.disabled = true
