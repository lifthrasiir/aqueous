#!/bin/sh
# Sizes of the given files or directories under packMP3, zstd -19 and xz -9.
# Usage: compare.sh PACKMP3_BINARY WORKDIR PATH...
# Files that packMP3 rejects count with their original size.
set -e
pack=$(cd "$(dirname "$1")" && pwd)/$(basename "$1"); work=$2; shift 2
rm -rf "$work"; mkdir -p "$work"
find "$@" -name '*.mp3' | sort > "$work/list"
total=0; pm=0; pmfail=0; zs=0; xzs=0; n=0
i=0
while read -r f; do
    i=$((i + 1)); cp "$f" "$work/$i.mp3"
done < "$work/list"
start=$(date +%s)
(cd "$work" && "$pack" -np -o *.mp3 > pack.log 2>&1 || true)
end=$(date +%s)
for f in "$work"/*.mp3; do
    s=$(stat -f %z "$f"); total=$((total + s))
    p="${f%.mp3}.pmp"
    if [ -f "$p" ]; then pm=$((pm + $(stat -f %z "$p"))); else pm=$((pm + s)); pmfail=$((pmfail + 1)); fi
    zs=$((zs + $(zstd -19 -q -c "$f" | wc -c))); xzs=$((xzs + $(xz -9 -c "$f" | wc -c)))
done
echo "input $total bytes"
echo "packMP3 $pm ($(echo "scale=4; $pm / $total" | bc)), $pmfail files rejected, $((end - start)) s"
echo "zstd -19 $zs ($(echo "scale=4; $zs / $total" | bc))"
echo "xz -9 $xzs ($(echo "scale=4; $xzs / $total" | bc))"
