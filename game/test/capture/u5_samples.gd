extends RefCounted
## Hand-built facade data in the shapes of crates/gdclient/src/shapes.rs for the u5 captures and the
## ui_reports_test: a prize-capture briefing, its Fit Check, an after-action report, a skirmish summary,
## and special-operations bar rows. Real text keys, so the screens show real English.


static func msg(key: String, args: Array = []) -> Dictionary:
	return {"key": StringName(key), "args": args}


static func num(v: float, unit: String) -> Dictionary:
	return {"num": v, "unit": StringName(unit)}


static func weather() -> Dictionary:
	return {
		"kind": 3, "sea_state": 4, "wave_height_m": 1.9, "wind_from_deg": 245.0, "wind_kn": 19.0, "cloud": 0.8,
		"visibility_m": 7400.0, "rain": 0.4, "sun_elevation_deg": 6.0, "sun_azimuth_deg": 260.0,
	}


static func briefing() -> Dictionary:
	return {
		"operation": &"op.act1.prize_capture", "kind": 0, "name": &"op.act1.prize_capture.name",
		"text": &"op.act1.prize_capture.briefing", "region": &"gannet", "node": &"", "weather": weather(),
		"time_of_day": 1, "distance_km": 18.5,
		"threats": [
			{"class_id": &"class.patrol_boat", "name": msg("class.patrol_boat.name"), "count_min": 2, "count_max": 3, "confidence": 1, "ace": &""},
			{"class_id": &"class.coaster", "name": msg("class.coaster.name"), "count_min": 1, "count_max": 1, "confidence": 2, "ace": &""},
			{"class_id": &"class.torpedo_boat", "name": msg("class.torpedo_boat.name"), "count_min": 0, "count_max": 2, "confidence": 0, "ace": &"ace.auditor"},
		],
		"tags": [
			msg("campaign.tag.draft_limit", [num(2.4, "none")]),
			msg("campaign.tag.requires", [{"key": &"campaign.part_tag.boarding_kit"}]),
			msg("campaign.tag.min_speed", [num(24.0, "kn")]),
		],
		"ghosts": [
			{"text": msg("campaign.ghost.no_radar_emissions"), "reward": msg("campaign.reward.intel", [num(1.0, "intel")])},
			{"text": msg("campaign.ghost.no_losses"), "reward": msg("campaign.reward.salvage", [num(120.0, "salvage")])},
		],
		"actions": PackedInt32Array([0]), "recommended_emcon": 0,
		"route": PackedVector2Array([Vector2(-6000, 4000), Vector2(-1500, 800), Vector2(2200, -900)]),
		"map": {"size_m": Vector2(20000, 20000), "land": [], "shallows": [], "markers": []},
		"committed_hulls": 5, "committed_tonnage_t": 640.0, "pushback_days": 1.0, "forced": false,
		"expires_h": 160, "dtg": "061400Z MAR",
	}


## A Fit Check result; `state` is a Bp.JOB_* value.
static func fit(state: int) -> Dictionary:
	return {
		"state": state, "passes": false,
		"tags": [
			{"text": msg("campaign.tag.draft_limit", [num(2.4, "none")]), "pass": true, "value": 2.1, "limit": 2.4, "unit": &"m"},
			{"text": msg("campaign.tag.requires", [{"key": &"campaign.part_tag.boarding_kit"}]), "pass": true, "value": 1.0, "limit": 1.0, "unit": &"none"},
			{"text": msg("campaign.tag.min_speed", [num(24.0, "kn")]), "pass": false, "value": 22.6, "limit": 24.0, "unit": &"kn"},
		],
		"top_speed_kn": 22.6, "time_to_turn_180_s": 41.0, "range_km": 610.0,
		"detect": [
			{"class_id": &"class.patrol_boat", "we_detect_km": 21.0, "they_detect_km": 17.5},
			{"class_id": &"class.torpedo_boat", "we_detect_km": 18.0, "they_detect_km": 19.5},
		],
		"simulated": true,
	}


static func players() -> Array[Dictionary]:
	return [
		{"player": 0, "name": "Ada", "hull_name": "Kestrel", "blueprint_hash": "", "hull_lost": false, "hull_ready_h": -1, "damage": 0.1, "fuel": 0.8, "ammo": 0.6, "connected": true, "is_local": true},
		{"player": 1, "name": "Bram", "hull_name": "Osprey", "blueprint_hash": "", "hull_lost": false, "hull_ready_h": -1, "damage": 0.4, "fuel": 0.5, "ammo": 0.3, "connected": true, "is_local": false},
	]


static func accept_proposal() -> Dictionary:
	return {"id": 4, "by": 0, "action": {"kind": 3, "node": &"", "hours": 0, "operation": &"op.act1.prize_capture", "player": 0, "intel": null}, "accepted": PackedInt32Array([0]), "made_h": 130, "text": msg("")}


static func regions() -> Array[Dictionary]:
	return [
		{"id": &"gannet", "name": &"region.gannet.name"},
		{"id": &"corran", "name": &"region.corran.name"},
	]


