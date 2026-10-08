#!/bin/sh
# Per-file sizes under generic compressors, cached in cache/baseline.tsv (path, size, mtime are the key).
# usage: scripts/baseline.sh FILE...   -> prints totals per compressor
set -e
cd "$(dirname "$0")/.."
cache=cache/baseline.tsv
mkdir -p cache
touch "$cache"
for f in "$@"; do
    key="$f	$(stat -f '%z	%m' "$f")"
    if ! grep -qF "$key	" "$cache"; then
        z3=$(zstd -3 -c --single-thread "$f" | wc -c)
        z19=$(zstd -19 --long=27 -c --single-thread "$f" | wc -c)
        x9=$(xz -9 -T1 -c "$f" | wc -c)
        printf '%s\t%d\t%d\t%d\n' "$key" $z3 $z19 $x9 >> "$cache"
    fi
    grep -F "$key	" "$cache" | tail -1
done | awk -F'\t' '{n+=$2; a+=$4; b+=$5; c+=$6} END {printf "%d files %d bytes: zstd-3 %.4f  zstd-19 %.4f  xz-9 %.4f\n", NR, n, a/n, b/n, c/n}'
