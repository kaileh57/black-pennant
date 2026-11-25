extends VBoxContainer
## Ship's Notes with Fix previews and Suggest buttons.
## DESIGN.md L188-212. Unit: ui-editor-notes.
## Reads and calls: Editor.notes, Editor.note_preview, Editor.note_apply_fix, Editor.suggestions, Editor.suggest_preview, Editor.suggest_apply.
##
## The right dock's first tab. Notes come ranked from Rust (L190-198: severity tier first, then Fixes that
## flip a Fit Check tag, then reach, then part index) and are shown in that order, one row each:
## - the tier as glyph, word and hue (L1689: tiers 1 and 2 of L190 are Fail, a cross on alert.warning;
##   tiers 3 and 4 Caution, a triangle on alert.caution; tier 5 Advisory), and the sentence in the matching
##   Severity look with its side stripe (L1596 allows the stripe only there);
## - "Show part", which selects the part the note names (copy rule 5: notes link the part);
## - the Fix button, labelled with what it does ("Move Gun 2"). Pressing it previews the change as a ghost
##   (L188); the row then offers "Accept" and "Cancel". Only one preview is open at a time. A preview is
##   dropped when the design changes, when another preview starts and when this tab is hidden.
## Open notes never block Launch (L188); the Launch button counts them itself.
##
## Under the notes, Suggest (L210): one row per value the editor will not set by itself (ballast and trim,
## armour, engine and generator sizing, fuel, sensor placement, bulkheads, Aggression, the EMCON trigger).
## Each row's Suggest button previews the suggestion as a ghost with its sentence, then Accept or Cancel.
## Asking `suggestions()` makes Rust work out the set a kind per frame, so the section asks only while it
## is open and this tab is on screen, and the section starts closed.
##
## States: dock closed (nothing to say); notes being worked out (the worker's `notes` job runs); no open
## notes; a list. Focus survives a rebuild: the control that had it is found again by its key.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Strip := preload("res://ui/editor/gauge_strip.gd")

## `Bp.NOTE_*` tier to the badge's check code and the Severity look.
const TIER_CHECK: Array[int] = [2, 2, 1, 1, 3]
const TIER_LOOK: Array[StringName] = [&"SeverityFail", &"SeverityFail", &"SeverityCaution", &"SeverityCaution", &"SeverityAdvisory"]
## `Bp.SUGGEST_*` in order, with the name of what each sets.
const SUGGEST_KEYS: Array[StringName] = [
	&"editor_notes.suggest.ballast", &"editor_notes.suggest.armour", &"editor_notes.suggest.engine",
	&"editor_notes.suggest.fuel", &"editor_notes.suggest.sensor", &"editor_notes.suggest.bulkhead",
	&"editor_notes.suggest.aggression", &"editor_notes.suggest.emcon",
]

var _status: Label
var _scroll: ScrollContainer
var _list: VBoxContainer
var _suggest_toggle: Button
var _suggest_box: VBoxContainer
var _notes: Array[Dictionary] = []
var _suggestions: Array[Dictionary] = []
var _notes_job := -1
## The open preview: `{ "kind": &"note" | &"suggest", "index": int }` or empty.
var _preview: Dictionary = {}
var _refresh_queued := false


func _ready() -> void:
	Ui.set_separation(self, 2)
	accessibility_name = tr(&"editor_notes.name")
	var margin := MarginContainer.new()
	margin.add_theme_constant_override(&"margin_left", Ui.spacing(2))
	margin.add_theme_constant_override(&"margin_right", Ui.spacing(2))
	margin.add_theme_constant_override(&"margin_top", Ui.spacing(2))
	margin.add_theme_constant_override(&"margin_bottom", 0)
	add_child(margin)
	_status = Kit.label("", &"Body", true)
	margin.add_child(_status)
	_scroll = ScrollContainer.new()
	_scroll.name = &"Scroll"
	_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	_scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	add_child(_scroll)
	var inner := MarginContainer.new()
	inner.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	inner.add_theme_constant_override(&"margin_left", Ui.spacing(2))
	inner.add_theme_constant_override(&"margin_right", Ui.spacing(2))
	inner.add_theme_constant_override(&"margin_top", 0)
	inner.add_theme_constant_override(&"margin_bottom", Ui.spacing(2))
	_scroll.add_child(inner)
	var column := Kit.vbox(4)
	column.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	inner.add_child(column)
	_list = Kit.vbox(3)
	_list.name = &"List"
	column.add_child(_list)
	column.add_child(HSeparator.new())
	_suggest_toggle = Kit.button("editor_notes.suggest.title")
	_suggest_toggle.toggle_mode = true
	_suggest_toggle.icon = Ui.icon(&"chevron_right")
	_suggest_toggle.tooltip_text = tr(&"editor_notes.suggest.tip")
	_suggest_toggle.set_meta(&"focus_key", "suggest_toggle")
	_suggest_toggle.set_meta(&"control_id", "editor.notes.suggest")
	_suggest_toggle.toggled.connect(_on_suggest_toggled)
	column.add_child(_suggest_toggle)
	_suggest_box = Kit.vbox(2)
	_suggest_box.name = &"Suggest"
	_suggest_box.visible = false
	column.add_child(_suggest_box)
	var editor := Kit.autoload(&"Editor")
	if editor != null and editor.has_signal(&"notes_changed"):
		editor.connect(&"notes_changed", _queue_refresh)
		editor.connect(&"design_changed", _on_design_changed)
		editor.connect(&"job_changed", _on_job_changed)
		editor.connect(&"ghost_changed", _on_ghost_changed)
	var tutorial := Kit.autoload(&"Tutorial")
	if tutorial != null and tutorial.has_signal(&"step_changed"):
		tutorial.connect(&"step_changed", func(_s: Dictionary) -> void: _queue_refresh())
	visibility_changed.connect(_on_visibility_changed)
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		accessibility_name = tr(&"editor_notes.name")
		_queue_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


