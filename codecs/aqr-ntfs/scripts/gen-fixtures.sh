#!/bin/sh
# tests/data: NTFS volumes by mkntfs, their files written through ntfs-3g (both black boxes, in
# the Lima VM aqr-linux: docs/disk-image-design.md), zstd -19.
#   ntfs.img (16 MiB, 4 KiB clusters): readme.txt, docs/deeper/a rather long file name.json and
#   docs/한글.txt (resident), bin/data.bin, empty.txt, link.bin (a hard link of bin/data.bin),
#   ads.txt with a stream `extra`, sparse.bin (a hole between two clusters of data), packed/
#   (compressed: packed/text.txt), and photo.jpg (aqr-jpeg's gradient.jpg) and frag.bin written
#   into the gaps that deleting every other of 600 small files gap.NNN leaves once fill.bin
#   (zeros) took the free space: frag.bin in so many pieces that its runs are in an extension
#   record (an attribute list).
set -eu
crate="$(cd "$(dirname "$0")/.." && pwd)"
vm=aqr-linux
limactl copy "$crate/../aqr-jpeg/tests/data/gradient.jpg" "$vm:/tmp/aqr-ntfs-gradient.jpg"
limactl shell --workdir / "$vm" -- sudo sh -eu <<'VM'
tmp=$(mktemp -d)
trap 'umount "$tmp/m" 2>/dev/null || true; rm -rf "$tmp"' EXIT
img=$tmp/ntfs.img
truncate -s 16M "$img"
mkntfs -q -F -Q -c 4096 -L AQR "$img" 2>/dev/null
mkdir "$tmp/m"
ntfs-3g -o streams_interface=windows,compression "$img" "$tmp/m"
m=$tmp/m
mkdir -p "$m/docs/deeper" "$m/bin" "$m/packed"
python3 -I - "$m" <<'PY'
import os, sys
m = sys.argv[1]
def put(p, d):
    with open(os.path.join(m, p), 'wb') as f:
        f.write(d)
put('readme.txt', b'Aqueous NTFS fixture\r\n' * 40)
put('docs/deeper/a rather long file name.json', b'{"a": [1, 2, 3], "b": "c"}')
put('docs/한글.txt', '물 aqueous'.encode())
put('bin/data.bin', bytes((i * 7 + (i >> 8)) % 251 for i in range(10000)))
put('empty.txt', b'')
put('ads.txt', b'the main stream\n' * 100)
put('ads.txt:extra', b'an alternate data stream\n' * 200)
with open(os.path.join(m, 'sparse.bin'), 'wb') as f:
    f.write(b'\x11' * 4096)
    f.seek(4096 * 64)
    f.write(b'\x22' * 4096)
for i in range(600):
    put('gap.%03d' % i, bytes([i % 251]) * 4096)
PY
ln "$m/bin/data.bin" "$m/link.bin"
# (compressed: a directory's flag, which its new files take)
python3 -I -c 'import os, sys; os.setxattr(sys.argv[1], "system.ntfs_attrib_be", bytes([0, 0, 8, 0]))' "$m/packed"
python3 -I -c 'import sys; open(sys.argv[1], "wb").write(b"compressed text, line after line\n" * 3000)' "$m/packed/text.txt"
sync
# (the free space filled, so that the files after take the gaps)
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
cp /tmp/aqr-ntfs-gradient.jpg "$m/photo.jpg"
python3 -I -c 'import sys; open(sys.argv[1], "wb").write(bytes((i * 13 + (i >> 9)) % 253 for i in range(4096 * 250 + 100)))' "$m/frag.bin"
sync
umount "$m"
zstd -q -19 -f "$img" -o /tmp/aqr-ntfs.img.zst
VM
limactl copy "$vm:/tmp/aqr-ntfs.img.zst" "$crate/tests/data/ntfs.img.zst"
limactl shell "$vm" -- sudo rm -f /tmp/aqr-ntfs.img.zst /tmp/aqr-ntfs-gradient.jpg
