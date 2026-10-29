#!/usr/bin/env python3
"""How much of the record structure of known formats rec_probe.py finds.

For files whose layout is known from their specifications (ELF64, Mach-O 64, TrueType and
OpenType, pcap, git pack .idx v2 and index v2), this lists the tables of fixed-size records
and the chains of entries with a length field that the format defines (.eh_frame's records
with a head of 16 bytes: length, CIE pointer, PC begin and range), and compares:

- zstd -3 of the file;
- rec_probe.py's detector (its own candidates, chosen as it does);
- the oracle: the format's regions as candidates, chosen the same way (taken only where
  they make the file smaller);
- recall: of the bytes the oracle took, those the detector took with the same structure
  (fixed records of the same size and phase; entries starting at the same places).

Results are cached in scripts/cache/rec-oracle/ by the file's hash and this script's and
rec_probe.py's hashes.

Usage (from the root): .venv/bin/python scripts/rec_oracle.py [-v] FILE...
"""

import argparse
import hashlib
import json
import struct
import sys
from multiprocessing import Pool
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
import rec_probe as rp  # noqa: E402

CACHE = Path(__file__).parent / 'cache' / 'rec-oracle'
MIN = rp.MIN_RECORDS


def kind_of(w, big, mask):
    return rp.TLV_KINDS.index((w, big, mask))


# -- formats: each gives [('fixed', name, s, k, n) | ('tlv', name, (P, d, kind, b, align)[, head])]


