extends Control
## Track card: stance and rung with reasons, candidates, overrides.
## DESIGN.md L470-477, L496-503. Unit: ui-hud-tracks.
## Reads and calls: Hud.track, Orders.query_track_card, Hud.track_card_reply, Orders.set_stance, Orders.set_identity.
##
## Shown under the track list while a track is selected (the HUD shell shows and hides it on
## `Hud.selection_changed`). Top to bottom:
## - Symbol and "Track 0412"; the close button clears the selection.
## - Designate (the most used action, so first, above the fold) or clear the designation, and
##   Show on plot.
## - Stance as word on the stance stripe (rule 7), and the reason for it, which the card always
##   shows (L503 rule 4).
## - ID rung as zero to four bars and the word, with the class once known, and what moved it up the
##   last rung (L477: an emitter match, a visual sighting, an acoustic signature).
## - Facts with units: bearing and range, course and speed, height or depth, closest approach,
##   quality grade, sources, weapon range, struck colours, radiating, inbound, age of a last-known mark.
## - The shrinking candidate list and the evidence that climbed each rung, from
##   `Orders.query_track_card` (asked on selection and again when the rung or stance moves, at most
##   once a second). Until the reply arrives the card shows the candidate count it already has; no
##   spinner (rule 8). A reply for another track is ignored.
## - Manual stance and identity (L503 rule 4, L470): "By the rules" and "By the evidence" give the
##   decision back. Friend makes a track unengageable, as the rules say.
## States: no selection (hidden by the shell), populated, and dropped (the track left the picture:
## the card says so and keeps only Close).

signal close_requested
signal centre_requested(track: int)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

const STANCE_VARIATIONS: Array[StringName] = [&"StanceFriend", &"StanceNeutral", &"StanceSuspect", &"StanceHostile"]
## Stance picks in menu order: by the rules, then the four stances.
const STANCE_PICKS: Array[int] = [-1, 3, 2, 1, 0]
const SOURCE_BITS: Array[int] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024]
const SOURCE_KEYS: Array[StringName] = [
	&"hud_tracks.source.search_radar", &"hud_tracks.source.fire_control", &"hud_tracks.source.seeker",
	&"hud_tracks.source.esm", &"hud_tracks.source.passive_sonar", &"hud_tracks.source.active_sonar",
	&"hud_tracks.source.visual", &"hud_tracks.source.camera", &"hud_tracks.source.datalink",
	&"hud_tracks.source.offboard", &"hud_tracks.source.briefing",
]
## Candidates listed by name; the rest are counted.
const MAX_CANDIDATES := 8
const MAX_EVIDENCE := 6
## The card asks for the candidate list again at most this often (milliseconds).
const QUERY_GAP_MS := 1000
## The card's scroll area is at most this share of the screen height.
const MAX_SCREEN_SHARE := 0.45

@onready var _scroll: ScrollContainer = $Scroll
@onready var _column: VBoxContainer = $Scroll/Column
@onready var _header: HBoxContainer = $Scroll/Column/Header
@onready var _symbol: Control = $Scroll/Column/Header/Symbol
@onready var _title: Label = $Scroll/Column/Header/Title
@onready var _close: Button = $Scroll/Column/Header/Close
@onready var _stance: Label = $Scroll/Column/Stance
@onready var _stance_reason: Label = $Scroll/Column/StanceReason
@onready var _id_row: HBoxContainer = $Scroll/Column/Id
@onready var _rung_icon: TextureRect = $Scroll/Column/Id/Rung
@onready var _id_word: Label = $Scroll/Column/Id/Word
@onready var _id_reason: Label = $Scroll/Column/IdReason
@onready var _facts: Label = $Scroll/Column/Facts
@onready var _evidence_rule: HSeparator = $Scroll/Column/EvidenceRule
@onready var _candidates: Label = $Scroll/Column/Candidates
@onready var _candidate_list: Label = $Scroll/Column/CandidateList
@onready var _evidence: Label = $Scroll/Column/Evidence
@onready var _override_rule: HSeparator = $Scroll/Column/OverrideRule
@onready var _overrides: GridContainer = $Scroll/Column/Overrides
@onready var _stance_pick: OptionButton = $Scroll/Column/Overrides/StancePick
@onready var _id_pick: OptionButton = $Scroll/Column/Overrides/IdPick
@onready var _buttons: HFlowContainer = $Scroll/Column/Buttons
@onready var _designate: Button = $Scroll/Column/Buttons/Designate
@onready var _centre: Button = $Scroll/Column/Buttons/Centre

