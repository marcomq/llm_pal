class_name Settings
extends PanelContainer
## The settings form: the user's name, Pal's voice per language and where replies come from. Saves to the settings
## file, emits `saved` and closes; a new one is made for each opening, in the current language.

signal saved

const PROVIDERS := ["local", "ollama", "openai", "anthropic"]
const PROVIDER_NAMES := ["llama (lokal)", "Ollama", "OpenAI", "Anthropic"]
const DEFAULT_NAME := "Michael"
## Pocket TTS catalog voices; `build.sh push-models` copies the same list to the phone. The first is the default.
const VOICES := ["juergen", "stuart_bell", "michael"]
const VOICE_NAMES := ["Jürgen", "Stuart Bell", "Michael"]
const LANGUAGES := ["de", "en"]
## Shown as placeholders and used while a field is empty.
const DEFAULT_URLS := {
	"ollama": "http://localhost:11434/v1", "openai": "https://api.openai.com/v1",
	"anthropic": "https://api.anthropic.com/v1",
}
const DEFAULT_MODELS := {"ollama": "llama3.2", "openai": "gpt-5-mini", "anthropic": "claude-opus-5-5"}
const TEXTS := {
	"de": {"title": "Einstellungen", "name": "Dein Name", "voice_de": "Stimme Deutsch",
		"voice_en": "Stimme Englisch", "llm": "Sprachmodell", "url": "Server-Adresse",
		"model": "Modell", "key": "API-Schlüssel", "fallback": "Bei Serverfehler lokales llama nutzen",
		"save": "Speichern", "cancel": "Abbrechen"},
	"en": {"title": "Settings", "name": "Your name", "voice_de": "German voice", "voice_en": "English voice",
		"llm": "Language model", "url": "Server address",
		"model": "Model", "key": "API key", "fallback": "Use local llama when the server fails",
		"save": "Save", "cancel": "Cancel"},
}

var _cfg: ConfigFile
var _path: String
var _texts: Dictionary
var _name := LineEdit.new()
var _voices := {"de": OptionButton.new(), "en": OptionButton.new()}
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


## The saved voice for `lang`, or the default if none or an unknown one is saved.
static func voice(cfg: ConfigFile, lang: String) -> String:
	var v: String = cfg.get_value("voice", lang, VOICES[0])
	return v if v in VOICES else VOICES[0]


static func url(cfg: ConfigFile) -> String:
	return cfg.get_value("llm", "url", "")


static func model(cfg: ConfigFile) -> String:
	var m: String = cfg.get_value("llm", "model", "")
	return m if not m.is_empty() else DEFAULT_MODELS.get(provider(cfg), "")


func _init(cfg: ConfigFile, path: String, lang: String) -> void:
	_cfg = cfg
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
	for voice_lang in LANGUAGES:
		for i in VOICES.size():
			_voices[voice_lang].add_item(VOICE_NAMES[i], i)
		_field(form, "voice_" + voice_lang, _voices[voice_lang])
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
		_voices[lang].select(VOICES.find(voice(_cfg, lang)))
	_provider.select(maxi(PROVIDERS.find(provider(_cfg)), 0))
	_url.text = url(_cfg)
	_model.text = _cfg.get_value("llm", "model", "")
	_key.text = _cfg.get_value("llm", "api_key", "")
	_fallback.button_pressed = _cfg.get_value("llm", "fallback", false)
	_show_provider()
	visible = true


func _show_provider() -> void:
	var p: String = PROVIDERS[_provider.selected]
	for c in _remote:
		c.visible = p != "local"
	_url.placeholder_text = DEFAULT_URLS.get(p, "")
	_model.placeholder_text = DEFAULT_MODELS.get(p, "")


func _save() -> void:
	_cfg.set_value("user", "name", _name.text.strip_edges())
	for lang in LANGUAGES:
		_cfg.set_value("voice", lang, VOICES[_voices[lang].selected])
	_cfg.set_value("llm", "provider", PROVIDERS[_provider.selected])
	_cfg.set_value("llm", "url", _url.text.strip_edges())
	_cfg.set_value("llm", "model", _model.text.strip_edges())
	_cfg.set_value("llm", "api_key", _key.text.strip_edges())
	_cfg.set_value("llm", "fallback", _fallback.button_pressed)
	_cfg.save(_path)
	saved.emit()
	queue_free()
