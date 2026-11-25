extends ScrollContainer
## Cove services: dry dock, repair, rearm, rebuild, parts store, save.
## DESIGN.md L799-828. Unit: ui-chart-cove.
## Reads and calls: Campaign.open_dry_dock, Campaign.service_quotes, Campaign.buy_service, Campaign.unlocks, Campaign.save_now, Campaign.can_save.
##
## Each cove is a full save point and a dry dock (L739). Top to bottom: where SEXTANT lies; Enter
## dry dock (the refit, L801-807: its Salvage and hours are quoted in the dock); the local hull's
## fuel, ammunition and damage against their bands, and in co-op one line per other player; the
## services with their prices from `Campaign.service_quotes` (repairs cost half the damaged parts,
## L818; a reload is bought only for what SEXTANT's stock lacks, L815; a lost hull rebuilds for 60%
## and 48 h, never under Ironman, L826); the parts store (removed parts refit for free, L805); the
## blueprints recovered and the open tier (L807, L815); and Save.
##
## Away from a cove, on passage or with a sortie out, every service is closed and says why. A save
## the player asks for here toasts "Campaign saved"; the automatic saves never toast (L1605).

signal focus_changed
signal rejected(text: Variant)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
## `Bp` values used here, kept so the script parses before the extension loads.
const PHASE_CHART := 1
const PHASE_BRIEFING := 3
const SAVE_ALLOWED := 0
const SAVE_IN_SORTIE := 1
const SAVE_NOT_AT_COVE := 2
const THEATRE_ACTION_REFIT := 1
const DOCK_CAMPAIGN := 1

var _source: RefCounted
var _signature := ""

@onready var _where: Label = $Column/Where
@onready var _dock: Button = $Column/Dock
@onready var _hull_name: Label = $Column/Hull/Name
@onready var _hull_state: Label = $Column/Hull/State
@onready var _fuel: Control = $Column/Hull/Fuel
@onready var _ammo: Control = $Column/Hull/Ammo
@onready var _damage: Control = $Column/Hull/Damage
@onready var _others: Label = $Column/Hull/Others
@onready var _repair: Button = $Column/Services/Repair
@onready var _repair_part: Button = $Column/Services/RepairPart
@onready var _rearm: Button = $Column/Services/Rearm
@onready var _rebuild: Button = $Column/Services/Rebuild
@onready var _wait_refit: Button = $Column/Services/WaitRefit
@onready var _service_note: Label = $Column/Services/Note
@onready var _store_list: Label = $Column/Store/List
@onready var _tier: Label = $Column/Blueprints/Tier
@onready var _blueprints: Label = $Column/Blueprints/List
@onready var _save: Button = $Column/Saving/Save
@onready var _save_note: Label = $Column/Saving/Note


func _ready() -> void:
	Ui.set_separation($Column, 4)
	for box: Control in [$Column/Hull, $Column/Services, $Column/Store, $Column/Blueprints, $Column/Saving]:
		Ui.set_separation(box, 1)
	for l: Label in [_where, _hull_name, _hull_state, _others, _service_note, _store_list, _tier, _blueprints, _save_note]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		l.text = ""
	$Column/Services/Title.text = &"chart.cove.services"
	$Column/Store/Title.text = &"chart.cove.store"
	$Column/Blueprints/Title.text = &"chart.cove.blueprints"
	$Column/Saving/Title.text = &"chart.cove.saving"
	for b: Button in [_dock, _repair, _repair_part, _rearm, _rebuild, _wait_refit, _save]:
		b.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
		b.alignment = HORIZONTAL_ALIGNMENT_LEFT
		b.focus_mode = Control.FOCUS_ALL
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	_dock.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_dock.text = tr(&"chart.cove.dock")
	_dock.pressed.connect(_on_dock)
	_repair.pressed.connect(_buy.bind(&"repair_full"))
	_repair_part.pressed.connect(_buy.bind(&"repair"))
	_rearm.pressed.connect(_buy.bind(&"rearm"))
	_rebuild.pressed.connect(_buy.bind(&"rebuild"))
	_wait_refit.pressed.connect(_on_wait_refit)
	_save.pressed.connect(_on_save)
	_fuel.call(&"configure", {"label": &"chart.cove.fuel", "unit": &"pct", "decimals": 0, "min": 0.0, "max": 100.0, "band_low": 50.0, "band_high": 100.0, "caution_low": 25.0, "trend_waiver": &"chart.cove.no_trend", "show_scale": true})
	_ammo.call(&"configure", {"label": &"chart.cove.ammo", "unit": &"pct", "decimals": 0, "min": 0.0, "max": 100.0, "band_low": 50.0, "band_high": 100.0, "caution_low": 25.0, "trend_waiver": &"chart.cove.no_trend", "show_scale": true})
	_damage.call(&"configure", {"label": &"chart.cove.damage", "unit": &"pct", "decimals": 0, "min": 0.0, "max": 100.0, "band_low": 0.0, "band_high": 10.0, "caution_high": 25.0, "warning_high": 60.0, "trend_waiver": &"chart.cove.no_trend", "show_scale": true})
	accessibility_name = tr(&"chart.tab.cove")


