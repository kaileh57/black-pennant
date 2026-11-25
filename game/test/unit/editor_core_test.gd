extends SceneTree
## Headless scenario for the dry-dock model (units gd-editor-core, gd-editor-hull, gd-editor-parts,
## gd-worker). Run from game/:
##   godot --headless --path . -s res://test/unit/editor_core_test.gd
## Opens the tutorial hull, waits for the worker's gauges, drags the beam handle as one undo step, places
## the four tutorial kits with mirror symmetry, undoes and redoes, adds a deck, paints and sculpts, and
## checks that a design_changed handler may call back into Editor (getter and mutator) mid-emit.
## Exits 1 on any failure.

var editor: Node
var failures: int = 0
var changes: int = 0
var reentered: bool = false
var jobs: Dictionary = {}


func _initialize() -> void:
	_run.call_deferred()


func check(ok: bool, what: String) -> void:
	if not ok:
		failures += 1
		push_error("editor_core_test: " + what)


func _on_design_changed(rev: int) -> void:
	changes += 1
	# Re-entrant calls while Rust is emitting: a getter and a mutator.
	check(int(editor.rev()) == rev, "rev() inside design_changed")
	var _sel: Dictionary = editor.selection()
	if not reentered:
		reentered = true
		editor.set_tool(Bp.TOOL_SELECT)


func _on_job(job: StringName, state: int) -> void:
	jobs[String(job)] = state


func wait_job(job: String, frames: int) -> bool:
	for i in frames:
		await process_frame
		if int(jobs.get(job, -1)) == Bp.JOB_DONE:
			return true
	return false


## Wait until the worker has built the design of the current revision (compartments come from it).
func wait_built(frames: int) -> bool:
	for i in frames:
		await process_frame
		if int(jobs.get("gauges", -1)) == Bp.JOB_DONE and editor.compartments().size() > 0:
			return true
	return false


