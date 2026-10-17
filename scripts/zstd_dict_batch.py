#!/usr/bin/env python3
"""Amortized decompression throughput when a batch of files is read together.

Each file is requested independently with probability f. A frame is decoded
once, from its start up to the furthest requested file, rounded up to whole
zstd blocks (128 KiB of output). Decoding runs at the configuration's measured
full-extraction rate (zstd_dict_bench.py's "extract"). Throughput = requested file bytes
/ decode time, so f = 1 is full extraction and f -> 0 approaches random access.
The f -> 0 limit is printed next to zstd_dict_bench.py's measured random-access speed as
a check of the model.

For every f the smallest configuration whose throughput is >= --floor MB/s is
reported (compression is assumed parallel enough, see zstd_dict_mt.py).

With --threads N the decode rates come from zstd_dict_mtd.py (zstd-dict-mtd-<name>.json,
measured in isolation): batches use the N-process full-extraction rate (frames
decoded in parallel; a single frame stays single-threaded by construction) and
the single-file column uses the 1-process rate, since one file is one frame.

Usage (from scripts/): .venv/bin/python zstd_dict_batch.py [zstd-dict-*.json ...] [--level 3]
"""
import argparse, json
from pathlib import Path

import zstd_dict_bench as bench

BLOCK = 128 << 10


def frames_of(sizes, limit):
    out, cur, n = [], [], 0
    for s in sizes:
        if cur and n + s > limit:
            out.append(cur)
            cur, n = [], 0
        cur.append(s)
        n += s
    out.append(cur)
    return out


def decoded_bytes(frames, f):
    """Expected decoded bytes over all frames; f=0 gives the per-file single-access limit (sum)."""
    total = 0.0
    for fr in frames:
        flen = sum(fr)
        ends, e = [], 0
        for s in fr:
            e += s
            ends.append(min(-(-e // BLOCK) * BLOCK, flen))
        if f == 0:
            total += sum(ends)  # each file fetched on its own
            continue
        # P(furthest requested file is i) = f (1-f)^(k-1-i)
        q, p = 1.0, 0.0
        for e in reversed(ends):
            total += e * f * q
            q *= 1 - f
    return total


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('files', nargs='*', type=Path)
    ap.add_argument('--level', type=int, default=3)
    ap.add_argument('--fracs', default='0,0.001,0.01,0.1,1')
    ap.add_argument('--floor', type=float, default=1000)
    ap.add_argument('--threads', type=int, default=0, help='use zstd_dict_mtd.py rates for N decode threads')
    opts = ap.parse_args()
    files = opts.files or sorted(f for f in Path(__file__).parent.glob('zstd-dict-*.json')
                                 if not f.name.startswith(('zstd-dict-mtd-', 'zstd-dict-results')))
    fracs = [float(x) for x in opts.fracs.split(',')]

    check, best = [], []
    for fn in files:
        j = json.loads(fn.read_text())
        if 'results' not in j or 'pack' not in j['results'][0]:
            continue
        corpus = Path(__file__).parent / j['corpus']  # absolute paths stay as they are
        paths = sorted({p for g in j['glob'].split(',') for p in corpus.rglob(g) if p.is_file()})
        sizes = [p.stat().st_size for p in paths]
        if sum(sizes) != j['raw']:
            raise SystemExit(f'{fn}: corpus changed since the run')
        name = fn.stem.removeprefix('zstd-dict-')
        rs = [r for r in j['results'] if r['level'] == opts.level]
        base = next(r['size'] for r in rs if r['pack'] == bench.ALL and not r['dict_size'])
        framings = {}
        if opts.threads:
            m = json.loads((fn.parent / f'zstd-dict-mtd-{name}.json').read_text())
            if m['threads'] != opts.threads or m['level'] != opts.level:
                raise SystemExit(f'zstd-dict-mtd-{name}.json is for {m["threads"]} threads, L{m["level"]}')
            rates = {x['name']: x for x in m['results']}
            for r in rs:
                r['rate1'], r['rateN'] = rates[r['name']]['extract1'], rates[r['name']]['extractN']
        else:
            for r in rs:
                r['rate1'] = r['rateN'] = r['extract']
        for r in rs:
            if r['pack'] not in framings:
                framings[r['pack']] = frames_of(sizes, r['pack'])
            r['tp'] = {}
            for f in fracs:
                want = j['raw'] * (f if f else 1)
                rate = r['rateN'] if f else r['rate1']
                r['tp'][f] = want / (decoded_bytes(framings[r['pack']], f) / (rate * 1e6)) / 1e6
            if not r['random_modeled'] and r['pack'] in (0, 64 << 10, 1 << 20) and r['dict_size'] in (0, 1 << 20):
                check.append((name, r))
        row = [name]
        for f in fracs:
            ok = [r for r in rs if r['tp'][f] >= opts.floor]
            if not ok:
                row.append('—')
                continue
            b = min(ok, key=lambda r: r['size'])
            lab = bench.fmt_size(b['pack']) + (f"+{bench.fmt_size(b['dict_size'])}" if b['dict_size'] else '')
            row.append(f"{b['size'] / base:.3f}× · {lab} · {b['tp'][f]:,.0f}")
        best.append(row)

    print(f'Level {opts.level}, decode threads: {opts.threads or "1 (zstd_dict_bench.py rates)"}. Model check, f -> 0 vs measured random access (MB/s):\n')
    print('| dataset | pack | dict | model | measured |')
    print('|---|---|---|--:|--:|')
    for name, r in check:
        print(f"| {name} | {bench.fmt_size(r['pack'])} | {bench.fmt_size(r['dict_size']) if r['dict_size'] else '—'} "
              f"| {r['tp'][0]:,.0f} | {r['random']:,.0f} |")
    print(f'\nSmallest config with amortized decompression >= {opts.floor:.0f} MB/s, when a random fraction f '
          f'of files is read together; cell = size vs solid/all · pack[+dict] · MB/s\n')
    print('| dataset | ' + ' | '.join('single file' if f == 0 else f'f = {f:g}' for f in fracs) + ' |')
    print('|---|' + '---|' * len(fracs))
    for row in best:
        print('| ' + ' | '.join(row) + ' |')


if __name__ == '__main__':
    main()
