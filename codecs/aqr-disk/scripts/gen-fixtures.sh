#!/bin/sh
# tests/data: empty volumes in partition tables, by macOS's hdiutil (raw images, UDRW), zstd -19.
#   gpt.img  GPT, a FAT volume (8 MiB)
#   mbr.img  MBR, a FAT volume (2 MiB)
#   apm.img  APM, an HFS+ volume (8 MiB)
# (hdiutil create makes UDRW images by default, of the disk's bytes alone)
set -eu
out="$(cd "$(dirname "$0")/.." && pwd)/tests/data"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp:?}"' EXIT
make() { # name layout size fs
    hdiutil create -size "$3" -layout "$2" -fs "$4" -volname T -o "$tmp/$1.dmg" >/dev/null 2>&1
    zstd -q -19 -f "$tmp/$1.dmg" -o "$out/$1.img.zst"
}
make gpt GPTSPUD 8m MS-DOS
make mbr MBRSPUD 2m MS-DOS
make apm SPUD 8m HFS+
