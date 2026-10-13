#!/usr/bin/env python3
"""Per-file ratios of objr against the cached baselines (scripts/baseline.sh), and totals
per corpus set over the files every tool has.

usage: .venv/bin/python -I scripts/compare.py [objr bench options] FILE...
"""
import collections, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOOLS = ["zstd3", "zstd19", "bzip2", "xz9", "paq8px", "draco"]
args = sys.argv[1:]
opts = [a for a in args if not a.endswith(".obj")]
files = [a for a in args if a.endswith(".obj")]
out = subprocess.run([os.path.join(ROOT, "target/release/objr"), "bench", "-v", *opts, *files],
                     capture_output=True, text=True, cwd=ROOT).stdout
ours, speed = {}, {}
for l in out.splitlines():
    t = l.split()
    if len(t) >= 6 and t[-1].endswith(".obj"):
        ours[t[-1]] = (int(t[0]), int(t[1]))
        speed[t[-1]] = (float(t[3]), float(t[4]))
base = collections.defaultdict(dict)
for tool in TOOLS:
    p = os.path.join(ROOT, "cache", f"base-{tool}.tsv")
    if os.path.exists(p):
        for l in open(p):
            f, size, _mtime, c = l.rstrip("\n").split("\t")
            base[f][tool] = int(c)
print(f"{'file':<52} {'size':>11} {'objr':>7} " + " ".join(f"{t:>7}" for t in TOOLS))
sets = collections.defaultdict(lambda: collections.Counter())
for f in files:
    n, c = ours[f]
    b = base.get(f, {})
    print(f"{f:<52} {n:>11} {c / n:7.4f} " + " ".join(f"{b[t] / n:7.4f}" if t in b else f"{'-':>7}" for t in TOOLS))
    s = f.split("/")[1]
    for k in ("all", s):
        sets[k]["files"] += 1
        sets[k]["size"] += n
        sets[k]["objr"] += c
        sets[k]["tc"] += n / speed[f][0]
        sets[k]["td"] += n / speed[f][1]
        for t in TOOLS:
            if t in b and b[t] > 0:
                sets[k][t + "_n"] += n
                sets[k][t] += b[t]
                sets[k][t + "_o"] += c
print()
print("set: files, size, objr ratio, compress/decompress MB/s; per tool: its ratio (objr's on the same files)")
for k, v in sets.items():
    tools = "  ".join(f"{t} {v[t] / v[t + '_n']:.4f} ({v[t + '_o'] / v[t + '_n']:.4f}, {v[t + '_n'] / 1e6:.0f} MB)" for t in TOOLS if v[t + "_n"])
    print(f"{k:<9} {v['files']:>3} {v['size'] / 1e6:8.0f} MB  objr {v['objr'] / v['size']:.4f}  {v['size'] / v['tc']:.0f}/{v['size'] / v['td']:.0f} MB/s  {tools}")
