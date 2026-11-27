class_name Settings
extends PanelContainer
## The settings form: the user's name, Pal's voice and where replies come from. Saves to the settings
## file, emits `saved` and closes; a new one is made for each opening, in the current language.

signal saved

const PROVIDERS := ["local", "ollama", "openai", "anthropic"]
const PROVIDER_NAMES := ["llama (lokal)", "Ollama", "OpenAI", "Anthropic"]
const DEFAULT_NAME := "Michael"
## Pocket TTS catalog voices; `downloader.rs` lists the same voices. The first is the default. Voices of one's
## own are `.safetensors` files in `<model dir>/pocket/voices/<lang>/`, e.g. cloned with pal-tts.
const VOICES := ["juergen", "stuart_bell", "michael"]
const VOICE_NAMES := ["Jürgen", "Stuart Bell", "Michael"]
const LANGUAGES := ["de", "en"]
## Shown as placeholders and used while a field is empty.
const DEFAULT_URLS := {
	"ollama": "http://localhost:11434/v1", "openai": "https://api.openai.com/v1",
	"anthropic": "https://api.anthropic.com/v1",
}
const DEFAULT_MODELS := {"ollama": "llama3.2", "openai": "gpt-6-luna", "anthropic": "claude-haiku-5-5"}
const DEFAULT_EFFORTS := {"ollama": "none", "openai": "none", "anthropic": "low"}
const VOICE_SOURCES := ["local", "fish"]
const DEFAULT_FISH_MODEL := "s2.1-pro-free"
## Same files as in `downloader.rs`, fetched once chosen or as the default (`default_model`).
const LOCAL_MODELS := ["gemma-4-E2B-it-Q4_K_M.gguf", "granite-3.3-2b-instruct-Q4_K_M.gguf", "SmolLM3-Q4_K_M.gguf",
	"granite-4.0-h-1b-Q4_0.gguf"]