def elf(b):
    if b[:4] != b'\x7fELF' or b[4] != 2 or b[5] != 1:
        return None
    phoff, shoff = struct.unpack_from('<QQ', b, 32)
    phentsize, phnum, shentsize, shnum, shstrndx = struct.unpack_from('<HHHHH', b, 54)
    out = [('fixed', 'phdr', phoff, phentsize, phnum), ('fixed', 'shdr', shoff, shentsize, shnum)]
    secs = [struct.unpack_from('<IIQQQQIIQQ', b, shoff + i * shentsize) for i in range(shnum)]
    stro = secs[shstrndx][4]

    def name(i):
        o = stro + i
        return b[o:b.index(b'\0', o)].decode()

    for nm, typ, _, _, off, size, _, _, _, ent in secs:
        n = name(nm)
        if typ == 8 or not size:  # NOBITS
            continue
        if ent and size // ent >= MIN:
            out.append(('fixed', n, off, ent, size // ent))
        if n == '.eh_frame_hdr' and b[off] == 1 and b[off + 3] == 0x3b:
            count = struct.unpack_from('<I', b, off + 8)[0]
            out.append(('fixed', n, off + 12, 8, count))
        if n == '.eh_frame':
            out.append(('tlv', n, chain(b, off, off + size, 0, 4, False, 4), 16))
    return out


def chain(b, s, e, d, w, big, add, mask=None):
    """Entry starts from s while the entries (length at d, plus add) stay before e and the
    length is not zero."""
    mask = mask if mask is not None else (1 << 8 * w) - 1
    P = [s]
    while P[-1] + d + w <= e:
        v = int.from_bytes(b[P[-1] + d:P[-1] + d + w], 'big' if big else 'little') & mask
        if not v or P[-1] + add + v > e:
            break
        P.append(P[-1] + add + v)
    return P, d, kind_of(w, big, mask), add, 1


def macho(b):
    if b[:4] != b'\xcf\xfa\xed\xfe':
        return None
    ncmds, sizeofcmds = struct.unpack_from('<II', b, 16)
    out = []
    at = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', b, at)
        if cmd == 0x19:  # LC_SEGMENT_64
            nsects = struct.unpack_from('<I', b, at + 64)[0]
            if nsects >= MIN:
                out.append(('fixed', 'section_64', at + 72, 80, nsects))
            for i in range(nsects):
                o = at + 72 + 80 * i
                sect = b[o:o + 16].rstrip(b'\0').decode()
                addr, sz, off, align, reloff, nreloc, flags, r1, r2 = struct.unpack_from('<QQIIIIIII', b, o + 32)
                typ = flags & 0xFF
                ent = {3: 4, 4: 8, 14: 16, 5: 8, 6: 8, 7: 8, 9: 8, 8: r2}.get(typ)
                if sect in ('__got', '__auth_got', '__auth_ptr', '__const_ptr'):
                    ent = 8
                if sect == '__compact_unwind':
                    ent = 32
                if ent and off and sz // ent >= MIN:
                    out.append(('fixed', sect, off, ent, sz // ent))
                if nreloc >= MIN:
                    out.append(('fixed', f'reloc {sect}', reloff, 8, nreloc))
                if sect == '__eh_frame' and off:
                    out.append(('tlv', sect, chain(b, off, off + sz, 0, 4, False, 4), 16))
        elif cmd == 0x2:  # LC_SYMTAB
            symoff, nsyms = struct.unpack_from('<II', b, at + 8)
            out.append(('fixed', 'nlist_64', symoff, 16, nsyms))
        elif cmd == 0xb:  # LC_DYSYMTAB
            off, n = struct.unpack_from('<II', b, at + 56)
            if n >= MIN:
                out.append(('fixed', 'indirect symbols', off, 4, n))
        at += size
    return out


def sfnt(b):
    if b[:4] not in (b'\0\1\0\0', b'OTTO', b'true'):
        return None
    n = struct.unpack_from('>H', b, 4)[0]
    tabs = {}
    for i in range(n):
        tag, _, off, ln = struct.unpack_from('>4sIII', b, 12 + 16 * i)
        tabs[tag.decode('latin-1')] = (off, ln)
    out = [('fixed', 'table records', 12, 16, n)]
    nglyphs = struct.unpack_from('>H', b, tabs['maxp'][0] + 4)[0]
    if 'hhea' in tabs and 'hmtx' in tabs:
        nhm = struct.unpack_from('>H', b, tabs['hhea'][0] + 34)[0]
        out.append(('fixed', 'hmtx', tabs['hmtx'][0], 4, nhm))
        if nglyphs - nhm >= MIN:
            out.append(('fixed', 'hmtx lsb', tabs['hmtx'][0] + 4 * nhm, 2, nglyphs - nhm))
    if 'loca' in tabs:
        long = struct.unpack_from('>h', b, tabs['head'][0] + 50)[0]
        out.append(('fixed', 'loca', tabs['loca'][0], 4 if long else 2, nglyphs + 1))
    if 'name' in tabs:
        o = tabs['name'][0]
        out.append(('fixed', 'name records', o + 6, 12, struct.unpack_from('>H', b, o + 2)[0]))
    if 'post' in tabs:
        o = tabs['post'][0]
        if struct.unpack_from('>I', b, o)[0] == 0x20000:
            out.append(('fixed', 'post glyph names', o + 34, 2, struct.unpack_from('>H', b, o + 32)[0]))
    return [r for r in out if r[0] != 'fixed' or r[4] >= MIN]


def pcap(b):
    if b[:4] != b'\xd4\xc3\xb2\xa1':
        return None
    return [('tlv', 'packets', chain(b, 24, len(b), 8, 4, False, 16))]


def git_idx(b):
    if b[:8] != b'\xfftOc\0\0\0\2':
        return None
    n = struct.unpack_from('>I', b, 8 + 255 * 4)[0]
    at = 8 + 1024
    return [('fixed', 'fanout', 8, 4, 256), ('fixed', 'names', at, 20, n), ('fixed', 'crc', at + 20 * n, 4, n),
            ('fixed', 'offsets', at + 24 * n, 4, n)]


def git_index(b):
    if b[:8] != b'DIRC\0\0\0\2':
        return None
    n = struct.unpack_from('>I', b, 8)[0]
    P, d, kind, add, al = [12], 60, kind_of(2, True, 0xFFF), 63, 8
    for _ in range(n):
        v = struct.unpack_from('>H', b, P[-1] + 60)[0] & 0xFFF
        P.append(P[-1] + int(rp.entry_length(v, add, al)))
    return [('tlv', 'entries', (P, d, kind, add, al))]


FORMATS = [('ELF', elf), ('Mach-O', macho), ('sfnt', sfnt), ('pcap', pcap), ('git idx', git_idx),
           ('git index', git_index)]


# -- measuring


def covered(o, chosen, exact=True):
    """Bytes of the oracle region o that the detector took with the same structure: fixed
    records of the same size, in the same phase if exact (8-byte fields of a 24-byte record
    stay whole in three phases, so zstd cannot tell them apart)."""
    tot = 0
    for c in chosen:
        lo, hi = max(o['s'], c['s']), min(o['e'], c['e'])
        if lo >= hi or c['shape'][0] != o['shape'][0]:
            continue
        if o['shape'][0] == 'fixed':
            k = o['shape'][1]
            if c['shape'][1] == k and (not exact or (c['s'] - o['s']) % k == 0):
                tot += hi - lo
        else:
            P, Q = o['shape'][1], c['shape'][1]
            L = dict(zip(P[:-1].tolist(), np.diff(P).tolist()))
            tot += sum(L[p] for p in np.intersect1d(P[:-1], Q[:-1]).tolist())
    return tot


def measure(path):
    data = np.fromfile(path, dtype=np.uint8)
    b = data.tobytes()
    fmt, regions = next(((n, r) for n, f in FORMATS if (r := f(b))), (None, []))
    z0 = len(rp._zc.compress(b))
    auto, achosen, alines = rp.choose(data, rp.auto_cands(data), z0)
    ocands = []
    for r in regions:
        if r[0] == 'fixed':
            _, name, s, k, n = r
            if n >= MIN and s + n * k <= len(data):
                ocands.append(dict(rp.fixed_cand(data, s, k, n), name=name))
        else:
            _, name, (P, d, kind, add, al), *head = r
            if len(P) > MIN:
                ocands.append(dict(rp.tlv_cand(data, np.array(P), d, kind, add, al, head=head[0] if head else None),
                                   name=name))
    oracle, ochosen, olines = rp.choose(data, ocands, z0)
    took = sum(o['e'] - o['s'] for o in ochosen)
    hit = sum(covered(o, achosen) for o in ochosen)
    near = sum(covered(o, achosen, exact=False) for o in ochosen)
    names = {(c['s'], c['e']): c['name'] for c in ocands}
    return {'path': str(path), 'format': fmt, 'size': len(data), 'zstd': z0, 'auto': auto, 'oracle': oracle,
            'oracle_bytes': took, 'recall_bytes': hit, 'recall_any_phase': near,
            'oracle_regions': [{'name': names[(c['s'], c['e'])], 'label': c['label'], 'form': c['form'],
                                'covered': covered(c, achosen)} for c in ochosen],
            'auto_lines': alines, 'oracle_lines': olines}


def cached(path):
    h = hashlib.sha256(Path(path).read_bytes()).hexdigest()[:16]
    code = hashlib.sha256(Path(__file__).read_bytes() + Path(rp.__file__).read_bytes()).hexdigest()[:8]
    f = CACHE / f'{h}-{code}.json'
    if f.exists():
        return json.loads(f.read_text())
    r = measure(path)
    CACHE.mkdir(parents=True, exist_ok=True)
    f.write_text(json.dumps(r, indent=1))
    return r


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('files', nargs='+')
    ap.add_argument('-j', type=int, default=None)
    ap.add_argument('-v', action='store_true', help='the regions of both')
    a = ap.parse_args()
    print('| file | format | MB | zstd -3 | detector | oracle | oracle took | recall | any phase |')
    print('|---|---|---|---|---|---|---|---|---|')
    with Pool(a.j) as pool:
        for r in pool.imap(cached, a.files):
            rec, near = (r[x] / r['oracle_bytes'] if r['oracle_bytes'] else float('nan')
                         for x in ('recall_bytes', 'recall_any_phase'))
            print(f'| {Path(r["path"]).name} | {r["format"]} | {r["size"] / 1e6:.2f} | {r["zstd"]} |'
                  f' {r["auto"]} ({r["auto"] / r["zstd"]:.3f}) | {r["oracle"]} ({r["oracle"] / r["zstd"]:.3f}) |'
                  f' {r["oracle_bytes"] / r["size"]:.1%} | {rec:.0%} | {near:.0%} |', flush=True)
            if a.v:
                for o in r['oracle_regions']:
                    print(f'    oracle {o["name"]} {o["label"]} {o["form"]}: covered {o["covered"]}')
                print('\n'.join('   auto' + x for x in r['auto_lines'] if not x.startswith('    ')))
                print('\n'.join(' oracle' + x for x in r['oracle_lines'] if not x.startswith('    ')))


if __name__ == '__main__':
    main()
