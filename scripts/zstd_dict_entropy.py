#!/usr/bin/env python3
"""What the entropy tables of a formatted zstd dictionary are worth.

A formatted dictionary is Magic | Dictionary_ID | Entropy_Tables | Repeat_Offsets
| Content (RFC 8878, Section 5). This compares, for each dictionary trained by
zstd_dict_bench.py, the dictionary as is ("full") against its Content alone
loaded as a raw content dictionary ("raw"), with the same file-aligned packing
and the same zstd configuration as zstd_dict_bench.py. The dictionary itself,
zstd-compressed at the same level, is counted in both.

It reuses the trained dictionaries cached by zstd_dict_bench.py, so the corpus
fingerprint has to match (run zstd-dict-run-all.sh first otherwise).

Usage (from scripts/): .venv/bin/python zstd_dict_entropy.py zstd-dict-<name>.json... [--packs ...]
"""

import argparse
import ctypes
import hashlib
import json
import os
import sys
from multiprocessing import Pool
from pathlib import Path

import zstandard as zstd

from zstd_dict_bench import Compressor, fmt_size, libzstd, parse_size, read_frame, zstd_version

_CTX = {}


def dict_header_size(d):
    lib = libzstd()
    f = lib.ZDICT_getDictHeaderSize
    f.restype, f.argtypes = ctypes.c_size_t, [ctypes.c_char_p, ctypes.c_size_t]
    n = f(d, len(d))
    if lib.ZSTD_isError(n):
        raise RuntimeError(f'not a formatted dictionary ({len(d)} bytes)')
    return n


def variant_bytes(dict_path, variant):
    d = Path(dict_path).read_bytes()
    return d if variant == 'full' else d[dict_header_size(d):]


def contexts(level, dict_path, variant):
    key = (level, dict_path, variant)
    if key not in _CTX:
        d = variant_bytes(dict_path, variant)
        # A raw content never starts with the dictionary magic here (it is Content of a trained
        # dictionary), so libzstd's auto detection in ZSTD_createCDict picks the intended type.
        assert variant == 'full' or d[:4] != b'\x37\xa4\x30\xec'
        dtype = zstd.DICT_TYPE_FULLDICT if variant == 'full' else zstd.DICT_TYPE_RAWCONTENT
        _CTX[key] = (Compressor(level, d), zstd.ZstdDecompressor(dict_data=zstd.ZstdCompressionDict(d, dict_type=dtype)))
    return _CTX[key]


def task_size(args):
    key, level, dict_path, variant, frames = args
    cctx, dctx = contexts(level, dict_path, variant)
    total = 0
    for paths in frames:
        data, _ = read_frame(paths)
        c = cctx.compress(data)
        if dctx.decompress(c) != data:
            raise AssertionError(f'round trip failed: {paths[0]}')
        total += len(c)
    return key, total


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('results', nargs='+', type=Path, help='zstd_dict_bench.py --out files')
    ap.add_argument('--level', type=int, default=3)
    ap.add_argument('--packs', default='0,4K,16K,64K,256K')
    ap.add_argument('--max-dict', default='1M')
    ap.add_argument('-j', '--jobs', type=int, default=os.cpu_count())
    ap.add_argument('--cache', type=Path, default=Path(__file__).parent / 'cache')
    opts = ap.parse_args()
    packs = [parse_size(x) for x in opts.packs.split(',')]
    max_dict = parse_size(opts.max_dict)
    zver = zstd_version()
    here = Path(__file__).parent

    print(f'Level {opts.level}, libzstd {zver}. Sizes include the dictionary compressed at the same level; '
          f'"tables" is the formatted dictionary header (Magic to Repeat_Offsets).\n')
    print('| dataset | dict | tables | pack | full | raw | raw vs full |')
    print('|---|--:|--:|--:|--:|--:|--:|')
    for rf in opts.results:
        meta = json.loads(rf.read_text())
        corpus = Path(meta['corpus'])
        if not corpus.is_absolute():
            corpus = here / corpus
        paths = sorted({p for g in meta['glob'].split(',') for p in corpus.rglob(g) if p.is_file()})
        h = hashlib.sha256()
        for p in paths:
            h.update(f'{p.relative_to(corpus)}\0'.encode())
            h.update(hashlib.sha256(p.read_bytes()).digest())
        fp = h.hexdigest()[:16]
        if fp != meta['fingerprint']:
            print(f'{rf}: corpus fingerprint {fp} != {meta["fingerprint"]}, skipped; rerun zstd-dict-run-all.sh',
                  file=sys.stderr)
            continue
        cache = opts.cache / f'{fp}-zstd{zver}'
        tag = f'-s{meta["seed"]}-f{meta["train_frac"]}-k{meta["k"]}d{meta["d"]}'
        dict_sizes = sorted({r['dict_size'] for r in meta['results']
                             if r['level'] == opts.level and 0 < r['dict_size'] <= max_dict})
        dicts = {s: str(cache / f'dict-L{opts.level}-{fmt_size(s)}{tag}.bin') for s in dict_sizes}
        sizes = [p.stat().st_size for p in paths]
        name = rf.stem.removeprefix('zstd-dict-')

        def pack(limit):
            frames, cur, n = [], [], 0
            for p, s in zip(paths, sizes):
                if cur and n + s > limit:
                    frames.append(cur)
                    cur, n = [], 0
                cur.append(str(p))
                n += s
            frames.append(cur)
            return frames

        results, todo = {}, []
        for limit in packs:
            frames = pack(limit)
            chunks = [c for c in (frames[i::opts.jobs * 4] for i in range(opts.jobs * 4)) if c]
            for ds, dp in dicts.items():
                for variant in ('full', 'raw'):
                    key = (limit, ds, variant)
                    f = cache / f'entropy-L{opts.level}-pack{fmt_size(limit)}-dict{fmt_size(ds)}{tag}-{variant}.json'
                    if f.exists():
                        results[key] = json.loads(f.read_text())['total']
                        continue
                    results[key] = [0, len(chunks), f]
                    todo += [(key, opts.level, dp, variant, c) for c in chunks]
        if todo:
            print(f'{name}: {len(todo)} tasks', file=sys.stderr)
            with Pool(opts.jobs) as pool:
                for key, n in pool.imap_unordered(task_size, todo):
                    acc = results[key]
                    acc[0] += n
                    acc[1] -= 1
                    if acc[1] == 0:
                        acc[2].write_text(json.dumps({'total': acc[0]}))
                        results[key] = acc[0]

        comp = Compressor(opts.level)
        for ds, dp in dicts.items():
            full_d, raw_d = variant_bytes(dp, 'full'), variant_bytes(dp, 'raw')
            dcost = {'full': len(comp.compress(full_d)), 'raw': len(comp.compress(raw_d))}
            for limit in packs:
                full = results[(limit, ds, 'full')] + dcost['full']
                raw = results[(limit, ds, 'raw')] + dcost['raw']
                print(f'| {name} | {fmt_size(ds)} | {len(full_d) - len(raw_d):,} | {fmt_size(limit)} '
                      f'| {full:,} | {raw:,} | {(raw / full - 1) * 100:+.2f}% |')


if __name__ == '__main__':
    main()
