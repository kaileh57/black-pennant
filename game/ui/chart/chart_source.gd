extends RefCounted
## Where the chart table reads and sends: the live `Campaign`, `Content` and `Lobby` autoloads. Units
## ui-chart, ui-chart-panels, ui-chart-cove. Not a screen; loaded with preload().
##
## Every chart-table script reads through one of these and never calls the facades directly, so a
## capture or test can hand the table a fixture with the same methods (res://test/ui/chart_fixture.gd)
## and draw states a short real run never reaches. Readers return `{}` or `[]` when a facade is
## missing, refuses or gives the wrong type, so no screen breaks on a closed campaign. Senders return
## false the same way.
##
## `changed` signals are the Campaign's own; a fixture emits none and the caller refreshes by hand.

const Kit := preload("res://ui/screens/menu_kit.gd")

## Theatre graph cache: it is static for a build, and `Content` caches it too.
var _graph: Dictionary = {}
## Readers' results until the next `invalidate`: one Campaign signal makes every panel re-read, and
## `chart()` and `players()` rebuild Dictionaries each call, so they are built once per change.
var _memo: Dictionary[StringName, Variant] = {}
var _memo_frame := -1


## Forget remembered reads. The table calls this at the top of each Campaign signal; every sender
## calls it after sending.
func invalidate() -> void:
	_memo.clear()


func _memo_call(method: StringName, args: Array = []) -> Variant:
	# Never older than this frame, whatever signal was missed.
	var frame := Engine.get_process_frames()
	if frame != _memo_frame:
		_memo.clear()
		_memo_frame = frame
	if not _memo.has(method):
		_memo[method] = Kit.call_on(&"Campaign", method, args)
	return _memo[method]


## The Campaign autoload, or null (compile check, main menu).
func campaign() -> Node:
	return Kit.autoload(&"Campaign")


func is_live() -> bool:
	return true


func is_active() -> bool:
	return Kit.call_on(&"Campaign", &"is_active") == true


func graph() -> Dictionary:
	if _graph.is_empty():
		_graph = Kit.as_dict(Kit.call_on(&"Content", &"theatre_graph"))
	return _graph


func chart() -> Dictionary:
	return Kit.as_dict(_memo_call(&"chart"))


func resources() -> Dictionary:
	return Kit.as_dict(_memo_call(&"resources"))


func players() -> Array[Dictionary]:
	return Kit.dicts(_memo_call(&"players"))


func act_info() -> Dictionary:
	return Kit.as_dict(_memo_call(&"act_info"))


func bulletin() -> Array[Dictionary]:
	return Kit.dicts(_memo_call(&"doctrine_bulletin"))


func intel_options() -> Array[Dictionary]:
	return Kit.dicts(_memo_call(&"intel_options"))


func theatre_log(limit: int) -> Array[Dictionary]:
	return Kit.dicts(Kit.call_on(&"Campaign", &"theatre_log", [limit]))


func transit_preview(node: StringName) -> Dictionary:
	return Kit.as_dict(Kit.call_on(&"Campaign", &"transit_preview", [node]))


## The `BriefingDict` of an offered operation, or `{}`.
func briefing(operation: StringName) -> Dictionary:
	return Kit.as_dict(Kit.call_on(&"Campaign", &"briefing", [operation]))


func service_quotes() -> Dictionary:
	return Kit.as_dict(_memo_call(&"service_quotes"))


func unlocks() -> Dictionary:
	return Kit.as_dict(_memo_call(&"unlocks"))


func current_slot() -> Dictionary:
	return Kit.as_dict(_memo_call(&"current_slot"))


func can_save() -> int:
	var v: Variant = _memo_call(&"can_save")
	return v if v is int else -1


func is_advancing() -> bool:
	return Kit.call_on(&"Campaign", &"is_advancing") == true


## True when this machine hosts the co-op lobby (only the host may confirm a proposal, L839).
func is_host() -> bool:
	if Kit.call_on(&"Lobby", &"is_active") != true:
		return true
	return Kit.call_on(&"Lobby", &"is_host") == true


## One `PartDefDict`, or `{}` for an unknown id.
func part(id: StringName) -> Dictionary:
	return Kit.as_dict(Kit.call_on(&"Content", &"part", [id]))


func aces() -> Array[Dictionary]:
	return Kit.dicts(Kit.call_on(&"Content", &"aces"))


# --- decisions -----------------------------------------------------------------------------------

