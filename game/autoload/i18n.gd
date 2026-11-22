extends Node
## Autoload `I18n`. Loads every res://i18n/*.csv into the TranslationServer at start-up, so a new CSV
## needs no registration and no import step. DESIGN.md L1707, L1740.
##
## CSV format (the engine's own CSV translation columns, so the editor's importer reads the same
## files): UTF-8, comma separated, first row is the header `keys,?context,en[,<locale>...]`.
## `keys` is the text key passed to tr(); `?context` is the note for translators (every row has
## one, L1707) and is not part of the lookup, because keys are unique, so tr(key) needs no context
## argument; every other column is a locale code. A column whose name starts with `_` is ignored. Text uses {0}, {1}... for arguments, in the order the Rust side supplies them (see
## `Tx.msg`).

const DIR := "res://i18n"

var _loaded: Array[Translation] = []


func _ready() -> void:
	reload()


## Drop what was loaded before and read every CSV again. Returns the number of files read.
func reload() -> int:
	for translation: Translation in _loaded:
		TranslationServer.remove_translation(translation)
	_loaded.clear()
	var dir := DirAccess.open(DIR)
	if dir == null:
		return 0
	var files := Array(dir.get_files())
	files.sort()
	var by_locale: Dictionary[String, Translation] = {}
	var count := 0
	for file: String in files:
		if file.get_extension() == "csv" and _load_csv(DIR.path_join(file), by_locale):
			count += 1
	for locale: String in by_locale:
		_loaded.append(by_locale[locale])
		TranslationServer.add_translation(by_locale[locale])
	return count


func _load_csv(path: String, by_locale: Dictionary[String, Translation]) -> bool:
	var file := FileAccess.open(path, FileAccess.READ)
	if file == null:
		return false
	var header := file.get_csv_line()
	if header.size() < 3 or header[0] != "keys" or header[1] != "?context":
		push_warning("i18n: %s does not start with keys,?context" % path)
		return false
	while not file.eof_reached():
		var row := file.get_csv_line()
		if row.size() < 3 or row[0].is_empty():
			continue
		for column in range(2, mini(row.size(), header.size())):
			var locale := header[column]
			if locale.begins_with("_") or locale.begins_with("?") or row[column].is_empty():
				continue
			if not by_locale.has(locale):
				var translation := Translation.new()
				translation.locale = locale
				by_locale[locale] = translation
			by_locale[locale].add_message(row[0], row[column])
	return true