func _on_visibility_changed() -> void:
	if not is_visible_in_tree():
		_clear_preview()
	elif _suggest_toggle.button_pressed:
		# Suggestions are worked out only while they are on screen.
		_queue_refresh()


func _on_design_changed(_rev: int) -> void:
	# A Fix or suggestion was worked out for the design as it was: its preview no longer applies.
	_clear_preview()
	_queue_refresh()


func _on_job_changed(job: StringName, state: int) -> void:
	if job == &"notes":
		_notes_job = state
		_queue_refresh()


## Rust drops an edit preview when it is accepted or replaced; follow it so the row stops offering Accept.
func _on_ghost_changed() -> void:
	if _preview.is_empty():
		return
	var e := _editor()
	if e != null and e.has_method(&"ghost") and e.call(&"ghost") != null:
		# A part or kit ghost replaced the preview.
		_preview = {}
		_queue_refresh()


func _queue_refresh() -> void:
	if _refresh_queued:
		return
	_refresh_queued = true
	_refresh.call_deferred()


func _refresh() -> void:
	_refresh_queued = false
	var e := _editor()
	var open := e != null and e.has_method(&"notes") and bool(e.call(&"is_open"))
	_notes = Kit.dicts(e.call(&"notes") if open else [])
	if open and _suggest_toggle.button_pressed and is_visible_in_tree():
		_suggestions = Kit.dicts(e.call(&"suggestions"))
	elif not _suggest_toggle.button_pressed:
		_suggestions = []
	render(open)


## Draw `_notes` and `_suggestions` (captures fill them by hand).
func render(open: bool) -> void:
	var focus_key := _focused_key()
	if not open:
		_status.text = tr(&"editor_notes.closed")
	elif _notes.is_empty():
		_status.text = tr(&"editor_notes.checking") if _notes_job == Bp.JOB_RUNNING else tr(&"editor_notes.none")
	else:
		var n := _notes.size()
		_status.text = tr(&"editor_notes.count_one" if n == 1 else &"editor_notes.count").format([Kit.number(float(n), &"none", 0)])
		if _notes_job == Bp.JOB_RUNNING:
			_status.text = tr(&"editor_notes.count_rechecking").format([_status.text])
	Kit.clear(_list)
	var focusables: Array[Control] = []
	for note: Dictionary in _notes:
		focusables.append_array(_note_row(note))
	_list.visible = not _notes.is_empty()
	_suggest_toggle.disabled = not open
	_suggest_toggle.icon = Ui.icon(&"chevron_down" if _suggest_toggle.button_pressed else &"chevron_right")
	focusables.append(_suggest_toggle)
	Kit.clear(_suggest_box)
	_suggest_box.visible = open and _suggest_toggle.button_pressed
	if _suggest_box.visible:
		focusables.append_array(_suggest_rows())
	Kit.chain_vertical(focusables)
	_apply_tutorial()
	_restore_focus(focus_key)


