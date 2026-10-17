#!/usr/bin/env python3
"""Wall-clock decompression throughput with frames decoded in parallel.

For every configuration of one level in a zstd_dict_bench.py result file, all frames are
compressed (in parallel, not timed), then fully decompressed by 1 and by N
processes (fork; each process takes whole frames). Throughput is output MB /
wall-clock seconds, best of --reps. A single frame (pack/all) cannot be split,
so its N-process number is effectively single-threaded.

Writes zstd-dict-mtd-<name>.json, read by zstd_dict_batch.py --threads N.

Usage (from scripts/): .venv/bin/python zstd_dict_mtd.py zstd-dict-rustdoc.json [--level 3] [--threads 4]
"""
import argparse, json, multiprocessing as mp, time
from pathlib import Path

import zstd_dict_bench as bench
import zstandard as zstd

RAW = []    # uncompressed frames, filled before forking the compression pool
COMP = []   # compressed frames, filled before forking the decompression pool
DICTS = {}


def tasks_for(frames, target=1 << 20):
    out, lo, n = [], 0, 0
    for i, f in enumerate(frames):
        n += len(f)
        if n >= target:
            out.append((lo, i + 1))
            lo, n = i + 1, 0
    if lo < len(frames):
        out.append((lo, len(frames)))
    return out


def compress(args):
    level, dict_path, lo, hi = args
    c = bench.Compressor(level, Path(dict_path).read_bytes() if dict_path else None)
    return lo, [c.compress(f) for f in RAW[lo:hi]]


def dctx(dict_path):
    if dict_path not in DICTS:
        d = zstd.ZstdCompressionDict(Path(dict_path).read_bytes()) if dict_path else None
        DICTS[dict_path] = zstd.ZstdDecompressor(dict_data=d)
    return DICTS[dict_path]


def decompress(args):
    dict_path, lo, hi = args
    d = dctx(dict_path)
    return sum(len(d.decompress(c)) for c in COMP[lo:hi])


def main():
    global RAW, COMP
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('result', type=Path)
    ap.add_argument('--level', type=int, default=3)
    ap.add_argument('--threads', type=int, default=4)
    ap.add_argument('--reps', type=int, default=3)
    opts = ap.parse_args()

    j = json.loads(opts.result.read_text())
    corpus = Path(__file__).parent / j['corpus']  # absolute paths stay as they are
    paths = sorted({p for g in j['glob'].split(',') for p in corpus.rglob(g) if p.is_file()})
    data = [p.read_bytes() for p in paths]
    raw = sum(map(len, data))
    if raw != j['raw']:
        raise SystemExit(f'{opts.result}: corpus changed since the run')
    cache = Path(__file__).parent / 'cache' / f"{j['fingerprint']}-zstd{j['libzstd']}"
    tag = f"-s{j['seed']}-f{j['train_frac']}-k{j['k']}d{j['d']}"
    name = opts.result.stem.removeprefix('zstd-dict-')
    out = {'corpus': j['corpus'], 'level': opts.level, 'threads': opts.threads, 'results': []}

    rs = sorted((r for r in j['results'] if r['level'] == opts.level), key=lambda r: (r['pack'], r['dict_size']))
    for r in rs:
        RAW, cur = [], []
        for x in data:
            if cur and sum(map(len, cur)) + len(x) > r['pack']:
                RAW.append(b''.join(cur))
                cur = []
            cur.append(x)
        RAW.append(b''.join(cur))
        dpath = str(cache / f"dict-L{opts.level}-{bench.fmt_size(r['dict_size'])}{tag}.bin") if r['dict_size'] else None
        tasks = tasks_for(RAW)
        COMP = [None] * len(RAW)
        with mp.get_context('fork').Pool() as pool:
            for lo, cs in pool.imap_unordered(compress, [(opts.level, dpath, lo, hi) for lo, hi in tasks]):
                COMP[lo:lo + len(cs)] = cs
        if sum(map(len, COMP)) != r['total']:
            raise SystemExit(f"{r['name']}: compressed size {sum(map(len, COMP))} != {r['total']}")
        RAW = []
        speeds = {}
        for t in (1, opts.threads):
            best = float('inf')
            with mp.get_context('fork').Pool(t) as pool:
                pool.map(decompress, [(dpath, 0, 1)] * t)  # warm up
                for _ in range(opts.reps):
                    t0 = time.perf_counter()
                    n = sum(pool.imap_unordered(decompress, [(dpath, lo, hi) for lo, hi in tasks]))
                    best = min(best, time.perf_counter() - t0)
            if n != raw:
                raise SystemExit(f"{r['name']}: decompressed {n} != {raw}")
            speeds[t] = raw / best / 1e6
        out['results'].append({'name': r['name'], 'pack': r['pack'], 'dict_size': r['dict_size'],
                               'extract1': speeds[1], 'extractN': speeds[opts.threads]})
        print(f"{name} {r['name']}: {speeds[1]:,.0f} -> {speeds[opts.threads]:,.0f} MB/s", flush=True)
    (Path(__file__).parent / f'zstd-dict-mtd-{name}.json').write_text(json.dumps(out, indent=1))


if __name__ == '__main__':
    main()
