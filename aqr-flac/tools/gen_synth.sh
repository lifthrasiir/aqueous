#!/bin/bash
# Synthetic FLAC / WAV corpus from MP3s: tools/gen_synth.sh OUTDIR INDEX MP3
# The encoder and format are chosen by INDEX % 12. Run e.g. with xargs -P.
set -eu
out=$1; i=$2; f=$3
mkdir -p "$out"
tmp="$out/.tmp.$$.wav"
trap "rm -f $tmp" EXIT
b=$(basename "$f" .mp3)
case $((i % 12)) in
  0) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a pcm_s16le "$out/$b.wav"; flac -s -f -5 "$out/$b.wav" -o "$out/$b.f5.flac" ;;
  1) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a pcm_s16le $tmp; flac -s -f -8 $tmp -o "$out/$b.f8.flac" ;;
  2) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a pcm_s16le $tmp; flac -s -f -0 $tmp -o "$out/$b.f0.flac" ;;
  3) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a flac "$out/$b.ff.flac" ;;
  4) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a flac -compression_level 12 "$out/$b.ff12.flac" ;;
  5) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a pcm_s24le $tmp; flac -s -f -5 $tmp -o "$out/$b.s24.flac" ;;
  6) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -ac 1 -c:a pcm_s16le $tmp; flac -s -f -5 $tmp -o "$out/$b.mono.flac" ;;
  7) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -ar 48000 -c:a pcm_s16le $tmp; flac -s -f -3 -b 1152 $tmp -o "$out/$b.b1152.flac" ;;
  8) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a pcm_s16le $tmp; flac -s -f -8 -p -e -b 8192 --lax $tmp -o "$out/$b.f8pe.flac" ;;
  9) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -ar 96000 -c:a pcm_s24le "$out/$b.s24.wav"; flac -s -f -8 "$out/$b.s24.wav" -o "$out/$b.h96.flac" ;;
  10) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a pcm_s16le $tmp; flac -s -f -6 --no-mid-side $tmp -o "$out/$b.noms.flac" ;;
  11) ffmpeg -v error -y -i "$f" -map 0:a -map_metadata -1 -fflags +bitexact -c:a pcm_s16le $tmp; flac -s -f -5 -A tukey\(0.5\) -l 12 -r 8 $tmp -o "$out/$b.l12.flac" ;;
esac
