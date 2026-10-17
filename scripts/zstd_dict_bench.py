#!/usr/bin/env python3
"""Solid compression, per-file zstd with a trained dictionary, and their mix.

Files (sorted by path) are packed greedily into frames of at most N bytes,
never splitting a file; a file larger than N gets a frame of its own. Every
frame may additionally use a shared trained dictionary. So the grid

  pack/0          one frame per file (classic per-file compression)
  pack/<N>        solid frames of up to N bytes, file-aligned
  pack/all        everything in one frame (fully solid)
  ... +dict/<D>   the same, every frame compressed with a D-byte dictionary;
                  the dictionary (zstd-compressed at the same level) is counted

covers both extremes and the hybrids in between. Every method uses the same
zstd configuration per level (single-threaded, no checksum, content size
written, dictionary ID omitted, dictionary digested once).

Compression calls libzstd directly (ctypes) with ZSTD_createCDict(dict, level)
+ ZSTD_CCtx_refCDict, the plain API. python-zstandard only builds CDicts through
the advanced API, which leaves the CDict without a level; libzstd then always
attaches/copies the dictionary tables, even into a 500 MB frame, which at the
binary-tree levels (15+) costs up to 30% (a 16-byte dictionary doubles the
output). With a leveled CDict, libzstd reloads the dictionary into tables sized
for the input once the input is >= 128 KB and >= 6x the dictionary.
Decompression uses python-zstandard.

Two passes:

- Sizes: the whole corpus on all cores, round trips verified.
- Speeds: a seeded sample of frames (--time-frac of the input) on --time-jobs
  workers (default: the number of performance cores), with all methods of the
  same N timing the same frames. Speeds are MB/s of *output file bytes*:
    comp     input bytes / compression time
    extract  input bytes / time to decompress every sampled frame once
    random   file bytes / time to fetch each file on its own, i.e. decompress
             its frame from the start up to the file's end (frames above
             --model-above are estimated from their full decompression rate)

Dictionaries are trained (fastCover) on a seeded random subset of the files.
ZDICT picks k,d once (--tune-size, --tune-level) and every size and level reuses
them: optimizing at level 19 is ~50x slower, and at 1M it picked a poor k.

Trained dictionaries and per-method results are cached under --cache, keyed by
a hash of the corpus contents, the libzstd version and the parameters.

Usage (from scripts/): .venv/bin/python zstd_dict_bench.py [--corpus DIR] [--levels 3] ...
"""

import argparse
import ctypes
import ctypes.util
import hashlib
import json
import os
import random
import subprocess
import sys
import time
from multiprocessing import Pool
from pathlib import Path

import zstandard as zstd

DEFAULT_CORPUS = Path.home() / '.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html'
ALL = 1 << 62  # pack size meaning "one frame"


def parse_size(s):
    s = s.strip().upper()
    if s == 'ALL':
        return ALL
    mult = {'K': 1 << 10, 'M': 1 << 20, 'G': 1 << 30}.get(s[-1:], 1)
    return int(s[:-1] if mult > 1 else s) * mult


def fmt_size(n):
    if n == ALL:
        return 'all'
    for unit, k in (('M', 1 << 20), ('K', 1 << 10)):
        if n >= k and n % k == 0:
            return f'{n // k}{unit}'
    return str(n)


def perf_cores():
    try:
        return int(subprocess.check_output(['sysctl', '-n', 'hw.perflevel0.physicalcpu'], text=True))
    except (OSError, subprocess.CalledProcessError, ValueError):
        return os.cpu_count()


# ---------------------------------------------------------------- workers ----

_LIB = None
_CTX = {}


def libzstd():
    global _LIB
    if _LIB is None:
        path = os.environ.get('LIBZSTD') or ctypes.util.find_library('zstd') or '/opt/homebrew/lib/libzstd.dylib'
        lib = ctypes.CDLL(path)
        vp, sz = ctypes.c_void_p, ctypes.c_size_t
        for name, res, args in [('ZSTD_versionNumber', ctypes.c_uint, []),
                                ('ZSTD_createCCtx', vp, []),
                                ('ZSTD_CCtx_setParameter', sz, [vp, ctypes.c_int, ctypes.c_int]),
                                ('ZSTD_createCDict', vp, [ctypes.c_char_p, sz, ctypes.c_int]),
                                ('ZSTD_CCtx_refCDict', sz, [vp, vp]),
                                ('ZSTD_compressBound', sz, [sz]),
                                ('ZSTD_compress2', sz, [vp, vp, sz, ctypes.c_char_p, sz]),
                                ('ZSTD_isError', ctypes.c_uint, [sz]),
                                ('ZSTD_getErrorName', ctypes.c_char_p, [sz])]:
            f = getattr(lib, name)
            f.restype, f.argtypes = res, args
        _LIB = lib
    return _LIB


