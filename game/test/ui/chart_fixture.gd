extends "res://ui/chart/chart_source.gd"
## A chart-table source with hand-built state over the real theatre graph, for captures and tests
## of states a short real run never reaches: contacts and convoys, cut lanes, queued and active
## doctrine cards with Intel spends, a hunter-group warning, co-op proposals, a lost or refitting
## hull, a closed region, SEXTANT away from a cove. Units ui-chart, ui-chart-panels, ui-chart-cove.
##
## Senders record what they were asked in `sent` and change nothing.

var state: Dictionary = {}
var sent: Array[Dictionary] = []
## Pretend to be the live source, so the table's Campaign-signal handlers read this fixture.
var live := false


func is_live() -> bool:
	return live


func is_active() -> bool:
	return not state.is_empty()


func chart() -> Dictionary:
	return state.get("chart", {})


func resources() -> Dictionary:
	return state.get("resources", {})


func players() -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	out.assign(state.get("players", []))
	return out


func act_info() -> Dictionary:
	return state.get("act", {})


func bulletin() -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	out.assign(state.get("bulletin", []))
	return out


func intel_options() -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	out.assign(state.get("intel", []))
	return out


func theatre_log(limit: int) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	var all: Array = state.get("log", [])
	out.assign(all.slice(maxi(0, all.size() - limit)))
	return out


func transit_preview(node: StringName) -> Dictionary:
	var previews: Dictionary = state.get("previews", {})
	if previews.has(node):
		return previews[node]
	return {"reachable": false, "hours": 0, "path": [], "passes_cut_lane": false, "text": {"key": &"campaign.transit.not_cove", "args": []}}


func briefing(operation: StringName) -> Dictionary:
	var b: Dictionary = state.get("briefings", {})
	return b.get(operation, {})


func service_quotes() -> Dictionary:
	return state.get("quotes", {})


func unlocks() -> Dictionary:
	return state.get("unlocks", {})


func current_slot() -> Dictionary:
	return state.get("slot", {"campaign_id": "fixture"})


func can_save() -> int:
	return int(state.get("can_save", 0))


func is_advancing() -> bool:
	return bool(state.get("advancing", false))


func is_host() -> bool:
	return bool(state.get("host", true))


func propose(action: Dictionary) -> bool:
	sent.append({"propose": action})
	return true


func respond(proposal: int, accept: bool) -> bool:
	sent.append({"respond": proposal, "accept": accept})
	return true


func confirm(proposal: int) -> bool:
	sent.append({"confirm": proposal})
	return true


func withdraw(proposal: int) -> bool:
	sent.append({"withdraw": proposal})
	return true


func advance() -> bool:
	sent.append({"advance": true})
	return true


func set_clock_paused(paused: bool) -> void:
	sent.append({"paused": paused})


func set_advance_speed(hours_per_s: float) -> void:
	sent.append({"speed": hours_per_s})


func open_briefing(operation: StringName) -> bool:
	sent.append({"open_briefing": operation})
	return true


func buy_service(service: StringName) -> bool:
	sent.append({"buy": service})
	return true


func save_now() -> bool:
	sent.append({"save": true})
	return true


func open_dry_dock(dock_mode: int) -> PackedStringArray:
	sent.append({"dock": dock_mode})
	return PackedStringArray()


# --- a busy mid-campaign state ------------------------------------------------------------------

static func weather(kind: int, sea: int) -> Dictionary:
	return {"kind": kind, "sea_state": sea, "wave_height_m": 1.5, "wind_from_deg": 220.0, "wind_kn": 14.0, "cloud": 0.4, "visibility_m": 9000.0, "rain": 0.0, "sun_elevation_deg": 8.0, "sun_azimuth_deg": 250.0}


static func msg(key: StringName, args: Array = []) -> Dictionary:
	return {"key": key, "args": args}