static func stats(won: bool) -> Dictionary:
	return {
		"rounds_fired": 412, "rounds_hit": 63, "missiles_fired": 2, "missiles_hit": 1, "torpedoes_fired": 0,
		"torpedoes_hit": 0, "decoys_fired": 3, "missiles_seduced": 2, "fires": 1, "first_detection_s": 134.0,
		"radiating_s": 95.0, "enemy_tonnage_disabled_t": 410.0 if won else 120.0, "enemy_tonnage_total_t": 640.0,
		"own_cells_lost": 0.14 if won else 0.61, "alerts_raised": 9,
		"kills_by_method": [{"method": 0, "count": 2}, {"method": 6, "count": 1}] if won else [{"method": 0, "count": 1}],
	}


static func after_action(won: bool) -> Dictionary:
	return {
		"operation": &"op.act1.prize_capture", "outcome": 0 if won else 1, "stats": stats(won),
		"payout": {"salvage_wrecks": 180 if won else 40, "salvage_prizes": 330 if won else 0, "intel": 3 if won else 0,
			"blueprints": [&"gun.20mm.single"] if won else [], "fuel_l": 2400.0 if won else 0.0, "ammo_kg": 650.0 if won else 0.0},
		"tallies": [
			{"text": msg("campaign.tally.method", [{"key": &"units.word.kill.gunfire"}]), "value": 7.4, "key": "method.gunfire"},
			{"text": msg("campaign.tally.time", [{"key": &"units.word.time.dusk"}]), "value": 10.6, "key": "time.dusk"},
		],
		"kills_by_method": stats(won)["kills_by_method"],
		"queued_cards": [{"id": &"stand_off_gunnery", "name": &"card.stand_off_gunnery.name", "text": &"card.stand_off_gunnery.desc", "arrives_h": 214, "region": &"gannet"}] if won else [],
		"alert_changes": [
			{"region": &"gannet", "alert": 1.5, "cause": 2},
			{"region": &"gannet", "alert": 2.5, "cause": 0},
		],
		"clock_pushback_days": 1.0 if won else 0.0,
		"belief": PackedVector3Array([Vector3(-1200, 300, 0.8), Vector3(400, -200, 0.5)]),
		"ghosts": [
			{"text": msg("campaign.ghost.no_radar_emissions"), "met": true, "reward": msg("campaign.reward.intel", [num(1.0, "intel")])},
			{"text": msg("campaign.ghost.no_losses"), "met": won, "reward": msg("campaign.reward.salvage", [num(120.0, "salvage")])},
		],
		"repair_cost": [{"player": 0, "salvage": 96}, {"player": 1, "salvage": 210}],
		"hulls_lost": PackedInt32Array() if won else PackedInt32Array([1]),
		"events": [
			{"kind": &"alert_changed", "text": msg("campaign.event.alert_changed", [{"key": &"region.gannet.name"}, 2, {"key": &"campaign.cause.observed_kill"}]), "hour": 142},
			{"kind": &"clock_pushed_back", "text": msg("campaign.event.tally_changed", [{"key": &"units.word.kill.gunfire"}, num(7.4, "none")]), "hour": 142},
		],
	}


static func summary(won: bool) -> Dictionary:
	return {"outcome": 0 if won else 1, "winner_side": 0 if won else 1, "duration_s": 1104.0, "objectives": [{"id": 1, "done": won}, {"id": 2, "done": true}], "stats": stats(won), "tip": msg("")}


static func objectives() -> Array[Dictionary]:
	return [
		{"id": 1, "ghost": false, "state": 1, "text": msg("campaign.ghost.no_losses"), "progress": 1.0, "marker": null},
		{"id": 2, "ghost": true, "state": 1, "text": msg("campaign.ghost.no_radar_emissions"), "progress": 1.0, "marker": null},
	]


static func available() -> Array[Dictionary]:
	return [
		{"action": 0, "enabled": true, "reason": msg(""), "needs": &"track"},
		{"action": 3, "enabled": false, "reason": msg("hud.action.rhib_busy"), "needs": &"none"},
		{"action": 4, "enabled": true, "reason": msg(""), "needs": &"line"},
	]


static func actions() -> Array[Dictionary]:
	return [
		{"id": 3, "action": 0, "actor": 100, "state": 1, "progress": 0.42, "block": msg("sim.action.too_fast"), "at": Vector2(100, 200)},
	]


static func actions_done() -> Array[Dictionary]:
	return [
		{"id": 3, "action": 0, "actor": 100, "state": 3, "progress": 1.0, "block": msg(""), "at": Vector2(100, 200)},
	]


static func offboard() -> Array[Dictionary]:
	return [
		{"slot": 7, "body": 9001, "aboard": 1, "endurance_s": 1320.0, "recharging_s": 0.0, "name": msg("part.bay.drone.rotor.name")},
		{"slot": 8, "body": -1, "aboard": 0, "endurance_s": 0.0, "recharging_s": 75.0, "name": msg("part.bay.drone.fixed.name")},
	]
