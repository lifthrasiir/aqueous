#!/bin/bash
# Per-file sizes under other compressors, cached per tool in cache/base-TOOL.tsv (path, size,
# mtime are the key). Bit-exact: zstd -3, zstd -19 --long=27, xz -9, bzip2 -9, paq8px -8 (only
# with PAQ8PX set to its binary; about 1 MB/s). Not bit-exact, for reference: draco -qp 0 (no
# quantization; geometry and connectivity only, reordered) with DRACO=1.
# usage: scripts/baseline.sh FILE...   -> prints per-file sizes and totals per tool
set -e
cd "$(dirname "$0")/.."
mkdir -p cache
tools=(zstd3 zstd19 xz9 bzip2)
[ -n "${PAQ8PX:-}" ] && tools+=(paq8px)
[ -n "${DRACO:-}" ] && tools+=(draco)

size() { # TOOL FILE
    case $1 in
        zstd3) zstd -3 -c --single-thread "$2" | wc -c ;;
        zstd19) zstd -19 --long=27 -c --single-thread "$2" | wc -c ;;
        xz9) xz -9 -T1 -c "$2" | wc -c ;;
        bzip2) bzip2 -9 -c "$2" | wc -c ;;
        paq8px)
            local t; t=$(mktemp -d)
            "$PAQ8PX" -8 "$2" "$t/o" >/dev/null && wc -c < "$t/o"; rm -rf "$t" ;;
        draco)
            local t; t=$(mktemp -d)
            draco_encoder -i "$2" -o "$t/o.drc" -cl 10 -qp 0 >/dev/null 2>&1 && wc -c < "$t/o.drc" || echo 0
            rm -rf "$t" ;;
    esac
}

for f in "$@"; do
    key="$f	$(stat -f '%z	%m' "$f")"
    row="$f	$(stat -f '%z' "$f")"
    for t in "${tools[@]}"; do
        c=cache/base-$t.tsv
        touch "$c"
        if ! grep -qF "$key	" "$c"; then
            printf '%s\t%d\n' "$key" "$(size $t "$f")" >> "$c"
        fi
        row="$row	$(grep -F "$key	" "$c" | tail -1 | cut -f4)"
    done
    echo "$row"
done | awk -F'\t' -v names="${tools[*]}" '
    BEGIN { n = split(names, nm, " ") }
    { print; s += $2; for (i = 1; i <= n; i++) t[i] += $(i + 2) }
    END { printf "%d files %d bytes:", NR, s; for (i = 1; i <= n; i++) printf "  %s %.4f", nm[i], t[i] / s; print "" }'
