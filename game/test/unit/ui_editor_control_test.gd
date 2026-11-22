extends SceneTree
## Headless checks of the dry dock's control panels (units ui-editor-control and ui-editor-graph).
## Run from game/:
##   godot --headless --path . -s res://test/unit/ui_editor_control_test.gd
## Opens the 22 m fast attack craft in a sandbox dock and drives the panels the way a player does:
## showing the Officers tab, the graph and the scripts changes nothing; a preset, a slider drag (one undo
## step), a doctrine row, a power priority and the repair order store and revert through their A chips;
## undo redraws the panel; the rule builder stores a rule (percent typed, fraction stored), switches it,
## moves and deletes it; the graph filters by officer, pins and unpins a node, forks a stage, wires its
## output from the inspector and restores it. Exits 1 on any failure.

const DOCK_PATH := "res://ui/editor/dry_dock.tscn"

var editor: Node
var failures := 0


func _initialize() -> void:
	_run.call_deferred()


func check(ok: bool, what: String) -> void:
	if not ok:
		failures += 1
		push_error("ui_editor_control_test: " + what)


func frames(n: int) -> void:
	for i in n:
		await process_frame


func depth() -> int:
	return int(Dictionary(editor.call(&"undo_state")).get("depth", 0))


func _run() -> void:
	editor = root.get_node("Editor")
	var issues: Array = editor.call(&"open", Bp.DOCK_SANDBOX, {"blueprint": "starters/fac22.bp.ron"})
	for i: Dictionary in issues:
		check(not bool(i.get("fatal", false)), "open: " + str(issues))
	var dock := (load(DOCK_PATH) as PackedScene).instantiate() as Control
	root.add_child(dock)
	await frames(60)
	var rev0 := int(editor.call(&"rev"))
	var tabs := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs") as TabContainer
	tabs.current_tab = 3
	await frames(5)
	dock.call(&"_set_mode", 1)
	await frames(5)
	dock.call(&"_set_mode", 2)
	await frames(5)
	dock.call(&"_set_mode", 0)
	await frames(5)
	check(int(editor.call(&"rev")) == rev0, "showing the control panels changes nothing (rev %d to %d)" % [rev0, int(editor.call(&"rev"))])

	var panel := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Officers/Column/Control")
	var rules := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Officers/Column/Rules")
	var graph := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Graph")

	# Preset: the Aggressive button stores it; the chip reverts it.
	var buttons: Dictionary = panel.get(&"_preset_buttons")
	(buttons[Bp.PRESET_AGGRESSIVE] as Button).pressed.emit()
	await frames(2)
	check(int(Dictionary(editor.call(&"preset", -1)).get("preset", -1)) == Bp.PRESET_AGGRESSIVE, "the preset button stores the preset")
	var chip := panel.get(&"_preset_chip") as Button
	check(bool(chip.get(&"overridden")), "a hand-set preset shows the hand")
	chip.emit_signal(&"revert_requested")
	await frames(2)
	check(int(Dictionary(editor.call(&"preset", -1)).get("preset", -1)) == Bp.PRESET_CAUTIOUS, "the chip reverts the preset")
	check(not bool((panel.get(&"_preset_chip") as Button).get(&"overridden")), "the A chip is back")

	# A slider drag is one undo step, and undo redraws the slider.
	var rows: Dictionary = panel.get(&"_slider_rows")
	var row: Dictionary = rows["gunnery.aggression"]
	var slider := row["slider"] as HSlider
	var d0 := depth()
	slider.drag_started.emit()
	for v: float in [0.6, 0.7, 0.8]:
		slider.value = v
	slider.drag_ended.emit(true)
	await frames(2)
	check(depth() == d0 + 1, "one drag is one undo step (%d to %d)" % [d0, depth()])
	check(absf(float(_slider(editor.call(&"sliders", Bp.OFFICER_GUNNERY), "gunnery.aggression").get("value", 0.0)) - 0.8) < 0.001, "the slider value is stored")
	editor.call(&"undo")
	await frames(3)
	var after: Dictionary = (panel.get(&"_slider_rows") as Dictionary)["gunnery.aggression"]
	check(absf((after["slider"] as HSlider).value - 0.5) < 0.001, "undo moves the slider back: %f" % (after["slider"] as HSlider).value)

	# A doctrine row: hold fire on the first row, then revert it from its chip.
	var drows: Array = panel.get(&"_doctrine_rows")
	check(not drows.is_empty(), "the FAC has doctrine rows")
	if not drows.is_empty():
		var salvo := drows[0]["salvo"] as OptionButton
		salvo.select(salvo.get_item_index(0))
		salvo.item_selected.emit(salvo.get_item_index(0))
		await frames(2)
		var first: Dictionary = Array(editor.call(&"doctrine", -1))[0]
		check(int(first.get("salvo", -1)) == 0 and bool(first.get("overridden", false)), "hold fire is stored: " + str(first))
		check(first.has("default"), "an overridden row carries its default")
		var drows2: Array = panel.get(&"_doctrine_rows")
		(drows2[0]["chip"] as Button).emit_signal(&"revert_requested")
		await frames(2)
		var reverted: Dictionary = Array(editor.call(&"doctrine", -1))[0]
		check(not bool(reverted.get("overridden", true)), "the row's chip reverts it: " + str(reverted))

	# Power priority and repair order.
	var prows: Dictionary = panel.get(&"_power_rows")
	if not prows.is_empty():
		var slot: int = prows.keys()[0]
		var pick := prows[slot]["pick"] as OptionButton
		pick.select(pick.get_item_index(5))
		pick.item_selected.emit(pick.get_item_index(5))
		await frames(2)
		var p := _power(editor.call(&"power_priorities"), slot)
		check(int(p.get("priority", 0)) == 5 or int(p.get("default", 0)) == 5, "the priority is stored: " + str(p))
		((panel.get(&"_power_rows") as Dictionary)[slot]["chip"] as Button).emit_signal(&"revert_requested")
		await frames(2)
		check(not bool(_power(editor.call(&"power_priorities"), slot).get("overridden", true)), "the priority reverts")
	panel.call(&"_move_repair", 0, 1)
	await frames(2)
	check(Array(editor.call(&"repair_order"))[0] == Bp.REPAIR_BREACHES_BELOW_WATERLINE, "the repair order moves")
	check(bool(editor.call(&"repair_order_overridden")), "a moved order is overridden")
	(panel.get(&"_repair_chip") as Button).emit_signal(&"revert_requested")
	await frames(2)
	check(not bool(editor.call(&"repair_order_overridden")), "the repair order reverts")

	# The rule builder: hull health below 40 percent sets helm speed.
	rules.call(&"_on_add")
	await frames(2)
	var draft: Dictionary = rules.get(&"_draft")
	check(not draft.is_empty(), "Add rule opens the builder")
	draft["items"] = [{"op": &"compare", "lhs": {"kind": &"hull_health", "value": 0.0, "part": -1, "filter": {}}, "cmp": Bp.CMP_LT,
		"rhs": {"kind": &"const", "value": 0.0, "part": -1, "filter": {}}}]
	draft["sink"] = {"kind": &"helm_speed", "part": -1, "track": -1}
	draft["value"] = {"type": &"scalar", "value": 25.0}
	rules.call(&"_rebuild")
	await frames(2)
	var spins: Array = (rules.get(&"_builder") as Node).find_children("*", "SpinBox", true, false)
	check(spins.size() >= 2, "the builder shows a number for the comparison and the value")
	if spins.size() >= 1:
		(spins[0] as SpinBox).value = 40.0
	rules.call(&"_on_store")
	await frames(3)
	var stored: Array = editor.call(&"rules")
	check(stored.size() == 1, "the rule is stored: " + str(stored))
	if stored.size() == 1:
		var when: Dictionary = stored[0]["when"]
		check(absf(float(Dictionary(when.get("rhs", {})).get("value", 0.0)) - 0.4) < 0.0001, "40 percent is stored as 0.4: " + str(when))
		rules.call(&"_on_toggle", false, 0)
		await frames(2)
		check(not bool(Array(editor.call(&"rules"))[0]["enabled"]), "the switch turns the rule off")
	editor.call(&"add_rule", -1, {"enabled": true, "when": {"op": &"compare", "lhs": {"kind": &"own_speed_kn"}, "cmp": Bp.CMP_GT,
		"rhs": {"kind": &"const", "value": 20.0}}, "sink": {"kind": &"helm_speed", "part": -1}, "value": {"type": &"scalar", "value": 15.0}})
	await frames(2)
	rules.call(&"_on_move", 1, 0)
	await frames(2)
	check(bool(Array(editor.call(&"rules"))[0]["enabled"]), "moving brings the second rule to the top")
	rules.call(&"_on_delete", 0)
	rules.call(&"_on_delete", 0)
	await frames(2)
	check(Array(editor.call(&"rules")).is_empty(), "delete removes rules")

	# The graph: Gunnery, pin and unpin threat ranking, fork it, wire it, restore it.
	dock.call(&"_set_mode", 1)
	await frames(4)
	graph.call(&"set_officer", Bp.OFFICER_GUNNERY)
	await frames(4)
	var nodes: Dictionary = graph.get(&"_nodes")
	var ranking := -1
	for id: int in (graph.get(&"_data") as Dictionary):
		if String((graph.get(&"_data") as Dictionary)[id].get("type", "")) == "stage.threat_ranking":
			ranking = id
	check(ranking >= 0 and nodes.has(ranking), "Gunnery shows threat ranking")
	var canvas := graph.get_node(^"Column/Body/CanvasBox/Canvas") as GraphEdit
	check(canvas.get_connection_list().size() > 0, "the canvas draws the model's connections")
	check(not canvas.show_menu and not canvas.minimap_enabled and not canvas.show_zoom_buttons, "stock GraphEdit chrome is off")
	if ranking >= 0:
		graph.call(&"focus_node", ranking)
		await frames(3)
		check(int(graph.get(&"_selected")) == ranking, "focus_node selects the node")
		check(bool(editor.call(&"graph_pin", ranking, true)), "pin")
		await frames(3)
		check(bool(_node(editor.call(&"graph_nodes", Bp.OFFICER_GUNNERY), ranking).get("pinned", false)), "the node is pinned")
		check(bool(editor.call(&"graph_pin", ranking, false)), "unpin")
		await frames(3)
		graph.call(&"_replace_stage", Bp.STAGE_THREAT_RANKING)
		await frames(4)
		var first := -1
		for n: Dictionary in Array(editor.call(&"graph_nodes", Bp.OFFICER_GUNNERY)):
			if int(n.get("id", 0)) >= (1 << 24):
				first = int(n.get("id", 0))
		check(first >= 0, "forking adds the subgraph's first node")
		check(int(graph.get(&"_selected")) == first, "the fork's node is selected")
		# Drive the stage's first output from the Select node through the inspector's picker.
		var edges_before := Array(editor.call(&"graph_edges")).size()
		graph.call(&"_pick_source", ranking, 0, true, {}, {"node": first, "port": 0})
		await frames(3)
		check(Array(editor.call(&"graph_edges")).size() == edges_before + 1, "the inspector wires the fork")
		# The canvas: a drag from the fork's output into the stage's drive port is a model connection; a
		# drag off a generated input is refused with a reason.
		var gdata: Dictionary = graph.get(&"_data")
		var n_in := Array(Dictionary(gdata[ranking]).get("inputs", [])).size()
		editor.call(&"graph_disconnect", first, 0, ranking, 0)
		await frames(3)
		var before := Array(editor.call(&"graph_edges")).size()
		canvas.connection_request.emit(StringName(str(first)), 0, StringName(str(ranking)), n_in)
		await frames(3)
		check(Array(editor.call(&"graph_edges")).size() == before + 1, "a canvas drag into a drive port connects through the model")
		canvas.connection_request.emit(StringName(str(first)), 0, StringName(str(ranking)), 0)
		await frames(3)
		check(String(graph.get(&"_status").text) == tr(&"editor_graph.refused.boundary"), "a drag into a replaced stage's own input is refused: " + String(graph.get(&"_status").text))
		var gen_edge: Dictionary = {}
		for ed: Dictionary in Array(editor.call(&"graph_edges")):
			if bool(ed.get("generated", false)) and int(ed.get("to_node", -1)) == ranking:
				gen_edge = ed
		if not gen_edge.is_empty():
			var e0 := Array(editor.call(&"graph_edges")).size()
			canvas.disconnection_request.emit(StringName(str(int(gen_edge.from_node))), int(gen_edge.from_port), StringName(str(ranking)), int(gen_edge.to_port))
			await frames(3)
			check(Array(editor.call(&"graph_edges")).size() == e0, "a generated edge is not cut")
			check(String(graph.get(&"_status").text) == tr(&"editor_graph.refused.generated_edge"), "and the bar says why")
		# A dragged generated node keeps the place it was given.
		var other := -1
		for id: int in (graph.get(&"_nodes") as Dictionary):
			if id < (1 << 24) and id != ranking:
				other = id
		if other >= 0:
			var gnode := (graph.get(&"_nodes") as Dictionary)[other] as GraphNode
			gnode.position_offset = Vector2(1234.0, 567.0)
			canvas.set_selected(gnode)
			graph.call(&"_on_end_move")
			editor.call(&"set_slider", "gunnery.aggression", 0.9)
			await frames(4)
			var moved := (graph.get(&"_nodes") as Dictionary)[other] as GraphNode
			check(moved.position_offset.is_equal_approx(Vector2(1234.0, 567.0)), "a moved node stays put after a refresh: " + str(moved.position_offset))
		graph.call(&"_on_delete_request", [StringName(str(ranking))])
		await frames(2)
		check(not String(graph.get(&"_status").text).is_empty(), "deleting a generated node says why it was refused")
		check(bool(editor.call(&"graph_restore_stage", Bp.STAGE_THREAT_RANKING)), "restore the stock stage")
		await frames(3)
	dock.call(&"_set_mode", 0)

	# Scripts, when the build has Luau: add, type, store, a compile error, switch, delete.
	var scripts := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Scripts")
	if bool(Dictionary(root.get_node("App").call(&"build_info")).get("luau", false)):
		dock.call(&"_set_mode", 2)
		await frames(3)
		(scripts.get(&"_new_name") as LineEdit).text = "Close-in guard"
		scripts.call(&"_on_add")
		await frames(2)
		check(Array(editor.call(&"scripts")).size() == 1, "Add script stores one")
		var code := scripts.get(&"_code") as CodeEdit
		code.text = "--@in range: scalar\n--@out fire: bool\nfunction tick(i)\n  return { fire = i.range < 2000 }\nend\n"
		scripts.call(&"_store")
		await frames(2)
		var s0: Dictionary = Array(editor.call(&"scripts"))[0]
		check(Array(s0.get("inputs", [])).size() == 1 and Array(s0.get("outputs", [])).size() == 1, "declared ports are read: " + str(s0))
		check(not (scripts.get(&"_problem") as Label).visible, "a clean script shows no problem")
		code.text = "--@in range: scalar\nfunction tick(i\n"
		scripts.call(&"_store")
		await frames(2)
		check((scripts.get(&"_problem") as Label).visible, "a compile error shows")
		(scripts.get(&"_new_name") as LineEdit).text = "Second"
		scripts.call(&"_on_add")
		await frames(2)
		check(int(scripts.get(&"_current")) == 1 and (scripts.get(&"_code") as CodeEdit).text.is_empty(), "the new script is picked with an empty field")
		scripts.call(&"_on_pick", 0)
		await frames(2)
		check((scripts.get(&"_code") as CodeEdit).text.begins_with("--@in range"), "picking the first shows its source")
		scripts.call(&"_on_enabled", false)
		check(not bool(Array(editor.call(&"scripts"))[0].get("enabled", true)), "the switch turns it off")
		scripts.call(&"_on_place")
		await frames(2)
		check(String((scripts.get(&"_status") as Label).text) == tr(&"editor_script.placed"), "place in graph: " + String((scripts.get(&"_status") as Label).text))
		check(bool(editor.call(&"script_remove", 1)), "remove")
		await frames(2)
		check((scripts.get(&"_list") as ItemList).item_count == 1, "the list follows a removal")
		dock.call(&"_set_mode", 0)
	else:
		print("ui_editor_control_test: no Luau in this build; script checks skipped")

	# Up and Down leave the control panel for the rules table.
	tabs.current_tab = 3
	await frames(3)
	var last: Control = null
	for c: Node in panel.find_children("*", "Control", true, false):
		var ctl := c as Control
		if ctl.focus_mode != Control.FOCUS_NONE and ctl.is_visible_in_tree():
			last = ctl
	if last != null:
		check(last.focus_neighbor_bottom == NodePath(), "the panel's last control does not trap Down")
		var below := last.find_valid_focus_neighbor(SIDE_BOTTOM)
		check(below != null and not panel.is_ancestor_of(below), "Down from the panel's last control leaves the panel")

	# Closed, then opened again: both panels come back.
	editor.call(&"close")
	await frames(4)
	check(rules.visible, "the rules table never hides itself")
	editor.call(&"open", Bp.DOCK_SANDBOX, {"blueprint": "starters/destroyer90.bp.ron"})
	await frames(90)
	check(((rules.get(&"_title") as Label).visible), "the rules table shows again after a design opens")
	check(not (panel.get(&"_slider_rows") as Dictionary).is_empty(), "the control panel shows again after a design opens")

	# Main-thread cost on the largest starter (L1449: 2 ms adapter budget per frame).
	var t0 := Time.get_ticks_usec()
	panel.call(&"_refresh")
	var t1 := Time.get_ticks_usec()
	panel.set(&"_mounts_dirty", true)
	panel.call(&"_refresh")
	var t2 := Time.get_ticks_usec()
	rules.call(&"_refresh")
	var t3 := Time.get_ticks_usec()
	editor.call(&"set_slider", "gunnery.aggression", 0.7)
	var t4 := Time.get_ticks_usec()
	print("ui_editor_control_test: destroyer90 refresh %.2f ms, with mounts %.2f ms, rules %.2f ms, set_slider %.2f ms" % [
		(t1 - t0) / 1000.0, (t2 - t1) / 1000.0, (t3 - t2) / 1000.0, (t4 - t3) / 1000.0])
	dock.call(&"_set_mode", 1)
	await frames(3)
	graph.call(&"set_officer", -1)
	var t5 := Time.get_ticks_usec()
	graph.call(&"_refresh")
	var t6 := Time.get_ticks_usec()
	await frames(2)
	print("ui_editor_control_test: destroyer90 graph pull %.2f ms, %d nodes" % [(t6 - t5) / 1000.0, (graph.get(&"_nodes") as Dictionary).size()])
	dock.call(&"_set_mode", 0)

	dock.queue_free()
	editor.call(&"close")
	await frames(2)
	print("ui_editor_control_test: %s" % ("ok" if failures == 0 else "%d failures" % failures))
	quit(1 if failures > 0 else 0)


func _slider(list: Array, key: String) -> Dictionary:
	for s: Dictionary in list:
		if String(s.get("key", "")) == key:
			return s
	return {}


func _power(list: Array, slot: int) -> Dictionary:
	for p: Dictionary in list:
		if int(p.get("slot", -1)) == slot:
			return p
	return {}


func _node(list: Array, id: int) -> Dictionary:
	for n: Dictionary in list:
		if int(n.get("id", -1)) == id:
			return n
	return {}
