#!/usr/bin/env python3
"""Fits the cost model (src/budget.rs) to measurements of examples/calib.rs: the model's
estimate of zlib emulation enters with a fixed factor (chosen on a grid), the rest by
nonnegative least squares on relative errors; deconstruction and reconstruction apart.
Prints the constants (ns) and how well each run is predicted."""
import csv, sys
import numpy as np
from scipy.optimize import nnls

FEATS = ["files", "other_bytes", "pack_bytes", "loose_bytes", "work_bytes", "objects", "emulated", "emulated_bytes"]

rows = list(csv.DictReader(open(sys.argv[1])))
for r in rows:
    for k in r:
        if k not in ("dir", "budget"):
            r[k] = float(r[k])
    r["other_bytes"] = r["bytes"] - r["pack_bytes"] - r["loose_bytes"] - r["work_bytes"]
rows = [r for r in rows if r["bytes"] > 1e6]  # tiny runs are timer noise
X = np.array([[r[f] for f in FEATS] for r in rows])
est = np.array([r["emulated_est"] for r in rows])
for side in ("enc_s", "dec_s"):
    y = np.array([r[side] * 1e9 for r in rows])
    w = 1 / y
    best = None
    for k in np.arange(0.8, 2.01, 0.05):
        c, _ = nnls(X * w[:, None], (y - k * est) * w)
        pred = X @ c + k * est
        err = pred / y - 1
        rms = np.sqrt(np.mean(err**2))
        if best is None or rms < best[0]:
            best = (rms, k, c, pred, err)
    rms, k, c, pred, err = best
    print(f"== {side}: emulation factor {k:.2f}")
    for f, v in zip(FEATS, c):
        print(f"  {f:15s} {v:10.3f}")
    print(f"  relative error: rms {rms:.3f}, max {np.max(np.abs(err)):.3f}")
    for r, p, e in sorted(zip(rows, pred, err), key=lambda x: -abs(x[2]))[:10]:
        print(f"    {r['dir'].split('/')[-1]:28s} {r['budget']:>4s} actual {r[side]:7.3f}s predicted {p * 1e-9:7.3f}s ({e:+.2f})")
