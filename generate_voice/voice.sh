#!/bin/sh
# Clones a Pal voice from a recording: ./voice.sh <de|en> <recording> <name> [start] [length]
# Any format ffmpeg reads (wav, mp3, mp4, ...). start/length in seconds pick the part to use (default 0 / 30).
# VOCALS=1 first strips music and noise with Demucs. PLAY=0 skips playing the probe.
# Writes out/<name>_<lang>.safetensors and a probe WAV next to it. INSTALL=1 copies the voice into Pal's
# models folder (PAL_MODELS), PUSH=1 onto the phone over adb; Pal then offers it in its settings.
set -eu
cd "$(dirname "$0")"

[ $# -ge 3 ] || { sed -n '2,6p' "$0"; exit 2; }
lang=$1 input=$2 name=$3 start=${4:-0} length=${5:-30}
case $lang in
  de) language=german ;;
  en) language=english ;;
  *) echo "language must be de or en" >&2; exit 2 ;;
esac

work=out/work
mkdir -p "$work"
src=$input
if [ "${VOCALS:-0}" = 1 ]; then
  uvx -p 3.12 demucs --two-stems=vocals -o "$work/demucs" "$input"
  src="$work/demucs/htdemucs/$(basename "${input%.*}")/vocals.wav"
fi

sample="$work/${name}_$lang.wav"
ffmpeg -loglevel error -y -ss "$start" -t "$length" -i "$src" -ac 1 -ar 24000 "$sample"

cargo build --release --quiet
out="out/${name}_$lang.safetensors"
./target/release/pal-voice "$language" "$sample" "$out"
if [ "${PLAY:-1}" != 0 ] && command -v afplay >/dev/null; then
  afplay "${out%.safetensors}.wav" || echo "could not play the probe" >&2
fi

own="pocket/voices/$lang/$name.safetensors"
if [ "${INSTALL:-0}" = 1 ]; then
  models=${PAL_MODELS:-../models}
  mkdir -p "$(dirname "$models/$own")" && cp "$out" "$models/$own" && echo "installed -> $models/$own"
fi
if [ "${PUSH:-0}" = 1 ]; then
  phone_models=/storage/emulated/0/Android/data/io.github.marcomq.llmpal/files/models
  # A folder made by adb belongs to the shell user, and Pal could not write to it.
  if ! adb shell test -d "$phone_models/pocket"; then
    echo "start Pal once so it creates $phone_models/pocket, then push again" >&2
    exit 1
  fi
  adb push "$out" "$phone_models/$own"
fi