func _note_row(note: Dictionary) -> Array[Control]:
	var index := int(note.get("index", -1))
	var tier := clampi(int(note.get("tier", Bp.NOTE_ADVISORY)), 0, TIER_CHECK.size() - 1)
	var part := int(note.get("part", -1))
	var row := Kit.vbox(0)
	_list.add_child(row)
	var head := Kit.hbox(1)
	row.add_child(head)
	var badge := Strip.CheckBadge.new()
	badge.check = TIER_CHECK[tier]
	head.add_child(badge)
	var spacer := Control.new()
	spacer.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	spacer.mouse_filter = Control.MOUSE_FILTER_IGNORE
	head.add_child(spacer)
	var out: Array[Control] = []
	var sentence := _Text.resolve(note.get("text", {}))
	var text := Kit.label(sentence, TIER_LOOK[tier], true)
	text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(text)
	row.accessibility_name = "%s: %s" % [Strip.Badges.word(TIER_CHECK[tier]), sentence]
	if part >= 0:
		var show := Kit.inline_button("editor_notes.show_part")
		show.custom_minimum_size.y = 40.0
		show.tooltip_text = tr(&"editor_notes.show_part.tip")
		show.set_meta(&"focus_key", "show:%d" % index)
		show.set_meta(&"control_id", "editor.notes.part")
		show.pressed.connect(_show_part.bind(part))
		head.add_child(show)
		out.append(show)
	if not bool(note.get("has_fix", false)):
		return out
	var fix_text := _Text.resolve(note.get("fix_label", {}))
	# `editor.notes.fix.<note id>`: the tutorial frames one note's Fix (the blocked arc, L258) by it.
	var fix_id := "editor.notes.fix" + ("." + String(note.get("id", "")) if String(note.get("id", "")) != "" else "")
	var previewing: bool = _preview.get("kind", &"") == &"note" and int(_preview.get("index", -1)) == index
	var actions := Kit.hbox(1)
	row.add_child(actions)
	if previewing:
		var what := Kit.label(tr(&"editor_notes.previewing").format([fix_text]), &"Dense", true)
		what.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(what)
		row.move_child(what, actions.get_index())
		var accept := Kit.inline_button("editor_notes.accept")
		accept.tooltip_text = tr(&"editor_notes.accept.tip")
		accept.set_meta(&"focus_key", "accept:%d" % index)
		accept.set_meta(&"control_id", fix_id)
		accept.pressed.connect(_accept_note.bind(index))
		actions.add_child(accept)
		var cancel := Kit.inline_button("editor_notes.cancel")
		cancel.set_meta(&"focus_key", "cancel:%d" % index)
		cancel.set_meta(&"control_id", fix_id)
		cancel.pressed.connect(_cancel_preview)
		actions.add_child(cancel)
		out.append_array([accept, cancel])
	else:
		# The button says what the fix does; a long one is trimmed and the tooltip keeps it whole.
		var fix := Kit.button("")
		fix.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		fix.custom_minimum_size.y = 40.0
		fix.text = fix_text if not fix_text.is_empty() else tr(&"editor_notes.fix")
		fix.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		fix.accessibility_name = tr(&"editor_notes.fix.name").format([fix.text])
		fix.tooltip_text = fix.text + "\n" + tr(&"editor_notes.fix.tip")
		if bool(note.get("flips_tag", false)):
			fix.tooltip_text += "\n" + tr(&"editor_notes.fix.flips_tag")
		fix.set_meta(&"focus_key", "fix:%d" % index)
		fix.set_meta(&"control_id", fix_id)
		fix.pressed.connect(_preview_note.bind(index))
		actions.add_child(fix)
		out.append(fix)
	return out


func _suggest_rows() -> Array[Control]:
	var out: Array[Control] = []
	var by_kind: Dictionary[int, Dictionary] = {}
	for s: Dictionary in _suggestions:
		by_kind[int(s.get("kind", -1))] = s
	for kind in SUGGEST_KEYS.size():
		var s: Dictionary = by_kind.get(kind, {})
		var row := Kit.vbox(0)
		_suggest_box.add_child(row)
		var head := Kit.hbox(1)
		row.add_child(head)
		var name_label := Kit.label(SUGGEST_KEYS[kind], &"Body")
		name_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		head.add_child(name_label)
		var available := bool(s.get("available", false))
		var previewing: bool = _preview.get("kind", &"") == &"suggest" and int(_preview.get("index", -1)) == kind
		var detail := _Text.resolve(s.get("text", {})) if not s.is_empty() else tr(&"editor_notes.suggest.waiting")
		var line := Kit.label(detail, &"Dense", true)
		row.add_child(line)
		if previewing:
			var accept := Kit.inline_button("editor_notes.suggest.accept")
			accept.set_meta(&"focus_key", "s_accept:%d" % kind)
			accept.set_meta(&"control_id", "editor.notes.suggest")
			accept.pressed.connect(_accept_suggestion.bind(kind))
			var cancel := Kit.inline_button("editor_notes.cancel")
			cancel.set_meta(&"focus_key", "s_cancel:%d" % kind)
			cancel.set_meta(&"control_id", "editor.notes.suggest")
			cancel.pressed.connect(_cancel_preview)
			var actions := Kit.hbox(1)
			actions.add_child(accept)
			actions.add_child(cancel)
			row.add_child(actions)
			out.append_array([accept, cancel])
		else:
			var b := Kit.inline_button("editor_notes.suggest.button")
			b.custom_minimum_size.y = 40.0
			b.disabled = not available
			b.tooltip_text = tr(&"editor_notes.suggest.button.tip")
			b.accessibility_name = "%s: %s" % [tr(&"editor_notes.suggest.button"), tr(SUGGEST_KEYS[kind])]
			b.set_meta(&"focus_key", "suggest:%d" % kind)
			b.set_meta(&"control_id", "editor.notes.suggest")
			b.pressed.connect(_preview_suggestion.bind(kind))
			head.add_child(b)
			out.append(b)
	return out