const LOCAL_MODEL_BYTES := [3_110_000_000, 1_550_000_000, 1_920_000_000, 870_000_000]
## Gemma 4 E2B is the default from this much RAM (6 GB phones report ~5.7 GB), Granite 3.3 2B below or if unknown.
const GEMMA_DEFAULT_RAM := 5_500_000_000
## GGUF files the user brought, in the model folder; `downloader.rs` has the same folder. Pal offers none of its own.
const OWN_MODEL_DIR := "llm"
const COPY_CHUNK := 16 * 1024 * 1024
## The default of earlier versions, which saved no choice for it.
const OLD_DEFAULT_MODEL := "Qwen3.5-2B-Q4_K_M.gguf"
## The least RAM a model can be chosen with.
const LOCAL_MODEL_MIN_RAM := [0, 0, 0, 0]
## Kept free beside a new model, for its cache files and the system.
const SPARE_BYTES := 1_000_000_000
## Same limit as in `character.rs`.
const MAX_PERSONA_BYTES := 256 * 1024
## Same limit as in `voice_file.rs`.
const MAX_VOICE_BYTES := 64 * 1024 * 1024
const TEMPLATE_NAMES := {"de": "vorlage", "en": "template"}
const TEXTS := {
	"de": {"title": "Einstellungen", "name": "Dein Name", "voice_pocket": "Lokale Stimme",
		"voice": "Stimme", "voice_local": "Lokal",
		"voice_fish": "Fish Audio (online)", "fish_voice": "Stimmen-ID",
		"llm": "Sprachmodell", "url": "Server-Adresse", "local_model": "Lokales Modell",
		"local_models": ["Gemma 4 E2B (3,1 GB, schnell, bestes Deutsch)", "Granite 3.3 2B (1,5 GB)",
			"SmolLM3 3B (1,9 GB, am langsamsten)", "Granite 4.0 1B (0,9 GB, schwach)"],
		"no_room": "Zu wenig Speicher: %s GB frei, %s GB nötig.",
		"model_import": "Modell laden (GGUF)", "model_bad": "Modell nicht geladen: %s",
		"model_copying": "Modell wird kopiert …", "own_model": "%s (eigene Datei)",
		"persona": "Persönlichkeit",
		"persona_default": "KITT (Standard)", "persona_import": "Datei laden", "persona_template": "Vorlage",
		"persona_bad": "Datei nicht geladen: %s", "persona_written": "Zum Bearbeiten: %s",
		"voice_import": "Stimme laden", "voice_bad": "Stimme nicht geladen: %s",
		"consent_title": "Eigene Stimme laden",
		"consent_text": "Lade nur deine eigene Stimme oder die einer Person, die ausdrücklich zugestimmt hat.",
		"consent_yes": "Ich bestätige das",
		"model": "Modell", "effort": "Reasoning", "key": "API-Schlüssel",
		"fallback": "Bei Serverfehler lokales llama nutzen",
		"save": "Speichern", "cancel": "Abbrechen"},
	"en": {"title": "Settings", "name": "Your name", "voice_pocket": "Local voice",
		"voice": "Voice", "voice_local": "Local", "voice_fish": "Fish Audio (online)",
		"fish_voice": "Voice ID", "llm": "Language model", "url": "Server address",
		"local_model": "Local model",
		"local_models": ["Gemma 4 E2B (3.1 GB, fast, best)", "Granite 3.3 2B (1.5 GB)",
			"SmolLM3 3B (1.9 GB, slowest)", "Granite 4.0 1B (0.9 GB, weak)"],
		"no_room": "Not enough storage: %s GB free, %s GB needed.",
		"model_import": "Load model (GGUF)", "model_bad": "Model not loaded: %s",
		"model_copying": "Copying the model …", "own_model": "%s (own file)",
		"persona": "Personality",
		"persona_default": "KITT (default)", "persona_import": "Load file", "persona_template": "Template",
		"persona_bad": "File not loaded: %s", "persona_written": "To edit: %s",
		"voice_import": "Load voice", "voice_bad": "Voice not loaded: %s",
		"consent_title": "Load a voice of your own",
		"consent_text": "Only load your own voice, or that of a person who has explicitly agreed.",
		"consent_yes": "I confirm",
		"model": "Model", "effort": "Reasoning", "key": "API key",
		"fallback": "Use local llama when the server fails",
		"save": "Save", "cancel": "Cancel"},
}

var _cfg: ConfigFile
var _path: String
var _model_dir: String
var _texts: Dictionary
var _lang: String
var _local_model := OptionButton.new()
var _model_list := []
var _copy: Thread
var _stop_copy := false
var _local_model_field: Control
var _notice := Label.new()
var _name := LineEdit.new()
var _voice := OptionButton.new()
var _voice_list := []
var _voice_source := OptionButton.new()
var _persona := OptionButton.new()
var _persona_list := []
var _local_fields: Array[Control] = []
var _fish_fields: Array[Control] = []
var _fish_key := LineEdit.new()
var _fish_voice := LineEdit.new()
var _fish_model := LineEdit.new()
var _provider := OptionButton.new()
var _url := LineEdit.new()
var _model := LineEdit.new()
var _effort := LineEdit.new()
var _key := LineEdit.new()
var _fallback := CheckBox.new()
var _remote: Array[Control] = []


static func provider(cfg: ConfigFile) -> String:
	return cfg.get_value("llm", "provider", "local")


static func user_name(cfg: ConfigFile) -> String:
	var n: String = cfg.get_value("user", "name", "")
	return n.strip_edges() if not n.strip_edges().is_empty() else DEFAULT_NAME


## The catalog voices and those of one's own for `lang`.
static func voices(model_dir: String, lang: String) -> Array:
	var own := model_dir.path_join("pocket/voices").path_join(lang)
	if not DirAccess.dir_exists_absolute(own):
		return VOICES
	var found := Array(DirAccess.get_files_at(own)).filter(func(f: String) -> bool: return f.ends_with(".safetensors"))
	return VOICES + found.map(func(f: String) -> String: return f.get_basename())


## Every voice for the one selection: the catalog, then those of one's own in any language.
static func all_voices(model_dir: String) -> Array:
	var list: Array = VOICES.duplicate()
	for lang in LANGUAGES:
		for v in voices(model_dir, lang):
			if v not in list:
				list.append(v)
	return list


