#!/usr/bin/env python3
"""Writes the fixtures of tests/data: Snappy streams and LevelDB tables made by the reference
Snappy 1.2.2, which aqr-snappy must reproduce bit for bit, with either hash.

- `<name>-<hash>.snappy`: `snappy_compress`.
- `<name>-<hash>.ldb`: LevelDB tables written as LevelDB's `TableBuilder` does, with
  `snappy_compress` on their blocks.

`<hash>` is `mul` or `crc`, by how the library hashes: a build with
`-DSNAPPY_HAVE_NEON_CRC32=0` (and `-DSNAPPY_HAVE_X86_CRC32=0`) for `mul`, and a build using the
CRC-32C instruction (Homebrew's on Apple silicon, any built for SSE4.2) for `crc`. The
plaintexts are generated here, deterministically.

    python3 codecs/aqr-snappy/scripts/gen-fixtures.py MUL_LIB CRC_LIB
"""

import ctypes, random, struct, sys
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / 'tests' / 'data'

def load(path):
    lib = ctypes.CDLL(path)
    lib.snappy_compress.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p, ctypes.POINTER(ctypes.c_size_t)]
    lib.snappy_max_compressed_length.argtypes = [ctypes.c_size_t]
    lib.snappy_max_compressed_length.restype = ctypes.c_size_t
    def compress(d):
        n = ctypes.c_size_t(lib.snappy_max_compressed_length(len(d)))
        dst = ctypes.create_string_buffer(n.value)
        assert lib.snappy_compress(d, len(d), dst, ctypes.byref(n)) == 0
        return dst.raw[:n.value]
    return compress

if len(sys.argv) != 3:
    sys.exit(__doc__)
LIBS = {'mul': load(sys.argv[1]), 'crc': load(sys.argv[2])}

def crc32c(d, crc=0):
    crc ^= 0xffffffff
    for b in d:
        crc ^= b
        for _ in range(8):
            crc = crc >> 1 ^ (0x82f63b78 if crc & 1 else 0)
    return crc ^ 0xffffffff

def masked(d):
    c = crc32c(d)
    return ((c >> 15 | c << 17) + 0xa282ead8) & 0xffffffff

def varint(v):
    out = bytearray()
    while v >= 0x80:
        out.append(v & 0x7f | 0x80)
        v >>= 7
    out.append(v)
    return bytes(out)

def block(entries, restart=16):
    """A LevelDB block (`BlockBuilder`): prefix-compressed entries, then the restart points."""
    out, restarts, last = bytearray(), [], b''
    for i, (k, v) in enumerate(entries):
        shared = 0
        if i % restart == 0:
            restarts.append(len(out))
        else:
            while shared < min(len(k), len(last)) and k[shared] == last[shared]:
                shared += 1
        out += varint(shared) + varint(len(k) - shared) + varint(len(v)) + k[shared:] + v
        last = k
    for r in restarts or [0]:
        out += struct.pack('<I', r)
    return bytes(out + struct.pack('<I', len(restarts or [0])))

def handle(off, size):
    return varint(off) + varint(size)

def table(blocks, compress, odd=None):
    """A table of data blocks (lists of entries), as `TableBuilder` writes it: a data block is
    stored compressed where that saves 1/8, a filter block raw, then the metaindex and the index.
    With `odd`, the data block of that index is compressed by `odd` instead."""
    out, index = bytearray(), []
    def write(raw, comp):
        c = comp(raw) if comp else None
        if c is not None and len(c) < len(raw) - len(raw) // 8:
            body, kind = c, 1
        else:
            body, kind = raw, 0
        at = len(out)
        out.extend(body + bytes([kind]) + struct.pack('<I', masked(body + bytes([kind]))))
        return handle(at, len(body))
    for i, entries in enumerate(blocks):
        h = write(block(entries), odd if i == odd_index(odd, blocks) else compress)
        index.append((entries[-1][0] + b'\xff', h))
    fh = write(bytes(range(256)) * 3 + b'\x0b', None)  # a filter block, raw
    meta = write(block([(b'filter.leveldb.BuiltinBloomFilter2', fh)]), compress)
    ix = write(block(index, restart=1), compress)
    footer = meta + ix
    return bytes(out + footer + bytes(40 - len(footer)) + struct.pack('<Q', 0xdb4775248b80fb57))

def odd_index(odd, blocks):
    return len(blocks) // 2 if odd else -1

rnd = random.Random(20261023)
WORDS = 'origin key value session local storage indexeddb extension state token'.split()

def records(n):
    out, i, t = bytearray(), rnd.randrange(1000), 1790000000000
    while len(out) < n:
        i += rnd.randrange(1, 3)
        t += rnd.randrange(100)
        out += (f'{{"id":{i},"name":"{" ".join(rnd.choices(WORDS, k=rnd.randrange(1, 4)))}",'
                f'"t":{t},"on":{str(rnd.random() < .2).lower()}}},\n').encode()
    return bytes(out[:n])

def noise(n):
    return bytes(rnd.randrange(256) for _ in range(n))

def runs(n):
    out = bytearray()
    while len(out) < n:
        out += bytes([rnd.randrange(4)]) * rnd.randrange(1, 3000)
    return bytes(out[:n])

def entries(n, value):
    """Entries of about 4 KiB per block, as LevelDB cuts them, of `n` blocks."""
    blocks, key = [], 0
    for _ in range(n):
        es, size = [], 0
        while size < 4096:
            key += rnd.randrange(1, 9)
            k = b'_https://site.example\x00\x01key%08d' % key
            v = value(rnd.randrange(20, 400))
            es.append((k, v))
            size += len(k) + len(v)
        blocks.append(es)
    return blocks

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for f in OUT.iterdir():
        f.unlink()
    texts = {
        **{f'tiny{n}': records(n) for n in [0, 1, 14, 15, 16, 31, 32, 33, 300]},
        'small': records(20000), 'fragments': records(150000), 'runs': runs(140000),
        'mixed': records(30000) + noise(3000) + records(30000), 'noise': noise(5000),
    }
    for hash, comp in LIBS.items():
        for name, d in texts.items():
            (OUT / f'{name}-{hash}.snappy').write_bytes(comp(d))
    for hash, comp in LIBS.items():
        (OUT / f'table-{hash}.ldb').write_bytes(table(entries(12, records), comp))
    # blocks of noise, stored raw; a block of 70 KB, two fragments
    (OUT / 'mixed-mul.ldb').write_bytes(
        table(entries(3, records) + entries(2, noise) + [[(b'big', records(70000))]] + entries(2, records), LIBS['mul']))
    # one block by the other hash: kept as it is
    (OUT / 'odd-crc.ldb').write_bytes(table(entries(6, records), LIBS['crc'], odd=LIBS['mul']))
    total = sum(f.stat().st_size for f in OUT.iterdir())
    print(f'{len(list(OUT.iterdir()))} fixtures, {total} bytes')

main()
