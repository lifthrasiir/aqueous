#!/bin/sh
# Builds a small, varied training corpus from files found on this machine (macOS paths
# with fallbacks) into corpus/, each truncated to MAX bytes.
set -eu
MAX=${MAX:-131072}
ZOO=${ZOO:-../../flate-zoo}
OUT=${1:-corpus}
mkdir -p "$OUT"
n=0
add() {
    for f in "$@"; do
        [ -f "$f" ] || continue
        n=$((n + 1))
        name=$(printf '%02d-%s' "$n" "$(basename "$f")")
        head -c "$MAX" "$f" > "$OUT/$name"
    done
}
add "$ZOO/vendor/zlib/deflate.c" "$ZOO/vendor/zlib/ChangeLog" "$ZOO/vendor/libdeflate/lib/deflate_compress.c"
add "$ZOO/vendor/js-pako/dist/pako.js" "$ZOO/vendor/zopfli/src/zopfli/squeeze.c"
add /usr/share/dict/words /usr/share/dict/propernames
add /bin/bash /usr/bin/perl5.34 /bin/ls
add /System/Library/Sounds/Submarine.aiff /System/Library/Sounds/Ping.aiff
add /System/Library/CoreServices/SystemVersion.plist
add "$(find /System/Library/Frameworks/AppKit.framework -name '*.tiff' -size +40k 2>/dev/null | head -1)"
add "$(find /usr/share/man -name '*.1' -size +40k 2>/dev/null | head -1)"
add "$(find /System/Library -name '*.strings' -size +40k 2>/dev/null | head -1)"
add "$(find /System/Library/Fonts -name '*.ttf' -size +100k 2>/dev/null | head -1)"
add "$(find /System/Library -name '*.json' -size +60k 2>/dev/null | head -1)"
# Synthetic: noise, a periodic pattern, and sparse data.
head -c 65536 /dev/urandom > "$OUT/90-random"
perl -e 'print "abcabcabd" x 8000' > "$OUT/91-periodic"
perl -e 'srand(1); print map { rand() < 0.05 ? chr(int rand 256) : "\0" } 1..100000' > "$OUT/92-sparse"
ls -la "$OUT"