## The first voice of one's own in `list`, or else the first catalog voice.
static func default_voice(list: Array) -> String:
	return list[VOICES.size()] if list.size() > VOICES.size() else VOICES[0]


## The saved voice if it exists in `lang`, or else the default. Settings from before the single voice name one per
## language.
static func voice(cfg: ConfigFile, lang: String, model_dir: String) -> String:
	var list := voices(model_dir, lang)
	var v: String = cfg.get_value("voice", "name", cfg.get_value("voice", lang, ""))
	return v if v in list else default_voice(list)


## The file of a voice of one's own.
static func voice_path(model_dir: String, lang: String, voice_name: String) -> String:
	return model_dir.path_join("pocket/voices").path_join(lang).path_join(voice_name + ".safetensors")


static func voice_source(cfg: ConfigFile) -> String:
	return cfg.get_value("voice", "source", "local")


static func url(cfg: ConfigFile) -> String:
	return cfg.get_value("llm", "url", "")


static func model(cfg: ConfigFile) -> String:
	var m: String = cfg.get_value("llm", "model", "")
	return m if not m.is_empty() else DEFAULT_MODELS.get(provider(cfg), "")


## An own OpenAI model gets no default: which efforts it accepts differs from model to model.
static func effort(cfg: ConfigFile) -> String:
	var e: String = cfg.get_value("llm", "effort", "")
	if not e.is_empty():
		return e
	var own_model: bool = not (cfg.get_value("llm", "model", "") as String).is_empty()
	return "" if own_model and provider(cfg) == "openai" else DEFAULT_EFFORTS.get(provider(cfg), "")


static func fish_model(cfg: ConfigFile) -> String:
	var m: String = cfg.get_value("fish", "model", "")
	return m if not m.is_empty() else DEFAULT_FISH_MODEL


## `ram` is the device's memory in bytes; if it is unknown (not positive), every model may be tried.
static func fits_ram(index: int, ram: int) -> bool:
	return ram <= 0 or ram >= LOCAL_MODEL_MIN_RAM[index]


## The chosen local GGUF file, or the default.
static func local_model(cfg: ConfigFile) -> String:
	var m: String = cfg.get_value("llm", "local_model", "")
	return m if m in LOCAL_MODELS or m.begins_with(OWN_MODEL_DIR + "/") else default_model()


## The model used while none is chosen, by the device's RAM.
static func default_model() -> String:
	var ram: int = OS.get_memory_info()["physical"]
	return LOCAL_MODELS[0] if ram >= GEMMA_DEFAULT_RAM else LOCAL_MODELS[1]


## The user's own GGUF files, as paths in the model folder.
static func own_models(model_dir: String) -> Array:
	var dir := model_dir.path_join(OWN_MODEL_DIR)
	if not DirAccess.dir_exists_absolute(dir):
		return []
	var found := Array(DirAccess.get_files_at(dir)).filter(func(f: String) -> bool: return f.ends_with(".gguf"))
	return found.map(func(f: String) -> String: return OWN_MODEL_DIR.path_join(f))


## A catalog model, or one of one's own that is still there.
static func known_model(m: String, model_dir: String) -> bool:
	return m in LOCAL_MODELS or (m.begins_with(OWN_MODEL_DIR + "/") and FileAccess.file_exists(model_dir.path_join(m)))


## Whether a start loads the chosen catalog model, so that it can prove or fail its trial.
static func runs_local_model(cfg: ConfigFile) -> bool:
	var local: bool = provider(cfg) == "local" or cfg.get_value("llm", "fallback", false)
	return local and OS.get_environment("PAL_LLM").is_empty()


## A size in GB with the language's decimal sign.
static func gigabytes(bytes: int, lang: String) -> String:
	var text := String.num(bytes / 1e9, 1)
	return text.replace(".", ",") if lang == "de" else text


static func _persona_dir(model_dir: String, lang: String) -> String:
	return model_dir.path_join("personas").path_join(lang)