func bind(source: RefCounted) -> void:
	_source = source
	_signature = ""


## Re-read quotes, hulls, store and save state (chart, resources and save changes).
func refresh() -> void:
	if _source == null or not is_node_ready():
		return
	# A hidden tab reads nothing (service quotes run a rearm on a copy of the ledger); the table
	# refreshes the tab when it opens.
	if not is_visible_in_tree() and not _signature.is_empty():
		_signature = "stale"
		return
	var chart: Dictionary = _source.call(&"chart")
	var players: Array[Dictionary] = []
	players.assign(_source.call(&"players"))
	var quotes: Dictionary = _source.call(&"service_quotes")
	var res: Dictionary = _source.call(&"resources")
	var unlocks: Dictionary = _source.call(&"unlocks")
	var can_save: int = _source.call(&"can_save")
	var slot: Dictionary = _source.call(&"current_slot")
	var advancing: bool = _source.call(&"is_advancing") == true
	var sig := str([chart.get("sextant"), chart.get("at_cove"), chart.get("phase"), chart.get("hour") if not advancing else -1, players, quotes, res, unlocks, can_save, Kit.get_bool(slot, "ironman"), advancing])
	if sig == _signature:
		return
	_signature = sig
	render(chart, players, quotes, res, unlocks, can_save, Kit.get_bool(slot, "ironman"), advancing)