var _track := -1
var _data: Dictionary = {}
var _card: Dictionary = {}
var _dropped := false
var _last_query_ms := -QUERY_GAP_MS
var _query_pending := false
var _last_rung := -1
var _last_stance := -1
var _last_candidates := -1
## What the player set by hand this sortie, per track: { "stance": int, "class_id": StringName }.
var _manual: Dictionary[int, Dictionary] = {}
## Identity picks after "By the evidence": the class ids, in menu order.
var _id_choices: Array[StringName] = []
var _filling := false
## Height the HUD shell gives the card (`set_max_height`); -1 falls back to the screen share.
var _max_height := -1.0


func _ready() -> void:
	var root := self as Control
	if root is MarginContainer:
		Ui.set_margins(root as MarginContainer, 2)
	Ui.set_separation(_column, 1)
	Ui.set_separation(_header, 2)
	Ui.set_separation(_id_row, 1)
	Ui.set_separation(_overrides, 2)
	# A flow: in a narrow dock (large UI scale) the second button moves under the first.
	_buttons.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	_buttons.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	for l: Label in [_title, _stance, _stance_reason, _id_word, _id_reason, _facts, _candidates, _candidate_list, _evidence]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_designate.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_close.icon = Ui.icon(&"close")
	_close.accessibility_name = tr(&"hud_tracks.card.close")
	_close.tooltip_text = tr(&"hud_tracks.card.close")
	_close.pressed.connect(func() -> void: close_requested.emit())
	_designate.pressed.connect(_on_designate)
	_centre.pressed.connect(_on_centre)
	_stance_pick.item_selected.connect(_on_stance_pick)
	_id_pick.item_selected.connect(_on_id_pick)
	_stance_pick.accessibility_name = tr(&"hud_tracks.card.set_stance")
	_id_pick.accessibility_name = tr(&"hud_tracks.card.set_identity")
	_stance_pick.tooltip_text = tr(&"hud_tracks.card.stance_tooltip")
	_id_pick.tooltip_text = tr(&"hud_tracks.card.identity_tooltip")
	accessibility_name = tr(&"hud_tracks.card.access")
	Kit.chain_vertical([_close, _designate, _stance_pick, _id_pick])
	Kit.chain_horizontal([_designate, _centre])
	_column.minimum_size_changed.connect(_fit, CONNECT_DEFERRED)
	get_viewport().size_changed.connect(_fit)
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		for pair: Array in [[&"selection_changed", _on_selection], [&"tracks_changed", _on_tracks], [&"track_card_reply", _on_reply], [&"track_event", _on_track_event]]:
			if hud.has_signal(pair[0]):
				hud.connect(pair[0], pair[1])
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", _on_state)
	var sel: Variant = Kit.call_on(&"Hud", &"selected_track")
	_on_selection(int(sel) if sel is int else -1)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_fill_stance_pick()
		_render()


## The card's focusable controls, top first (the HUD shell links them to its neighbours).
func focus_targets() -> Array[Control]:
	var out: Array[Control] = []
	for c: Control in [_designate, _centre, _stance_pick, _id_pick, _close]:
		if c.is_visible_in_tree() and not (c is BaseButton and (c as BaseButton).disabled):
			out.append(c)
	return out


## The card's scroll area is as tall as its rows, up to a share of the screen (L1714: no clipping
## at 200% UI scale; the rows scroll instead).
func _fit() -> void:
	if not is_inside_tree():
		return
	var cap := maxf(get_viewport_rect().size.y * MAX_SCREEN_SHARE, 160.0)
	if _max_height >= 0.0:
		cap = _max_height
	var want := ceilf(minf(_column.get_combined_minimum_size().y, cap))
	if not is_equal_approx(_scroll.custom_minimum_size.y, want):
		_scroll.custom_minimum_size = Vector2(0.0, want)


## The card takes the height its rows need, up to `height` (the HUD shell shares the dock's height
## out); past it the rows scroll.
func set_max_height(height: float) -> void:
	if is_equal_approx(height, _max_height):
		return
	_max_height = height
	_fit()


func _on_state(state: int) -> void:
	if state == Bp.SESSION_LOADING or state == Bp.SESSION_IDLE:
		_manual.clear()
		_track = -1
		_card = {}


# --- data ----------------------------------------------------------------------------------------

func _on_selection(track: int) -> void:
	if track == _track and not _dropped:
		return
	_track = track
	_card = {}
	_dropped = false
	_last_rung = -1
	_last_stance = -1
	_last_candidates = -1
	_last_query_ms = -QUERY_GAP_MS
	_pull()
	_query()


