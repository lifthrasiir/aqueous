"""Makes the file icons from droplet.svg: the drop with a label of the extension in front (an
Aqueous archive's a blue label on the blue drop, the other formats' a dark gray one on the drop in
gray), as `.ico` (aqr-win) and `.icns` (aqr-mac). The program's own icon (droplet.ico,
droplet.icns) is the blue drop alone, unlabeled.

    .venv/Scripts/python scripts/icons.py [--svg DIR] [--png DIR] [EXT...]

Renders with ImageMagick (`magick`, librsvg) and the label's font (Segoe UI Bold, else Arial
Bold), so the results depend on the fonts at hand: they are committed, made on Windows.
Both containers take PNG images only (Windows Vista on, macOS 10.7 on).
"""

import argparse
import re
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WIN = ROOT / "crates/aqr-win/icons"
MAC = ROOT / "crates/aqr-mac/icons"

BLUE = "#0099D6"
DARK = "#3A4048"
# The extensions aqr-unpack reads (crates/aqr-unpack/src/lib.rs) by their last extension, as
# Explorer and Finder tell a file's type (`.tar.gz` is `.gz`). The aliases of ZIP, 7z and RAR
# (`.jar`, `.cbz`, ...) are other programs' and left out.
# (the label's color; the drop stays blue under a blue label, else it is in gray)
LABELS = {"aqr": BLUE} | {
    e: DARK for e in ["zip", "7z", "rar", "alz", "egg", "tar", "tgz", "tbz2", "txz", "gz", "bz2", "xz", "lzma", "zst"]
}

# droplet.svg's blues and the grays put for them (lightest to darkest)
GRAYS = {
    "#D1F2FF": "#F1F3F5",
    "#80DAFE": "#C9CED3",
    "#0099D6": "#8C939A",
    "#1477E5": "#767D85",
    "#2D4FF8": "#5F666E",
    "#3640FF": "#50575F",
}

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
# (type, pixels) of the PNG entries, as iconutil writes them up to 512 points
ICNS_TYPES = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"ic11", 32),
    (b"icp6", 64),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
]
# Below this many pixels the label has no text (unreadable), only its band.
TEXT_MIN = 32


def svg(ext: str, text: bool) -> str:
    src = (ROOT / "droplet.svg").read_text(encoding="utf-8")
    if LABELS[ext] != BLUE:
        src = re.sub("|".join(GRAYS), lambda m: GRAYS[m.group(0)], src)
    label = ext.upper()
    # (in the 24-unit box of droplet.svg; four letters narrower)
    size = 5.6 if len(label) <= 3 else 4.7
    badge = (
        f'<rect x="2.2" y="13.4" width="19.6" height="7" rx="1.6" fill="{LABELS[ext]}" stroke="#fff" stroke-width=".6"/>'
    )
    if text:
        badge += (
            f'<text x="12" y="{16.9 + size * 0.366:.2f}" text-anchor="middle" font-family="Segoe UI, Arial, sans-serif"'
            f' font-weight="700" font-size="{size}" fill="#fff">{label}</text>'
        )
    else:
        # (a band of the label at small sizes, thicker for its lack of text)
        badge = badge.replace('y="13.4"', 'y="12.8"').replace('height="7"', 'height="8.2"')
    return src.replace("</svg>", f"<g>{badge}</g></svg>")


def render(svg_path: Path, px: int, inner: int, out: Path):
    """`svg_path` at `inner` pixels, centred on a transparent `px` square."""
    subprocess.run(
        # (droplet.svg is 960 pixels square: rendered at that, then made smaller)
        ["magick", "-background", "none", str(svg_path)]
        + ["-resize", f"{inner}x{inner}", "-gravity", "center", "-extent", f"{px}x{px}", "+repage"]
        + ["-define", "png:color-type=6", "-depth", "8", "-strip", str(out)],
        check=True,
    )
    return out.read_bytes()


def ico(images: list[tuple[int, bytes]]) -> bytes:
    head = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    dirs, data = b"", b""
    for px, png in images:
        dirs += struct.pack("<BBBBHHII", px % 256, px % 256, 0, 0, 1, 32, len(png), offset + len(data))
        data += png
    return head + dirs + data


def icns(images: list[tuple[bytes, bytes]]) -> bytes:
    body = b"".join(kind + struct.pack(">I", 8 + len(png)) + png for kind, png in images)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--svg", type=Path, help="keep the SVGs here")
    p.add_argument("--png", type=Path, help="keep the PNGs here")
    p.add_argument("ext", nargs="*", default=list(LABELS))
    a = p.parse_args()
    for e in a.ext:
        if e not in LABELS:
            sys.exit(f"no label for {e!r}")
    WIN.mkdir(exist_ok=True)
    MAC.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        svg_dir = a.svg or tmp
        png_dir = a.png or tmp
        svg_dir.mkdir(parents=True, exist_ok=True)
        png_dir.mkdir(parents=True, exist_ok=True)
        for e in a.ext:
            paths = {}
            for text, name in [(True, e), (False, f"{e}-small")]:
                paths[text] = svg_dir / f"{name}.svg"
                paths[text].write_text(svg(e, text), encoding="utf-8")
            cache = {}

            def png(px, inner):
                if (px, inner) not in cache:
                    out = png_dir / f"{e}-{px}{'' if inner == px else f'-{inner}'}.png"
                    cache[px, inner] = render(paths[inner >= TEXT_MIN], px, inner, out)
                return cache[px, inner]

            # Explorer: the whole square, as droplet.ico; Finder: 80%, as icns.sh
            (WIN / f"{e}.ico").write_bytes(ico([(px, png(px, px)) for px in ICO_SIZES]))
            (MAC / f"{e}.icns").write_bytes(icns([(k, png(px, px * 4 // 5)) for k, px in ICNS_TYPES]))
            print(e, (WIN / f"{e}.ico").stat().st_size, (MAC / f"{e}.icns").stat().st_size)


if __name__ == "__main__":
    main()
