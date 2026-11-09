#!/bin/sh
# tests/data: FAT12, FAT16, FAT32 and exFAT volumes by macOS's hdiutil (raw images, of no
# partition table), their files written through the mounted volume, zstd -19:
#   readme.txt, docs/deeper/a rather long file name.json, docs/한글.txt, bin/data.bin,
#   empty.txt, and photo.jpg (aqr-jpeg's gradient.jpg) and frag.bin written into the gaps
#   that deleting every other of small files gap.NN (a cluster each) leaves: in pieces.
set -eu
crate="$(cd "$(dirname "$0")/.." && pwd)"
out="$crate/tests/data"
jpeg="$crate/../aqr-jpeg/tests/data/gradient.jpg"
tmp="$(mktemp -d)"
trap 'hdiutil detach -quiet "$tmp/mnt" 2>/dev/null || true; rm -rf "${tmp:?}"' EXIT
make() { # name fs size
    hdiutil create -size "$3" -fs "$2" -volname AQR -layout NONE -o "$tmp/$1.dmg" >/dev/null 2>&1
    # (the bytes of a cluster, of FAT's BPB or exFAT's boot sector)
    cluster=$(python3 -I -c 'import sys; b = open(sys.argv[1], "rb").read(512)
print(1 << (b[108] + b[109]) if b[3:11] == b"EXFAT   " else int.from_bytes(b[11:13], "little") * b[13])' "$tmp/$1.dmg")
    hdiutil attach -nobrowse -mountpoint "$tmp/mnt" "$tmp/$1.dmg" >/dev/null 2>&1
    m="$tmp/mnt"
    mkdir -p "$m/docs/deeper" "$m/bin"
    python3 -I - "$m" "$cluster" <<'PY'
import os, sys
m, cluster = sys.argv[1], int(sys.argv[2])
def put(p, d):
    with open(os.path.join(m, p), 'wb') as f:
        f.write(d)
put('readme.txt', b'Aqueous FAT fixture\r\n' * 40)
put('docs/deeper/a rather long file name.json', b'{"a": [1, 2, 3], "b": "c"}')
put('docs/한글.txt', '물 aqueous'.encode())
put('bin/data.bin', bytes((i * 7 + (i >> 8)) % 251 for i in range(10000)))
put('empty.txt', b'')
for i in range(24):
    put('gap.%02d' % i, bytes([i]) * cluster)
os.sync()
PY
    for i in 01 03 05 07 09 11 13 15 17 19 21 23; do rm "$m/gap.$i"; done
    sync
    cp -X "$jpeg" "$m/photo.jpg"
    python3 -I -c 'import sys; open(sys.argv[1], "wb").write(bytes((i * 13 + (i >> 9)) % 253 for i in range(int(sys.argv[2]) * 5 + 100)))' "$m/frag.bin" "$cluster"
    sync
    hdiutil detach -quiet "$m"
    zstd -q -19 -f "$tmp/$1.dmg" -o "$out/$1.img.zst"
}
# (not 4m: macOS's msdosfs wrote that volume of 1 KiB clusters wrong, files cut and misnamed)
make fat12 "MS-DOS FAT12" 8m
make fat16 "MS-DOS FAT16" 24m
make fat32 "MS-DOS FAT32" 40m
make exfat ExFAT 8m
