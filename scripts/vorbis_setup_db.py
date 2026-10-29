#!/usr/bin/env python3
"""Splits Vorbis setup headers into shared parts and checks that the parts rebuild them.

A setup header (Vorbis I section 4.2.4) is a list of codebooks, time placeholders, floors,
residues, mappings and modes. Floors and residues name codebooks, mappings name floors and
residues, and modes name mappings, all by their index in the same setup. Here every part is
kept once, with its references turned into the ids of the parts they name, so that setups
listing the same codebooks in another order still share their floors and residues. A residue's
`end` is not part of the residue: it varies with the sample rate and quality while the rest
stays, so it is kept per setup.

A setup can list the same part twice (libvorbis repeats codebooks and floors), so a reference
is the named part with its occurrence among equal parts of that setup: (part, 0) for the first
copy, (part, 1) for the second and so on.

A setup is then: its codebook ids, floor ids, (residue id, end) pairs, mapping ids and mode ids.
`build()` serializes such a list back to the header's bytes; `main()` checks that this gives
every input back exactly.

Mappings depend on the channel count (coupling steps and the multiplex are sized by it); a
setup is parsed with the channel count it was made for, from the file name `<crc>.bin` and a
runs.tsv of vorbis_setups.py next to the setups directory.

Usage: python3 scripts/vorbis_setup_db.py RUNS_DIR...
"""

import sys
import zlib
from pathlib import Path


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