func propose(action: Dictionary) -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"propose", [action]) == true
	invalidate()
	return ok


func respond(proposal: int, accept: bool) -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"respond", [proposal, accept]) == true
	invalidate()
	return ok


func confirm(proposal: int) -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"confirm", [proposal]) == true
	invalidate()
	return ok


func withdraw(proposal: int) -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"withdraw_proposal", [proposal]) == true
	invalidate()
	return ok


func advance() -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"advance") == true
	invalidate()
	return ok


func set_clock_paused(paused: bool) -> void:
	Kit.call_on(&"Campaign", &"set_clock_paused", [paused])
	invalidate()


func set_advance_speed(hours_per_s: float) -> void:
	Kit.call_on(&"Campaign", &"set_advance_speed", [hours_per_s])


func open_briefing(operation: StringName) -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"open_briefing", [operation]) == true
	invalidate()
	return ok


func buy_service(service: StringName) -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"buy_service", [service]) == true
	invalidate()
	return ok


func save_now() -> bool:
	var ok: bool = Kit.call_on(&"Campaign", &"save_now") == true
	invalidate()
	return ok


## Open the campaign dry dock: `Campaign.open_dry_dock` hands the editor its campaign limits and moves
## the phase (the router then shows the dock), and `Screens.open_dry_dock` opens the editor on them.
## Returns the fatal issues as text; empty when the dock opened.
func open_dry_dock(dock_mode: int) -> PackedStringArray:
	var out := PackedStringArray()
	var opened: bool = Kit.call_on(&"Campaign", &"open_dry_dock") == true
	invalidate()
	if not opened:
		return out
	var screens := Kit.autoload(&"Screens")
	if screens == null:
		return out
	var issues: Variant = screens.call(&"open_dry_dock", dock_mode, {})
	for issue: Dictionary in Kit.dicts(issues):
		if Kit.get_bool(issue, "fatal"):
			out.append_array(Kit.problem_texts([issue], &"chart.cove.dock_failed"))
	return out


# --- names ---------------------------------------------------------------------------------------

var _node_index: Dictionary[StringName, Dictionary] = {}
var _region_index: Dictionary[StringName, Dictionary] = {}


## The static `NodeDict` of a theatre node, or `{}`.
func node_info(id: StringName) -> Dictionary:
	_index()
	return _node_index.get(id, {})


## The translated name of a theatre node; the id itself when the graph does not know it.
func node_name(id: StringName) -> String:
	var n := node_info(id)
	return tr_key(StringName(Kit.get_str(n, "name", String(id))))


## The translated name of a region; the id itself when the graph does not know it.
func region_name(id: StringName) -> String:
	_index()
	var r: Dictionary = _region_index.get(id, {})
	return tr_key(StringName(Kit.get_str(r, "name", String(id))))


## The translated name of an ace (with its callsign when it has one).
func ace_name(id: StringName) -> String:
	for a: Dictionary in aces():
		if StringName(Kit.get_str(a, "id")) == id:
			return tr_key(StringName(Kit.get_str(a, "name", String(id))))
	return String(id)


## The translated name of a part.
func part_name(id: StringName) -> String:
	var p := part(id)
	return tr_key(StringName(Kit.get_str(p, "name", String(id))))


## The name of a campaign player by slot (player-typed, never translated); "" when unknown.
func player_name(slot: int) -> String:
	for p: Dictionary in players():
		if Kit.get_int(p, "player", -1) == slot:
			return Kit.get_str(p, "name")
	return ""


## The local player's slot, or -1.
func local_player() -> int:
	for p: Dictionary in players():
		if Kit.get_bool(p, "is_local"):
			return Kit.get_int(p, "player", -1)
	return -1


## More than one player is connected: decisions become proposals (L839).
func is_coop() -> bool:
	var connected := 0
	for p: Dictionary in players():
		if Kit.get_bool(p, "connected"):
			connected += 1
	return connected > 1


static func tr_key(key: StringName) -> String:
	if key == &"":
		return ""
	return String(TranslationServer.translate(key))


func _index() -> void:
	if not _node_index.is_empty():
		return
	var g := graph()
	for n: Dictionary in Kit.dicts(g.get("nodes")):
		_node_index[StringName(Kit.get_str(n, "id"))] = n
	for r: Dictionary in Kit.dicts(g.get("regions")):
		_region_index[StringName(Kit.get_str(r, "id"))] = r
