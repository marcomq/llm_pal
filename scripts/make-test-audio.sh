#!/bin/sh
# macOS: German test speech with long pauses, at 48 kHz so the resampler is exercised.
set -eu
cd "$(dirname "$0")/.."
mkdir -p testdata
say -v Anna -o testdata/de.aiff \
    "Hallo, ich teste den Audio Prozessor. [[slnc 1500]] Das ist der zweite Satz, ganz ruhig gesprochen. [[slnc 1500]] Hörst du mich jetzt?"
afconvert -f WAVE -d LEI16@48000 testdata/de.aiff testdata/de_48k.wav
rm testdata/de.aiff
echo "ok: testdata/de_48k.wav"
