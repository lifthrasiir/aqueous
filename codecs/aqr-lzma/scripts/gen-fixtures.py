#!/usr/bin/env python3
"""Writes tests/data/*.lzma: deterministic inputs compressed by xz (XZ Utils) in its LZMA-alone
format, in fast and normal modes and with other literal and position bits; tests/data/*.lzma2:
raw LZMA2 streams of them (and of an input mixing text with random bytes, for stored chunks and
several LZMA chunks); tests/data/*.xz: .xz files of every check, of several blocks, of several
streams with padding between.

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

def mixed():
    """Letters (coded in several chunks), random bytes (stored chunks), then letters again."""
    r = random.Random(2)
    letters = lambda n: bytes(r.choice(b'etaoinshrdlucmfwyp') for _ in range(n))
    return letters(150000) + bytes(r.randrange(256) for _ in range(100000)) + letters(50000)

VARIANTS = {
    'p0': ['-0'],
    'p1': ['-1'],
    'p6': ['-6'],
    'p9e': ['-9e'],
    'lc0lp2pb0': ['--lzma1=preset=6,lc=0,lp=2,pb=0'],
    'lc4pb4': ['--lzma1=preset=6,lc=4,pb=4'],
    'dict4k': ['--lzma1=preset=6,dict=4KiB'],
}

LZMA2 = {
    'p0': ['--lzma2=preset=0'],
    'p6': ['--lzma2=preset=6'],
    'p9e': ['--lzma2=preset=9e'],
    'lc0lp2pb0': ['--lzma2=preset=6,lc=0,lp=2,pb=0'],
    'lc4pb4': ['--lzma2=preset=6,lc=4,pb=4'],
    'dict4k': ['--lzma2=preset=6,dict=4KiB'],
}

XZ = {
    'crc32': ['--check=crc32'],
    'crc64': ['--check=crc64'],
    'sha256': ['--check=sha256'],
    'none': ['--check=none'],
    # blocks of 64 KiB, with their sizes in the block headers as multithreaded xz writes them
    'blocks': ['-T2', '--block-size=64KiB'],
}

def xz(data, args):
    return subprocess.run(['xz', '-c'] + args, input=data, capture_output=True, check=True).stdout

def write(name, data):
    with open(os.path.join(OUT, name), 'wb') as f:
        f.write(data)

def main():
    os.makedirs(OUT, exist_ok=True)
    ins = inputs()
    for name, data in list(ins.items()) + [('mixed', mixed())]:
        for v, args in LZMA2.items():
            if name in ('empty', 'one', 'mixed') and v != 'p6':
                continue
            write(f'{name}-{v}.lzma2', xz(data, ['--format=raw', '-T1'] + args))
    for v, args in XZ.items():
        data = mixed() if v == 'blocks' else ins['text']
        write(f'{v}.xz', xz(data, (['-T1'] if v != 'blocks' else []) + args))
    # two streams with 8 bytes of padding between and 4 after, and an empty file's stream
    write('streams.xz', xz(ins['text'], ['-T1']) + bytes(8) + xz(ins['binary'], ['-T1', '-1']) + bytes(4))
    write('empty.xz', xz(b'', ['-T1']))
    for name, data in inputs().items():
        for v, args in VARIANTS.items():
            if name in ('empty', 'one') and v != 'p6':
                continue
            out = subprocess.run(['xz', '--format=lzma', '-T1', '-c'] + args, input=data, capture_output=True, check=True).stdout
            write(f'{name}-{v}.lzma', out)

if __name__ == '__main__':
    main()
