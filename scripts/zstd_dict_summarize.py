#!/usr/bin/env python3
"""Cross-dataset summary of zstd-dict-*.json from zstd_dict_bench.py.

For each dataset and level: the best size reachable when random access must be
at least T MB/s (speeds above --cap count as equal), with and without a
dictionary, relative to the fully solid frame without dictionary.
"""
import argparse, json, sys
from pathlib import Path

ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument('files', nargs='*', type=Path)
ap.add_argument('--targets', default='1000,500,100,0')
opts = ap.parse_args()
files = opts.files or sorted(f for f in Path(__file__).parent.glob('zstd-dict-*.json')
                              if not f.name.startswith(('zstd-dict-mtd-', 'zstd-dict-results')))
targets = [float(x) for x in opts.targets.split(',')]
ALL = 1 << 62


def fmt(n):
    if n == ALL:
        return 'all'
    for u, k in (('M', 1 << 20), ('K', 1 << 10)):
        if n >= k and n % k == 0:
            return f'{n // k}{u}'
    return str(n)


def spd(x):
    return f'{x / 1000:.1f}G' if x >= 1000 else f'{x:.0f}'


print('Best size per random-access floor; cell = size vs solid/all · pack[+dict] · comp MB/s\n')
hdr = ' | '.join(f'RA ≥ {t:.0f}: no dict | RA ≥ {t:.0f}: dict' for t in targets)
print(f'| dataset | L | {hdr} |')
print('|---|--:|' + '---|' * (2 * len(targets)))
for f in files:
    j = json.loads(f.read_text())
    name = f.stem.removeprefix('zstd-dict-')
    for level in sorted({r['level'] for r in j['results']}):
        rs = [r for r in j['results'] if r['level'] == level]
        cap = j.get('cap', 1000)
        base = next(r['size'] for r in rs if r['pack'] == ALL and not r['dict_size'])
        cells = []
        for t in targets:
            for want_dict in (False, True):
                ok = [r for r in rs if min(r['random'], cap) >= min(t, cap) and bool(r['dict_size']) == want_dict]
                if not ok:
                    cells.append('—')
                    continue
                b = min(ok, key=lambda r: r['size'])
                lab = fmt(b['pack']) + (f"+{fmt(b['dict_size'])}" if b['dict_size'] else '')
                cells.append(f"{b['size'] / base:.3f}× · {lab} · {spd(b['comp'])}")
        print(f'| {name} | {level} | ' + ' | '.join(cells) + ' |')
