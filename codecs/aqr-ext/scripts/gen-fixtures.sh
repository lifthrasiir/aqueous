#!/bin/sh
# tests/data: ext4 and ext2 volumes by mke2fs, their files written through the kernel's drivers on
# a loop mount (both black boxes, in the Lima VM aqr-linux: docs/disk-image-design.md), zstd -19:
#   ext4.img (16 MiB, 4 KiB blocks, inline_data): readme.txt, docs/deeper/a rather long file
#   name.json and docs/한글.txt (inline), bin/data.bin, empty.txt, link.bin (a hard link of
#   bin/data.bin), link.sym (a symbolic link), sparse.bin (a hole between two blocks of data),
#   prealloc.bin (a block written of 64 allocated: unwritten extents), many/ (400 files, not
#   inline: an htree directory), photo.jpg (aqr-jpeg's gradient.jpg) and frag.bin written into the gaps
#   that deleting every other of 600 small files gap.NNN leaves once fill.bin took the free
#   space (frag.bin's extents more than its inode holds: a tree);
#   ext2.img (8 MiB, 1 KiB blocks): the same but inline files, prealloc.bin and many/; and
#   big.bin of 300 blocks (double indirect blocks), frag.bin's in indirect blocks;
#   ext4m.img (160 MiB, 1 KiB blocks, meta_bg: 20 groups, the descriptors of groups 16 on in
#   group 16; 40 inodes a group, so that the files' inodes are in groups 16 on too): the files
#   of ext2.img, and top00/ to top39/ with a file each.
set -eu
crate="$(cd "$(dirname "$0")/.." && pwd)"
vm=aqr-linux
limactl copy "$crate/../aqr-jpeg/tests/data/gradient.jpg" "$vm:/tmp/aqr-ext-gradient.jpg"
limactl shell --workdir / "$vm" -- sudo sh -eu <<'VM'
tmp=$(mktemp -d)
trap 'umount "$tmp/m" 2>/dev/null || true; rm -rf "$tmp"' EXIT
mkdir "$tmp/m"
make() { # name size block mke2fs-options...
    name=$1 size=$2 block=$3
    shift 3
    img=$tmp/$name.img
    truncate -s "$size" "$img"
    mke2fs -q -F -b "$block" -L AQR -E root_owner=0:0 "$@" "$img"
    mount -o loop "$img" "$tmp/m"
    m=$tmp/m
    mkdir -p "$m/docs/deeper" "$m/bin"
    python3 -I - "$m" "$block" "$name" <<'PY'
import os, sys
m, block, name = sys.argv[1], int(sys.argv[2]), sys.argv[3]
def put(p, d):
    with open(os.path.join(m, p), 'wb') as f:
        f.write(d)
put('readme.txt', b'Aqueous ext fixture\n' * 40)
put('docs/deeper/a rather long file name.json', b'{"a": [1, 2, 3], "b": "c"}')
put('docs/한글.txt', '물 aqueous'.encode())
put('bin/data.bin', bytes((i * 7 + (i >> 8)) % 251 for i in range(10000)))
put('empty.txt', b'')
with open(os.path.join(m, 'sparse.bin'), 'wb') as f:
    f.write(b'\x11' * block)
    f.seek(block * 64)
    f.write(b'\x22' * block)
if name == 'ext4':
    with open(os.path.join(m, 'prealloc.bin'), 'wb') as f:
        os.posix_fallocate(f.fileno(), 0, block * 64)
        f.write(b'\x33' * block)
    os.mkdir(os.path.join(m, 'many'))
    for i in range(400):
        put('many/entry with a longer name %03d.txt' % i, b'line %d\n' % i * 40)
else:
    put('big.bin', bytes((i * 3 + (i >> 10)) % 241 for i in range(block * 300)))
if name == 'ext4m':
    # (directories at the top spread over the groups, their files with them)
    for k in range(40):
        os.mkdir(os.path.join(m, 'top%02d' % k))
        put('top%02d/f.bin' % k, bytes([k]) * 3000)
for i in range(600):
    put('gap.%03d' % i, bytes([i % 251]) * block)
PY
    ln "$m/bin/data.bin" "$m/link.bin"
    ln -s bin/data.bin "$m/link.sym"
    sync
    python3 -I - "$m/fill.bin" <<'PY'
import sys
with open(sys.argv[1], 'wb', buffering=0) as f:
    for n in (65536, 1024):
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
    cp /tmp/aqr-ext-gradient.jpg "$m/photo.jpg"
    python3 -I -c 'import sys; open(sys.argv[1], "wb").write(bytes((i * 13 + (i >> 9)) % 253 for i in range(int(sys.argv[2]) * 250 + 100)))' "$m/frag.bin" "$block"
    sync
    umount "$m"
    zstd -q -19 -f "$img" -o "/tmp/aqr-$name.img.zst"
    chmod a+r "/tmp/aqr-$name.img.zst"
}
make ext4 16M 4096 -t ext4 -O inline_data
make ext2 8M 1024 -t ext2
make ext4m 160M 1024 -t ext4 -O meta_bg,^resize_inode -N 800
VM
for n in ext4 ext2 ext4m; do
    limactl copy "$vm:/tmp/aqr-$n.img.zst" "$crate/tests/data/$n.img.zst"
done
limactl shell "$vm" -- sudo rm -f /tmp/aqr-ext4.img.zst /tmp/aqr-ext2.img.zst /tmp/aqr-ext4m.img.zst /tmp/aqr-ext-gradient.jpg
