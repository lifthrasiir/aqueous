#!/usr/bin/env python3
"""Writes the fixtures of tests/data: LZ4 blocks and frames made by the reference liblz4
(1.10.0, 64-bit little endian), which aqr-lz4 must reproduce bit for bit.

- `<name>-a<A>.mozlz4`: Firefox's container around `LZ4_compress_fast(.., A)`.
- `<name>.lz4`: `LZ4F_compressFrame` with the preferences in the table below.

The plaintexts are generated here, deterministically; the tests decode them back from the
fixtures, so only the fixtures are in the repository.

    python3 aqr-lz4/scripts/gen-fixtures.py [LIBLZ4]
"""

import ctypes, ctypes.util, random, struct, sys
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / 'tests' / 'data'

lib = ctypes.CDLL(sys.argv[1] if len(sys.argv) > 1 else
                  ctypes.util.find_library('lz4') or '/opt/homebrew/lib/liblz4.dylib')
lib.LZ4_versionString.restype = ctypes.c_char_p
if lib.LZ4_versionString() != b'1.10.0':
    sys.exit(f'liblz4 1.10.0 wanted, not {lib.LZ4_versionString().decode()}')
c_int, c_size_t, c_char_p, c_void_p = ctypes.c_int, ctypes.c_size_t, ctypes.c_char_p, ctypes.c_void_p
lib.LZ4_compress_fast.argtypes = [c_char_p, c_char_p, c_int, c_int, c_int]
lib.LZ4_compressBound.argtypes = [c_int]
lib.LZ4F_compressFrameBound.argtypes, lib.LZ4F_compressFrameBound.restype = [c_size_t, c_void_p], c_size_t
lib.LZ4F_compressFrame.argtypes = [c_char_p, c_size_t, c_char_p, c_size_t, c_void_p]
lib.LZ4F_compressFrame.restype = c_size_t
lib.LZ4F_isError.argtypes = [c_size_t]

class Prefs(ctypes.Structure):
    _fields_ = [('blockSizeID', c_int), ('blockMode', c_int), ('contentChecksumFlag', c_int),
                ('frameType', c_int), ('contentSize', ctypes.c_ulonglong), ('dictID', ctypes.c_uint),
                ('blockChecksumFlag', c_int), ('compressionLevel', c_int), ('autoFlush', ctypes.c_uint),
                ('favorDecSpeed', ctypes.c_uint), ('reserved', ctypes.c_uint * 3)]

def block(d, a):
    cap = lib.LZ4_compressBound(len(d))
    dst = ctypes.create_string_buffer(cap)
    n = lib.LZ4_compress_fast(d, dst, len(d), cap, a)
    assert n > 0
    return dst.raw[:n]

def frame(d, bsid, level=0, cchk=0, bchk=0, csize=0, linked=0):
    p = Prefs(blockSizeID=bsid, blockMode=0 if linked else 1, contentChecksumFlag=cchk,
              contentSize=csize, blockChecksumFlag=bchk, compressionLevel=level)
    cap = lib.LZ4F_compressFrameBound(len(d), ctypes.byref(p))
    dst = ctypes.create_string_buffer(cap)
    n = lib.LZ4F_compressFrame(dst, cap, d, len(d), ctypes.byref(p))
    assert not lib.LZ4F_isError(n)
    return dst.raw[:n]

rnd = random.Random(20261023)
WORDS = 'lorem ipsum dolor sit amet tab url title favicon session window closed entry'.split()

def records(n):
    """JSON-ish lines: repeated keys, slowly varying numbers and a few words."""
    out, i, t = bytearray(), rnd.randrange(1000), 1790000000000
    while len(out) < n:
        i += rnd.randrange(1, 3)
        t += rnd.randrange(100)
        out += (f'{{"id":{i},"title":"{" ".join(rnd.choices(WORDS, k=rnd.randrange(1, 4)))}",'
                f'"t":{t},"open":{str(rnd.random() < .2).lower()}}},\n').encode()
    return bytes(out[:n])

def noise(n):
    return bytes(rnd.randrange(256) for _ in range(n))

def runs(n):
    """Long runs of bytes, for long matches and overlapping copies."""
    out = bytearray()
    while len(out) < n:
        out += bytes([rnd.randrange(4)]) * rnd.randrange(1, 3000)
    return bytes(out[:n])

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for f in OUT.iterdir():
        f.unlink()
    small = records(30000)
    large = records(70000)
    texts = {
        'tiny0': b'', 'tiny1': b'x', 'tiny12': small[:12], 'tiny13': small[:13], 'tiny20': small[:20],
        # 65547 bytes is where the table of 16-bit positions gives way to one of 32-bit ones
        'edge65546': records(65546), 'edge65547': records(65547),
        'small': small, 'large': large, 'runs': runs(100000),
        'mixed': records(40000) + noise(3000) + records(40000),
        'noise': noise(5000),
    }
    for name, d in texts.items():
        for a in {'small': [1, 2, 5, 33, 65537], 'large': [1, 33]}.get(name, [1]):
            (OUT / f'{name}-a{a}.mozlz4').write_bytes(b'mozLz40\0' + struct.pack('<I', len(d)) + block(d, a))
    big = runs(2 * 262144 - 20000) + records(21500)
    frames = {
        'f64k': frame(records(140000), 4),
        # a block under 4 KiB after full ones of 256 KiB: the table is kept, not cleared
        'f256k-checks': frame(big, 5, cchk=1, bchk=1, csize=1),
        # a last block of noise, stored
        'f64k-level-3': frame(records(131072) + noise(5000), 4, level=-3, cchk=1),
        # 4 MiB asked for, 256 KiB written: the frame is cut to the blocks it needs
        'f4m-shrunk': frame(records(100000), 7),
        'fempty': frame(b'', 4, cchk=1),
        'f1m-runs': frame(runs(1200000), 6, bchk=1),
        # linked blocks: not reproduced
        'f64k-linked': frame(records(80000), 4, linked=1),
    }
    for name, d in frames.items():
        (OUT / f'{name}.lz4').write_bytes(d)
    total = sum(f.stat().st_size for f in OUT.iterdir())
    print(f'{len(list(OUT.iterdir()))} fixtures, {total} bytes')

main()
