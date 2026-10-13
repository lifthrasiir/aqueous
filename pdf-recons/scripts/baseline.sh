#!/bin/sh
# Output sizes of generic compressors on each set, per file and summed; cached in
# cache/baseline-SET.tsv (file, size, zstd -3, zstd -19, xz -9).
set -eu
cd "$(dirname "$0")/.."
mkdir -p cache
for set in "$@"; do
    out=cache/baseline-$set.tsv
    [ -s "$out" ] && continue
    for f in corpus/$set/*; do
        printf '%s\t%s\t%s\t%s\t%s\n' "$f" "$(wc -c < "$f")" \
            "$(zstd -3 --single-thread -c < "$f" | wc -c)" \
            "$(zstd -19 --single-thread -c < "$f" | wc -c)" \
            "$(xz -9 -T1 -c < "$f" | wc -c)"
    done > "$out.part"
    mv "$out.part" "$out"
done
for set in "$@"; do
    awk -v s="$set" -F'\t' '{n+=$2; a+=$3; b+=$4; c+=$5} END {printf "%s: %d files, %d B; zstd -3 %.4f, zstd -19 %.4f, xz -9 %.4f\n", s, NR, n, a/n, b/n, c/n}' "cache/baseline-$set.tsv"
done
