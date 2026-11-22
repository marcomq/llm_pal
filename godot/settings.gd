class_name Settings
extends PanelContainer
## The settings form: the user's name, Pal's voice per language and where replies come from. Saves to the settings
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
const DEFAULT_MODELS := {"ollama": "llama3.2", "openai": "gpt-5-mini", "anthropic": "claude-opus-5-5"}
const VOICE_SOURCES := ["local", "fish"]
const DEFAULT_FISH_MODEL := "s2.1-pro-free"
const TEXTS := {
	"de": {"title": "Einstellungen", "name": "Dein Name", "voice_de": "Stimme Deutsch",
		"voice_en": "Stimme Englisch", "voice": "Stimme", "voice_local": "Lokal",
		"voice_fish": "Fish Audio (online)", "fish_voice": "Stimmen-ID",
		"llm": "Sprachmodell", "url": "Server-Adresse",
		"model": "Modell", "key": "API-Schlüssel", "fallback": "Bei Serverfehler lokales llama nutzen",
		"save": "Speichern", "cancel": "Abbrechen"},
	"en": {"title": "Settings", "name": "Your name", "voice_de": "German voice", "voice_en": "English voice",
		"voice": "Voice", "voice_local": "Local", "voice_fish": "Fish Audio (online)",
		"fish_voice": "Voice ID", "llm": "Language model", "url": "Server address",
		"model": "Model", "key": "API key", "fallback": "Use local llama when the server fails",
		"save": "Save", "cancel": "Cancel"},
}

var _cfg: ConfigFile
var _path: String
var _texts: Dictionary
var _name := LineEdit.new()
var _voices := {"de": OptionButton.new(), "en": OptionButton.new()}
var _voice_lists := {}
var _voice_source := OptionButton.new()
var _local_fields: Array[Control] = []
var _fish_fields: Array[Control] = []
var _fish_key := LineEdit.new()
var _fish_voice := LineEdit.new()
var _fish_model := LineEdit.new()
var _provider := OptionButton.new()
var _url := LineEdit.new()
var _model := LineEdit.new()
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


## The first voice of one's own in `list`, or else the first catalog voice.
static func default_voice(list: Array) -> String:
	return list[VOICES.size()] if list.size() > VOICES.size() else VOICES[0]


## The saved voice for `lang`, or the default if none or an unknown one is saved.
static func voice(cfg: ConfigFile, lang: String, model_dir: String) -> String:
	var list := voices(model_dir, lang)
	var v: String = cfg.get_value("voice", lang, "")
	return v if v in list else default_voice(list)


static func voice_source(cfg: ConfigFile) -> String:
	return cfg.get_value("voice", "source", "local")


static func url(cfg: ConfigFile) -> String:
	return cfg.get_value("llm", "url", "")


static func model(cfg: ConfigFile) -> String:
	var m: String = cfg.get_value("llm", "model", "")
	return m if not m.is_empty() else DEFAULT_MODELS.get(provider(cfg), "")


static func fish_model(cfg: ConfigFile) -> String:
	var m: String = cfg.get_value("fish", "model", "")
	return m if not m.is_empty() else DEFAULT_FISH_MODEL


func _init(cfg: ConfigFile, path: String, lang: String, model_dir: String) -> void:
	_cfg = cfg
	for voice_lang in LANGUAGES:
		_voice_lists[voice_lang] = voices(model_dir, voice_lang)
	_path = path
	_texts = TEXTS[lang]
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
	margin.add_child(form)

	var title := Label.new()
	title.text = _texts["title"]
	title.add_theme_font_size_override("font_size", 36)
	form.add_child(title)
	_field(form, "name", _name)
	_name.placeholder_text = DEFAULT_NAME
	for i in VOICE_SOURCES.size():
		_voice_source.add_item(_texts["voice_" + VOICE_SOURCES[i]], i)
	_voice_source.item_selected.connect(func(_i: int) -> void: _show_voice_source())
	_field(form, "voice", _voice_source)
	for voice_lang in LANGUAGES:
		var list: Array = _voice_lists[voice_lang]
		for i in list.size():
			_voices[voice_lang].add_item(VOICE_NAMES[i] if i < VOICE_NAMES.size() else list[i].capitalize(), i)
		_local_fields.append(_field(form, "voice_" + voice_lang, _voices[voice_lang]))
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
		_key.text = ""
		_show_provider())
	_field(form, "llm", _provider)
	_remote.append(_field(form, "url", _url))
	_remote.append(_field(form, "model", _model))
	_key.secret = true
	_remote.append(_field(form, "key", _key))
	_fallback.text = _texts["fallback"]
	_fallback.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	form.add_child(_fallback)
	_remote.append(_fallback)

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


## A label above `input`; returns both as one box, so they can be hidden together.
func _field(form: VBoxContainer, key: String, input: Control) -> Control:
	var box := VBoxContainer.new()
	var label := Label.new()
	label.text = _texts[key]
	box.add_child(label)
	box.add_child(input)
	form.add_child(box)
	return box


## Shows the form with the saved values.
func open() -> void:
	_name.text = _cfg.get_value("user", "name", "")
	for lang in LANGUAGES:
		var list: Array = _voice_lists[lang]
		var v: String = _cfg.get_value("voice", lang, "")
		_voices[lang].select(list.find(v if v in list else default_voice(list)))
	_voice_source.select(maxi(VOICE_SOURCES.find(voice_source(_cfg)), 0))
	_show_voice_source()
	_fish_key.text = _cfg.get_value("fish", "api_key", "")
	_fish_voice.text = _cfg.get_value("fish", "voice_id", "")
	_fish_model.text = _cfg.get_value("fish", "model", "")
	_provider.select(maxi(PROVIDERS.find(provider(_cfg)), 0))
	_url.text = url(_cfg)
	_model.text = _cfg.get_value("llm", "model", "")
	_key.text = _cfg.get_value("llm", "api_key", "")
	_fallback.button_pressed = _cfg.get_value("llm", "fallback", false)
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
	_url.placeholder_text = DEFAULT_URLS.get(p, "")
	_model.placeholder_text = DEFAULT_MODELS.get(p, "")


func _save() -> void:
	_cfg.set_value("user", "name", _name.text.strip_edges())
	for lang in LANGUAGES:
		_cfg.set_value("voice", lang, _voice_lists[lang][_voices[lang].selected])
	_cfg.set_value("voice", "source", VOICE_SOURCES[_voice_source.selected])
	_cfg.set_value("fish", "api_key", _fish_key.text.strip_edges())
	_cfg.set_value("fish", "voice_id", _fish_voice.text.strip_edges())
	_cfg.set_value("fish", "model", _fish_model.text.strip_edges())
	_cfg.set_value("llm", "provider", PROVIDERS[_provider.selected])
	_cfg.set_value("llm", "url", _url.text.strip_edges())
	_cfg.set_value("llm", "model", _model.text.strip_edges())
	_cfg.set_value("llm", "api_key", _key.text.strip_edges())
	_cfg.set_value("llm", "fallback", _fallback.button_pressed)
	_cfg.save(_path)
	saved.emit()
	queue_free()
