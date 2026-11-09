#!/bin/sh
# tests/data: APFS containers by macOS's hdiutil (raw images, UDRW, of a GPT), their files
# written through the mounted volumes, zstd -19:
#   apfs.img (64 MiB), volume AQR: readme.txt, docs/deeper/a rather long file name.json,
#   docs/한글.txt, bin/data.bin, empty.txt, link.bin (a hard link of bin/data.bin), link.sym (a
#   symbolic link), clone.bin (a clone of bin/data.bin, `cp -c`), sparse.bin (a hole between two
#   blocks of data), packed.txt (compressed by decmpfs, `ditto --hfsCompression`), many/ (400
#   files), photo.jpg (aqr-jpeg's gradient.jpg) and frag.bin written into the gaps that deleting
#   every other of 600 small files gap.NNN leaves once fill.bin took the free space;
#   apfs2.img (1100 MiB, as APFS has a volume for each 512 MiB of its container): volume AQR of
#   readme.txt, and a second volume AQR2 of other.txt.
set -eu
crate="$(cd "$(dirname "$0")/.." && pwd)"
out="$crate/tests/data"
jpeg="$crate/../aqr-jpeg/tests/data/gradient.jpg"
tmp="$(mktemp -d)"
trap 'hdiutil detach -quiet "$tmp/mnt" 2>/dev/null || true; rm -rf "${tmp:?}"' EXIT
m="$tmp/mnt"

# the container of one volume, all its files
img="$tmp/apfs.dmg"
hdiutil create -size 64m -fs APFS -volname AQR -o "$img" >/dev/null 2>&1
hdiutil attach -nobrowse -mountpoint "$m" "$img" >/dev/null 2>&1
mkdir -p "$m/docs/deeper" "$m/bin" "$m/many"
python3 -I - "$m" <<'PY'
import os, sys
m = sys.argv[1]
def put(p, d):
    with open(os.path.join(m, p), 'wb') as f:
        f.write(d)
put('readme.txt', b'Aqueous APFS fixture\n' * 40)
put('docs/deeper/a rather long file name.json', b'{"a": [1, 2, 3], "b": "c"}')
put('docs/한글.txt', '물 aqueous'.encode())
put('bin/data.bin', bytes((i * 7 + (i >> 8)) % 251 for i in range(10000)))
put('empty.txt', b'')
with open(os.path.join(m, 'sparse.bin'), 'wb') as f:
    f.write(b'\x11' * 4096)
    f.seek(4096 * 64)
    f.write(b'\x22' * 4096)
for i in range(400):
    put('many/entry with a longer name %03d.txt' % i, b'line %d\n' % i * 40)
for i in range(600):
    put('gap.%03d' % i, bytes([i % 251]) * 4096)
os.sync()
PY
# (ditto compresses a copy from another volume, not one of the same)
python3 -I -c 'import sys; open(sys.argv[1], "wb").write(b"decmpfs text, line after line\n" * 2000)' "$tmp/plain.txt"
ln "$m/bin/data.bin" "$m/link.bin"
ln -s bin/data.bin "$m/link.sym"
cp -c "$m/bin/data.bin" "$m/clone.bin"
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
zstd -q -19 -f "$img" -o "$out/apfs.img.zst"

# the container of two volumes
img="$tmp/apfs2.dmg"
hdiutil create -size 1100m -fs APFS -volname AQR -o "$img" >/dev/null 2>&1
hdiutil attach -nobrowse -mountpoint "$m" "$img" >/dev/null 2>&1
printf 'Aqueous APFS fixture\n' > "$m/readme.txt"
container=$(diskutil info "$m" | awk -F': *' '/APFS Container:/{print $2}')
diskutil apfs addVolume "$container" APFS AQR2 >/dev/null
vol=$(diskutil list "$container" | awk '/AQR2/{print $NF}')
m2=$(diskutil info "$vol" | awk -F': *' '/Mount Point:/{print $2}')
printf 'the second volume\n' > "$m2/other.txt"
sync
diskutil unmount "$vol" >/dev/null
hdiutil detach -quiet "$m"
zstd -q -19 -f "$img" -o "$out/apfs2.img.zst"
