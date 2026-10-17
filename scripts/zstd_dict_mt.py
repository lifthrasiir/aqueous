#!/usr/bin/env python3
"""Wall-clock multi-threaded compression throughput (no timing of I/O).

Two ways to use several cores:
  procs   frames are independent, so N processes compress different frames;
          output is byte-identical to the single-threaded one
  nbw     libzstd's own ZSTD_c_nbWorkers inside one frame; output changes
          (the frame is cut into jobs), so the size is reported too

Throughput is input MB / wall-clock seconds. Frames are packed exactly like
zstd_dict_bench.py; files are read before the clock starts and shared via fork.

Usage (from scripts/): .venv/bin/python zstd_dict_mt.py --corpus DIR --glob '*.rs' [--dict FILE] ...
"""
import argparse, multiprocessing as mp, os, sys, time
from pathlib import Path

import zstd_dict_bench as bench

FRAMES = []
CTX = {}


def work(args):
    level, dict_path, lo, hi = args
    key = (level, dict_path)
    if key not in CTX:
        CTX[key] = bench.Compressor(level, Path(dict_path).read_bytes() if dict_path else None)
    return sum(len(CTX[key].compress(f)) for f in FRAMES[lo:hi])


def main():
    global FRAMES
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--corpus', type=Path, default=bench.DEFAULT_CORPUS)
    ap.add_argument('--glob', default='*.html')
    ap.add_argument('--level', type=int, default=3)
    ap.add_argument('--dict', type=Path, help='dictionary for the +dict rows')
    ap.add_argument('--packs', default='0,16K,64K,1M,16M,all')
    ap.add_argument('--threads', default='1,4,10')
    ap.add_argument('--reps', type=int, default=3, help='best of N wall-clock runs')
    opts = ap.parse_args()
    threads = [int(x) for x in opts.threads.split(',')]

    paths = sorted({p for g in opts.glob.split(',') for p in opts.corpus.rglob(g) if p.is_file()})
    data = [p.read_bytes() for p in paths]
    raw = sum(map(len, data))
    print(f'{opts.corpus} ({opts.glob}): {len(paths)} files, {raw:,} B; L{opts.level}; '
          f'{os.cpu_count()} cores ({bench.perf_cores()} P); MB/s wall-clock, best of {opts.reps}\n')
    print('| pack | dict | mode | ' + ' | '.join(f'{t} thr' for t in threads) + ' | size (1 thr → max thr) |')
    print('|---|---|---|' + '--:|' * (len(threads) + 1))

    for limit in [bench.parse_size(x) for x in opts.packs.split(',')]:
        FRAMES, cur = [], []
        for x in data:
            if cur and sum(map(len, cur)) + len(x) > limit:
                FRAMES.append(b''.join(cur))
                cur = []
            cur.append(x)
        FRAMES.append(b''.join(cur))
        dicts = [None] + ([str(opts.dict)] if opts.dict and limit != bench.ALL else [])
        for dpath in dicts:
            dname = opts.dict.stem.split('-s')[0].split('-')[-1] if dpath else '—'
            # procs: chunks of ~1 MB of input per task, so 48k one-file frames don't drown in IPC
            tasks, lo, n = [], 0, 0
            for i, f in enumerate(FRAMES):
                n += len(f)
                if n >= 1 << 20:
                    tasks.append((opts.level, dpath, lo, i + 1))
                    lo, n = i + 1, 0
            if lo < len(FRAMES):
                tasks.append((opts.level, dpath, lo, len(FRAMES)))
            if len(FRAMES) > 1:
                cells, sizes = [], []
                for t in threads:
                    best = float('inf')
                    with mp.get_context('fork').Pool(t) as pool:
                        pool.map(work, [(opts.level, dpath, 0, 1)] * t)  # warm up contexts
                        for _ in range(opts.reps):
                            t0 = time.perf_counter()
                            size = sum(pool.imap_unordered(work, tasks))
                            best = min(best, time.perf_counter() - t0)
                    cells.append(f'{raw / best / 1e6:,.0f}')
                    sizes.append(size)
                assert len(set(sizes)) == 1, sizes
                print(f'| {bench.fmt_size(limit)} | {dname} | procs | ' + ' | '.join(cells) +
                      f' | {sizes[0]:,} (same)' + ' |', flush=True)
            if limit >= 1 << 20 and not dpath:
                cells, sizes = [], []
                for t in threads:
                    c = bench.Compressor(opts.level, None, workers=0 if t == 1 else t)
                    best = float('inf')
                    for _ in range(opts.reps):
                        t0 = time.perf_counter()
                        size = sum(len(c.compress(f)) for f in FRAMES)
                        best = min(best, time.perf_counter() - t0)
                    cells.append(f'{raw / best / 1e6:,.0f}')
                    sizes.append(size)
                print(f'| {bench.fmt_size(limit)} | {dname} | nbw | ' + ' | '.join(cells) +
                      f' | {sizes[0]:,} → {sizes[-1]:,} ({sizes[-1] / sizes[0] - 1:+.2%}) |', flush=True)


if __name__ == '__main__':
    main()
