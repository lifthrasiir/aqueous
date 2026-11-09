#!/bin/sh
# tests/data: HFS+ and HFSX volumes by macOS's hdiutil (raw images, UDRW: an APM of HFS+, a GPT of
# HFSX), their files written through the mounted volumes, zstd -19:
#   hfs.img (32 MiB, HFS+) and hfsx.img (32 MiB, case-sensitive HFSX): readme.txt,
#   docs/deeper/a rather long file name.json, docs/한글.txt, bin/data.bin, empty.txt, link.bin
#   (a hard link of bin/data.bin: both indirect, of the private directory), link.sym (a symbolic
#   link), fork.txt (with a resource fork), packed.txt (compressed by decmpfs, `ditto
#   --hfsCompression` from outside), many/ (400 files), photo.jpg (aqr-jpeg's gradient.jpg) and
#   frag.bin written into the gaps that deleting every other of 600 small files gap.NNN leaves
#   once fill.bin took the free space (frag.bin's extents past the 8 of its catalog record: in
#   the extents overflow file).
set -eu
crate="$(cd "$(dirname "$0")/.." && pwd)"
out="$crate/tests/data"
jpeg="$crate/../aqr-jpeg/tests/data/gradient.jpg"
tmp="$(mktemp -d)"
trap 'hdiutil detach -quiet "$tmp/mnt" 2>/dev/null || true; rm -rf "${tmp:?}"' EXIT
m="$tmp/mnt"
python3 -I -c 'import sys; open(sys.argv[1], "wb").write(b"decmpfs text, line after line\n" * 2000)' "$tmp/plain.txt"
make() { # name fs layout
    img="$tmp/$1.dmg"
    hdiutil create -size 32m -fs "$2" -layout "$3" -volname AQR -o "$img" >/dev/null 2>&1
    hdiutil attach -nobrowse -mountpoint "$m" "$img" >/dev/null 2>&1
    mkdir -p "$m/docs/deeper" "$m/bin" "$m/many"
    python3 -I - "$m" <<'PY'
import os, sys
m = sys.argv[1]
def put(p, d):
    with open(os.path.join(m, p), 'wb') as f:
        f.write(d)
put('readme.txt', b'Aqueous HFS+ fixture\n' * 40)
put('docs/deeper/a rather long file name.json', b'{"a": [1, 2, 3], "b": "c"}')
put('docs/한글.txt', '물 aqueous'.encode())
put('bin/data.bin', bytes((i * 7 + (i >> 8)) % 251 for i in range(10000)))
put('empty.txt', b'')
put('fork.txt', b'the data fork\n' * 100)
for i in range(400):
    put('many/entry with a longer name %03d.txt' % i, b'line %d\n' % i * 40)
for i in range(600):
    put('gap.%03d' % i, bytes([i % 251]) * 4096)
os.sync()
PY
    printf 'the resource fork\n%.0s' $(seq 1 200) > "$m/fork.txt/..namedfork/rsrc"
    ln "$m/bin/data.bin" "$m/link.bin"
    ln -s bin/data.bin "$m/link.sym"
    ditto --hfsCompression "$tmp/plain.txt" "$m/packed.txt"
    sync
    python3 -I - "$m/fill.bin" <<'PY'
import sys
with open(sys.argv[1], 'wb', buffering=0) as f:
    for n in (65536, 4096):
        try:
            while True:
                f.write(bytes(n))
        except OSError:
            pass
PY
    sync
    i=1
    while [ $i -lt 600 ]; do rm "$m/$(printf gap.%03d $i)"; i=$((i + 2)); done
    sync
    cp "$jpeg" "$m/photo.jpg"
    python3 -I -c 'import sys; open(sys.argv[1], "wb").write(bytes((i * 13 + (i >> 9)) % 253 for i in range(4096 * 250 + 100)))' "$m/frag.bin"
    sync
    hdiutil detach -quiet "$m"
    zstd -q -19 -f "$img" -o "$out/$1.img.zst"
}
make hfs HFS+ SPUD
make hfsx "Case-sensitive HFS+" GPTSPUD
