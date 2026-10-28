#!/usr/bin/env python3
"""Writes the fixtures of tests/data: LZO1X streams made by the reference liblzo2 (2.10) and
lzop files made by lzop (1.04), which aqr-lzo must rebuild bit for bit from their data and
records. Both are used as black boxes only: LZO is GPL, and aqr-lzo is written without its
code.

- `<name>.<compressor>.lzo1x`: a raw stream of `lzo1x_<compressor>_compress` (1, 1_11, 1_12,
  1_15) or of `lzo1x_999_compress_level` (999-<level>).
- `<name>.<level>[-crc].lzo`: `lzop -<level>` (with `--crc32`).

The plaintexts are generated here, deterministically; the tests decode them back from the
fixtures, so only the fixtures are in the repository.

    python3 -I codecs/aqr-lzo/scripts/gen-fixtures.py [LIBLZO2]
"""

import ctypes, random, subprocess, sys, tempfile
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / 'tests' / 'data'

lib = ctypes.CDLL(sys.argv[1] if len(sys.argv) > 1 else '/opt/homebrew/opt/lzo/lib/liblzo2.dylib')
lib.lzo_version.restype = ctypes.c_uint
if lib.lzo_version() != 0x20a0:
    sys.exit(f'liblzo2 2.10 wanted, not {lib.lzo_version():#x}')
U = ctypes.c_size_t
WORK = ctypes.create_string_buffer(1 << 22)  # more than any compressor's work memory


def compress(name, d):
    dst = ctypes.create_string_buffer(len(d) + len(d) // 16 + 64 + 3)
    n = U(0)
    if name.startswith('999-'):
        r = lib.lzo1x_999_compress_level(d, U(len(d)), dst, ctypes.byref(n), WORK, None, U(0), None,
                                         ctypes.c_int(int(name[4:])))
    else:
        r = getattr(lib, f'lzo1x_{name}_compress')(d, U(len(d)), dst, ctypes.byref(n), WORK)
    assert r == 0, (name, r)
    return dst.raw[:n.value]


def lzop(d, *opts):
    with tempfile.TemporaryDirectory() as t:
        p = Path(t) / 'file'
        p.write_bytes(d)
        return subprocess.run(['lzop', '-c', *opts, str(p)], check=True, capture_output=True).stdout


rnd = random.Random(20261028)
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


def table(n):
    """Rows of little-endian integers: short matches at all distances, far ones beyond 16 KiB."""
    out, x = bytearray(), 0
    while len(out) < n:
        x = (x + rnd.choice([1, 1, 2, 3, 1000])) % 70000
        out += x.to_bytes(4, 'little') + bytes([rnd.randrange(4)]) * 2 + rnd.choice(WORDS).encode()[:2]
    return bytes(out[:n])


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for f in OUT.iterdir():
        f.unlink()
    texts = {
        'empty': b'', 'one': b'x', 'three': b'abc', 'four': b'abcd', 'ten': b'abcdabcdab',
        'records': records(30000), 'table': table(30000), 'runs': runs(30000),
        'mixed': records(10000) + noise(2000) + table(10000) + runs(5000),
        'noise': noise(3000),
    }
    for name, d in texts.items():
        for c in ['1', '1_11', '1_12', '1_15', '999-1', '999-5', '999-9']:
            if len(d) < 1000 and c not in ('1', '999-9'):
                continue
            (OUT / f'{name}.{c}.lzo1x').write_bytes(compress(c, d))
    # A block of 256 KiB, then a short one of noise (stored).
    big = records(150000) + table(62144) + runs(50000) + noise(4000)
    for level in [1, 7, 9]:
        (OUT / f'big.{level}.lzo').write_bytes(lzop(big, f'-{level}'))
    (OUT / 'big.3-crc.lzo').write_bytes(lzop(big, '-3', '--crc32'))
    (OUT / 'empty.3.lzo').write_bytes(lzop(b'', '-3'))
    (OUT / 'mixed.9.lzo').write_bytes(lzop(texts['mixed'], '-9'))
    total = sum(f.stat().st_size for f in OUT.iterdir())
    print(f'{len(list(OUT.iterdir()))} fixtures, {total} bytes')


main()