## The character files for `lang`, without extension.
static func personas(model_dir: String, lang: String) -> Array:
	var dir := _persona_dir(model_dir, lang)
	if not DirAccess.dir_exists_absolute(dir):
		return []
	var found := Array(DirAccess.get_files_at(dir)).filter(func(f: String) -> bool: return f.ends_with(".txt"))
	return found.map(func(f: String) -> String: return f.get_basename())


## Every character for the one selection: the built-in one (""), then the files of any language.
static func all_personas(model_dir: String) -> Array:
	var list: Array = [""]
	for lang in LANGUAGES:
		for p in personas(model_dir, lang):
			if p not in list:
				list.append(p)
	return list


## The saved character. Settings from before the single character name one per language.
static func saved_persona(cfg: ConfigFile, lang: String) -> String:
	return cfg.get_value("persona", "name", cfg.get_value("persona", lang, ""))


## The chosen character's file for `lang`; empty for the built-in character, also if it has no file in `lang`.
static func persona_path(cfg: ConfigFile, lang: String, model_dir: String) -> String:
	var chosen := saved_persona(cfg, lang)
	if chosen not in personas(model_dir, lang):
		return ""
	return _persona_dir(model_dir, lang).path_join(chosen + ".txt")


func _init(cfg: ConfigFile, path: String, lang: String, model_dir: String) -> void:
	_cfg = cfg
	_model_dir = model_dir
	_voice_list = all_voices(model_dir)
	_path = path
	_texts = TEXTS[lang]
	_lang = lang
	visible = false
	set_anchors_preset(Control.PRESET_FULL_RECT)
	var background := StyleBoxFlat.new()
	background.bg_color = Color(0, 0, 0, 0.92)
	add_theme_stylebox_override("panel", background)
	theme = Theme.new()
	theme.default_font_size = 28
	var margin := MarginContainer.new()
	for side in ["left", "right", "top", "bottom"]:
		margin.add_theme_constant_override("margin_" + side, 32)
	add_child(margin)
	var form := VBoxContainer.new()
	form.add_theme_constant_override("separation", 12)
	form.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	var scroll := ScrollContainer.new()
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	scroll.add_child(form)
	margin.add_child(scroll)

	var title := Label.new()
	title.text = _texts["title"]
	title.add_theme_font_size_override("font_size", 36)
	form.add_child(title)
	_field(form, "name", _name)
	_name.placeholder_text = DEFAULT_NAME
	_field(form, "persona", _persona_input())
	for i in VOICE_SOURCES.size():
		_voice_source.add_item(_texts["voice_" + VOICE_SOURCES[i]], i)
	_voice_source.item_selected.connect(func(_i: int) -> void: _show_voice_source())
	_field(form, "voice", _voice_source)
	for i in _voice_list.size():
		_voice.add_item(_voice_label(i), i)
	var load_voice := Button.new()
	load_voice.text = _texts["voice_import"]
	load_voice.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	load_voice.pressed.connect(_ask_voice_consent)
	var voice_box := VBoxContainer.new()
	voice_box.add_child(_voice)
	voice_box.add_child(load_voice)
	_local_fields.append(_field(form, "voice_pocket", voice_box))
	_fish_key.secret = true
	_fish_fields.append(_field(form, "key", _fish_key))
	_fish_fields.append(_field(form, "fish_voice", _fish_voice))
	_fish_model.placeholder_text = DEFAULT_FISH_MODEL
	_fish_fields.append(_field(form, "model", _fish_model))
	for i in PROVIDERS.size():
		_provider.add_item(PROVIDER_NAMES[i], i)
	_provider.item_selected.connect(func(_i: int) -> void:
		_url.text = ""
		_model.text = ""
		_effort.text = ""
		_key.text = ""
		_show_provider())
	_field(form, "llm", _provider)
	_model_list = LOCAL_MODELS + own_models(model_dir)
	for i in _model_list.size():
		_local_model.add_item(_model_label(_model_list[i]), i)
		if i < LOCAL_MODELS.size():
			_local_model.set_item_disabled(i, not fits_ram(i, OS.get_memory_info()["physical"]))
	var load_model := Button.new()
	load_model.text = _texts["model_import"]
	load_model.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	load_model.pressed.connect(_pick_file.bind("*.gguf;GGUF", _import_model))
	var model_box := VBoxContainer.new()
	model_box.add_child(_local_model)
	model_box.add_child(load_model)
	_local_model_field = _field(form, "local_model", model_box)
	_remote.append(_field(form, "url", _url))
	_remote.append(_field(form, "model", _model))
	_remote.append(_field(form, "effort", _effort))
	_key.secret = true
	_remote.append(_field(form, "key", _key))
	_fallback.text = _texts["fallback"]
	_fallback.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_fallback.toggled.connect(func(_on: bool) -> void: _show_provider())
	form.add_child(_fallback)
	_remote.append(_fallback)
	_notice.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	form.add_child(_notice)

	var buttons := HBoxContainer.new()
	buttons.add_theme_constant_override("separation", 16)
	form.add_child(buttons)
	var save := Button.new()
	save.text = _texts["save"]
	save.pressed.connect(_save)
	buttons.add_child(save)
	var cancel := Button.new()
	cancel.text = _texts["cancel"]
	cancel.pressed.connect(queue_free)
	buttons.add_child(cancel)


