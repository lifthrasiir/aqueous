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
import zlib
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
# Up to this many pixels the label is drawn as pixels (its text in FONTS), as antialiased text is
# a blur there.
PIXEL_MAX = 32

# Pixel fonts of the letters the labels need, by their height: rows of `#`.
FONTS = {
    5: {
        "A": ".#. #.# ### #.# #.#",
        "B": "##. #.# ##. #.# ##.",
        "E": "### #.. ##. #.. ###",
        "G": ".## #.. #.# #.# .##",
        "I": "### .#. .#. .#. ###",
        "L": "#.. #.. #.. #.. ###",
        "M": "#.# ### ### #.# #.#",
        "P": "##. #.# ##. #.. #..",
        "Q": ".#. #.# #.# ##. .##",
        "R": "##. #.# ##. #.# #.#",
        "S": ".## #.. .#. ..# ##.",
        "T": "### .#. .#. .#. .#.",
        "X": "#.# #.# .#. #.# #.#",
        "Z": "### ..# .#. #.. ###",
        "2": "##. ..# .#. #.. ###",
        "7": "### ..# .#. .#. .#.",
    },
    7: {
        "A": ".###. #...# #...# ##### #...# #...# #...#",
        "B": "####. #...# #...# ####. #...# #...# ####.",
        "E": "##### #.... #.... ####. #.... #.... #####",
        "G": ".###. #...# #.... #.### #...# #...# .####",
        "I": "### .#. .#. .#. .#. .#. ###",
        "L": "#.... #.... #.... #.... #.... #.... #####",
        "M": "#...# ##.## #.#.# #.#.# #...# #...# #...#",
        "P": "####. #...# #...# ####. #.... #.... #....",
        "Q": ".###. #...# #...# #...# #.#.# #..#. .##.#",
        "R": "####. #...# #...# ####. #.#.. #..#. #...#",
        "S": ".#### #.... #.... .###. ....# ....# ####.",
        "T": "##### ..#.. ..#.. ..#.. ..#.. ..#.. ..#..",
        "X": "#...# #...# .#.#. ..#.. .#.#. #...# #...#",
        "Z": "##### ....# ...#. ..#.. .#... #.... #####",
        "2": ".###. #...# ....# ...#. ..#.. .#... #####",
        "7": "##### ....# ...#. ..#.. .#... .#... .#...",
    },
}


def svg(ext: str, label: bool) -> str:
    """The drop of `ext`'s color, with its label where `label`."""
    src = (ROOT / "droplet.svg").read_text(encoding="utf-8")
    if LABELS[ext] != BLUE:
        src = re.sub("|".join(GRAYS), lambda m: GRAYS[m.group(0)], src)
    if not label:
        return src
    text = ext.upper()
    # (in the 24-unit box of droplet.svg; four letters narrower)
    size = 5.6 if len(text) <= 3 else 4.7
    badge = (
        f'<rect x="2.2" y="13.4" width="19.6" height="7" rx="1.6" fill="{LABELS[ext]}" stroke="#fff" stroke-width=".6"/>'
        f'<text x="12" y="{16.9 + size * 0.366:.2f}" text-anchor="middle" font-family="Segoe UI, Arial, sans-serif"'
        f' font-weight="700" font-size="{size}" fill="#fff">{text}</text>'
    )
    return src.replace("</svg>", f"<g>{badge}</g></svg>")


