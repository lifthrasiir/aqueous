#!/usr/bin/env python3
"""Fits the cost of zlib emulation per stream (EMU in src/budget.rs) to measurements of
examples/emucost.rs (CSV files): by FLEVEL 0 (levels 1-3, deflate_fast) or higher, and by
bytes of data below 16, 64 and 256 KiB and beyond, the time as a nonnegative combination
of a constant, literals, matches, bytes of the zlib stream and the chain statistic (capped
at 32), weighted by 1/sqrt(time)."""
import csv, sys
import numpy as np
from scipy.optimize import nnls

CUTS = [0, 16384, 65536, 262144, float("inf")]
FEATS = ["literals", "matches", "zsize", "chain32"]

rows = []
for path in sys.argv[1:]:
    for r in csv.DictReader(open(path)):
        r = {k: (v if k == "pack" else float(v)) for k, v in r.items()}
        r.setdefault("level", 6.0)
        r["literals"] = r["tokens"] - r["matches"]
        rows.append(r)
for fast in (True, False):
    R = [r for r in rows if (r["level"] <= 3) == fast]
    if not R:
        continue
    y = np.array([r["ns"] for r in R])
    size = np.array([r["size"] for r in R])
    X = np.array([[1.0] + [r[f] for f in FEATS] for r in R])
    pred = np.zeros(len(y))
    print(f"== {'deflate_fast' if fast else 'deflate_slow'} ({len(R)} streams): const, " + ", ".join(FEATS))
    for lo, hi in zip(CUTS, CUTS[1:]):
        m = (size >= lo) & (size < hi)
        if m.sum() < 5:
            continue
        w = 1 / np.sqrt(y[m])
        c, _ = nnls(X[m] * w[:, None], y[m] * w)
        pred[m] = X[m] @ c
        e = pred[m] / y[m] - 1
        print(f"  [{', '.join(f'{v:.3f}' for v in c)}]  size {lo:.0f}-{hi:.0f}: n={m.sum()}, rms {np.sqrt(np.mean(e**2)):.3f}, sum {pred[m].sum() / y[m].sum() - 1:+.3f}")
    for pk in sorted(set(r["pack"] for r in R)):
        i = np.array([r["pack"] == pk for r in R])
        print(f"    {pk:28s} sum error {pred[i].sum() / y[i].sum() - 1:+.3f}")
