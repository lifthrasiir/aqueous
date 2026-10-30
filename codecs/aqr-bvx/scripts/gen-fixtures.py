"""Writes tests/data: streams of Apple's libcompression (macOS) used as a black box, of
synthetic data and of this repository's own text. Run with `python3 -I`, from anywhere."""

import ctypes
import os
import random

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, '..', 'tests', 'data')
ROOT = os.path.join(HERE, '..', '..', '..')

LZFSE, LZVN = 0x801, 0x900

lib = ctypes.CDLL('/usr/lib/libcompression.dylib')
for f in (lib.compression_encode_buffer, lib.compression_decode_buffer):
    f.restype = ctypes.c_size_t
    f.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_int]


def encode(d, alg):
    cap = len(d) + len(d) // 2 + 4096
    b = ctypes.create_string_buffer(cap)
    n = lib.compression_encode_buffer(b, cap, d, len(d), None, alg)
    assert n > 0 or not d
    return b.raw[:n]


def decode(s, n, alg):
    b = ctypes.create_string_buffer(n + 64)
    m = lib.compression_decode_buffer(b, n + 64, s, len(s), None, alg)
    return b.raw[:m]


def words(rng, n):
    vocab = [''.join(rng.choice('etaoinshrdlucmfw') for _ in range(rng.randint(2, 9))) for _ in range(400)]
    out = []
    while sum(map(len, out)) < n:
        out.append(rng.choice(vocab) + rng.choice('  \n,.'))
    return ''.join(out).encode()[:n]


def pixels(rng, w, h):
    # rows of BGRA pixels: transparent borders, gradients and noise, as an icon's
    out = bytearray()
    for y in range(h):
        for x in range(w):
            if abs(x - w // 2) + abs(y - h // 2) > w // 2:
                out += b'\0\0\0\0'
            else:
                out += bytes([(x * 3) & 255, (y * 5) & 255, (x + y + rng.randint(0, 3)) & 255, 255])
    return bytes(out)


def main():
    rng = random.Random(1)
    spec = open(os.path.join(ROOT, 'spec', 'index.bs'), 'rb').read()
    cases = {
        'empty': b'',
        'stored': bytes(rng.randrange(256) for _ in range(100)),
        'small': spec[:3000],
        'text': spec[:120000],
        'words': words(rng, 300000),
        'icon': pixels(rng, 160, 160),
    }
    os.makedirs(OUT, exist_ok=True)
    for name, d in cases.items():
        s = encode(d, LZFSE)
        assert decode(s, len(d), LZFSE) == d
        open(os.path.join(OUT, name + '.lzfse'), 'wb').write(s)
        print(name, len(d), '->', len(s), sorted({s[i + 3:i + 4].decode() for i in range(len(s)) if s[i:i + 3] == b'bvx'}))


main()