def pixel_label(ext: str, drop: Path, px: int, out: Path) -> bytes:
    """The drop (`drop`, a PNG of `px` pixels) with `ext`'s label drawn on it as pixels: its
    text in a font of FONTS (7 rows from 24 pixels where it fits, else 5), on a band of its
    color with corners cut and a white outline, low in the square as the label of the larger
    sizes. Where the band is too wide for the square, its sides lose their padding, then their
    outline (four letters at 16 pixels: the text from edge to edge of the band)."""
    from PIL import Image

    img = Image.open(drop).convert("RGBA")
    text = ext.upper()

    def measure(rows):
        glyphs = [FONTS[rows][c].split() for c in text]
        return glyphs, sum(len(g[0]) for g in glyphs) + len(glyphs) - 1

    rows = 7 if px >= 24 and measure(7)[1] + 4 <= px else 5
    glyphs, width = measure(rows)
    # the band: the text with a pixel or two around it (pad), the outline (side) around that
    pad_x, side = 2, 1
    while width + 2 * (pad_x + side) > px and (pad_x or side):
        pad_x, side = (pad_x - 1, side) if pad_x else (0, 0)
    pad_y = 1 if px < 32 else 2
    w, h = min(px, width + 2 * (pad_x + side)), rows + 2 * pad_y + 2
    x0 = (px - w) // 2
    # (its bottom a pixel or so above the square's, the drop's tip below it)
    y0 = px - h - max(1, px // 16)
    color = tuple(int(LABELS[ext][i : i + 2], 16) for i in (1, 3, 5)) + (255,)
    white = (255, 255, 255, 255)
    for y in range(h):
        for x in range(w):
            edge_x, edge_y = side and x in (0, w - 1), y in (0, h - 1)
            if (x in (0, w - 1)) and edge_y:
                continue  # (the corners cut)
            img.putpixel((x0 + x, y0 + y), white if edge_x or edge_y else color)
    x = x0 + (w - width) // 2
    for g in glyphs:
        for dy, r in enumerate(g):
            for dx, c in enumerate(r):
                if c == "#":
                    img.putpixel((x + dx, y0 + 1 + pad_y + dy), white)
        x += len(g[0]) + 1
    img.save(out)
    return flush_around(out.read_bytes(), px, y0, y0 + h)


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


# The top and bottom of the label (its outline's) in droplet.svg's 24 units.
LABEL_TOP = 13.1
LABEL_BOTTOM = 20.7


def flush_around(png: bytes, px: int, top: int, bottom: int) -> bytes:
    """`png` (of `px` pixels, 8-bit RGBA) deflated again by zlib at level 9, in one IDAT, flushed
    above row `top` (`Z_SYNC_FLUSH`) and below row `bottom` (`Z_FULL_FLUSH`, its window
    forgotten), the label's rows between: the rows above and below it are the same drop in every
    icon of a color, and so become the same bytes, which the installer's zstd (aqr-win-install)
    then finds across the icons. Unflushed, one block's Huffman tables, made from the label's
    rows too, made every icon's stream differ from its first bytes (408 -> 222 KB of the 15
    .ico as zstd -19 packed them; 112 KB had they been stored inflated)."""
    head, idat, tail = [], [], []
    at = 8
    while at < len(png):
        n, kind = struct.unpack_from(">I4s", png, at)
        chunk = png[at : at + 12 + n]
        (idat.append(chunk[8:-4]) if kind == b"IDAT" else (tail if idat else head).append(chunk))
        at += 12 + n
    raw = zlib.decompress(b"".join(idat))
    stride = 4 * px + 1  # (and the filter's byte)
    top, bottom = max(0, top) * stride, min(px, bottom) * stride
    z = zlib.compressobj(9)
    data = z.compress(raw[:top]) + z.flush(zlib.Z_SYNC_FLUSH)
    data += z.compress(raw[top:bottom]) + z.flush(zlib.Z_FULL_FLUSH)
    data += z.compress(raw[bottom:]) + z.flush()
    chunk = struct.pack(">I", len(data)) + b"IDAT" + data + struct.pack(">I", zlib.crc32(b"IDAT" + data))
    return png[:8] + b"".join(head) + chunk + b"".join(tail)


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
            for label, name in [(True, e), (False, f"{e}-drop")]:
                paths[label] = svg_dir / f"{name}.svg"
                paths[label].write_text(svg(e, label), encoding="utf-8")
            cache = {}

            def png(px, inner):
                if (px, inner) not in cache:
                    out = png_dir / f"{e}-{px}{'' if inner == px else f'-{inner}'}.png"
                    if px <= PIXEL_MAX:
                        drop = png_dir / f"{e}-{px}-drop.png"
                        render(paths[False], px, px, drop)
                        cache[px, inner] = pixel_label(e, drop, px, out)
                    else:
                        row = lambda y: int((px - inner) / 2 + inner * y / 24)  # noqa: E731
                        png = render(paths[True], px, inner, out)
                        cache[px, inner] = flush_around(png, px, row(LABEL_TOP) - 2, row(LABEL_BOTTOM) + 3)
                return cache[px, inner]

            # Explorer: the whole square, as droplet.ico; Finder: 80%, as icns.sh (but the sizes
            # of pixels, whole: there is no room to spare)
            (WIN / f"{e}.ico").write_bytes(ico([(px, png(px, px)) for px in ICO_SIZES]))
            (MAC / f"{e}.icns").write_bytes(icns([(k, png(px, px * 4 // 5)) for k, px in ICNS_TYPES]))
            print(e, (WIN / f"{e}.ico").stat().st_size, (MAC / f"{e}.icns").stat().st_size)


if __name__ == "__main__":
    main()
