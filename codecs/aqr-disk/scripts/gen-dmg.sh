#!/bin/sh
# tests/data: a disk of an HFS+ volume (hdiutil create, raw: disk.dmg) and its UDIF images, as
# hdiutil converts it (macOS), zstd -19: dmg-udro.dmg (raw chunks), dmg-udzo.dmg (zlib),
# dmg-udbz.dmg (bzip2), dmg-ulfo.dmg (LZFSE). The files: photo.jpg (aqr-jpeg's gradient.jpg),
# text.txt (a line, 20000 times) and data.bin (512 KiB of a pattern).
set -eu
crate="$(cd "$(dirname "$0")/.." && pwd)"
out="$crate/tests/data"
tmp="$(mktemp -d)"
trap 'hdiutil detach -quiet "$tmp/m" 2>/dev/null || true; rm -rf "${tmp:?}"' EXIT
hdiutil create -size 8m -fs HFS+ -volname D -o "$tmp/disk" >/dev/null 2>&1
hdiutil attach -nobrowse -mountpoint "$tmp/m" "$tmp/disk.dmg" >/dev/null 2>&1
cp "$crate/../aqr-jpeg/tests/data/gradient.jpg" "$tmp/m/photo.jpg"
python3 -I - "$tmp/m" <<'PY'
import os, sys
m = sys.argv[1]
open(os.path.join(m, 'text.txt'), 'wb').write(b'some text of the dmg\n' * 20000)
open(os.path.join(m, 'data.bin'), 'wb').write(bytes((i * 7 + (i >> 8)) % 251 for i in range(512 << 10)))
PY
hdiutil detach -quiet "$tmp/m"
zstd -q -19 -f "$tmp/disk.dmg" -o "$out/dmg-disk.img.zst"
for f in UDRO UDZO UDBZ ULFO; do
    lower=$(echo "$f" | tr A-Z a-z)
    hdiutil convert "$tmp/disk.dmg" -format "$f" -o "$tmp/$lower" >/dev/null 2>&1
    zstd -q -19 -f "$tmp/$lower.dmg" -o "$out/dmg-$lower.dmg.zst"
done
