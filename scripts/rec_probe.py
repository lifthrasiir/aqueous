#!/usr/bin/env python3
"""Find tables of records in files of unknown formats, and transpose them.

A prototype of the generic record transform: header + entries + footer, where nothing is
known of the format. For each file it finds candidate regions of two kinds:

- fixed-size records: windows of the file are scanned for a record size k, by the code
  length of the window under k adaptive order-0 models, one per byte column (position mod
  k), against one model (k = 1). The adaptive (KT-style) code length charges the models'
  cost, so multiples of the true k lose by themselves. Each window's k grows into a region,
  a maximum-sum run of the per-byte log-ratio of the column models (refitted on the region)
  against an order-0 model of the file;
- entries with a length field: chains of entries, each holding a length v at a fixed offset
  d and the next starting align_up(b + v, align) later, are followed from the start of the
  file for every hypothesis at once (find_chains). The chain is then rotated (it fixes the
  entries only up to where they start) and trimmed; each entry's first b bytes are its head,
  the rest its tail.

The records (or the entries' heads) are split into fields by dynamic programming over the
record: each field is raw, a delta or xor against the record before, or a difference or xor
against an earlier field of the record, with the byte planes' zstd size as the cost. A
region is written as the fields' planes one after another (or its bytes transposed), then
the tails; the script checks that this inverts. Last, greedily from the best candidate
alone, each region is taken in the form that makes the whole file smaller under zstd -3.

`--layout S:K:N` forces a region of fixed-size records (an oracle, for files whose layout
is known). scripts/rec_synth.py makes two files of known layouts.

Usage (from the root): .venv/bin/python scripts/rec_probe.py [-v] [--layout S:K:N] FILE...
"""

import argparse
import math
import sys
from multiprocessing import Pool

import numpy as np
import zstandard as zstd

ALPHA = 1 / 16  # the adaptive models' prior count per symbol
WINDOW = 1 << 16
MAX_K = 1024
MAX_WINDOWS = 8
MIN_RECORDS = 16
FIELD_BITS = 16  # the description of a field, which breaks ties towards fewer, wider fields

_lg = None  # _lg[c] = ln Γ(c + α) − ln Γ(α), in bits


def lg_table(n):
    global _lg
    if _lg is None or len(_lg) <= n:
        m = max(n + 1, 1 << 16)
        _lg = np.concatenate([[0.0], np.cumsum(np.log2(np.arange(m) + ALPHA))])
    return _lg


def kt_bits(counts):
    """Code length in bits of sequences with these counts (rows of 256) under adaptive models."""
    counts = np.asarray(counts).reshape(-1, 256)
    n = counts.sum(axis=1)
    lg = lg_table(int(counts.max(initial=0)))
    # ln Γ(n + 256α) − ln Γ(256α) = Σ_{i<n} log(i + 256α)
    tot = np.cumsum(np.log2(np.arange(int(n.max(initial=0)) + 1) + 256 * ALPHA))
    tot = np.concatenate([[0.0], tot])
    return float(tot[n].sum() - lg[counts].sum())


def plane_bits(planes):
    """Code length of byte planes (rows of an n × p uint8 matrix, coded column by column)."""
    p = planes.shape[1]
    idx = (np.arange(p, dtype=np.int64) * 256)[None, :] + planes
    return kt_bits(np.bincount(idx.ravel(), minlength=p * 256))


_zc = zstd.ZstdCompressor(level=3)
_z0 = len(_zc.compress(b''))


def zbits(planes):
    """Code length of byte planes (columns of an n × p matrix, one after another) under zstd -3."""
    return 8 * (len(_zc.compress(np.ascontiguousarray(planes.T).tobytes())) - _z0)