def zstd_version():
    v = libzstd().ZSTD_versionNumber()
    return f'{v // 10000}.{v // 100 % 100}.{v % 100}'


class Compressor:
    """The one and only zstd configuration; the dictionary is the only variable."""

    def __init__(self, level, dict_bytes=None, workers=0):
        lib = self.lib = libzstd()
        self.cctx = lib.ZSTD_createCCtx()
        for param, value in ((100, level),  # ZSTD_c_compressionLevel
                             (200, 1),      # ZSTD_c_contentSizeFlag
                             (201, 0),      # ZSTD_c_checksumFlag
                             (202, 0),      # ZSTD_c_dictIDFlag
                             (400, workers)):  # ZSTD_c_nbWorkers (0 = single-threaded, this thread)
            self.check(lib.ZSTD_CCtx_setParameter(self.cctx, param, value))
        if dict_bytes:
            self.cdict = lib.ZSTD_createCDict(dict_bytes, len(dict_bytes), level)
            self.check(lib.ZSTD_CCtx_refCDict(self.cctx, self.cdict))
        self.buf = ctypes.create_string_buffer(0)

    def check(self, r):
        if self.lib.ZSTD_isError(r):
            raise RuntimeError(self.lib.ZSTD_getErrorName(r).decode())
        return r

    def compress(self, data):
        cap = self.lib.ZSTD_compressBound(len(data))
        if len(self.buf) < cap:
            self.buf = ctypes.create_string_buffer(cap)
        n = self.check(self.lib.ZSTD_compress2(self.cctx, self.buf, cap, data, len(data)))
        return ctypes.string_at(self.buf, n)


def contexts(level, dict_path):
    key = (level, dict_path)
    if key not in _CTX:
        raw = Path(dict_path).read_bytes() if dict_path else None
        d = zstd.ZstdCompressionDict(raw) if raw else None
        _CTX[key] = (Compressor(level, raw), zstd.ZstdDecompressor(dict_data=d))
    return _CTX[key]


def read_frame(paths):
    parts = [Path(p).read_bytes() for p in paths]
    ends, e = [], 0
    for x in parts:
        e += len(x)
        ends.append(e)
    return b''.join(parts), ends


def task_size(args):
    key, level, dict_path, frames = args
    cctx, dctx = contexts(level, dict_path)
    sizes, hdr = [], 0
    for paths in frames:
        data, _ = read_frame(paths)
        c = cctx.compress(data)
        if dctx.decompress(c) != data:
            raise AssertionError(f'round trip failed: {paths[0]}')
        sizes.append(len(c))
        hdr += zstd.frame_header_size(c)
    return key, sizes, hdr


def task_time(args):
    key, level, dict_path, frames, model_above = args
    cctx, dctx = contexts(level, dict_path)
    nbytes = ctime = dtime = rtime = 0.0
    modeled = False
    for paths in frames:
        data, ends = read_frame(paths)
        t = time.perf_counter()
        c = cctx.compress(data)
        ctime += time.perf_counter() - t
        t = time.perf_counter()
        out = dctx.decompress(c)
        full = time.perf_counter() - t
        if out != data:
            raise AssertionError(f'round trip failed: {paths[0]}')
        dtime += full
        nbytes += len(data)
        if len(data) > model_above:
            modeled = True
            rtime += sum(ends) / len(data) * full
            continue
        for e in ends:
            if e == len(data):
                rtime += full
                continue
            t = time.perf_counter()
            r = dctx.stream_reader(c)
            got = 0
            while got < e:
                b = r.read(e - got)
                if not b:
                    raise AssertionError('short read')
                got += len(b)
            rtime += time.perf_counter() - t
    return key, nbytes, ctime, dtime, rtime, modeled


