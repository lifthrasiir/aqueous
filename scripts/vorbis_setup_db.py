#!/usr/bin/env python3
"""Builds aqr-fsb's database of Vorbis setup headers from vorbis_setups.py's output.

A setup header (Vorbis I section 4.2.4) is a list of codebooks, time placeholders, floors,
residues, mappings and modes, each a run of bits; the setups libvorbis makes share nearly all of
them. Each distinct run is kept once as a part, with a residue's 24-bit `end` field cleared: it
varies with the sample rate and quality while the rest stays. A skeleton lists a setup's parts
in order; a setup is a skeleton and its residues' ends. aqr-fsb's FORMAT.md describes the
database and how a setup is assembled; this script checks that every setup assembles back byte
for byte before writing.

Every distinct setup found contributes its parts and skeleton; only those found at the rates of
`--rows-rates` get a row (a CRC-32 to look up). The TSV beside it records, per row, how the
setup is reproduced (the oracle that made it, and the channels, rate and quality intervals) and
whether `--table` (vgmstream's vorbis_codebooks_fsb.h, by CRC only) has it.

Usage: python3 scripts/vorbis_setup_db.py RUNS_DIR --write DB --tsv TSV [--rows-rates R,...]
       [--stock RUNS_DIR...] [--table FILE]
"""

import argparse
import re
import zlib
from pathlib import Path

PART_KINDS = ('codebook', 'floor', 'residue', 'mapping', 'mode')
END_SHIFT, END_BITS = 40, 24  # a residue's end: after its 16-bit type and 24-bit begin
ROW_RATES = '8000,11000,11025,16000,22050,24000,32000,44100,48000,96000'


def ilog(v):
    return v.bit_length()


def lookup1_values(entries, dims):
    r = int(round(entries ** (1.0 / dims)))
    while (r + 1) ** dims <= entries:
        r += 1
    while r ** dims > entries:
        r -= 1
    return r


class Reader:
    """Reads LSB-first, as Vorbis packs bits."""

    def __init__(self, data):
        self.value = int.from_bytes(data, 'little')
        self.pos = 0
        self.size = len(data) * 8

    def read(self, n):
        assert self.pos + n <= self.size, 'past the end'
        v = (self.value >> self.pos) & ((1 << n) - 1)
        self.pos += n
        return v


def skip_codebook(r):
    assert r.read(24) == 0x564342, 'codebook sync'
    dims, entries = r.read(16), r.read(24)
    if r.read(1):  # ordered
        cur = 0
        r.read(5)
        while cur < entries:
            cur += r.read(ilog(entries - cur))
    else:
        sparse = r.read(1)
        for _ in range(entries):
            if not sparse or r.read(1):
                r.read(5)
    lookup = r.read(4)
    if lookup in (1, 2):
        r.read(64)
        bits = r.read(4) + 1
        r.read(1)
        r.read(bits * (lookup1_values(entries, dims) if lookup == 1 else entries * dims))
    else:
        assert lookup == 0, 'lookup type'


def skip_floor(r):
    assert r.read(16) == 1, 'floor type 0 is not handled'
    partitions = [r.read(4) for _ in range(r.read(5))]
    dims = []
    for _ in range(max(partitions) + 1 if partitions else 0):
        dims.append(r.read(3) + 1)
        subclasses = r.read(2)
        if subclasses:
            r.read(8)
        r.read(8 << subclasses)
    r.read(2)
    rangebits = r.read(4)
    r.read(rangebits * sum(dims[c] for c in partitions))


def residue(r):
    """Skips a residue; returns its end and partition size."""
    assert r.read(16) <= 2, 'residue type'
    r.read(24)
    end, partition = r.read(24), r.read(24) + 1
    nclass = r.read(6) + 1
    r.read(8)
    cascade = []
    for _ in range(nclass):
        low = r.read(3)
        cascade.append((r.read(5) if r.read(1) else 0) * 8 + low)
    r.read(8 * sum(bin(c).count('1') for c in cascade))
    return end, partition


def skip_mapping(r, channels):
    assert r.read(16) == 0, 'mapping type'
    submaps = r.read(4) + 1 if r.read(1) else 1
    if r.read(1):
        r.read(2 * ilog(channels - 1) * (r.read(8) + 1))
    assert r.read(2) == 0, 'mapping reserved'
    if submaps > 1:
        r.read(4 * channels)
    r.read(24 * submaps)


def split(data, channels):
    """The setup's parts per kind, as (bits, value) runs with residues' ends cleared, and the
    residues' (end, partition size)."""
    r = Reader(data)
    assert r.read(8) == 5 and bytes(r.read(8) for _ in range(6)) == b'vorbis', 'not a setup header'
    parts = {k: [] for k in PART_KINDS}
    ends = []

    def take(kind, skip, count):
        for _ in range(count):
            start = r.pos
            out = skip()
            n = r.pos - start
            v = (r.value >> start) & ((1 << n) - 1)
            if kind == 'residue':
                v &= ~(((1 << END_BITS) - 1) << END_SHIFT)
                ends.append(out)
            parts[kind].append((n, v))

    take('codebook', lambda: skip_codebook(r), r.read(8) + 1)
    times = r.read(6) + 1
    assert times == 1 and r.read(16) == 0, 'time placeholder'
    take('floor', lambda: skip_floor(r), r.read(6) + 1)
    take('residue', lambda: residue(r), r.read(6) + 1)
    take('mapping', lambda: skip_mapping(r, channels), r.read(6) + 1)
    take('mode', lambda: r.read(41), r.read(6) + 1)
    assert r.read(1) == 1, 'framing'
    assert (r.pos + 7) // 8 == len(data), 'trailing bytes'
    return parts, ends


