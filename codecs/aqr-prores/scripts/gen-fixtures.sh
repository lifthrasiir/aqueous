#!/bin/sh
# Makes tests/data: small QuickTime files of ProRes frames of ffmpeg's test patterns, by
# its two encoders (prores_ks, prores_aw), with a little audio between the frames. (ffmpeg 4.2
# writes no ProRes in MXF: MXF is checked on the corpus.)
set -eu
cd "$(dirname "$0")/../tests/data"
FF=${FFMPEG:-ffmpeg}
src="-f lavfi -i testsrc2=size=96x64:rate=24:duration=0.5 -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.5"
"$FF" -v error -y $src -c:v prores_ks -profile:v 2 -pix_fmt yuv422p10le -c:a pcm_s16le ks-422.mov
"$FF" -v error -y $src -c:v prores_aw -profile:v 3 -pix_fmt yuv422p10le -c:a pcm_s16le aw-422hq.mov
"$FF" -v error -y $src -c:v prores_ks -profile:v 4 -pix_fmt yuva444p10le -an ks-4444-alpha.mov
"$FF" -v error -y $src -c:v prores_ks -profile:v 1 -pix_fmt yuv422p10le -flags +ildct+ilme -top 1 -an ks-interlaced.mov
ls -l
