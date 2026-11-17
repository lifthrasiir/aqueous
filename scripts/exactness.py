#!/usr/bin/env python3
"""Where the bytes of an archive go, by the class of the streams holding them.

Reads the JSON of `aqr -c ... --stats=FILE.json` (its contents' `classes`) and tells, of each
archive made:

- the bytes by class, of all the files: `raw` (data as it is) and the recompressors'
  representations (records of a parse, indexes, skeletons, what they kept as it was);
- the classes that hold only what exactness costs ([`EXACTNESS`]): the records of a parse or
  of corrections, the compressed data kept as it is inside containers;
- the contents kept as they are (no module) by kind: of a compressed kind, what a
  normalisation (`-Y`) could take apart where exactness cannot;
- of each class of representation the kinds of files it is of.

This is the ledger of what exactness costs (docs: what `-Y` may gain); the class names are the
recompressors' own (`flate/record`, `rar/record`, `mca/kept`, ...).

    python scripts/exactness.py STATS.json [--top N] [--kind KIND]
"""

import argparse
import json
import re
import sys
from collections import defaultdict

# Classes holding only what exactness costs: what a normalisation (`-Y`) drops or makes far
# smaller. `*/record` are the parse records and corrections of the LZ-like recompressors
# (aqr-flate's corrections; aqr-lzma's, aqr-rar's, aqr-lzo's... parses; aqr-gif's LZW codes);
# `*/kept`, `flate/deflated` compressed data left as it is in a container (a ZIP's entries not
# recompressed, a region file's stale sectors, ...). The lengths (`*/index`) stay.
EXACTNESS = re.compile(r"^[\w-]+/(record|kept|deflated|holes|drop|stored)$")


def size(n):
    for unit in ("B", "KB", "MB", "GB"):
        if abs(n) < 1000 or unit == "GB":
            return f"{n:,.0f} {unit}" if unit == "B" else f"{n:,.2f} {unit}"
        n /= 1000


def table(rows, header):
    widths = [max(len(str(r[i])) for r in [header, *rows]) for i in range(len(header))]
    line = lambda r: "  ".join(str(c).rjust(w) if i else str(c).ljust(w) for i, (c, w) in enumerate(zip(r, widths)))
    print(line(header))
    for r in rows:
        print(line(r))


def run(r, top, only):
    contents = [c for c in r.get("contents") or [] if only is None or c["kind"] == only]
    written = r["written"]
    print(f"== {r['input']} -> {r['output']}: {size(r['read'])} -> {size(written)}, overhead {size(r['overhead'] or 0)}")
    if not any("classes" in c for c in contents):
        print("(no classes: JSON of aqr before them, or a file archived alone)")
        return
    by_class = defaultdict(lambda: [0.0, 0])
    by_class_kind = defaultdict(lambda: defaultdict(float))
    kept = defaultdict(lambda: [0.0, 0, 0])
    for c in contents:
        if c.get("same_as") is not None:
            continue
        for b in c.get("classes", []):
            e = by_class[b["class"]]
            e[0] += b["archived"]
            e[1] += 1
            by_class_kind[b["class"]][c["kind"]] += b["archived"]
        if not c.get("modules"):
            k = kept[c["kind"]]
            k[0] += c.get("archived", 0)
            k[1] += c["len"]
            k[2] += 1
    print("-- by class")
    rows = sorted(by_class.items(), key=lambda kv: -kv[1][0])[:top]
    table([(cl, size(v[0]), f"{100 * v[0] / written:.2f}%", v[1]) for cl, v in rows], ["class", "archived", "of archive", "files"])
    print()
    print("-- what exactness costs, by class")
    rows = sorted(((cl, v) for cl, v in by_class.items() if EXACTNESS.match(cl)), key=lambda kv: -kv[1][0])
    table([(cl, size(v[0]), f"{100 * v[0] / written:.2f}%", v[1]) for cl, v in rows], ["class", "archived", "of archive", "files"])
    exact = sum(v[0] for _, v in rows)
    print(f"in all {size(exact)} ({100 * exact / written:.2f}% of the archive)")
    print()
    print("-- kept as they are (no module), by kind")
    rows = sorted(((k, v) for k, v in kept.items() if k != "-"), key=lambda kv: -kv[1][0])[:top]
    table(
        [(k, v[2], size(v[1]), size(v[0]), f"{100 * v[0] / max(v[1], 1):.1f}%") for k, v in rows],
        ["kind", "files", "bytes", "archived", "ratio"],
    )
    if "-" in kept:
        v = kept["-"]
        print(f"(and of no kind detected: {v[2]} files, {size(v[1])} -> {size(v[0])})")
    print()
    print("-- representations by the kinds of files they are of")
    rows = sorted(((k, v) for k, v in by_class.items() if k != "raw"), key=lambda kv: -kv[1][0])[:top]
    for cl, v in rows:
        kinds = sorted(by_class_kind[cl].items(), key=lambda kv: -kv[1])[:5]
        print(f"{cl:24} {size(v[0]):>12}  " + ", ".join(f"{k} {size(b)}" for k, b in kinds))
    print()


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("stats")
    p.add_argument("--top", type=int, default=40)
    p.add_argument("--kind", help="only the contents of this kind")
    a = p.parse_args()
    with open(a.stats, encoding="utf-8") as f:
        s = json.load(f)
    for r in s["runs"]:
        if r["mode"] == "compress" and not r.get("partial"):
            run(r, a.top, a.kind)


if __name__ == "__main__":
    sys.exit(main())