func _run() -> void:
	await process_frame
	editor = root.get_node("/root/Editor")
	editor.design_changed.connect(_on_design_changed)
	editor.job_changed.connect(_on_job)

	# Closed dock: every getter answers without a panic.
	check(not editor.is_open(), "starts closed")
	check(editor.hull().is_empty(), "hull() closed")
	check(editor.tray({}).is_empty(), "tray() closed")
	check(editor.ghost() == null, "ghost() closed")
	check(not editor.undo(), "undo() closed")

	var issues: Array = editor.open(Bp.DOCK_TUTORIAL, {})
	for i: Dictionary in issues:
		check(not bool(i["fatal"]), "tutorial dock opened with a fatal issue")
	check(editor.is_open(), "dock open")
	check(int(editor.dock_mode()) == Bp.DOCK_TUTORIAL, "dock mode")
	var limits: Dictionary = editor.limits()
	check(is_equal_approx(float(limits["cap_t"]), 150.0), "tutorial cap is the Act 1 cap")
	check(await wait_job("gauges", 600), "fast gauges arrive")
	check(await wait_job("notes", 600), "slow gauges and notes arrive")
	for i in 30:
		await process_frame
	check(not bool(editor.undo_state()["dirty"]), "opening a design leaves it clean")

	# Drag the beam handle of a middle station as one undo step.
	var handles: Array = editor.handles()
	check(handles.size() > 0, "a lofted hull has handles")
	var beam: Dictionary = {}
	for h: Dictionary in handles:
		if String(h["id"]) == "beam" and int(h["station"]) == 1:
			beam = h
	check(not beam.is_empty(), "beam handle at station 1")
	var depth0: int = int(editor.undo_state()["depth"])
	editor.begin_group(&"dock.undo.handle")
	var v: float = float(beam["value"])
	for step in 5:
		v = editor.set_handle("beam", 1, v + 0.2, true)
		await process_frame
	editor.end_group(false)
	check(v > float(beam["value"]), "beam grew")
	check(int(editor.undo_state()["depth"]) == depth0 + 1, "a drag is one undo step")
	var applied: float = editor.set_handle("beam", 1, 1.0e9, false)
	check(applied <= float(beam["max"]) + 1.0e-6, "set_handle clamps to the valid range")

	# The four tutorial kits, mirrored where they fit.
	editor.set_symmetry(Bp.SYMMETRY_NONE, 2)
	var kits_before: int = editor.kits().size()
	for kit: String in ["kit.gun", "kit.radar_mast", "kit.engine", "kit.fuel_tank"]:
		check(editor.pick_kit(kit), "pick " + kit)
		var placed: PackedInt32Array = PackedInt32Array()
		var size: Vector3i = editor.hull()["size_cells"]
		for x in range(2, size.x - 2):
			for y in range(0, size.y + 4):
				var g: Dictionary = editor.ghost_move(Vector3i(x, y, size.z / 2))
				if bool(g["valid"]):
					placed = editor.ghost_place()
					break
			if placed.size() > 0:
				break
		check(placed.size() > 0, "placed " + kit)
	editor.ghost_cancel()
	check(editor.ghost() == null, "Esc ends placing")
	check(editor.kits().size() == kits_before + 4, "four kits placed")
	check(editor.undo(), "undo a placement")
	check(editor.kits().size() == kits_before + 3, "undo removed one kit")
	check(editor.redo(), "redo it")
	check(editor.kits().size() == kits_before + 4, "redo restored it")

	# Selection, copy and delete.
	editor.select_kits(PackedInt32Array([kits_before]), false)
	var sel: Dictionary = editor.selection()
	check(PackedInt32Array(sel["parts"]).size() > 0, "selecting a kit selects its parts")
	check(editor.copy_selection(), "copy")
	check(editor.paste(), "paste starts a ghost")
	editor.ghost_cancel()
	var removed: int = editor.delete_selection()
	check(removed > 0, "delete removed the kit's parts")
	check(editor.kits().size() == kits_before + 3, "delete removed the kit")

	# Decks, paint and sculpting.
	var decks0: int = PackedFloat32Array(editor.hull()["decks"]).size()
	var di: int = editor.add_plane(&"deck", 1.0)
	if di >= 0:
		check(PackedFloat32Array(editor.hull()["decks"]).size() == decks0 + 1, "deck added")
		check(editor.remove_plane(&"deck", di), "deck removed")
	var pal: Array = editor.palette()
	check(pal.size() == 16, "palette has 16 entries")
	var stored: Dictionary = editor.set_palette_entry(9, Vector3(1.0, 0.0, 0.0), false)
	var rgb: Vector3 = stored["rgb"]
	check(rgb.x < 0.9, "a pure red pick is clamped")
	var size2: Vector3i = editor.hull()["size_cells"]
	var top: PackedVector3Array = PackedVector3Array([Vector3(size2.x / 2, size2.y + 1, size2.z / 2)])
	check(editor.sculpt(top, true, 1, 0) >= 1, "sculpt adds a wedge")
	check(editor.undo(), "undo the sculpt")

	# Moving, loads, kit settings and stations.
	var parts: Array = editor.parts()
	var kit_list: Array = editor.kits()
	var kit_part: int = int(PackedInt32Array(kit_list[kits_before]["parts"])[0])
	var kp: Dictionary = parts[kit_part]
	var cell0: Vector3i = kp["cell"]
	var moved: bool = false
	for dx in [1, -1, 2, -2, 3, -3]:
		if editor.move_part(kit_part, cell0 + Vector3i(dx, 0, 0), int(kp["yaw"]), int(kp["tilt"])):
			moved = true
			break
	check(moved, "a kit part moves its kit")
	if moved:
		var k_after: Dictionary = editor.kits()[kits_before]
		check(Vector3i(k_after["cell"]) != Vector3i(kit_list[kits_before]["cell"]), "the kit moved with its part")
		check(editor.undo(), "undo the move")
	editor.select_kits(PackedInt32Array([kits_before]), false)
	var sel_moved: bool = editor.move_selection(Vector3i(1, 0, 0)) or editor.move_selection(Vector3i(-1, 0, 0))
	check(sel_moved, "the selection moves")
	editor.clear_selection()
	var loadable: int = -1
	for p: Dictionary in editor.parts():
		if float(p["load"]) >= 0.0:
			loadable = int(p["slot"])
			break
	if loadable >= 0:
		check(editor.set_load(loadable, 0.5), "set a load")
		check(is_equal_approx(float(editor.parts()[loadable]["load"]), 0.5), "load reads back")
	check(editor.kit_updates().size() == 0, "stock kits are up to date")
	var stations: int = (editor.hull()["stations"] as Array).size()
	var length: float = float(editor.hull()["length_m"])
	var si: int = editor.add_station(length * 0.45)
	if si >= 0:
		check((editor.hull()["stations"] as Array).size() == stations + 1, "station added")
		check(editor.remove_station(si), "station removed")
	var new_len: float = editor.set_length(length + 2.0)
	check(new_len > length, "the hull grows")
	check(editor.undo(), "undo the length")

	# Armour, compartments, routes, network edits, prefab capture.
	var hull_size: Vector3i = editor.hull()["size_cells"]
	var side: PackedVector3Array = PackedVector3Array()
	for x in range(2, hull_size.x - 2):
		for y in range(0, hull_size.y):
			side.append(Vector3(x, y, 0))
	var armour: Dictionary = editor.paint_armour(side, Bp.ARMOUR_15MM)
	check(int(armour["cells"]) > 0 and float(armour["mass_delta_kg"]) > 0.0, "armour on the outer cells")
	check(await wait_built(600), "gauges after armour")
	var comps: Array = editor.compartments()
	check(comps.size() > 1, "compartments from the built design")
	var routes: Array = editor.routes()
	check(routes.size() == 3, "three networks")
	var pinned: bool = false
	for net: Dictionary in routes:
		for c: Dictionary in net["conduits"]:
			if not pinned and editor.pin_route(int(net["kind"]), int(c["id"]), true):
				pinned = true
	check(pinned, "a conduit pins")
	check(await wait_built(600), "gauges after the pin")
	var shows_pin: bool = false
	for net: Dictionary in editor.routes():
		for c: Dictionary in net["conduits"]:
			if bool(c["pinned"]):
				shows_pin = true
	check(shows_pin, "the rebuilt routes show the pinned conduit")
	var data_parts: Array[int] = []
	for p: Dictionary in editor.parts():
		data_parts.append(int(p["slot"]))
	var linked: bool = false
	for a in data_parts:
		for b in data_parts:
			if a < b and not linked:
				linked = editor.add_data_link(a, b)
	check(linked, "a data link between two data parts")
	editor.select_kits(PackedInt32Array([kits_before]), false)
	var prefab: Dictionary = editor.capture_prefab("Editor test kit")
	check(not prefab.is_empty(), "prefab captured")
	if not prefab.is_empty():
		var file: String = "user://blueprints/kits/%s.kit.ron" % String(prefab["id"])
		check(FileAccess.file_exists(file), "prefab saved")
		check(int(prefab["version"]) == 1, "prefab version 1")
		var listed: bool = false
		for t: Dictionary in editor.tray({"kits_only": true}):
			if String(t["id"]) == String(prefab["id"]):
				listed = true
		check(listed, "the tray lists the captured kit")
		DirAccess.remove_absolute(file)
	editor.clear_selection()

	# A failed open leaves the open dock as it was.
	var rev_before: int = editor.rev()
	var cap_before: float = float(editor.limits()["cap_t"])
	var bad: Array = editor.open(Bp.DOCK_SANDBOX, {"blueprint": "user://no/such/design.bp.ron"})
	check(bad.size() == 1 and bool(bad[0]["fatal"]), "a missing file is a fatal issue")
	check(editor.is_open() and int(editor.rev()) == rev_before, "the dock stayed open on its design")
	check(is_equal_approx(float(editor.limits()["cap_t"]), cap_before), "its limits are unchanged")
	var no_campaign: Array = editor.open(Bp.DOCK_CAMPAIGN, {})
	check(no_campaign.size() == 1 and bool(no_campaign[0]["fatal"]), "no campaign, no campaign dock")
	check(editor.is_open() and int(editor.dock_mode()) == Bp.DOCK_TUTORIAL, "still the tutorial dock")

	# Bad input never panics.
	check(editor.set_handle("no_such_handle", 99, 1.0, false) == 0.0, "unknown handle")
	check(not editor.remove_part(-5), "negative slot")
	check(not editor.move_part(9999, Vector3i(0, 0, 0), 0, 0), "missing slot")
	check(editor.add_marking(99, Vector3i(0, 0, 0), 9, 0, 1, "x") == -1, "bad marking")
	editor.select_parts(PackedInt32Array([-1, 70000]), false)
	check(PackedInt32Array(editor.selection()["parts"]).size() == 0, "bad slots are not selected")

	check(changes > 5, "design_changed fired")
	check(reentered, "a handler called back into Editor")
	var rev_kept: int = editor.rev()
	var depth_kept: int = int(editor.undo_state()["depth"])
	var kits_kept: int = editor.kits().size()
	editor.close()
	check(not editor.is_open(), "closed")
	var again: Array = editor.open(Bp.DOCK_TUTORIAL, {})
	check(editor.is_open() and again.size() == 0, "reopened with the kept work")
	check(int(editor.rev()) == rev_kept, "the same revision")
	check(int(editor.undo_state()["depth"]) == depth_kept, "the same history")
	check(editor.kits().size() == kits_kept, "the work survived close and open")
	editor.close()

	# The 90 m destroyer: the same path on the largest starter.
	var big: Array = editor.open(Bp.DOCK_SANDBOX, {"blueprint": "stock:starters/destroyer90.bp.ron"})
	for i: Dictionary in big:
		check(not bool(i["fatal"]), "destroyer opens")
	check(await wait_job("gauges", 900), "destroyer gauges")
	var t0: int = Time.get_ticks_msec()
	editor.begin_group(&"dock.undo.handle")
	var bv: float = float(editor.handles()[0]["value"])
	for step in 3:
		bv = editor.set_handle(String(editor.handles()[0]["id"]), int(editor.handles()[0]["station"]), bv + 0.1, true)
	editor.end_group(false)
	var drag_ms: int = Time.get_ticks_msec() - t0
	check(await wait_job("gauges", 900), "destroyer gauges after a drag")
	check(await wait_job("notes", 900), "destroyer notes after a drag")
	print("editor_core_test: destroyer drag of 3 handle steps took %d ms on the main thread" % drag_ms)
	check(editor.pick_kit("kit.ciws"), "pick a kit on the destroyer")
	var dsize: Vector3i = editor.hull()["size_cells"]
	var dplaced: bool = false
	for x in range(4, dsize.x - 4, 3):
		for y in range(0, dsize.y + 4):
			if bool(editor.ghost_move(Vector3i(x, y, dsize.z / 2))["valid"]):
				dplaced = editor.ghost_place().size() > 0
				break
		if dplaced:
			break
	check(dplaced, "a kit placed on the destroyer")
	editor.ghost_cancel()
	editor.close()
	if failures == 0:
		print("editor_core_test: ok")
	quit(1 if failures > 0 else 0)