func _model_label(m: String) -> String:
	var i := LOCAL_MODELS.find(m)
	return _texts["local_models"][i] if i >= 0 else _texts["own_model"] % m.get_file().get_basename()


## Copies a GGUF file into the own models' folder on a thread; the copy is selected once it is complete.
func _import_model(from: String) -> void:
	if _copy != null:
		return
	var source := FileAccess.open(from, FileAccess.READ)
	if source == null or source.get_length() < 4 or source.get_buffer(4).get_string_from_ascii() != "GGUF":
		_notice.text = _texts["model_bad"] % from.get_file()
		return
	var dir := _model_dir.path_join(OWN_MODEL_DIR)
	DirAccess.make_dir_recursive_absolute(dir)
	var access := DirAccess.open(dir)
	var free := access.get_space_left() if access != null else 0
	var size := source.get_length()
	var needed := size + SPARE_BYTES
	source.close()
	if free < needed:
		_notice.text = _texts["no_room"] % [gigabytes(free, _lang), gigabytes(needed, _lang)]
		return
	var stem := from.get_file().get_basename().validate_filename()
	stem = stem if not stem.is_empty() else "model"
	var free_name := stem
	var n := 2
	while FileAccess.file_exists(dir.path_join(free_name + ".gguf")):
		free_name = "%s_%d" % [stem, n]
		n += 1
	_notice.text = _texts["model_copying"]
	_copy = Thread.new()
	_copy.start(_copy_model.bind(from, dir.path_join(free_name + ".gguf"), size))


## Runs on `_copy`: through a `.part` file, so a broken copy never looks like a model. The copy must still start
## like a GGUF file and hold the `size` bytes checked on import.
func _copy_model(from: String, to: String, size: int) -> void:
	var part := to.get_basename() + ".part"
	var source := FileAccess.open(from, FileAccess.READ)
	var target := FileAccess.open(part, FileAccess.WRITE)
	var ok := source != null and target != null
	var copied := 0
	while ok and not _stop_copy and source.get_position() < source.get_length():
		var chunk := source.get_buffer(COPY_CHUNK)
		if copied == 0 and (chunk.size() < 4 or chunk.slice(0, 4).get_string_from_ascii() != "GGUF"):
			ok = false
			break
		target.store_buffer(chunk)
		copied += chunk.size()
		ok = source.get_error() in [OK, ERR_FILE_EOF] and target.get_error() == OK
	if target != null:
		target.close()
	ok = ok and not _stop_copy and copied == size and DirAccess.rename_absolute(part, to) == OK
	if not ok:
		DirAccess.remove_absolute(part)
	_model_copied.call_deferred(from.get_file(), OWN_MODEL_DIR.path_join(to.get_file()), ok)