class Writer:
    def __init__(self):
        self.value = 0
        self.pos = 0

    def write(self, n, v):
        assert 0 <= v < 1 << n, (n, v)
        self.value |= v << self.pos
        self.pos += n

    def bits(self, n, v):
        self.write(n, v)

    def bytes(self):
        return self.value.to_bytes((self.pos + 7) // 8, 'little')


def read_codebook(r):
    """A codebook's raw bits (no references to other parts)."""
    start = r.pos
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
    n = r.pos - start
    return ('raw', n, (r.value >> start) & ((1 << n) - 1))


def read_floor(r, books):
    kind = r.read(16)
    assert kind == 1, 'floor type 0 is not handled'
    partitions = [r.read(4) for _ in range(r.read(5))]
    classes = []
    for _ in range(max(partitions) + 1 if partitions else 0):
        dims = r.read(3) + 1
        subclasses = r.read(2)
        master = books[r.read(8)] if subclasses else None
        subbooks = tuple(r.read(8) - 1 for _ in range(1 << subclasses))
        classes.append((dims, subclasses, master, tuple(books[b] if b >= 0 else None for b in subbooks)))
    multiplier = r.read(2) + 1
    rangebits = r.read(4)
    xs = tuple(r.read(rangebits) for c in partitions for _ in range(classes[c][0]))
    return ('floor1', tuple(partitions), tuple(classes), multiplier, rangebits, xs)


def write_floor(w, floor, index):
    _, partitions, classes, multiplier, rangebits, xs = floor
    w.bits(16, 1)
    w.bits(5, len(partitions))
    for p in partitions:
        w.bits(4, p)
    for dims, subclasses, master, subbooks in classes:
        w.bits(3, dims - 1)
        w.bits(2, subclasses)
        if subclasses:
            w.bits(8, index[master])
        for b in subbooks:
            w.bits(8, 0 if b is None else index[b] + 1)
    w.bits(2, multiplier - 1)
    w.bits(4, rangebits)
    for x in xs:
        w.bits(rangebits, x)


def read_residue(r, books):
    """(residue without its end, end)."""
    kind = r.read(16)
    assert kind <= 2, 'residue type'
    begin, end, partition = r.read(24), r.read(24), r.read(24) + 1
    nclass = r.read(6) + 1
    classbook = books[r.read(8)]
    cascade = []
    for _ in range(nclass):
        low = r.read(3)
        cascade.append((r.read(5) if r.read(1) else 0) * 8 + low)
    stages = tuple(tuple(books[r.read(8)] if c >> j & 1 else None for j in range(8)) for c in cascade)
    return ('residue', kind, begin, partition, classbook, stages), end


def write_residue(w, residue, end, index):
    _, kind, begin, partition, classbook, stages = residue
    w.bits(16, kind)
    w.bits(24, begin)
    w.bits(24, end)
    w.bits(24, partition - 1)
    w.bits(6, len(stages) - 1)
    w.bits(8, index[classbook])
    for books in stages:
        c = sum(1 << j for j, b in enumerate(books) if b is not None)
        w.bits(3, c & 7)
        w.bits(1, c >= 8)
        if c >= 8:
            w.bits(5, c >> 3)
    for books in stages:
        for b in books:
            if b is not None:
                w.bits(8, index[b])


def read_mapping(r, channels, floors, residues):
    assert r.read(16) == 0, 'mapping type'
    submaps = r.read(4) + 1 if r.read(1) else 1
    coupling = []
    if r.read(1):
        for _ in range(r.read(8) + 1):
            coupling.append((r.read(ilog(channels - 1)), r.read(ilog(channels - 1))))
    assert r.read(2) == 0, 'mapping reserved'
    mux = tuple(r.read(4) for _ in range(channels)) if submaps > 1 else ()
    subs = []
    for _ in range(submaps):
        r.read(8)  # time, unused
        subs.append((floors[r.read(8)], residues[r.read(8)]))
    return ('mapping', channels, tuple(coupling), mux, tuple(subs))


def write_mapping(w, mapping, floor_index, residue_index):
    _, channels, coupling, mux, subs = mapping
    w.bits(16, 0)
    w.bits(1, len(subs) > 1)
    if len(subs) > 1:
        w.bits(4, len(subs) - 1)
    w.bits(1, bool(coupling))
    if coupling:
        w.bits(8, len(coupling) - 1)
        for m, a in coupling:
            w.bits(ilog(channels - 1), m)
            w.bits(ilog(channels - 1), a)
    w.bits(2, 0)
    for m in mux:
        w.bits(4, m)
    for f, res in subs:
        w.bits(8, 0)
        w.bits(8, floor_index[f])
        w.bits(8, residue_index[res])


def occurrences(parts):
    """Each part with its occurrence among the equal parts before it: the form references take."""
    seen = {}
    keys = []
    for p in parts:
        keys.append((p, seen.get(p, 0)))
        seen[p] = seen.get(p, 0) + 1
    return keys


def parse(data, channels):
    """The setup as lists of parts: codebooks, floors, (residue, end)s, mappings, modes. Parts
    that name other parts hold (part, occurrence) of those parts (hashable tuples), not indices."""
    r = Reader(data)
    assert r.read(8) == 5 and bytes(r.read(8) for _ in range(6)) == b'vorbis', 'not a setup header'
    books = [read_codebook(r) for _ in range(r.read(8) + 1)]
    book_refs = occurrences(books)
    for _ in range(r.read(6) + 1):
        assert r.read(16) == 0, 'time placeholder'
    floors = [read_floor(r, book_refs) for _ in range(r.read(6) + 1)]
    residues = [read_residue(r, book_refs) for _ in range(r.read(6) + 1)]
    mappings = [read_mapping(r, channels, occurrences(floors), occurrences([res for res, _ in residues]))
                for _ in range(r.read(6) + 1)]
    mapping_refs = occurrences(mappings)
    modes = []
    for _ in range(r.read(6) + 1):
        blockflag, window, transform = r.read(1), r.read(16), r.read(16)
        assert window == 0 and transform == 0, 'mode window/transform'
        modes.append(('mode', blockflag, mapping_refs[r.read(8)]))
    assert r.read(1) == 1, 'framing'
    assert (r.pos + 7) // 8 == len(data), 'trailing bytes'
    return books, floors, residues, mappings, modes


def build(books, floors, residues, mappings, modes):
    """The setup header's bytes."""

    def index_of(parts):
        return {key: i for i, key in enumerate(occurrences(parts))}

    book_index, floor_index = index_of(books), index_of(floors)
    residue_index, mapping_index = index_of([res for res, _ in residues]), index_of(mappings)
    w = Writer()
    w.bits(8, 5)
    for c in b'vorbis':
        w.bits(8, c)
    w.bits(8, len(books) - 1)
    for _, n, v in books:
        w.bits(n, v)
    w.bits(6, 0)
    w.bits(16, 0)
    w.bits(6, len(floors) - 1)
    for f in floors:
        write_floor(w, f, book_index)
    w.bits(6, len(residues) - 1)
    for res, end in residues:
        write_residue(w, res, end, book_index)
    w.bits(6, len(mappings) - 1)
    for m in mappings:
        write_mapping(w, m, floor_index, residue_index)
    w.bits(6, len(modes) - 1)
    for _, blockflag, mapping in modes:
        w.bits(1, blockflag)
        w.bits(16, 0)
        w.bits(16, 0)
        w.bits(8, mapping_index[mapping])
    w.bits(1, 1)
    return w.bytes()


def load(dirs):
    """{crc: (channels, bytes)} from vorbis_setups.py output directories."""
    setups = {}
    for d in dirs:
        channels = {}
        for line in (d / 'runs.tsv').read_text().splitlines():
            _, ch, _, crc = line.split('\t')
            if crc != '-':
                channels.setdefault(crc, set()).add(int(ch))
        for p in (d / 'setups').glob('*.bin'):
            for ch in channels.get(p.stem, ()):
                setups[p.stem, ch] = p.read_bytes()
    return setups


def main():
    setups = load([Path(a) for a in sys.argv[1:]])
    kinds = ('codebooks', 'floors', 'residues', 'mappings', 'modes')
    pools = {k: {} for k in kinds}
    skeletons = set()
    for (crc, ch), data in sorted(setups.items()):
        assert '%08x' % zlib.crc32(data) == crc, crc
        parts = parse(data, ch)
        assert build(*parts) == data, f'{crc} does not rebuild'
        for k, ps in zip(kinds, parts):
            for p in ps:
                p = p[0] if k == 'residues' else p
                pools[k].setdefault(p, len(pools[k]))
        skeletons.add(tuple(tuple(p[0] if k == 'residues' else p for p in ps) for k, ps in zip(kinds, parts)))
    print(f'{len(setups)} setups (crc, channels), all rebuilt from their parts')
    print('distinct parts:', ', '.join(f'{k} {len(v)}' for k, v in pools.items()))
    print(f'codebook bytes {sum(n for _, n, _ in pools["codebooks"]) // 8}; skeletons (ends aside) {len(skeletons)}')


if __name__ == '__main__':
    main()