# --- actions ----------------------------------------------------------------------------------------

func _show_part(part: int) -> void:
	var e := _editor()
	if e != null and part >= 0:
		e.call(&"select_parts", PackedInt32Array([part]), false)


func _preview_note(index: int) -> void:
	var e := _editor()
	if e == null:
		return
	_clear_preview()
	if bool(e.call(&"note_preview", index)):
		_preview = {"kind": &"note", "index": index}
	else:
		_status.text = tr(&"editor_notes.fix_gone")
	_refresh()
	_restore_focus("accept:%d" % index)


func _accept_note(index: int) -> void:
	var e := _editor()
	if e == null:
		return
	var ok := bool(e.call(&"note_apply_fix", index))
	_preview = {}
	if not ok:
		e.call(&"preview_clear")
		_status.text = tr(&"editor_notes.fix_gone")
	_queue_refresh()


func _preview_suggestion(kind: int) -> void:
	var e := _editor()
	if e == null:
		return
	_clear_preview()
	if bool(e.call(&"suggest_preview", kind)):
		_preview = {"kind": &"suggest", "index": kind}
	_refresh()
	_restore_focus("s_accept:%d" % kind)


func _accept_suggestion(kind: int) -> void:
	var e := _editor()
	if e == null:
		return
	if not bool(e.call(&"suggest_apply", kind)):
		e.call(&"preview_clear")
	_preview = {}
	_queue_refresh()


func _cancel_preview() -> void:
	var key := "fix:%d" % int(_preview.get("index", -1)) if _preview.get("kind", &"") == &"note" else "suggest:%d" % int(_preview.get("index", -1))
	_clear_preview()
	_refresh()
	_restore_focus(key)


func _clear_preview() -> void:
	if _preview.is_empty():
		return
	_preview = {}
	var e := _editor()
	if e != null and e.has_method(&"preview_clear"):
		e.call(&"preview_clear")


func _on_suggest_toggled(on: bool) -> void:
	if not on and _preview.get("kind", &"") == &"suggest":
		_clear_preview()
	_refresh()


# --- focus and tutorial -----------------------------------------------------------------------------

func _focused_key() -> String:
	var f := get_viewport().gui_get_focus_owner() if is_inside_tree() else null
	if f != null and is_ancestor_of(f) and f.has_meta(&"focus_key"):
		return String(f.get_meta(&"focus_key"))
	return ""


func _restore_focus(key: String) -> void:
	if key.is_empty():
		return
	var found := _find_key(self, key)
	if found == null and key.contains(":"):
		# The row went (its note was fixed): the next row's Fix, else the Suggest toggle.
		for n: Node in _list.find_children("*", "Button", true, false):
			found = n as Control
			break
		if found == null:
			found = _suggest_toggle
	if found != null and found.is_visible_in_tree():
		Ui.focus(found)
		# Accept and Cancel appear under the note: bring them into the list's view (a short dock at UI
		# scale 200% ends where the 3D view ends).
		_reveal(found)


## Scroll the list to `c` once the rebuilt rows have their places (two frames: build, then layout).
func _reveal(c: Control) -> void:
	for i in 2:
		await get_tree().process_frame
	if is_instance_valid(c) and c.is_visible_in_tree() and is_instance_valid(_scroll):
		_scroll.ensure_control_visible(c)


func _find_key(node: Node, key: String) -> Control:
	if node is Control and node.has_meta(&"focus_key") and String(node.get_meta(&"focus_key")) == key:
		return node as Control
	for c: Node in node.get_children():
		var f := _find_key(c, key)
		if f != null:
			return f
	return null


## Tutorial steps allow controls by id (`editor.notes.fix`, `editor.notes.part`, `editor.notes.suggest`).
func _apply_tutorial() -> void:
	var t := Kit.autoload(&"Tutorial")
	if t == null or not t.has_method(&"is_allowed"):
		return
	for n: Node in find_children("*", "BaseButton", true, false):
		var b := n as BaseButton
		if b.has_meta(&"control_id") and not bool(t.call(&"is_allowed", String(b.get_meta(&"control_id")))):
			b.disabled = true