func _on_tracks() -> void:
	if _track > 0:
		_pull()


func _on_track_event(kind: StringName, track: int, other: int) -> void:
	if track != _track:
		return
	match kind:
		&"rung", &"stance":
			_query()
		&"merged":
			# The track number went into a surviving track: follow the selection there.
			if other > 0:
				Kit.call_on(&"Hud", &"select_track", [other])
		&"dropped":
			_pull()


func _pull() -> void:
	if _track <= 0:
		show_track({}, {})
		return
	var t: Variant = Kit.call_on(&"Hud", &"track", [_track])
	if t is Dictionary:
		var d := t as Dictionary
		var rung := Kit.get_int(d, "rung", 0)
		var stance := Kit.get_int(d, "stance", 2)
		var n := Kit.get_int(d, "candidates", 0)
		if (_last_rung >= 0 and rung != _last_rung) or (_last_stance >= 0 and stance != _last_stance) or (_last_candidates >= 0 and n != _last_candidates):
			_query()
		_last_rung = rung
		_last_stance = stance
		_last_candidates = n
		_dropped = false
		show_track(d, _card)
	else:
		_dropped = true
		show_track(_data, _card)


func _query() -> void:
	if _track <= 0:
		return
	var now := Time.get_ticks_msec()
	if now - _last_query_ms < QUERY_GAP_MS:
		if not _query_pending:
			_query_pending = true
			get_tree().create_timer(float(QUERY_GAP_MS - (now - _last_query_ms)) / 1000.0, false).timeout.connect(_deferred_query)
		return
	_last_query_ms = now
	Kit.call_on(&"Orders", &"query_track_card", [_track])


func _deferred_query() -> void:
	_query_pending = false
	_query()


func _on_reply(card: Dictionary) -> void:
	if Kit.get_int(card, "track", -1) != _track:
		return
	_card = card
	show_track(_data, _card)


# --- drawing -------------------------------------------------------------------------------------

## Draw a `TrackDict` and its `TrackCardDict` (empty until the reply). Also the test entry point.
func show_track(track: Dictionary, card: Dictionary) -> void:
	_data = track
	_card = card
	if not track.is_empty() and _track <= 0:
		_track = Kit.get_int(track, "id", -1)
	_render()


func _render() -> void:
	if not is_node_ready():
		return
	var t := _data
	var has := not t.is_empty()
	var label := Kit.get_str(t, "label")
	_title.text = tr(&"hud_tracks.card.title").format([label]) if has else ""
	_symbol.visible = has
	if has and _symbol.has_method(&"show_track"):
		var shown := t.duplicate()
		shown["selected"] = false
		_symbol.call(&"show_track", shown)
	var live := has and not _dropped
	for c: Control in [_stance, _stance_reason, _id_row, _id_reason, _facts, _evidence_rule, _candidates, _candidate_list, _evidence, _override_rule, _overrides, _buttons]:
		c.visible = live
	if not has:
		return
	if _dropped:
		_facts.visible = true
		_facts.text = tr(&"hud_tracks.card.dropped").format([label])
		_fit.call_deferred()
		return
	var stance := clampi(Kit.get_int(t, "stance", 2), 0, 3)
	_stance.theme_type_variation = STANCE_VARIATIONS[stance]
	_stance.text = _word(&"stance", stance)
	var reason: Variant = _card.get("stance_reason", t.get("stance_reason", {}))
	_stance_reason.text = _Text.resolve(reason)
	_stance_reason.visible = not _stance_reason.text.is_empty()
	var rung := clampi(Kit.get_int(t, "rung", 0), 0, 4)
	_rung_icon.texture = Ui.icon(StringName("rung_%d" % rung))
	_rung_icon.self_modulate = Ui.colour(&"text.primary")
	var class_text := _Text.resolve(t.get("class_name", {}))
	_id_word.text = tr(&"hud_tracks.card.rung_class").format([_word(&"rung", rung), class_text]) if not class_text.is_empty() else tr(&"hud_tracks.card.rung").format([_word(&"rung", rung)])
	_id_reason.text = _Text.resolve(t.get("rung_reason", {}))
	_id_reason.visible = not _id_reason.text.is_empty()
	_facts.text = "\n".join(_fact_lines(t))
	_render_candidates(t)
	_render_overrides(t)
	_designate.text = tr(&"hud_tracks.card.clear_designation" if Kit.get_bool(t, "designated") else &"hud_tracks.card.designate")
	_designate.disabled = Kit.get_bool(t, "last_known")
	_designate.tooltip_text = tr(&"hud_tracks.card.designate_tooltip")
	_fit.call_deferred()