func _model_copied(from: String, added: String, ok: bool) -> void:
	_copy.wait_to_finish()
	_copy = null
	if not ok:
		_notice.text = _texts["model_bad"] % from
		return
	_notice.text = ""
	_model_list.append(added)
	_local_model.add_item(_model_label(added))
	_local_model.select(_model_list.size() - 1)


## A copy still running when the form closes is stopped and its `.part` file removed.
func _exit_tree() -> void:
	if _copy != null:
		_stop_copy = true
		_copy.wait_to_finish()


## A voice of one's own that exists in only one language names it: in the other, Pal uses the default voice.
func _voice_label(index: int) -> String:
	if index < VOICE_NAMES.size():
		return VOICE_NAMES[index]
	var v: String = _voice_list[index]
	return v.capitalize() + _only_in(v, voices)


## " (DE)" for an item that `lists` (model dir, language) has in only one language, else "".
func _only_in(item: String, lists: Callable) -> String:
	var langs := LANGUAGES.filter(func(l: String) -> bool: return item in lists.call(_model_dir, l))
	return "" if langs.size() != 1 else " (%s)" % langs[0].to_upper()


## A label above `input`; returns both as one box, so they can be hidden together.
func _field(form: VBoxContainer, key: String, input: Control) -> Control:
	var box := VBoxContainer.new()
	var label := Label.new()
	label.text = _texts[key]
	box.add_child(label)
	box.add_child(input)
	form.add_child(box)
	return box


## The select box, the built-in character first, with its two buttons.
func _persona_input() -> Control:
	_persona_list = all_personas(_model_dir)
	_persona.add_item(_texts["persona_default"])
	for persona_name in _persona_list.slice(1):
		_persona.add_item(persona_name + _only_in(persona_name, personas))
	var load_file := Button.new()
	load_file.text = _texts["persona_import"]
	load_file.pressed.connect(_pick_file.bind("*.txt;Text;text/plain", _import_persona))
	var template := Button.new()
	template.text = _texts["persona_template"]
	template.pressed.connect(_write_template)
	var buttons := HBoxContainer.new()
	buttons.add_child(load_file)
	buttons.add_child(template)
	var box := VBoxContainer.new()
	box.add_child(_persona)
	box.add_child(buttons)
	return box


## Calls `on_file` with the path of a file the user picks.
func _pick_file(filter: String, on_file: Callable) -> void:
	var dialog := FileDialog.new()
	dialog.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	dialog.access = FileDialog.ACCESS_FILESYSTEM
	dialog.filters = [filter]
	dialog.use_native_dialog = DisplayServer.has_feature(DisplayServer.FEATURE_NATIVE_DIALOG_FILE)
	dialog.file_selected.connect(func(from: String) -> void:
		on_file.call(from)
		dialog.queue_free())
	dialog.canceled.connect(dialog.queue_free)
	add_child(dialog)
	dialog.popup_centered_ratio(0.8)


## Copies a file into `dir` under a free name if `check` finds nothing wrong; returns that name or "".
func _import(from: String, dir: String, ext: String, max_bytes: int, taken: Array, check: Callable, bad: String) -> String:
	var source := FileAccess.open(from, FileAccess.READ)
	if source == null or source.get_length() > max_bytes:
		_notice.text = _texts[bad] % from.get_file()
		return ""
	var stem := from.get_file().get_basename().validate_filename()
	stem = stem if not stem.is_empty() else "custom"
	DirAccess.make_dir_recursive_absolute(dir)
	var free_name := stem
	var n := 2
	while free_name in taken or FileAccess.file_exists(dir.path_join(free_name + ext)):
		free_name = "%s_%d" % [stem, n]
		n += 1
	var to := dir.path_join(free_name + ext)
	var target := FileAccess.open(to, FileAccess.WRITE)
	if target == null:
		_notice.text = _texts[bad] % from.get_file()
		return ""
	target.store_buffer(source.get_buffer(source.get_length()))
	target.close()
	var wrong: String = check.call(to)
	if not wrong.is_empty():
		DirAccess.remove_absolute(to)
		_notice.text = _texts[bad] % wrong
		return ""
	_notice.text = ""
	return free_name


