#!/usr/bin/env python3
"""Writes tests/data/*.lzma: deterministic inputs compressed by xz (XZ Utils) in its LZMA-alone
format, in fast and normal modes and with other literal and position bits.

    python3 -I scripts/gen-fixtures.py   # needs xz on the PATH
"""
import os, random, subprocess

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'tests', 'data')

def inputs():
    r = random.Random(1)
    words = [''.join(r.choice('etaoinshrdlu') for _ in range(r.randint(1, 9))) for _ in range(300)]
    text = ' '.join(r.choice(words) for _ in range(6000)).encode()[:32768]
    # Code-like bytes: a few hundred "functions", copied with small changes.
    funcs = [bytes(r.randrange(256) for _ in range(r.randint(8, 120))) for _ in range(200)]
    binary = bytearray()
    while len(binary) < 32768:
        f = bytearray(r.choice(funcs))
        if f and r.random() < 0.3:
            f[r.randrange(len(f))] = r.randrange(256)
        binary += f
    runs = b''.join(bytes([r.choice((0, 0, 0xff, r.randrange(256)))]) * r.randint(1, 600) for _ in range(150))
    return {'text': text, 'binary': bytes(binary[:32768]), 'runs': runs[:32768], 'empty': b'', 'one': b'x'}

VARIANTS = {
    'p0': ['-0'],
    'p1': ['-1'],
    'p6': ['-6'],
    'p9e': ['-9e'],
    'lc0lp2pb0': ['--lzma1=preset=6,lc=0,lp=2,pb=0'],
    'lc4pb4': ['--lzma1=preset=6,lc=4,pb=4'],
    'dict4k': ['--lzma1=preset=6,dict=4KiB'],
}

def main():
    os.makedirs(OUT, exist_ok=True)
    for name, data in inputs().items():
        for v, args in VARIANTS.items():
            if name in ('empty', 'one') and v != 'p6':
                continue
            out = subprocess.run(['xz', '--format=lzma', '-T1', '-c'] + args, input=data, capture_output=True, check=True).stdout
            with open(os.path.join(OUT, f'{name}-{v}.lzma'), 'wb') as f:
                f.write(out)

if __name__ == '__main__':
    main()
