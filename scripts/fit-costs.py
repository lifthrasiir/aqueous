#!/usr/bin/env python3
"""Fits the time of each (mode, class) as a*items + b*raw + c*inflated (ns), by least squares
over the per-item measurements of `PDFR_CALIB=FILE pdfr bench`.

Usage: fit-costs.py CALIB.tsv...
"""
import collections, sys

MODES = ["raw", "dup", "full", "jpeg", "tok", "png"]
CLASSES = ["content", "font", "image", "objstm", "xref", "data", "-"]
rows = collections.defaultdict(list)
for path in sys.argv[1:]:
    for line in open(path):
        f, m, c, raw, plain, tc, td = line.split('\t')
        rows[(int(m), int(c))].append((int(raw), int(plain), float(tc) * 1e9, float(td) * 1e9))

def solve(a, b):
    n = len(b)
    m = [row[:] + [b[i]] for i, row in enumerate(a)]
    for i in range(n):
        p = max(range(i, n), key=lambda r: abs(m[r][i]))
        m[i], m[p] = m[p], m[i]
        if abs(m[i][i]) < 1e-12:
            return [0.0] * n
        for r in range(n):
            if r != i:
                f = m[r][i] / m[i][i]
                m[r] = [x - f * y for x, y in zip(m[r], m[i])]
    return [m[i][n] / m[i][i] for i in range(n)]

def fit(xs, ys):
    # Normal equations, features (1, raw, plain); weights 1/(raw+256) to keep large items from
    # dominating.
    k = len(xs[0])
    ata = [[0.0] * k for _ in range(k)]
    atb = [0.0] * k
    for x, y in zip(xs, ys):
        w = 1.0 / (x[1] + 256)
        for i in range(k):
            atb[i] += w * x[i] * y
            for j in range(k):
                ata[i][j] += w * x[i] * x[j]
    return solve(ata, atb)

print(f"{'mode':<5} {'class':<8} {'items':>7}   compress: per item, per raw B, per inflated B   decompress: same")
for (m, c), r in sorted(rows.items()):
    if len(r) < 20:
        continue
    use_plain = any(p for _, p, _, _ in r)
    xs = [(1.0, raw, plain) if use_plain else (1.0, raw) for raw, plain, _, _ in r]
    fc = fit(xs, [t for *_, t, _ in r])
    fd = fit(xs, [t for *_, t in r])
    pad = lambda v: (v + [0.0])[:3]
    fc, fd = pad(fc), pad(fd)
    print(f"{MODES[m]:<5} {CLASSES[c]:<8} {len(r):>7}   {fc[0]:9.0f} {fc[1]:7.2f} {fc[2]:7.2f}      {fd[0]:9.0f} {fd[1]:7.2f} {fd[2]:7.2f}")
