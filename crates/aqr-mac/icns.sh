#!/bin/sh
# Makes droplet.icns at the root from droplet.svg (rsvg-convert, iconutil): the drop at 80% of
# each size, centred, as macOS's icons leave a margin (824 of 1024 points of their grid).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
set_dir=$(mktemp -d)/droplet.iconset
mkdir -p "$set_dir"
for n in 16 32 128 256 512; do
    for scale in 1 2; do
        px=$((n * scale))
        inner=$((px * 4 / 5))
        off=$(((px - inner) / 2))
        name=icon_${n}x${n}
        [ "$scale" = 2 ] && name=${name}@2x
        rsvg-convert -w "$inner" -h "$inner" --page-width "$px" --page-height "$px" \
            --left "$off" --top "$off" -o "$set_dir/$name.png" "$root/droplet.svg"
    done
done
iconutil -c icns -o "$root/droplet.icns" "$set_dir"
rm -r "${set_dir:?}"
ls -l "$root/droplet.icns"