## Into the current language; a file of the same name loaded in the other language completes that character.
func _import_persona(from: String) -> void:
	var check := func(path: String) -> String: return PalNode.persona_error(path, _lang)
	var added := _import(from, _persona_dir(_model_dir, _lang), ".txt", MAX_PERSONA_BYTES, [], check, "persona_bad")
	if not added.is_empty():
		_select_persona(added)


## Asked once, before the first voice of one's own is loaded.
func _ask_voice_consent() -> void:
	if _cfg.get_value("voice", "consent", false):
		_pick_voice()
		return
	var dialog := ConfirmationDialog.new()
	dialog.title = _texts["consent_title"]
	dialog.dialog_text = _texts["consent_text"]
	dialog.dialog_autowrap = true
	dialog.ok_button_text = _texts["consent_yes"]
	dialog.cancel_button_text = _texts["cancel"]
	dialog.confirmed.connect(func() -> void:
		_cfg.set_value("voice", "consent", true)
		_cfg.save(_path)
		dialog.queue_free()
		_pick_voice())
	dialog.canceled.connect(dialog.queue_free)
	add_child(dialog)
	dialog.popup_centered(Vector2i(640, 0))


func _pick_voice() -> void:
	_pick_file("*.safetensors;Voice", _import_voice)


## Into the current language; a file of the same name loaded in the other language completes that voice.
func _import_voice(from: String) -> void:
	var dir := voice_path(_model_dir, _lang, "x").get_base_dir()
	var check := func(path: String) -> String: return PalNode.voice_error(path)
	var added := _import(from, dir, ".safetensors", MAX_VOICE_BYTES, VOICES, check, "voice_bad")
	if added.is_empty():
		return
	if added not in _voice_list:
		_voice_list.append(added)
		_voice.add_item(added)
	var index := _voice_list.find(added)
	_voice.set_item_text(index, _voice_label(index))
	_voice.select(index)


## Writes the built-in character as files to edit, one per language, keeping those already there.
func _write_template() -> void:
	var stem: String = TEMPLATE_NAMES[_lang]
	for lang in LANGUAGES:
		var dir := _persona_dir(_model_dir, lang)
		DirAccess.make_dir_recursive_absolute(dir)
		var to := dir.path_join(stem + ".txt")
		if FileAccess.file_exists(to):
			continue
		var file := FileAccess.open(to, FileAccess.WRITE)
		if file == null:
			_notice.text = _texts["persona_bad"] % to
			return
		file.store_string(PalNode.persona_template(lang))
		file.close()
	_notice.text = _texts["persona_written"] % _persona_dir(_model_dir, _lang).path_join(stem + ".txt")
	_select_persona(stem)


func _select_persona(persona_name: String) -> void:
	if persona_name not in _persona_list:
		_persona_list.append(persona_name)
		_persona.add_item(persona_name)
	var index := _persona_list.find(persona_name)
	_persona.set_item_text(index, persona_name + _only_in(persona_name, personas))
	_persona.select(index)


## Shows the form with the saved values.
func open() -> void:
	_name.text = _cfg.get_value("user", "name", "")
	_persona.select(maxi(_persona_list.find(saved_persona(_cfg, _lang)), 0))
	var saved: String = _cfg.get_value("voice", "name", _cfg.get_value("voice", _lang, ""))
	_voice.select(_voice_list.find(saved if saved in _voice_list else voice(_cfg, _lang, _model_dir)))
	_voice_source.select(maxi(VOICE_SOURCES.find(voice_source(_cfg)), 0))
	_show_voice_source()
	_fish_key.text = _cfg.get_value("fish", "api_key", "")
	_fish_voice.text = _cfg.get_value("fish", "voice_id", "")
	_fish_model.text = _cfg.get_value("fish", "model", "")
	_provider.select(maxi(PROVIDERS.find(provider(_cfg)), 0))
	_url.text = url(_cfg)
	_model.text = _cfg.get_value("llm", "model", "")
	_effort.text = _cfg.get_value("llm", "effort", "")
	_key.text = _cfg.get_value("llm", "api_key", "")
	_fallback.button_pressed = _cfg.get_value("llm", "fallback", false)
	_local_model.select(maxi(_model_list.find(local_model(_cfg)), 0))
	_show_provider()
	visible = true


