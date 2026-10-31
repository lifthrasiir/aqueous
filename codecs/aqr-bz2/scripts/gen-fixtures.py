"""Writes tests/data: streams of the reference libbzip2 (through Python's bz2 module) used as
a black box, of synthetic data and of this repository's own text, and the code lengths
libbzip2's `BZ2_hbMakeCodeLengths` gives (through the shared libbz2 of the system, or the one
at `LIBBZ2`, which must be 1.0.3 or later). Run with `python -I`, from anywhere. Names end in
`-<level>`."""

import bz2
import ctypes
import ctypes.util
import os
import random

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, '..', 'tests', 'data')
ROOT = os.path.join(HERE, '..', '..', '..')


def runs(rng, n):
    # runs of 1 to 300 of a few bytes: RLE1 at every length, and runs under way at the cuts
    out = bytearray()
    while len(out) < n:
        out += bytes([rng.choice(b'abcd\0\xff')]) * rng.choice([1, 2, 3, 4, 5, 250, 254, 255, 256, 259, 300, rng.randint(1, 300)])
    return bytes(out[:n])


def skewed(rng, n):
    # symbols of probability 2^-(k+1): deep Huffman trees (code-lengths.txt has deeper ones)
    out = bytearray()
    for _ in range(n):
        k = 0
        while rng.random() < 0.5 and k < 40:
            k += 1
        out.append(k * 5)
    return bytes(out)


def code_lengths(rng):
    # frequencies of every kind, Fibonacci ones (deeper than 17 bits) among them, a case a line:
    # the frequencies, then the lengths. They add up to at most 2^20, as a block's symbols do
    # (libbzip2's weights, 256 times them, would overflow far beyond)
    path = os.environ.get('LIBBZ2') or ctypes.util.find_library('bz2')
    if not path:
        raise SystemExit("no shared libbz2 found: set LIBBZ2 to its path (on Windows, MSYS2's libbz2-1.dll)")
    lib = ctypes.CDLL(path)
    fib = [1, 1]
    while len(fib) < 28:
        fib.append(fib[-1] + fib[-2])
    cases = [fib[:a] for a in (3, 18, 19, 22, 25, 28)] + [fib[::-1], [0] * 10 + fib[:26]]
    for _ in range(60):
        a = rng.randint(3, 258)
        kind = rng.randrange(4)
        if kind == 0:
            f = [rng.randrange(1000) for _ in range(a)]
        elif kind == 1:
            f = [int(4000 * 0.55 ** rng.randrange(40)) for _ in range(a)]
        elif kind == 2:
            f = [rng.choice([0, 0, 0, 1, 2, 50, 5000]) for _ in range(a)]
        else:
            f = [rng.randrange(3) * 7 for _ in range(a)]
        cases.append(f)
    lines = []
    for f in cases:
        n = len(f)
        ln = (ctypes.c_ubyte * n)()
        lib.BZ2_hbMakeCodeLengths(ln, (ctypes.c_int32 * n)(*f), n, 17)
        lines.append(' '.join(map(str, f)) + ';' + ' '.join(map(str, ln)))
    open(os.path.join(OUT, 'code-lengths.txt'), 'w', newline='\n').write('\n'.join(lines) + '\n')


def main():
    rng = random.Random(1)
    spec = open(os.path.join(ROOT, 'spec', 'index.bs'), 'rb').read()
    cases = {
        'empty-9': b'',
        'one-9': b'a',
        'three-9': b'aaa',
        'four-9': b'aaaa',
        'small-9': spec[:3000],
        'text-5': spec[:60000],
        'text-1': spec[:330000],
        'runs-1': runs(rng, 260000),
        'zeros-1': bytes(255 * 1000),
        'periodic-1': b'abcabcab' * 3000,
        'random-1': bytes(rng.randrange(256) for _ in range(30000)),
        'skewed-2': skewed(rng, 150000),
    }
    os.makedirs(OUT, exist_ok=True)
    for name, d in cases.items():
        s = bz2.compress(d, int(name.rsplit('-', 1)[1]))
        assert bz2.decompress(s) == d
        open(os.path.join(OUT, name + '.bz2'), 'wb').write(s)
        print(name, len(d), '->', len(s))
    code_lengths(rng)


main()