func render(chart: Dictionary, players: Array[Dictionary], quotes: Dictionary, res: Dictionary, unlocks: Dictionary, can_save: int, ironman: bool, advancing: bool) -> void:
	if chart.is_empty():
		_where.text = tr(&"chart.cove.no_campaign")
		for c: CanvasItem in [_dock, $Column/Hull, $Column/Services, $Column/Store, $Column/Blueprints, $Column/Saving]:
			c.visible = false
		focus_changed.emit()
		return
	for c: CanvasItem in [_dock, $Column/Hull, $Column/Services, $Column/Store, $Column/Blueprints, $Column/Saving]:
		c.visible = true
	var phase := Kit.get_int(chart, "phase", -1)
	var at_cove := Kit.get_bool(chart, "at_cove")
	var open := at_cove and not advancing and (phase == PHASE_CHART or phase == PHASE_BRIEFING)
	var sextant := StringName(Kit.get_str(chart, "sextant"))
	var hour := Kit.get_int(chart, "hour")
	if at_cove:
		_where.text = tr(&"chart.cove.at").format([_source.call(&"node_name", sextant)])
	elif advancing:
		_where.text = tr(&"chart.cove.passage")
	else:
		_where.text = tr(&"chart.cove.away")
	var closed_reason := tr(&"chart.place.busy") if advancing else tr(&"chart.cove.closed")
	_dock.disabled = not open
	_dock.tooltip_text = tr(&"chart.cove.dock_tip") if open else closed_reason
	# The local hull.
	var me := {}
	var others := PackedStringArray()
	for p: Dictionary in players:
		if Kit.get_bool(p, "is_local"):
			me = p
		else:
			others.append(tr(&"chart.cove.other").format([Kit.get_str(p, "name"), Kit.get_str(p, "hull_name"), _hull_word(p, hour)]))
	_others.visible = not others.is_empty()
	_others.text = "\n".join(others)
	var lost := Kit.get_bool(me, "hull_lost")
	_hull_name.text = Kit.get_str(me, "hull_name") if not me.is_empty() else tr(&"chart.cove.no_hull")
	_hull_state.text = _hull_word(me, hour) if not me.is_empty() else ""
	for g: Control in [_fuel, _ammo, _damage]:
		g.visible = not me.is_empty() and not lost
	if not me.is_empty() and not lost:
		_fuel.call(&"set_reading", Kit.get_float(me, "fuel") * 100.0)
		_ammo.call(&"set_reading", Kit.get_float(me, "ammo") * 100.0)
		_damage.call(&"set_reading", Kit.get_float(me, "damage") * 100.0)
	# Services, priced by the theatre.
	var salvage := Kit.get_int(res, "salvage")
	var repair := Kit.as_dict(quotes.get("repair"))
	var rearm := Kit.as_dict(quotes.get("rearm"))
	var rebuild := Kit.as_dict(quotes.get("rebuild"))
	var notes := PackedStringArray()
	var repair_cost := Kit.get_int(repair, "salvage")
	var repair_needed := Kit.get_bool(repair, "needed") and not lost
	_repair.visible = repair_needed
	_repair_part.visible = repair_needed and repair_cost > salvage and salvage > 0
	if repair_needed:
		_repair.text = tr(&"chart.cove.repair").format([Kit.number(repair_cost, &"salvage", 0)])
		_repair.disabled = not open or repair_cost > salvage
		_repair.tooltip_text = closed_reason if not open else (tr(&"chart.cove.short").format([Kit.number(repair_cost - salvage, &"salvage", 0)]) if repair_cost > salvage else tr(&"chart.cove.repair_tip"))
		_repair_part.text = tr(&"chart.cove.repair_part").format([Kit.number(salvage, &"salvage", 0)])
		_repair_part.disabled = not open
		_repair_part.tooltip_text = tr(&"chart.cove.repair_part_tip") if open else closed_reason
	elif not lost and not me.is_empty():
		notes.append(tr(&"chart.cove.no_damage"))
	var rearm_needed := Kit.get_bool(rearm, "needed") and not lost
	_rearm.visible = rearm_needed
	if rearm_needed:
		var cost := Kit.get_int(rearm, "salvage")
		_rearm.text = tr(&"chart.cove.rearm_stock") if Kit.get_bool(rearm, "from_stock") else tr(&"chart.cove.rearm").format([Kit.number(cost, &"salvage", 0)])
		_rearm.disabled = not open or cost > salvage
		_rearm.tooltip_text = closed_reason if not open else (tr(&"chart.cove.short").format([Kit.number(cost - salvage, &"salvage", 0)]) if cost > salvage else tr(&"chart.cove.rearm_tip"))
	elif not lost and not me.is_empty():
		notes.append(tr(&"chart.cove.full"))
	var rebuild_needed := Kit.get_bool(rebuild, "needed")
	var rebuild_allowed := Kit.get_bool(rebuild, "allowed") and not ironman
	_rebuild.visible = rebuild_needed and rebuild_allowed
	if _rebuild.visible:
		var cost := Kit.get_int(rebuild, "salvage")
		_rebuild.text = tr(&"chart.cove.rebuild").format([Kit.number(cost, &"salvage", 0), Kit.number(Kit.get_int(rebuild, "hours"), &"h", 0)])
		_rebuild.disabled = not open or cost > salvage
		_rebuild.tooltip_text = closed_reason if not open else (tr(&"chart.cove.short").format([Kit.number(cost - salvage, &"salvage", 0)]) if cost > salvage else tr(&"chart.cove.rebuild_tip"))
	elif rebuild_needed:
		notes.append(tr(&"chart.cove.ironman_lost"))
	# Hulls still in the dock: wait for them (the refit time action, L803).
	var waiting := false
	for p: Dictionary in players:
		if Kit.get_int(p, "hull_ready_h", -1) > hour:
			waiting = true
	_wait_refit.visible = waiting
	_wait_refit.text = tr(&"chart.cove.wait_refit")
	_wait_refit.disabled = not open
	_wait_refit.tooltip_text = tr(&"chart.cove.wait_refit_tip") if open else closed_reason
	_service_note.text = " ".join(notes)
	_service_note.visible = not notes.is_empty()
	_render_store(unlocks, res)
	_render_blueprints(unlocks)
	# Saving (L826-828).
	_save.text = tr(&"chart.cove.save")
	_save.disabled = can_save != SAVE_ALLOWED
	var save_notes := PackedStringArray()
	match can_save:
		SAVE_ALLOWED:
			save_notes.append(tr(&"chart.cove.save_ok"))
		SAVE_IN_SORTIE:
			save_notes.append(tr(&"chart.cove.save_sortie"))
		SAVE_NOT_AT_COVE:
			save_notes.append(tr(&"chart.cove.save_away"))
		_:
			save_notes.append(tr(&"chart.cove.save_closed"))
	if ironman:
		save_notes.append(tr(&"chart.cove.save_ironman"))
	_save_note.text = " ".join(save_notes)
	_save.tooltip_text = _save_note.text
	focus_changed.emit()


