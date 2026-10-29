#!/usr/bin/env python3
"""The speed of rec_probe.py's detector: per file, its CPU time and the size it gets.

The size is zstd -3 of the whole file with the chosen regions in their forms, compressed once
at the end whatever choose() measured. CPU time (of the process) rather than wall-clock, as
the files run in parallel and the machine throttles.

`--set NAME=VALUE` changes a constant of rec_probe.py (Python syntax; None turns the sampling
of LOCAL_CONTEXT, FIELD_ROWS and CHAIN_SCREEN off).

Usage (from the root): .venv/bin/python scripts/rec_bench.py [-j N] [--set NAME=VALUE]... FILE...
"""

import argparse
import sys
import time
from multiprocessing import Pool
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
import rec_probe as rp  # noqa: E402


def bench(path):
    data = np.fromfile(path, dtype=np.uint8)
    z0 = len(rp._zc.compress(data.tobytes()))
    t = time.process_time()
    _, chosen, _ = rp.choose(data, rp.auto_cands(data), z0)
    cpu = time.process_time() - t
    got = rp.size(data, [(c['s'], c['e'], c['form'], c['forms']) for c in chosen])
    return Path(path).name, len(data), z0, got, cpu


def setup(consts):
    for k, v in consts.items():
        setattr(rp, k, v)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('files', nargs='+')
    ap.add_argument('-j', type=int, default=None)
    ap.add_argument('--set', action='append', default=[])
    a = ap.parse_args()
    consts = {k: eval(v) for k, v in (x.split('=', 1) for x in a.set)}
    print('| file | MB | zstd -3 | detector | CPU s | MB/s |')
    print('|---|---|---|---|---|---|')
    tot = [0, 0, 0, 0.0]
    with Pool(a.j, setup, (consts,)) as pool:
        for name, n, z0, got, cpu in pool.imap(bench, a.files):
            print(f'| {name} | {n / 1e6:.2f} | {z0} | {got} ({got / z0:.3f}) | {cpu:.1f} | {n / 1e6 / cpu:.2f} |',
                  flush=True)
            for i, x in enumerate((n, z0, got, cpu)):
                tot[i] += x
    n, z0, got, cpu = tot
    print(f'| all | {n / 1e6:.2f} | {z0} | {got} ({got / z0:.3f}) | {cpu:.1f} | {n / 1e6 / cpu:.2f} |')


if __name__ == '__main__':
    main()