# ------------------------------------------------------------------- main ----

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--corpus', type=Path, default=DEFAULT_CORPUS)
    ap.add_argument('--glob', default='*.html', help='comma-separated file name patterns (default: %(default)s)')
    ap.add_argument('--levels', default='3')
    ap.add_argument('--packs', default='0,16K,64K,256K,1M,4M,16M,all', help='frame size limits')
    ap.add_argument('--dict-sizes', default='64K,256K,1M', help='dictionaries tried besides none')
    ap.add_argument('--dict-params', default='', metavar='K,D',
                    help='fixed fastCover k,d; by default ZDICT optimizes them once at --tune-size and '
                         '--tune-level and every dictionary reuses the result')
    ap.add_argument('--tune-size', default='256K')
    ap.add_argument('--tune-level', type=int, default=3)
    ap.add_argument('--train-frac', type=float, default=0.1)
    ap.add_argument('--seed', type=int, default=1)
    ap.add_argument('--time-frac', type=float, default=0.1)
    ap.add_argument('--time-jobs', type=int, default=perf_cores())
    ap.add_argument('--model-above', default='64M',
                    help='frames larger than this get an estimated random-access time')
    ap.add_argument('--cap', type=float, default=1000, help='MB/s considered fast enough (default: %(default)s)')
    ap.add_argument('-j', '--jobs', type=int, default=os.cpu_count())
    ap.add_argument('--cache', type=Path, default=Path(__file__).parent / 'cache')
    ap.add_argument('--out', type=Path, default=Path(__file__).parent / 'zstd-dict-results.json')
    opts = ap.parse_args()

    levels = [int(x) for x in opts.levels.split(',')]
    packs = [parse_size(x) for x in opts.packs.split(',')]
    dict_sizes = [parse_size(x) for x in opts.dict_sizes.split(',')] if opts.dict_sizes else []
    model_above = parse_size(opts.model_above)

    paths = sorted({p for g in opts.glob.split(',') for p in opts.corpus.rglob(g) if p.is_file()})
    if not paths:
        sys.exit(f'no files matching {opts.glob} under {opts.corpus}')
    sizes = [p.stat().st_size for p in paths]
    raw = sum(sizes)
    h = hashlib.sha256()
    for p in paths:
        h.update(f'{p.relative_to(opts.corpus)}\0'.encode())
        h.update(hashlib.sha256(p.read_bytes()).digest())
    fp = h.hexdigest()[:16]
    zver = zstd_version()
    if zver != '.'.join(map(str, zstd.ZSTD_VERSION)):
        print(f'warning: libzstd {zver} compresses, python-zstandard\'s {zstd.ZSTD_VERSION} decompresses',
              file=sys.stderr)
    print(f'corpus {opts.corpus} ({opts.glob}): {len(paths)} files, {raw} bytes, '
          f'fingerprint {fp}; libzstd {zver}', file=sys.stderr)

    order = list(range(len(paths)))
    random.Random(opts.seed).shuffle(order)
    train = sorted(order[:int(len(order) * opts.train_frac)])

    cache = opts.cache / f'{fp}-zstd{zver}'
    cache.mkdir(parents=True, exist_ok=True)

    def tune():
        """Let ZDICT pick fastCover k,d once; it is slow at high levels and unreliable at large sizes."""
        size = parse_size(opts.tune_size)
        f = cache / f'tune-L{opts.tune_level}-{fmt_size(size)}-s{opts.seed}-f{opts.train_frac}.json'
        if not f.exists():
            print(f'  tuning k,d with a {fmt_size(size)} dictionary at level {opts.tune_level}...', file=sys.stderr)
            t = time.perf_counter()
            d = zstd.train_dictionary(size, [paths[i].read_bytes() for i in train],
                                      level=opts.tune_level, threads=opts.jobs)
            f.write_text(json.dumps({'k': d.k, 'd': d.d, 'seconds': time.perf_counter() - t}))
        r = json.loads(f.read_text())
        print(f'  k={r["k"]} d={r["d"]}', file=sys.stderr)
        return [r['k'], r['d']]

    kd = [int(x) for x in opts.dict_params.split(',')] if opts.dict_params else tune()
    tag = f'-s{opts.seed}-f{opts.train_frac}-k{kd[0]}d{kd[1]}'

    def train_dict(level, size):
        f = cache / f'dict-L{level}-{fmt_size(size)}{tag}.bin'
        if not f.exists():
            print(f'  training {fmt_size(size)} dictionary for level {level}...', file=sys.stderr)
            t = time.perf_counter()
            d = zstd.train_dictionary(size, [paths[i].read_bytes() for i in train],
                                      k=kd[0], d=kd[1], level=level, threads=opts.jobs)
            f.write_bytes(d.as_bytes())
            (cache / (f.name + '.meta.json')).write_text(json.dumps(
                {'k': d.k, 'd': d.d, 'seconds': time.perf_counter() - t}))
        return str(f)

    def pack(limit):
        """Greedy file-aligned frames of at most `limit` bytes, as lists of path strings."""
        frames, cur, n = [], [], 0
        for p, s in zip(paths, sizes):
            if cur and n + s > limit:
                frames.append(cur)
                cur, n = [], 0
            cur.append(str(p))
            n += s
        frames.append(cur)
        return frames

    size_of = dict(zip(map(str, paths), sizes))

    def frame_bytes(fr):
        return sum(size_of[p] for p in fr)

    def batches(frames, target=4 << 20):
        out, cur, n = [], [], 0
        for fr in frames:
            cur.append(fr)
            n += frame_bytes(fr)
            if n >= target:
                out.append((n, cur))
                cur, n = [], 0
        if cur:
            out.append((n, cur))
        return out

    # Configurations: (level, pack, dict size or 0).
    configs = {}
    for level in levels:
        dicts = {s: train_dict(level, s) for s in dict_sizes}
        for limit in packs:
            for ds in [0] + dict_sizes:
                name = f'L{level}-pack{fmt_size(limit)}' + (f'-dict{fmt_size(ds)}' if ds else '')
                configs[name] = {'level': level, 'pack': limit, 'dict_size': ds,
                                 'dict_path': dicts[ds] if ds else None}
    framings = {limit: pack(limit) for limit in packs}

    def timing_sample(limit):
        frames = framings[limit]
        idx = list(range(len(frames)))
        random.Random(f'{opts.seed}-{limit}').shuffle(idx)
        chosen, n = [], 0
        for i in idx:
            if n >= opts.time_frac * raw:
                break
            chosen.append(frames[i])
            n += frame_bytes(chosen[-1])
        return chosen

    def load(kind, name, suffix):
        f = cache / f'v3-{kind}-{name}{suffix}.json'
        return f, (json.loads(f.read_text()) if f.exists() else None)

    size_suffix = tag
    time_suffix = f'{tag}-t{opts.time_frac}-m{fmt_size(model_above)}'

    # Pass 1: sizes.
    todo, pending, acc = [], {}, {}
    for name, c in configs.items():
        f, r = load('size', name, size_suffix)
        if r is not None:
            c.update(r)
            continue
        bs = batches(framings[c['pack']])
        pending[name] = (f, len(bs))
        acc[name] = {'sizes': [None] * len(bs), 'header': 0, 'done': 0}
        for i, (n, b) in enumerate(bs):
            todo.append((n, (name, i), c['level'], c['dict_path'], b))
    if todo:
        print(f'  size pass: {len(pending)} configs, {len(todo)} tasks', file=sys.stderr)
        todo.sort(key=lambda t: -t[0])
        with Pool(opts.jobs) as pool:
            for (name, i), s, hdr in pool.imap_unordered(task_size, [t[1:] for t in todo]):
                a = acc[name]
                a['sizes'][i] = s
                a['header'] += hdr
                a['done'] += 1
                f, total = pending[name]
                if a['done'] == total:
                    fs = [x for chunk in a['sizes'] for x in chunk]
                    r = {'total': sum(fs), 'frames': len(fs), 'header': a['header']}
                    f.write_text(json.dumps(r))
                    configs[name].update(r)
                    print(f'    {name}: {r["total"]:,}', file=sys.stderr)

    # Pass 2: speeds.
    todo, pending, acc = [], {}, {}
    for name, c in configs.items():
        f, r = load('time', name, time_suffix)
        if r is not None:
            c.update(r)
            continue
        bs = batches(timing_sample(c['pack']))
        pending[name] = (f, len(bs))
        acc[name] = {'bytes': 0.0, 'ctime': 0.0, 'dtime': 0.0, 'rtime': 0.0, 'modeled': False, 'done': 0}
        for n, b in bs:
            todo.append((n, name, c['level'], c['dict_path'], b, model_above))
    if todo:
        print(f'  time pass: {len(pending)} configs, {len(todo)} tasks on {opts.time_jobs} workers',
              file=sys.stderr)
        todo.sort(key=lambda t: -t[0])
        with Pool(opts.time_jobs) as pool:
            for name, n, ct, dt, rt, modeled in pool.imap_unordered(task_time, [t[1:] for t in todo]):
                a = acc[name]
                a['bytes'] += n
                a['ctime'] += ct
                a['dtime'] += dt
                a['rtime'] += rt
                a['modeled'] |= modeled
                a['done'] += 1
                f, total = pending[name]
                if a['done'] == total:
                    r = {'comp': a['bytes'] / a['ctime'] / 1e6, 'extract': a['bytes'] / a['dtime'] / 1e6,
                         'random': a['bytes'] / a['rtime'] / 1e6, 'random_modeled': a['modeled'],
                         'timed_bytes': a['bytes']}
                    f.write_text(json.dumps(r))
                    configs[name].update(r)

    for c in configs.values():
        c['dict'] = 0
        if c['dict_size']:
            dbytes = Path(c['dict_path']).read_bytes()
            c['dict_raw'] = len(dbytes)
            c['dict'] = len(contexts(c['level'], None)[0].compress(dbytes))
        c['size'] = c['total'] + c['dict']
        c['ratio'] = raw / c['size']

    results = [{'name': n, **{k: v for k, v in c.items() if k != 'dict_path'}} for n, c in configs.items()]
    opts.out.write_text(json.dumps({
        'corpus': str(opts.corpus), 'glob': opts.glob, 'files': len(paths), 'raw': raw,
        'fingerprint': fp, 'libzstd': zver, 'python_zstandard': zstd.__version__, 'seed': opts.seed,
        'train_frac': opts.train_frac, 'k': kd[0], 'd': kd[1], 'time_frac': opts.time_frac,
        'time_jobs': opts.time_jobs, 'cap': opts.cap, 'results': results}, indent=1))

    # Report.
    cap = opts.cap
    dcols = [0] + dict_sizes
    dname = lambda ds: f'dict/{fmt_size(ds)}' if ds else 'no dict'

    def speed(x):
        return f'{x / 1000:.1f}G' if x >= 1000 else f'{x:.0f}' if x >= 10 else f'{x:.1f}'

    print(f'{len(paths)} files, {raw:,} bytes, mean file {raw / len(paths):,.0f} B. '
          f'Speeds in MB/s of output file bytes, measured on {opts.time_frac:.0%} of the input with '
          f'{opts.time_jobs} workers; * = random access estimated.\n')
    for level in levels:
        cs = {(c['pack'], c['dict_size']): c for c in configs.values() if c['level'] == level}
        base = cs[(ALL, 0)]['size']
        print(f'### Level {level}\n')
        print('Each cell: size vs pack/all no dict · comp · random-access MB/s\n')
        print('| pack | ' + ' | '.join(dname(d) for d in dcols) + ' |')
        print('|---|' + '--:|' * len(dcols))
        for limit in packs:
            cells = []
            for ds in dcols:
                c = cs[(limit, ds)]
                star = '*' if c['random_modeled'] else ''
                cells.append(f"{c['size'] / base:.3f}× · {speed(c['comp'])} · {speed(c['random'])}{star}")
            print(f'| {fmt_size(limit)} | ' + ' | '.join(cells) + ' |')

        # Pareto frontier over (size, capped comp, capped random access).
        pts = list(cs.values())
        key = lambda c: (c['size'], -min(c['comp'], cap), -min(c['random'], cap))

        def dominated(a):
            return any(b is not a and b['size'] <= a['size'] and min(b['comp'], cap) >= min(a['comp'], cap)
                       and min(b['random'], cap) >= min(a['random'], cap) and key(b) != key(a)
                       for b in pts)
        front = sorted((c for c in pts if not dominated(c)), key=key)
        print(f'\nPareto frontier (speeds capped at {cap:.0f} MB/s):\n')
        print('| pack | dict | size | ratio | vs pack/all | comp | extract | random |')
        print('|---|---|--:|--:|--:|--:|--:|--:|')
        for c in front:
            print(f"| {fmt_size(c['pack'])} | {dname(c['dict_size'])} | {c['size']:,} | {c['ratio']:.2f} "
                  f"| {c['size'] / base:.3f}× | {speed(c['comp'])} | {speed(c['extract'])} "
                  f"| {speed(c['random'])}{'*' if c['random_modeled'] else ''} |")
        print()


if __name__ == '__main__':
    main()
