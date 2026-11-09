#!/bin/sh
# tests/data: a disk of a GPT of an ext4 and an ext2 volume (sfdisk, mke2fs, files written through
# a loop mount), as it is (disk.img) and as qemu-img converts it (all black boxes, in the Lima VM
# aqr-linux: docs/disk-image-design.md), zstd -19:
#   disk.vhd (dynamic VHD), disk.vhdx (dynamic VHDX), disk.vmdk (monolithicSparse), disk.qcow2
#   (version 3), disk-v2.qcow2 (version 2), disk-c.qcow2 (compressed clusters), disk.vdi.
# The files: readme.txt, photo.jpg (aqr-jpeg's gradient.jpg), data.bin (1 MiB of a pattern) in
# both volumes, and in the ext4 one big.bin (8 MiB of another), so that files cross the
# containers' blocks.
set -eu
crate="$(cd "$(dirname "$0")/.." && pwd)"
vm=aqr-linux
limactl copy "$crate/../aqr-jpeg/tests/data/gradient.jpg" "$vm:/tmp/aqr-disk-gradient.jpg"
limactl shell --workdir / "$vm" -- sudo sh -eu <<'VM'
tmp=$(mktemp -d)
dev=
trap 'umount "$tmp/m" 2>/dev/null || true; [ -n "$dev" ] && losetup -d "$dev"; rm -rf "$tmp"' EXIT
img=$tmp/disk.img
truncate -s 64M "$img"
printf 'label: gpt\nstart=2048, size=69632, type=linux\nstart=71680, size=59000, type=linux\n' | sfdisk -q "$img"
dev=$(losetup -f --show -P "$img")
mke2fs -q -F -t ext4 -L AQR4 "${dev}p1"
mke2fs -q -F -t ext2 -L AQR2 "${dev}p2"
mkdir "$tmp/m"
for p in p1 p2; do
    mount "$dev$p" "$tmp/m"
    python3 -I - "$tmp/m" "$p" <<'PY'
import os, sys
m, p = sys.argv[1], sys.argv[2]
def put(name, d):
    with open(os.path.join(m, name), 'wb') as f:
        f.write(d)
put('readme.txt', b'Aqueous container fixture\n' * 40)
put('data.bin', bytes((i * 7 + (i >> 8)) % 251 for i in range(1 << 20)))
if p == 'p1':
    put('big.bin', bytes((i * 11 + (i >> 12)) % 241 for i in range(8 << 20)))
PY
    cp /tmp/aqr-disk-gradient.jpg "$tmp/m/photo.jpg"
    umount "$tmp/m"
done
losetup -d "$dev"
dev=
cd "$tmp"
qemu-img convert -O vpc -o subformat=dynamic disk.img disk.vhd
qemu-img convert -O vhdx -o subformat=dynamic disk.img disk.vhdx
qemu-img convert -O vmdk -o subformat=monolithicSparse disk.img disk.vmdk
qemu-img convert -O qcow2 -o compat=1.1 disk.img disk.qcow2
qemu-img convert -O qcow2 -o compat=0.10 disk.img disk-v2.qcow2
qemu-img convert -c -O qcow2 disk.img disk-c.qcow2
qemu-img convert -O vdi disk.img disk.vdi
for f in disk.img disk.vhd disk.vhdx disk.vmdk disk.qcow2 disk-v2.qcow2 disk-c.qcow2 disk.vdi; do
    zstd -q -19 -f "$f" -o "/tmp/aqr-disk-$f.zst"
    chmod a+r "/tmp/aqr-disk-$f.zst"
done
VM
for f in disk.img disk.vhd disk.vhdx disk.vmdk disk.qcow2 disk-v2.qcow2 disk-c.qcow2 disk.vdi; do
    limactl copy "$vm:/tmp/aqr-disk-$f.zst" "$crate/tests/data/$f.zst"
    limactl shell "$vm" -- sudo rm -f "/tmp/aqr-disk-$f.zst"
done
limactl shell "$vm" -- sudo rm -f /tmp/aqr-disk-gradient.jpg
