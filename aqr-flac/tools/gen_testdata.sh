#!/bin/bash
# Regenerates the small test files in tests/data (synthetic signals, various formats).
set -eu
out=${1:-tests/data}
mkdir -p "$out"
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
syn() { sox -n -r "$1" -c "$2" -b "$3" "$4" synth 0.6 sine 220-880 pinknoise sine 3000 vol 0.6 remix - fade 0.1 0.6 0.1 "${@:5}"; }
# Generic sources.
syn 44100 2 16 "$t/s16.wav"
syn 96000 2 24 "$t/s24.wav"
syn 8000 1 8 "$t/u8m.wav"
sox -n -r 48000 -c 6 -b 16 "$t/c6.wav" synth 0.4 sine 100 sine 200 sine 300 pinknoise sine 500 sine 600
sox "$t/s24.wav" -b 32 "$t/s32.wav"
# FLAC, various encoders and settings.
flac -s -f -5 "$t/s16.wav" -o "$out/s16_f5.flac"
flac -s -f -0 --no-padding "$t/s16.wav" -o "$out/s16_f0.flac"
flac -s -f -8 -b 1152 "$t/s24.wav" -o "$out/s24_f8.flac"
flac -s -f -5 "$t/u8m.wav" -o "$out/u8_mono.flac"
flac -s -f -5 "$t/c6.wav" -o "$out/c6.flac"
flac -s -f -5 "$t/s32.wav" -o "$out/s32.flac"
ffmpeg -v error -y -i "$t/s16.wav" -c:a flac -compression_level 12 "$out/s16_ffmpeg.flac"
flac -s -f -5 --lax -b 16384 -l 32 "$t/s16.wav" -o "$out/s16_lax.flac"
# Silence and a constant offset (constant / verbatim subframes).
sox -n -r 44100 -c 2 -b 16 "$t/sil.wav" trim 0 0.5
flac -s -f -5 "$t/sil.wav" -o "$out/silence.flac"
# ID3v2 in front, an ID3v1 tag behind, junk in the middle, a cut end.
{ printf 'ID3\x03\x00\x00\x00\x00\x00\x20'; head -c 32 /dev/zero; cat "$out/s16_f5.flac"; printf 'TAG'; head -c 125 /dev/zero; } > "$out/s16_tags.flac"
python3 - "$out/s16_f0.flac" "$out/s16_junk.flac" "$out/s16_cut.flac" <<'PY'
import sys
d = open(sys.argv[1], 'rb').read()
n = len(d)
open(sys.argv[2], 'wb').write(d[:n // 2] + b'\xff\xf8junk' * 7 + d[n // 2:])
open(sys.argv[3], 'wb').write(d[:n * 2 // 3])
PY
# PCM.
cp "$t/s16.wav" "$out/s16.wav"
cp "$t/u8m.wav" "$out/u8_mono.wav"
sox "$t/s24.wav" -b 24 "$out/s24.aiff"
ffmpeg -v error -y -i "$t/s16.wav" -c:a pcm_s16le -f aiff "$out/s16_sowt.aifc"
ffmpeg -v error -y -i "$t/s24.wav" -c:a pcm_s24le -rf64 never -write_bext 0 -f wav "$out/s24_ext.wav"
sox "$t/c6.wav" "$out/c6.wav"