func _fact_lines(t: Dictionary) -> PackedStringArray:
	var lines := PackedStringArray()
	var range_m := Kit.get_float(t, "range_m", -1.0)
	if range_m >= 0.0:
		lines.append(tr(&"hud_tracks.card.position").format([_tx(&"bearing", [Kit.get_float(t, "bearing_deg", 0.0)]), _tx(&"range", [range_m])]))
	var speed := Kit.get_float(t, "speed_kn", 0.0)
	lines.append(tr(&"hud_tracks.card.motion").format([_tx(&"bearing", [Kit.get_float(t, "course_deg", 0.0)]), _number(speed, &"kn", 0)]))
	var domain := Kit.get_int(t, "domain", Bp.DOMAIN_SURFACE)
	var alt := Kit.get_float(t, "alt_m", 0.0)
	if domain == Bp.DOMAIN_AIR or (domain == Bp.DOMAIN_WEAPON and alt > 1.0):
		lines.append(tr(&"hud_tracks.card.altitude").format([_number(roundf(alt), &"m", 0)]))
	elif domain == Bp.DOMAIN_SUBSURFACE or alt < -1.0:
		lines.append(tr(&"hud_tracks.card.depth").format([_number(roundf(-alt), &"m", 0)]))
	var cpa_m := Kit.get_float(t, "cpa_m", -1.0)
	var cpa_s := Kit.get_float(t, "cpa_s", -1.0)
	if cpa_m >= 0.0:
		if cpa_s > 1.0:
			lines.append(tr(&"hud_tracks.card.cpa").format([_tx(&"range", [cpa_m]), _tx(&"duration", [cpa_s])]))
		else:
			lines.append(tr(&"hud_tracks.card.cpa_now").format([_tx(&"range", [cpa_m])]))
	var sources := _sources(Kit.get_int(t, "sources", 0))
	var grade := _word(&"grade", Kit.get_int(t, "grade", 0))
	if sources.is_empty():
		lines.append(tr(&"hud_tracks.card.grade").format([grade]))
	else:
		lines.append(tr(&"hud_tracks.card.grade_sources").format([grade, sources]))
	var envelope := Kit.get_float(t, "envelope_m", 0.0)
	if envelope > 0.0:
		lines.append(tr(&"hud_tracks.card.envelope").format([_tx(&"range", [envelope])]))
	if Kit.get_bool(t, "struck"):
		lines.append(tr(&"hud_tracks.card.struck"))
	if Kit.get_bool(t, "emitting"):
		lines.append(tr(&"hud_tracks.card.emitting"))
	if Kit.get_bool(t, "inbound"):
		lines.append(tr(&"hud_tracks.card.inbound"))
	var engaged := Kit.get_int(t, "engaged_by", 0)
	if engaged > 0:
		lines.append((tr(&"hud_tracks.note.engaged_one") if engaged == 1 else tr(&"hud_tracks.note.engaged").format([_num(engaged)])))
	if Kit.get_bool(t, "last_known"):
		lines.append(tr(&"hud_tracks.note.last_known").format([_tx(&"duration", [Kit.get_float(t, "age_s", 0.0)])]))
	return lines


func _sources(bits: int) -> String:
	var words := PackedStringArray()
	for i in SOURCE_BITS.size():
		if bits & SOURCE_BITS[i]:
			words.append(tr(SOURCE_KEYS[i]))
	return tr(&"hud_tracks.list_join").join(words)


func _render_candidates(t: Dictionary) -> void:
	var list := Kit.dicts(_card.get("candidates", []))
	var n := list.size() if not _card.is_empty() else Kit.get_int(t, "candidates", 0)
	_candidates.text = tr(&"hud_tracks.card.candidates_one" if n == 1 else &"hud_tracks.card.candidates").format([_num(n)])
	var names := PackedStringArray()
	for i in mini(list.size(), MAX_CANDIDATES):
		var c := list[i]
		var name := _Text.resolve(c.get("name", {}))
		if name.is_empty():
			name = Kit.get_str(c, "class_id")
		if Kit.get_bool(c, "own_side"):
			name = tr(&"hud_tracks.card.candidate_own").format([name])
		elif Kit.get_bool(c, "civilian"):
			name = tr(&"hud_tracks.card.candidate_civilian").format([name])
		names.append(name)
	if list.size() > MAX_CANDIDATES:
		names.append(tr(&"hud_tracks.card.more").format([_num(list.size() - MAX_CANDIDATES)]))
	_candidate_list.text = "\n".join(names)
	_candidate_list.visible = not names.is_empty()
	var ev := Kit.dicts(_card.get("evidence", []))
	var lines := PackedStringArray()
	var start := maxi(0, ev.size() - MAX_EVIDENCE)
	for i in range(start, ev.size()):
		var e := ev[i]
		var why := _Text.resolve(e.get("reason", {}))
		var at := _tx(&"clock", [Kit.get_float(e, "time_s", 0.0)])
		lines.append(tr(&"hud_tracks.card.evidence").format([_word(&"rung", Kit.get_int(e, "rung", 0)), at, why]))
	_evidence.text = "\n".join(lines)
	_evidence.visible = not lines.is_empty()


