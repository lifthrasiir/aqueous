#!/usr/bin/env python3
"""Ratios by format, as docs/file-formats.md gives them ("How it was measured").

    format-ratio.py sample [--min B] [--max-file B] [--total B] [--name GLOB]... ROOT...
        The sample of the files under the ROOTs: files of --min bytes or more (256 KiB) and
        --max-file at most, in the order of the SHA-256 of their paths relative to their ROOT,
        until --total bytes (400 MB) are taken. Prints one path a line.

    format-ratio.py measure [--show BIN] [-t N] [--cache DIR] LIST
        Runs aqr-show over the files of LIST twice, as it is (each file an archive of its own,
        its modules and resources in it) and with -r -m (formulas taken where they pay without
        them), and prints the ratios: best (Σ (archive − modules − resources) of the second run)
        and worst (Σ archive of the first), with zstd -3 of the files. The runs' output is kept
        in --cache, by the list's name, so that a run is made again only when it is removed.
"""

import argparse
import fnmatch
import hashlib
import os
import re
import subprocess
import sys

LINE = re.compile(
    r"^(?P<path>.*): (?P<fmt>[^,]*), [0-9.]+ MB, \d+ streams, archive (?P<archive>\d+) B, "
    r"modules (?P<modules>\d+) B, resources (?P<res>\d+) B, compress"
)


def sample(args):
    files = []
    for root in args.roots:
        for d, _, names in os.walk(root, followlinks=True):
            for n in names:
                if args.name and not any(fnmatch.fnmatch(n.lower(), g) for g in args.name):
                    continue
                p = os.path.join(d, n)
                if not os.path.isfile(p):
                    continue
                size = os.path.getsize(p)
                if size < args.min or (args.max_file and size > args.max_file):
                    continue
                rel = os.path.relpath(p, root)
                files.append((hashlib.sha256(rel.encode()).hexdigest(), p, size))
    files.sort()
    total = 0
    for _, p, size in files:
        if total >= args.total:
            break
        print(p)
        total += size


def run(show, threads, flags, paths, out):
    if os.path.exists(out):
        return open(out).read()
    lines = []
    for p in paths:
        r = subprocess.run([show, "-t", str(threads), *flags, p], capture_output=True, text=True)
        if r.returncode != 0:
            print(f"{p}: aqr-show failed: {r.stderr.strip()}", file=sys.stderr)
        lines.append(r.stdout)
        print(r.stdout.splitlines()[0] if r.stdout else f"{p}: no output", file=sys.stderr, flush=True)
    text = "".join(lines)
    with open(out + ".part", "w") as f:
        f.write(text)
    os.rename(out + ".part", out)
    return text


def parse(text):
    found = {}
    for line in text.splitlines():
        m = LINE.match(line)
        if m:
            found[m["path"]] = {k: int(m[k]) for k in ("archive", "modules", "res")} | {"fmt": m["fmt"]}
    return found


def zstd3(p):
    with open(p, "rb") as f:
        r = subprocess.run(["zstd", "-3", "-c", "-q"], stdin=f, capture_output=True, check=True)
    return len(r.stdout)


def measure(args):
    paths = [line.rstrip("\n") for line in open(args.list) if line.strip()]
    os.makedirs(args.cache, exist_ok=True)
    name = os.path.splitext(os.path.basename(args.list))[0]
    worst = parse(run(args.show, args.threads, [], paths, os.path.join(args.cache, f"{name}.worst.txt")))
    best = parse(run(args.show, args.threads, ["-r", "-m"], paths, os.path.join(args.cache, f"{name}.best.txt")))
    zfile = os.path.join(args.cache, f"{name}.zstd3.tsv")
    if not os.path.exists(zfile):
        with open(zfile + ".part", "w") as f:
            for p in paths:
                f.write(f"{p}\t{zstd3(p)}\n")
        os.rename(zfile + ".part", zfile)
    z = dict((p, int(n)) for p, n in (l.rstrip("\n").split("\t") for l in open(zfile)))
    missing = [p for p in paths if p not in worst or p not in best]
    if missing:
        print(f"failed or mismatched: {len(missing)} files, e.g. {missing[0]}", file=sys.stderr)
    paths = [p for p in paths if p in worst and p in best]
    size = sum(os.path.getsize(p) for p in paths)
    w = sum(worst[p]["archive"] for p in paths)
    b = sum(best[p]["archive"] - best[p]["modules"] - best[p]["res"] for p in paths)
    mods = sum(worst[p]["modules"] for p in paths)
    res = sum(worst[p]["res"] for p in paths)
    fmts = {}
    for p in paths:
        fmts[worst[p]["fmt"]] = fmts.get(worst[p]["fmt"], 0) + 1
    print(f"{name}: {len(paths)} files, {size / 1e6:.1f} MB; formats {fmts}")
    print(f"  best {b / size:.3f} ~ worst {w / size:.3f}; zstd -3 {sum(z[p] for p in paths) / size:.3f}")
    print(f"  worst's modules {mods / 1e6:.2f} MB, resources {res / 1e6:.2f} MB (of {w / 1e6:.2f} MB)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("sample")
    s.add_argument("--min", type=int, default=256 * 1024)
    s.add_argument("--max-file", type=int, default=0)
    s.add_argument("--total", type=int, default=400_000_000)
    s.add_argument("--name", action="append", default=[], help="file name globs (lower case)")
    s.add_argument("roots", nargs="+")
    m = sub.add_parser("measure")
    m.add_argument("--show", default="target/release/aqr-show")
    m.add_argument("-t", "--threads", type=int, default=1)
    m.add_argument("--cache", default="scripts/cache/format-ratio")
    m.add_argument("list")
    a = ap.parse_args()
    sample(a) if a.cmd == "sample" else measure(a)


if __name__ == "__main__":
    main()