## Act 2, day 19: SEXTANT at Pell Cove, Gannet Banks at Alert 3 with a cut lane and a queued card,
## convoys and a task group under an ace in sight, a hunter group warned, two co-op proposals, the
## local hull damaged and a partner's hull in the dock, Meridian Roads closed.
func busy_state(hour: int = 444) -> Dictionary:
	var g := graph()
	var regions: Array[Dictionary] = [
		{"id": &"region.gannet", "name": &"region.gannet.name", "open": true, "alert": 3, "supply": 38.0, "patrol_factor": 0.53, "combine_control": false,
			"cards": [{"id": &"card.point_defence_screen", "name": &"card.point_defence_screen.name", "text": &"card.point_defence_screen.desc", "arrives_h": 0, "region": &"region.gannet"}],
			"queued": [{"id": &"card.stand_off_gunnery", "name": &"card.stand_off_gunnery.name", "text": &"card.stand_off_gunnery.desc", "arrives_h": hour + 46, "region": &"region.gannet"}],
			"weather": weather(3, 5)},
		{"id": &"region.corran", "name": &"region.corran.name", "open": true, "alert": 1, "supply": 81.0, "patrol_factor": 0.86, "combine_control": false, "cards": [], "queued": [], "weather": weather(1, 3)},
		{"id": &"region.hollin", "name": &"region.hollin.name", "open": true, "alert": 0, "supply": 100.0, "patrol_factor": 1.0, "combine_control": true, "cards": [], "queued": [], "weather": weather(2, 4)},
		{"id": &"region.meridian", "name": &"region.meridian.name", "open": false, "alert": 0, "supply": 100.0, "patrol_factor": 1.0, "combine_control": true, "cards": [], "queued": [], "weather": weather(0, 2)},
	]
	var chart := {
		"hour": hour, "day": hour / 24 + 1, "act": 1, "retreat": false, "open_theatre": false, "clock_hours_left": 26.0 * 24.0,
		"sextant": &"cove.pell", "coves": [&"cove.wren", &"cove.pell"], "regions": regions,
		"cut_lanes": [{"a": &"depot.skerrow", "b": &"sea.saddle", "until_h": hour + 150}],
		"contacts": [
			{"id": 7, "node": &"sea.kessel", "destination": &"harbour.mareby", "convoy": true, "hulls": 4, "ace": &"", "last_seen_h": hour - 2},
			{"id": 9, "node": &"base.ollan", "destination": &"", "convoy": false, "hulls": 5, "ace": &"ace.auditor", "last_seen_h": hour - 1},
			{"id": 11, "node": &"depot.tarn", "destination": &"", "convoy": false, "hulls": 5, "ace": &"", "last_seen_h": hour - 20},
		],
		"offers": [
			{"id": &"op.act1.convoy_raid", "kind": 0, "template": 0, "name": &"op.act1.convoy_raid.name", "node": &"sea.kessel", "region": &"region.gannet", "forced": false, "transit_h": 9},
			{"id": &"op.act1.story.blind_crane", "kind": 1, "template": -1, "name": &"op.act1.story.blind_crane.name", "node": &"radar.crane", "region": &"region.gannet", "forced": false, "transit_h": 4},
			{"id": &"op.act1.feint", "kind": 0, "template": 7, "name": &"op.act1.feint.name", "node": &"radar.crane", "region": &"region.gannet", "forced": false, "transit_h": 4},
		],
		"proposals": [
			{"id": 3, "by": 1, "action": {"kind": 0, "node": &"cove.wren", "hours": 0, "operation": &"", "player": -1, "intel": null}, "accepted": PackedInt32Array([1]), "made_h": hour - 1, "text": msg(&"campaign.action.transit", [{"key": &"node.cove.wren.name"}])},
			{"id": 4, "by": 0, "action": {"kind": 2, "node": &"", "hours": 12, "operation": &"", "player": -1, "intel": null}, "accepted": PackedInt32Array([0]), "made_h": hour, "text": msg(&"campaign.action.wait", [{"num": 12.0, "unit": &"h"}])},
		],
		"hunter_warning_h": 30,
		"tallies": [
			{"text": msg(&"campaign.tally.method", [{"key": &"units.word.kill.missile"}]), "value": 7.9, "key": "method:missile"},
			{"text": msg(&"campaign.tally.time", [{"key": &"units.word.time.night"}]), "value": 4.2, "key": "time:night"},
		],
		"time_of_day": 1, "phase": 1, "at_cove": true, "can_save": 0,
	}
	return {
		"chart": chart,
		"resources": {"salvage": 1240, "intel": 4, "fuel_l": 31000.0, "ammo_kg": 2150.0, "munitions": {}, "parts_store": {&"gun.40mm.single": 1, &"gun.20mm.single": 2}, "unlocked_parts": 2, "max_tier": 2},
		"players": [
			{"player": 0, "name": "LANTERN", "hull_name": "Corvette Wren", "blueprint_hash": "", "hull_lost": false, "hull_ready_h": -1, "damage": 0.18, "fuel": 0.42, "ammo": 0.2, "connected": true, "is_local": true},
			{"player": 1, "name": "Halyard", "hull_name": "Fast attack craft", "blueprint_hash": "", "hull_lost": false, "hull_ready_h": hour + 7, "damage": 0.0, "fuel": 1.0, "ammo": 1.0, "connected": true, "is_local": false},
		],
		"act": {"act": 1, "cap_t": 400.0, "retreat": false, "clock_days_left": 26.0, "story_done": 2, "story_total": 4, "capstone_open": false, "starters": [], "max_tier": 2, "open_theatre": false},
		"bulletin": chart.regions[0].cards + chart.regions[0].queued,
		"intel": [
			{"kind": 0, "region": &"region.gannet", "card": &"card.stand_off_gunnery", "ace": &"", "cost": 2},
			{"kind": 1, "region": &"region.gannet", "card": &"card.stand_off_gunnery", "ace": &"", "cost": 5},
			{"kind": 2, "region": &"region.corran", "card": &"", "ace": &"", "cost": 1},
			{"kind": 3, "region": &"", "card": &"", "ace": &"ace.bailiff", "cost": 3},
		],
		"log": [
			{"kind": &"lane_cut", "text": msg(&"campaign.event.lane_cut", [{"key": &"node.depot.skerrow.name"}, {"key": &"node.sea.saddle.name"}, 10]), "hour": hour - 30, "a": &"depot.skerrow", "b": &"sea.saddle", "until_hour": hour + 150},
			{"kind": &"alert_escalated", "text": msg(&"campaign.event.alert_escalated", [{"key": &"region.gannet.name"}]), "hour": hour - 20, "region": &"region.gannet"},
			{"kind": &"card_queued", "text": msg(&"campaign.event.card_queued", [{"key": &"card.stand_off_gunnery.name"}, {"key": &"region.gannet.name"}, {"num": 72.0, "unit": &"h"}]), "hour": hour - 26, "region": &"region.gannet", "card": &"card.stand_off_gunnery", "arrives_hour": hour + 46},
			{"kind": &"group_sighted", "text": msg(&"campaign.event.group_sighted", [{"key": &"node.base.ollan.name"}]), "hour": hour - 1, "group": 9, "node": &"base.ollan"},
			{"kind": &"sextant_moved", "text": msg(&"campaign.event.sextant_moved", [{"key": &"node.cove.wren.name"}, {"key": &"node.cove.pell.name"}]), "hour": hour, "from": &"cove.wren", "to": &"cove.pell"},
		],
		"previews": {
			&"cove.wren": {"reachable": true, "hours": 15, "path": [&"cove.pell", &"radar.crane", &"base.ollan", &"sea.saltmere", &"harbour.mareby", &"cove.wren"], "passes_cut_lane": false, "text": msg(&"campaign.transit.hours", [{"num": 15.0, "unit": &"h"}, {"key": &"node.cove.wren.name"}])},
		},
		"briefings": {},
		"quotes": {"repair": {"salvage": 180, "hours": 0, "needed": true}, "rearm": {"salvage": 45, "from_stock": false, "needed": true}, "rebuild": {"salvage": 0, "hours": 0, "needed": false, "allowed": false}},
		"unlocks": {"max_tier": 2, "unlocked": [&"gun.40mm.single", &"launcher.ashm.light"], "recent": [&"launcher.ashm.light"], "store": {&"gun.40mm.single": 1, &"gun.20mm.single": 2}},
		"can_save": 0,
		"host": true,
		"advancing": false,
		"slot": {"campaign_id": "fixture-busy", "ironman": false},
		"graph_nodes": g.get("nodes", []).size(),
	}