## Ready, lost, or in the dock until a date-time group.
func _hull_word(p: Dictionary, hour: int) -> String:
	if Kit.get_bool(p, "hull_lost"):
		return tr(&"chart.cove.hull_lost")
	var ready := Kit.get_int(p, "hull_ready_h", -1)
	if ready > hour:
		return tr(&"chart.cove.hull_refit").format([_dtg(ready), Kit.number(ready - hour, &"h", 0)])
	return tr(&"chart.cove.hull_ready")


func _render_store(unlocks: Dictionary, res: Dictionary) -> void:
	var store := Kit.as_dict(unlocks.get("store"))
	if store.is_empty():
		store = Kit.as_dict(res.get("parts_store"))
	var rows: Array[String] = []
	for id: Variant in store:
		var v: Variant = store[id]
		var n := int(v) if (v is int or v is float) else 0
		if n <= 0:
			continue
		rows.append(tr(&"chart.cove.store_row").format([_source.call(&"part_name", StringName(id)), Kit.number(n, &"none", 0)]))
	rows.sort()
	_store_list.text = "\n".join(rows) if not rows.is_empty() else tr(&"chart.cove.store_empty")


func _render_blueprints(unlocks: Dictionary) -> void:
	if unlocks.is_empty():
		_tier.text = ""
		_blueprints.text = ""
		return
	_tier.text = tr(&"chart.cove.tier").format([_word(&"tier", Kit.get_int(unlocks, "max_tier"))])
	var recent: Array = Kit.as_array(unlocks.get("recent"))
	var rows: Array[String] = []
	for id: Variant in Kit.as_array(unlocks.get("unlocked")):
		if not (id is StringName or id is String):
			continue
		var name_text: String = _source.call(&"part_name", StringName(id))
		rows.append(tr(&"chart.cove.blueprint_recent").format([name_text]) if recent.has(id) else name_text)
	rows.sort()
	_blueprints.text = "\n".join(rows) if not rows.is_empty() else tr(&"chart.cove.blueprints_none")


func _buy(service: StringName) -> void:
	if _source == null:
		return
	_source.call(&"buy_service", service)
	_signature = ""
	refresh()


func _on_wait_refit() -> void:
	if _source == null:
		return
	_source.call(&"propose", {"kind": THEATRE_ACTION_REFIT})


func _on_dock() -> void:
	if _source == null:
		return
	var problems: PackedStringArray = _source.call(&"open_dry_dock", DOCK_CAMPAIGN)
	if problems.is_empty():
		return
	var screens := Kit.autoload(&"Screens")
	if screens != null:
		screens.call(&"open_message", &"chart.cove.dock_failed", "\n".join(problems))
	else:
		rejected.emit("\n".join(problems))


func _on_save() -> void:
	if _source == null:
		return
	if _source.call(&"save_now") == true:
		var screens := Kit.autoload(&"Screens")
		if screens != null:
			screens.call(&"toast", &"campaign.event.saved")
	_signature = ""
	refresh()


func focus_controls() -> Array[Control]:
	var out: Array[Control] = []
	for b: Button in [_dock, _repair, _repair_part, _rearm, _rebuild, _wait_refit, _save]:
		if b.visible and b.get_parent().visible and not b.disabled:
			out.append(b)
	return out


static func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"word"):
		return String(tx.call(&"word", kind, value))
	return ""


static func _dtg(hour: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"date_time_group"):
		return String(tx.call(&"date_time_group", hour))
	return ""
