# pal-voice

Makes a voice for Pal from a short speech recording. The result is a `.safetensors` file that Pal loads the same way
as its built-in voices (e.g. `juergen`). Nothing is trained: Pocket TTS reads the recording once, which takes a few
seconds.

Recordings and generated voices stay local; `.gitignore` keeps them out of the repository.

## Requirements

- The `pal-tts` crate of this repository (`../rust/pal-tts`)
- Rust, `ffmpeg`, `uv` (for `uvx`)
- Access to Kyutai's voice cloning model: read and accept the terms at https://huggingface.co/kyutai/pocket-tts,
  then log in locally:
  ```
  uvx hf auth login
  ```
  `uvx hf auth whoami` shows which account is logged in. It must be the one that accepted the terms.

## The recording

- 10–30 s, one single speaker, ideally connected sentences
- no music, no other voices, little reverb and background noise
- any format ffmpeg reads (wav, mp3, m4a, mp4 …); the script converts to 24 kHz mono
- German and English are separate voices: one recording per language, in that language

The voice takes on the timbre and the way of speaking of the recording, and also its quality: hiss, music and a
telephone sound get cloned too. For a particular style, read the sentences in exactly that style.

Several short recordings from the same source can be joined first. Don't mix recordings from different sources
(other room, other equipment): in tests, sentences then broke off early.
```
printf "file 'a.wav'\nfile 'b.wav'\nfile 'c.wav'\n" > list.txt
ffmpeg -f concat -safe 0 -i list.txt -ac 1 -ar 24000 samples/my_voice_de.wav
```

## Making a voice

```
./voice.sh <de|en> <recording> <name> [start] [length]
```

- `name`: the voice's name, shown in Pal's settings
- `start`, `length`: the part of the recording to use, in seconds (default 0 and 30)

Example:
```
./voice.sh de samples/my_voice_de.wav anna
```

The script writes `out/<name>_<language>.safetensors` and plays a probe sentence (`out/<name>_<language>.wav`).

Switches as environment variables:

| Variable | Effect |
|---|---|
| `VOCALS=1` | first strips music and noise with Demucs (downloads PyTorch the first time, takes a while) |
| `PLAY=0` | don't play the probe |
| `INSTALL=1` | copy the voice into Pal's models folder (`PAL_MODELS`, else `../models`) |
| `PUSH=1` | copy the voice to the Android device over `adb` |

## Using it in Pal

Pal looks for voices of one's own in

```
<models folder>/pocket/voices/<de|en>/<name>.safetensors
```

`INSTALL=1` and `PUSH=1` put the file there. The voice then shows up in Pal's settings (key S or "…"). On Android
this needs an APK that already supports voices of one's own.

Longer recordings make larger voice files (30 s ≈ 14 MB, `juergen` has 6 MB). On slow devices, compare the reply
latency with a shorter recording.

## Legal

In short: only clone voices you are allowed to clone, and don't pass anything on.

- **Consent:** use your own voice or that of a person who has expressly agreed. Kyutai's terms for the cloning model
  require this; a person's voice is also protected by their personality rights.
- **Third-party recordings:** don't use clips from films, series, radio plays or fan sites, or samples of voices
  from TTS services. They are protected, and the voice belongs to the speaker.
- **Sound-alikes:** a consenting speaker does not make a sound-alike safe. Imitating a recognizable person's voice or
  a protected character can still infringe their rights.
- **Don't pass it on:** recordings (`samples/`) and generated voices (`out/`, `pocket/voices/`) don't belong in a
  repository, an app package or a download.
- **No deception:** don't use a cloned voice in a way that makes others take it for the real person.

This is not legal advice. When in doubt, use only your own voice.

## Licenses

- This tool and `pal-tts`: MIT (code from kyutai-labs/pocket-tts, see `LICENSE-KYUTAI` in `pal-tts`)
- Kyutai's models and voices: terms on the Hugging Face pages of `kyutai/pocket-tts` and
  `kyutai/pocket-tts-without-voice-cloning`
- Demucs (optional): MIT
