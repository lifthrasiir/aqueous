#!/usr/bin/env python3
"""Rewrite real meshes the ways common OBJ writers print numbers, into corpus/writers/.

Numbers of v/vt/vn lines are re-printed; every other line is kept. Styles:
  py-repr     Python repr of the double ("0.1", "1e-05", "1.0")
  py-f32repr  Python repr of float32 values widened ("0.10000000149011612"; numpy .tolist())
  np-18e      numpy savetxt default "%.18e"
  c-17g       printf "%.17g" of doubles (libigl and the like)
  c-9g        printf "%.9g" of float32 values (shortest-safe float32 writers)
  c-e         printf "%e" of float32 values
  rust-f32    Rust Display of f32: shortest digits, never an exponent
JavaScript (three.js OBJExporter, Float32Array) is gen-writers.mjs.

usage: .venv/bin/python -I scripts/gen-writers.py   (sources and output fixed below)
"""
import os
import sys

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCES = ["corpus/c3d/igea.obj", "corpus/c3d/xyzrgb_dragon.obj", "corpus/mcguire/crytek_sponza.obj"]
f32 = np.float32


def rust_f32(x):
    s = np.format_float_positional(f32(x), unique=True, trim="-")
    return s


STYLES = {
    "py-repr": lambda x: repr(float(x)),
    "py-f32repr": lambda x: repr(float(f32(x))),
    "np-18e": lambda x: "%.18e" % x,
    "c-17g": lambda x: "%.17g" % x,
    "c-9g": lambda x: "%.9g" % float(f32(x)),
    "c-e": lambda x: "%e" % float(f32(x)),
    "rust-f32": rust_f32,
}


def main():
    out_dir = os.path.join(ROOT, "corpus", "writers")
    os.makedirs(out_dir, exist_ok=True)
    for src in SOURCES:
        base = os.path.splitext(os.path.basename(src))[0]
        with open(os.path.join(ROOT, src), "rb") as f:
            lines = f.read().split(b"\n")
        for name, fmt in STYLES.items():
            dest = os.path.join(out_dir, f"{base}-{name}.obj")
            if os.path.exists(dest):
                continue
            out = []
            for l in lines:
                t = l.split()
                if t and t[0] in (b"v", b"vt", b"vn") and len(t) > 1:
                    try:
                        nums = [float(x) for x in t[1:]]
                    except ValueError:
                        out.append(l)
                        continue
                    out.append((t[0].decode() + " " + " ".join(fmt(x) for x in nums)).encode())
                else:
                    out.append(l.rstrip(b"\r"))
            data = b"\n".join(out)
            if data == b"\n".join(lines):
                print(f"same  {os.path.relpath(dest, ROOT)} (the source is in this style)", file=sys.stderr)
                continue
            with open(dest + ".tmp", "wb") as f:
                f.write(data)
            os.replace(dest + ".tmp", dest)
            print(f"ok    {os.path.relpath(dest, ROOT)} ({os.path.getsize(dest)} bytes)", file=sys.stderr)


main()