def scan_window(a, max_k):
    """Best k of a window and its code length against k = 1, in bits per byte."""
    n = len(a)
    pos = np.arange(n, dtype=np.int64)
    a64 = a.astype(np.int64)
    base = kt_bits(np.bincount(a64, minlength=256))
    best = (base, 1)
    for k in range(2, min(max_k, n // MIN_RECORDS) + 1):
        bits = kt_bits(np.bincount((pos % k) * 256 + a64, minlength=k * 256))
        if bits < best[0]:
            best = (bits, k)
    return best[1], (base - best[0]) / n, base / n


def grow_region(data, k, wo, wlen, rounds=4):
    """The region of records of size k grown from the window at wo: (start, end, gain in bits).

    The column models are refitted on the region found and the region grown again, as a window
    alone misses values that drift along the file (counters, timestamps). Each byte's score is
    clipped below, so that one value the models have not seen does not end the region."""
    n = len(data)
    bg = np.bincount(data, minlength=256).astype(np.float64)
    lpbg = np.log2((bg + ALPHA) / (bg.sum() + 256 * ALPHA))
    pos = np.arange(n, dtype=np.int64)
    s, e = wo, wo + wlen
    for _ in range(rounds):
        fit = data[s:e].astype(np.int64)
        col = np.bincount(((pos[s:e] - wo) % k) * 256 + fit, minlength=k * 256).reshape(k, 256)
        pcol = (col + ALPHA) / (col.sum(axis=1, keepdims=True) + 256 * ALPHA)
        llr = np.maximum(np.log2(pcol) - lpbg[None, :], -4.0)
        score = llr[(pos - wo) % k, data]
        # maximum-sum subarray through prefix sums: the best end, and the last minimum before it
        pre = np.concatenate([[0.0], np.cumsum(score)])
        lo_at = np.maximum.accumulate(np.where(pre == np.minimum.accumulate(pre), np.arange(n + 1), 0))
        gain = pre - pre[lo_at]
        ne = int(np.argmax(gain))
        ns = int(lo_at[ne])
        if (ns, ne) == (s, e):
            break
        s, e = ns, ne
    return s, e, float(gain[e])


# -- fields


def field_values(m, o, w, big):
    cols = m[:, o:o + w].astype(np.uint64)
    v = np.zeros(len(m), dtype=np.uint64)
    for i in range(w):
        sh = np.uint64(8 * (w - 1 - i) if big else 8 * i)
        v |= cols[:, i] << sh
    return v


def to_planes(v, w):
    return np.stack([((v >> np.uint64(8 * i)) & np.uint64(0xFF)).astype(np.uint8) for i in range(w)], axis=1)


REF_K = 128  # records up to this size also try fields against earlier fields of their width


def xform(v, w, how, m=None, big=False):
    """A field's values coded: 'raw', against the record before ('delta', 'xor'), or against an
    earlier field of the record (('sub', offset), ('xor', offset))."""
    mask = np.uint64((1 << (8 * w)) - 1)
    if isinstance(how, tuple):
        ref = field_values(m, how[1], w, big)
        return ((v - ref) & mask) if how[0] == 'sub' else v ^ ref
    prev = np.concatenate([[np.uint64(0)], v[:-1]])
    if how == 'delta':
        return (v - prev) & mask
    if how == 'xor':
        return v ^ prev
    return v


def unxform(t, w, how, m=None, big=False):
    mask = (1 << (8 * w)) - 1
    if how == 'raw':
        return t
    if isinstance(how, tuple):
        ref = field_values(m, how[1], w, big)
        return ((t + ref) & np.uint64(mask)) if how[0] == 'sub' else t ^ ref
    out = np.empty_like(t)
    prev = 0
    for i, x in enumerate(t.tolist()):
        prev = ((prev + x) & mask) if how == 'delta' else (prev ^ x)
        out[i] = prev
    return out


def split_fields(m, cost):
    """Fields of the records m (n × k): [(offset, width, big, how)] by DP over the code length."""
    n, k = m.shape
    raw1 = [cost(m[:, o:o + 1]) for o in range(k)]
    best = [0.0] + [math.inf] * k
    back = [None] * (k + 1)
    for o in range(k):
        if best[o] == math.inf:
            continue
        cands = [((o, 1, False, 'raw'), raw1[o] + FIELD_BITS)]
        for w in (1, 2, 4, 8):
            if o + w > k:
                break
            for big in ((False,) if w == 1 else (False, True)):
                v = field_values(m, o, w, big)
                hows = ['delta', 'xor']
                if k <= REF_K and w > 1:
                    hows += [(op, r) for r in range(o - w + 1) for op in ('sub', 'xor')]
                for how in hows:
                    cands.append(((o, w, big, how), cost(to_planes(xform(v, w, how, m, big), w)) + FIELD_BITS))
        for f, bits in cands:
            end = o + f[1]
            if best[o] + bits < best[end]:
                best[end] = best[o] + bits
                back[end] = f
    fields, o = [], k
    while o > 0:
        f = back[o]
        fields.append(f)
        o = f[0]
    fields.reverse()
    return fields, best[k] - FIELD_BITS * len(fields), sum(raw1)


def encode_fields(m, fields):
    return [to_planes(xform(field_values(m, o, w, big), w, how, m, big), w).T.copy() for o, w, big, how in fields]


def decode_fields(planes, fields, n, k):
    m = np.zeros((n, k), dtype=np.uint8)
    for p, (o, w, big, how) in zip(planes, fields):
        t = np.zeros(n, dtype=np.uint64)
        for i in range(w):
            t |= p[i].astype(np.uint64) << np.uint64(8 * i)
        v = unxform(t, w, how, m, big)
        for i in range(w):
            sh = np.uint64(8 * (w - 1 - i) if big else 8 * i)
            m[:, o + i] = ((v >> sh) & np.uint64(0xFF)).astype(np.uint8)
    return m


def describe(fields):
    out = []
    for o, w, big, how in fields:
        if how == 'raw':
            if out and out[-1][0] == 'raw' and out[-1][2] == o:
                out[-1][2] = o + 1
                continue
            out.append(['raw', o, o + 1])
        else:
            name = f'{how[0]}@{how[1]}:' if isinstance(how, tuple) else how
            out.append([f'{name}{"be" if big and w > 1 else ""}{8 * w}', o, o + w])
    return ' '.join(f'{t}@{a}' if b - a <= 1 or t != 'raw' else f'raw@{a}-{b - 1}' for t, a, b in out)


# -- entries with a length field


TLV_SPAN = 1 << 20  # where chains are looked for: the start of the file
TLV_STARTS = range(0, 257, 4)
TLV_STEPS = 32
TLV_COLS = 16  # the entries' leading bytes that score a chain
TLV_KINDS = [(1, False, 0xFF)] + [(2, big, mask) for big in (False, True) for mask in (0xFFFF, 0x7FFF, 0x0FFF)] \
    + [(4, big, 0xFFFFFFFF) for big in (False, True)]


def int_tables(a):
    """Values of each field kind at every position of a (zero past the end)."""
    a = np.concatenate([a, np.zeros(8, dtype=np.uint8)]).astype(np.int64)
    n = len(a) - 8
    le = {1: a[:n]}
    be = {1: a[:n]}
    le[2] = a[:n] | a[1:n + 1] << 8
    be[2] = a[:n] << 8 | a[1:n + 1]
    le[4] = le[2] | a[2:n + 2] << 16 | a[3:n + 3] << 24
    be[4] = be[2] << 16 | a[2:n + 2] << 8 | a[3:n + 3]
    return np.stack([(be if big else le)[w] & mask for w, big, mask in TLV_KINDS])


def entry_length(v, b, align):
    return (b + v + align - 1) // align * align


def find_chains(data, max_d=64, max_b=64, verbose=False):
    """Hypotheses (p0, d, kind, b, align) whose chains of entries hold together, best first.

    Each hypothesis says that an entry starting at p holds at p + d a length v (a field kind)
    and that the next entry starts at p + align_up(b + v, align). All of them are followed
    from every start at once for TLV_STEPS entries; a chain dies when an entry is shorter than
    its length field or leaves the span, and chains of one length are fixed records, left to
    the column scan. A chain is scored by chain_score().

    The best chains of the span are then followed through the whole file; those that end
    with the same lengths are one (the one whose first entries score best), and the rest are
    ranked by their score times their entries."""
    span = data[:TLV_SPAN]
    n = len(span)
    tables = int_tables(span)
    starts = np.array([p for p in TLV_STARTS if p < n], dtype=np.int64)
    kinds = np.arange(len(TLV_KINDS))
    widths = np.array([w for w, _, _ in TLV_KINDS])
    bs = np.arange(max_b + 1)
    aligns = np.array([1, 4, 8])
    found = {}
    padded = np.concatenate([span, np.zeros(TLV_COLS, dtype=np.uint8)])
    for d in range(max_d):
        # lanes: start × kind × b × align
        g = np.meshgrid(starts, kinds, bs, aligns, indexing='ij')
        p0, kind, b, al = (x.ravel() for x in g)
        w = widths[kind]
        pos = p0.copy()
        hist_pos = [pos.copy()]
        lens = []
        alive = np.ones(len(pos), dtype=bool)
        for _ in range(TLV_STEPS):
            at = np.minimum(pos + d, n - 1)
            v = tables[kind, at]
            L = entry_length(v, b, al)
            alive &= (L >= d + w) & (pos + d + w <= n) & (pos + L <= n)
            pos = np.where(alive, pos + L, pos)
            hist_pos.append(pos.copy())
            lens.append(L)
            keep = np.flatnonzero(alive)
            if len(keep) < len(alive) // 4:
                p0, kind, b, al, w, pos, alive = (x[keep] for x in (p0, kind, b, al, w, pos, alive))
                hist_pos = [h[keep] for h in hist_pos]
                lens = [x[keep] for x in lens]
        keep = np.flatnonzero(alive)
        if not len(keep):
            continue
        P = np.stack([h[keep] for h in hist_pos])  # (steps + 1) × lanes
        Ls = np.stack([x[keep] for x in lens])
        varied = (Ls != Ls[0]).any(axis=0)
        P = P[:, varied]
        lane = keep[varied]
        if not len(lane):
            continue
        agree = chain_score(padded, P)
        for i in np.argsort(-agree)[:16]:
            key = tuple(P[:, i] + d)
            sc = float(agree[i])
            if sc > found.get(key, (-1,))[0]:
                found[key] = (sc, int(p0[lane[i]]), d, int(kind[lane[i]]), int(b[lane[i]]), int(al[lane[i]]))
    # through the whole file
    padded = np.concatenate([data, np.zeros(TLV_COLS, dtype=np.uint8)])
    chains = {}
    for _, p0, d, kind, b, al in sorted(found.values(), reverse=True)[:64]:
        P = follow(data, p0, d, kind, b, al)
        sample = P[np.unique(np.linspace(0, len(P) - 1, min(len(P), 4097)).astype(np.int64))]
        per = float(chain_score(padded, sample[:, None])[0])
        # chains that end alike differ in how they start: keep the one that starts best
        head = float(chain_score(padded, P[:65, None])[0])
        key = tuple(np.diff(P[-9:]).tolist())  # the same under rotation, and for copies of the length
        if head > chains.get(key, (-math.inf,))[0]:
            chains[key] = (head, per * (len(P) - 1), P, d, kind, b, al)
    ranked = sorted((c[1:] for c in chains.values()), key=lambda c: -c[0])
    if verbose:
        for sc, P, d, kind, b, al in ranked[:8]:
            print(f'  chain {sc:.0f}: {P[0]:#x}..{P[-1]:#x} ×{len(P) - 1}, d {d}, {TLV_KINDS[kind]}, +{b} align {al}',
                  file=sys.stderr)
    return [r for r in ranked if r[0] > 0]


def chain_score(padded, P):
    """How much more often consecutive entries of chains P (entries × chains) agree in their
    leading bytes, column by column, than with bytes a multiple of 8 into the entry before:
    runs of zeros, text or periodic data (code) agree either way, and score nothing."""
    cols = padded[P[:, :, None] + np.arange(TLV_COLS)]  # entries × chains × cols
    L = np.maximum(np.diff(P, axis=0) // 8, 1)
    u = 1 + (np.arange(len(L))[:, None] * 7919) % L
    ctrl = padded[(P[:-1] + 8 * u)[:, :, None] + np.arange(TLV_COLS)]
    return (cols[1:] == cols[:-1]).mean(axis=(0, 2)) - (cols[1:] == ctrl).mean(axis=(0, 2))


def follow(data, p0, d, kind, b, al):
    """Entry starts from p0 for as long as the entries hold, ending with where the last ends."""
    w, big, mask = TLV_KINDS[kind]
    n = len(data)
    out = [p0]
    p = p0
    while p + d + w <= n:
        v = int.from_bytes(data[p + d:p + d + w].tobytes(), 'big' if big else 'little') & mask
        L = int(entry_length(v, b, al))
        if L < d + w or p + L > n:
            break
        p += L
        out.append(p)
    return np.array(out, dtype=np.int64)


def rotate_chain(data, P, d, w, b, limit=2048):
    """The chain moved back by x bytes (its length field at d + x) where it codes best.

    A chain fixes the entries only up to rotation: (P, d) and (P − x, d + x) follow the same
    lengths. The right one has the variable part (as long as the length field says) at the
    end of the entry: its tails, one after another, are that part alone (names, payloads),
    which only an LZ coder sees. So each rotation is coded with zstd, on the first entries,
    with the heads (b bytes) transposed."""
    Q = P[:limit + 1]
    L = np.diff(Q)
    H = int(min(b if b >= 8 else L.min(), L.min()))
    best = (math.inf, 0)
    for x in range(-d, min(H - d - w, int(P[0])) + 1):
        heads = data[Q[:-1, None] - x + np.arange(H)]
        tails = b''.join(data[p + H:q].tobytes() for p, q in zip((Q[:-1] - x).tolist(), (Q[1:] - x).tolist()))
        best = min(best, (len(_zc.compress(heads.T.tobytes() + tails)), x))
    x = best[1]
    return P - x, d + x


def trim_chain(data, P, cols=TLV_COLS):
    """The run of entries whose leading bytes fit the column models of the first ones (Kadane)."""
    padded = np.concatenate([data, np.zeros(cols, dtype=np.uint8)])
    lead = padded[P[:-1, None] + np.arange(cols)].astype(np.int64)
    fit = lead[:min(len(lead), 256)]
    col = np.bincount((np.arange(cols)[None, :] * 256 + fit).ravel(), minlength=cols * 256).reshape(cols, 256)
    pcol = (col + ALPHA) / (col.sum(axis=1, keepdims=True) + 256 * ALPHA)
    bg = np.bincount(data, minlength=256).astype(np.float64)
    lpbg = np.log2((bg + ALPHA) / (bg.sum() + 256 * ALPHA))
    llr = np.maximum(np.log2(pcol) - lpbg[None, :], -4.0)
    score = llr[np.arange(cols)[None, :], lead].sum(axis=1)
    pre = np.concatenate([[0.0], np.cumsum(score)])
    lo_at = np.maximum.accumulate(np.where(pre == np.minimum.accumulate(pre), np.arange(len(pre)), 0))
    e = int(np.argmax(pre - pre[lo_at]))
    s = int(lo_at[e])
    return P[s:e + 1]


def tlv_parts(data, P, d, kind, b, al, cost):
    """The forms of the entries P: their fixed heads (n × H) and their tails, one after another."""
    w = TLV_KINDS[kind][0]
    L = np.diff(P)
    H = int(max(d + w, min(b, L.min())))
    heads = data[P[:-1, None] + np.arange(H)]
    tails = b''.join(data[p + H:q].tobytes() for p, q in zip(P[:-1].tolist(), P[1:].tolist()))
    fields, _, _ = split_fields(heads, cost)
    planes = encode_fields(heads, fields)
    # the inverse: heads from the planes, then each tail by the length in its head
    back = decode_fields(planes, fields, len(heads), H)
    w_, big, mask = TLV_KINDS[kind]
    out, at = [], 0
    for h in back:
        v = int.from_bytes(h[d:d + w].tobytes(), 'big' if big else 'little') & mask
        t = int(entry_length(v, b, al)) - H
        out.append(h.tobytes() + tails[at:at + t])
        at += t
    assert b''.join(out) == data[P[0]:P[-1]].tobytes(), 'entries do not invert'
    return {'raw': data[P[0]:P[-1]].tobytes(), 'bytes': heads.T.tobytes() + tails,
            'fields': b''.join(p.tobytes() for p in planes) + tails,
            'desc': f'head {H} B, length at {d} as {"be" if big else "le"}{8 * w}&{mask:#x}, +{b} align {al}:'
                    f' {describe(fields)}'}


# -- the file


def find_regions(data, max_k, verbose):
    n = len(data)
    wlen = min(WINDOW, n)
    count = min(MAX_WINDOWS, max(1, n // wlen))
    starts = sorted({int(x) for x in np.linspace(0, n - wlen, count)})
    found = []
    for wo in starts:
        a = data[wo:wo + wlen]
        k, gain, h0 = scan_window(a, max_k)
        if verbose:
            print(f'  window {wo:#x}: H0 {h0:.2f}, k {k}, gain {gain:.3f} bit/B', file=sys.stderr)
        if k == 1 or gain < 0.05 or h0 > 7.8:
            continue
        s, e, g = grow_region(data, k, wo, wlen)
        found.append((g, s, e, k))
    # non-overlapping, best gain first
    regions = []
    for g, s, e, k in sorted(found, reverse=True):
        nrec = (e - s) // k
        if nrec < MIN_RECORDS:
            continue
        e = s + nrec * k
        if all(e <= rs or s >= rs + rn * rk for rs, rk, rn in regions):
            regions.append((s, k, nrec))
    return sorted(regions)


def probe(args):
    path, layout, max_k, cost, verbose = args
    cost = zbits if cost == 'zstd' else plane_bits
    data = np.fromfile(path, dtype=np.uint8)
    z0 = len(_zc.compress(data.tobytes()))
    # candidates: (start, end, label, forms)
    cands = []
    if not layout:
        for sc, P, d, kind, b, al in find_chains(data, verbose=verbose)[:3]:
            P, d = rotate_chain(data, P, d, TLV_KINDS[kind][0], b)
            P = trim_chain(data, P)
            if len(P) > MIN_RECORDS:
                cands.append((int(P[0]), int(P[-1]), f'entries ×{len(P) - 1}', tlv_parts(data, P, d, kind, b, al, cost)))
    for s, k, nrec in [layout] if layout else find_regions(data, max_k, verbose):
        m = data[s:s + nrec * k].reshape(nrec, k)
        fields, _, _ = split_fields(m, cost)
        planes = encode_fields(m, fields)
        assert np.array_equal(decode_fields(planes, fields, nrec, k), m), 'fields do not invert'
        cands.append((s, s + nrec * k, f'{k}×{nrec}', {'raw': m.tobytes(), 'bytes': m.T.tobytes(),
                      'fields': b''.join(p.tobytes() for p in planes), 'desc': describe(fields)}))

    # each candidate alone, then greedily from the best, each in the form that makes the whole
    # file smallest among those chosen before
    alone = [min(size(data, [(s, e, f, forms)]) for f in ('bytes', 'fields')) for s, e, _, forms in cands]
    cands = [c for _, c in sorted(zip(alone, cands), key=lambda x: x[0])]
    chosen, best, lines = [], z0, []
    for s, e, label, forms in cands:
        if any(s < ce and cs < e for cs, ce, _, _ in chosen):
            lines.append(f'  {s:#x}..{e:#x} {label}: overlaps')
            continue
        sizes = {f: size(data, chosen + [(s, e, f, forms)]) for f in ('bytes', 'fields')}
        f = min(sizes, key=sizes.get)
        lines.append(f'  {s:#x}..{e:#x} {label} ({(e - s) / len(data):.1%}):'
                     f' bytes {sizes["bytes"] - best:+}, fields {sizes["fields"] - best:+}'
                     f'{"" if sizes[f] < best else ", left"}')
        lines.append(f'    {forms["desc"]}')
        if sizes[f] < best:
            best = sizes[f]
            chosen.append((s, e, f, forms))
    return '\n'.join([f'{path}: {len(data)} B, zstd -3 {z0} -> {best} ({best / z0:.3f})'] + lines)


def size(data, chosen):
    out, at = [], 0
    for s, e, f, forms in sorted(chosen, key=lambda c: c[0]):
        out += [data[at:s].tobytes(), forms[f]]
        at = e
    out.append(data[at:].tobytes())
    return len(_zc.compress(b''.join(out)))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('files', nargs='+')
    ap.add_argument('--layout', help='S:K:N, a region forced (numbers in C syntax)')
    ap.add_argument('--max-k', type=int, default=MAX_K)
    ap.add_argument('--cost', choices=('zstd', 'kt'), default='zstd', help='of the fields\' planes')
    ap.add_argument('-j', type=int, default=None, help='processes')
    ap.add_argument('-v', action='store_true')
    a = ap.parse_args()
    layout = tuple(int(x, 0) for x in a.layout.split(':')) if a.layout else None
    jobs = [(f, layout, a.max_k, a.cost, a.v) for f in a.files]
    with Pool(a.j) as pool:
        for out in pool.imap(probe, jobs):
            print(out, flush=True)


if __name__ == '__main__':
    main()