func _render_overrides(t: Dictionary) -> void:
	_filling = true
	_fill_stance_pick()
	var manual: Dictionary = _manual.get(_track, {})
	var s := int(manual.get("stance", -1))
	_stance_pick.select(maxi(0, STANCE_PICKS.find(s)))
	_id_pick.clear()
	_id_choices.clear()
	_id_pick.add_item(tr(&"hud_tracks.card.identity_auto"), 0)
	var idx := 1
	for c: Dictionary in Kit.dicts(_card.get("candidates", [])):
		var cid := StringName(Kit.get_str(c, "class_id"))
		if cid == &"" or _id_choices.has(cid):
			continue
		var name := _Text.resolve(c.get("name", {}))
		_id_pick.add_item(tr(&"hud_tracks.card.identity_class").format([name if not name.is_empty() else String(cid)]), idx)
		_id_choices.append(cid)
		idx += 1
	var chosen := StringName(manual.get("class_id", &""))
	var at := _id_choices.find(chosen)
	if chosen != &"" and at < 0:
		# A class set by hand that the candidate list no longer holds stays listed.
		_id_pick.add_item(tr(&"hud_tracks.card.identity_class").format([String(chosen)]), idx)
		_id_choices.append(chosen)
		at = _id_choices.size() - 1
	_id_pick.select(at + 1 if at >= 0 else 0)
	_id_pick.disabled = _id_choices.is_empty() and chosen == &""
	_stance_pick.disabled = Kit.get_bool(t, "last_known")
	_filling = false


func _fill_stance_pick() -> void:
	var keep := _stance_pick.selected
	_stance_pick.clear()
	for i in STANCE_PICKS.size():
		var s := STANCE_PICKS[i]
		_stance_pick.add_item(tr(&"hud_tracks.card.stance_auto") if s < 0 else _word(&"stance", s), i)
	if keep >= 0 and keep < STANCE_PICKS.size():
		_stance_pick.select(keep)


# --- actions -------------------------------------------------------------------------------------

func _on_designate() -> void:
	if _track <= 0:
		return
	var designated := Kit.get_bool(_data, "designated")
	Kit.call_on(&"Orders", &"designate", [Bp.NONE if designated else _track])


func _on_centre() -> void:
	if _track > 0:
		centre_requested.emit(_track)


func _on_stance_pick(index: int) -> void:
	if _filling or _track <= 0 or index < 0 or index >= STANCE_PICKS.size():
		return
	var s := STANCE_PICKS[index]
	if Kit.call_on(&"Orders", &"set_stance", [_track, s]) == true:
		var m: Dictionary = _manual.get(_track, {})
		m["stance"] = s
		_manual[_track] = m
		_query()


func _on_id_pick(index: int) -> void:
	if _filling or _track <= 0 or index < 0:
		return
	var ok: Variant
	var m: Dictionary = _manual.get(_track, {})
	if index == 0:
		ok = Kit.call_on(&"Orders", &"set_identity", [_track, Bp.NONE, &""])
		m["class_id"] = &""
	elif index - 1 < _id_choices.size():
		var cid := _id_choices[index - 1]
		ok = Kit.call_on(&"Orders", &"set_identity", [_track, Bp.RUNG_CLASS, cid])
		m["class_id"] = cid
	if ok == true:
		_manual[_track] = m
		_query()


# --- text ----------------------------------------------------------------------------------------

func _word(kind: StringName, value: int) -> String:
	var v: Variant = Kit.call_on(&"Tx", &"word", [kind, value])
	return String(v) if v is String or v is StringName else ""


func _tx(method: StringName, args: Array) -> String:
	var v: Variant = Kit.call_on(&"Tx", method, args)
	return String(v) if v is String or v is StringName else ""


func _number(value: float, unit: StringName, decimals: int) -> String:
	return Kit.number(value, unit, decimals)


static func _num(n: int) -> String:
	return TranslationServer.format_number(str(n), TranslationServer.get_locale())