func _show_voice_source() -> void:
	var fish: bool = VOICE_SOURCES[_voice_source.selected] == "fish"
	for c in _local_fields:
		c.visible = not fish
	for c in _fish_fields:
		c.visible = fish


func _show_provider() -> void:
	var p: String = PROVIDERS[_provider.selected]
	for c in _remote:
		c.visible = p != "local"
	_local_model_field.visible = _uses_local() and OS.get_environment("PAL_LLM").is_empty()
	_url.placeholder_text = DEFAULT_URLS.get(p, "")
	_model.placeholder_text = DEFAULT_MODELS.get(p, "")
	_effort.placeholder_text = DEFAULT_EFFORTS.get(p, "")


func _uses_local() -> bool:
	return PROVIDERS[_provider.selected] == "local" or _fallback.button_pressed


## Missing models need room for the download; says so in the form if there is too little.
func _has_room(index: int) -> bool:
	var dir := DirAccess.open(_model_dir)
	if dir == null or index >= LOCAL_MODELS.size() or FileAccess.file_exists(_model_dir.path_join(LOCAL_MODELS[index])):
		return true
	var needed: int = LOCAL_MODEL_BYTES[index] + SPARE_BYTES
	var free := dir.get_space_left()
	if free >= needed:
		return true
	_notice.text = _texts["no_room"] % [gigabytes(free, _lang), gigabytes(needed, _lang)]
	return false


## A new local model is on trial until Pal has started with it; `main.gd` goes back to `trying_from` if it doesn't.
func _choose_local_model(chosen: String) -> void:
	var before := local_model(_cfg)
	var from: String = _cfg.get_value("llm", "trying_from", "")
	if chosen != before and from.is_empty():
		from = before
	if from == chosen:
		from = ""
	_cfg.set_value("llm", "trying_from", from)
	_cfg.set_value("llm", "local_model", chosen)


## The saved prompt states belong to one model and character.
func _changes_the_cache(chosen: String) -> bool:
	if chosen != local_model(_cfg):
		return true
	return LANGUAGES.any(func(lang: String) -> bool:
		return saved_persona(_cfg, lang) != _persona_list[_persona.selected])


func _save() -> void:
	if _copy != null:
		return
	var chosen: String = _model_list[_local_model.selected]
	if _uses_local() and OS.get_environment("PAL_LLM").is_empty() and not _has_room(_local_model.selected):
		return
	if _changes_the_cache(chosen):
		_cfg.set_value("llm", "cache_stale", true)
	_choose_local_model(chosen)
	_cfg.set_value("user", "name", _name.text.strip_edges())
	_cfg.set_value("voice", "name", _voice_list[_voice.selected])
	_cfg.set_value("persona", "name", _persona_list[_persona.selected])
	for lang in LANGUAGES:
		for section in ["voice", "persona"]:
			if _cfg.has_section_key(section, lang):
				_cfg.erase_section_key(section, lang)
	_cfg.set_value("voice", "source", VOICE_SOURCES[_voice_source.selected])
	_cfg.set_value("fish", "api_key", _fish_key.text.strip_edges())
	_cfg.set_value("fish", "voice_id", _fish_voice.text.strip_edges())
	_cfg.set_value("fish", "model", _fish_model.text.strip_edges())
	_cfg.set_value("llm", "provider", PROVIDERS[_provider.selected])
	_cfg.set_value("llm", "url", _url.text.strip_edges())
	_cfg.set_value("llm", "model", _model.text.strip_edges())
	_cfg.set_value("llm", "effort", _effort.text.strip_edges())
	_cfg.set_value("llm", "api_key", _key.text.strip_edges())
	_cfg.set_value("llm", "fallback", _fallback.button_pressed)
	_cfg.save(_path)
	saved.emit()
	queue_free()
