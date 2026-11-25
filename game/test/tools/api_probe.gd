extends Node
## Compile check for the GDScript-facing surface: touches one implemented member of every Rust
## autoload and scene class, so a renamed class, autoload or constant fails at parse time.
## Run by res://test/tools/check_project.gd. Calls nothing that is still a stub.


func probe() -> String:
	var view := BpWorldView.new()
	var plot := BpPlotView.new()
	var dock := BpEditorView.new()
	var feed := BpTestFeed.new()
	var parts: Array[String] = [
		App.version(),
		str(Session.state() == Bp.SESSION_IDLE),
		str(Editor.is_open()),
		str(Editor.rev()),
		str(Editor.in_splash()),
		str(Campaign.is_active()),
		str(Campaign.phase() == Bp.PHASE_NONE),
		str(Lobby.is_active()),
		str(Tutorial.state() == Bp.TUTORIAL_OFF),
		str(Content.act_caps().size()),
		str(view.view() == Bp.VIEW_BRIDGE),
		str(plot.scale_step()),
		str(dock.overlays()),
		str(Bp.STANCE_HOSTILE),
		str(Bp.PRIORITY_EMERGENCY),
		str(Bp.NONE),
		str(Bp.QUALITY_FALLBACK),
		str(Bp.UI_SOUND_SAVED),
		str(Bp.NET_QUALITY_POOR),
		str(Bp.DROP_WARNING_FALLING_BEHIND),
		str(Bp.PORTMAP_RENEW_FAILED),
	]
	view.free()
	plot.free()
	dock.free()
	feed.free()
	# Present and typed, not called: Settings, Quality, Tx, Orders, Hud, AudioFeed, Bench.
	var autoloads: Array[Node] = [Settings, Quality, Tx, Orders, Hud, AudioFeed, Bench, I18n, Ui, Screens, Sound]
	parts.append(str(autoloads.size()))
	return " ".join(parts)