def assemble(parts, ends):
    """A setup header from its parts (as split() returns them) and its residues' ends; this is
    FORMAT.md's assembly."""
    value, pos = 0, 0

    def put(n, v):
        nonlocal value, pos
        value |= v << pos
        pos += n

    put(8, 5)
    for c in b'vorbis':
        put(8, c)
    put(8, len(parts['codebook']) - 1)
    for n, v in parts['codebook']:
        put(n, v)
    put(6, 0)
    put(16, 0)
    for kind in PART_KINDS[1:]:
        put(6, len(parts[kind]) - 1)
        for i, (n, v) in enumerate(parts[kind]):
            put(n, v | (ends[i] << END_SHIFT if kind == 'residue' else 0))
    put(1, 1)
    return value.to_bytes((pos + 7) // 8, 'little')


def runs(d):
    """{crc: [(channels, rate, qlo, qhi)]} from vorbis_setups.py's runs.tsv (intervals end where
    the next one starts, the last at 1.0)."""
    lines = [line.split('\t') for line in (d / 'runs.tsv').read_text().splitlines()]
    out = {}
    for i, (r, c, q, crc) in enumerate(lines):
        if crc == '-':
            continue
        nxt = lines[i + 1] if i + 1 < len(lines) else None
        hi = float(nxt[2]) if nxt and nxt[:2] == [r, c] else 1.0
        out.setdefault(crc, []).append((int(c), int(r), float(q), hi))
    return out


def varint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)


def bits_field(n, v):
    return varint(n) + v.to_bytes((n + 7) // 8, 'little')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('runs_dir', type=Path)
    ap.add_argument('--write', type=Path, required=True)
    ap.add_argument('--tsv', type=Path, required=True)
    ap.add_argument('--rows-rates', default=ROW_RATES)
    ap.add_argument('--stock', type=Path, nargs='*', default=[], help='runs of unmodified libvorbis')
    ap.add_argument('--table', type=Path)
    args = ap.parse_args()

    found = runs(args.runs_dir)
    stock = set()
    for d in args.stock:
        stock |= set(runs(d))
    known = set(re.findall(r'vcb_([0-9a-f]{8})\[\]', args.table.read_text())) if args.table else set()
    row_rates = {int(r) for r in args.rows_rates.split(',')}

    pools = {k: {} for k in PART_KINDS}
    skeletons = {}
    setups = {}
    for crc in sorted(found):
        data = (args.runs_dir / 'setups' / f'{crc}.bin').read_bytes()
        assert '%08x' % zlib.crc32(data) == crc, crc
        channels = min(c for c, *_ in found[crc])
        parts, ends = split(data, channels)
        assert assemble(parts, [e for e, _ in ends]) == data, f'{crc} does not assemble'
        ids = tuple(tuple(pools[k].setdefault(p, len(pools[k])) for p in parts[k]) for k in PART_KINDS)
        sk = skeletons.setdefault(ids, len(skeletons))
        for (end, partition) in ends:
            assert end % partition == 0, (crc, end, partition)
        setups[crc] = (sk, [e for e, _ in ends])

    rows = sorted(crc for crc in setups if any(r in row_rates for _, r, _, _ in found[crc]))
    db = bytearray(b'AQVS\x01')
    for k in PART_KINDS:
        db += varint(len(pools[k]))
        for n, v in pools[k]:
            db += bits_field(n, v)
    db += varint(len(skeletons))
    for ids in skeletons:
        for kind_ids in ids:
            db += varint(len(kind_ids)) + b''.join(varint(i) for i in kind_ids)
    db += varint(len(rows))
    for crc in rows:
        sk, ends = setups[crc]
        db += int(crc, 16).to_bytes(4, 'little') + varint(sk) + b''.join(varint(e) for e in ends)
    args.write.parent.mkdir(parents=True, exist_ok=True)
    args.write.write_bytes(bytes(db))

    with open(args.tsv, 'w', newline='\n') as f:
        f.write('# crc\tskeleton\tends\toracle\tknown\tchannels:rate:quality intervals\n')
        for crc in rows:
            sk, ends = setups[crc]
            oracle = 'libvorbis' if crc in stock else 'fmod-rule'
            spans = ' '.join(f'{c}:{r}:{lo:.3f}..{hi:.3f}' for c, r, lo, hi in sorted(found[crc]) if r in row_rates)
            f.write(f'{crc}\t{sk}\t{",".join(map(str, ends))}\t{oracle}\t{"yes" if crc in known else "no"}\t{spans}\n')

    print(f'{len(setups)} setups assembled back; {len(rows)} rows, {len(skeletons)} skeletons')
    print('parts:', ', '.join(f'{k} {len(v)}' for k, v in pools.items()),
          f'(codebooks {sum(n for n, _ in pools["codebook"]) // 8} bytes)')
    if known:
        print(f'rows known to the table: {len(known & set(rows))} of {len(known)}; '
              f'by unmodified libvorbis: {len(stock & set(rows))}')
    print(f'{args.write}: {len(db)} bytes')


if __name__ == '__main__':
    main()
