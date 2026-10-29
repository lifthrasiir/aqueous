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
import bisect
import math
import sys
from multiprocessing import Pool

import numpy as np
import zstandard as zstd

ALPHA = 1 / 16  # the adaptive models' prior count per symbol
WINDOW = 1 << 16
MAX_K = 1024
MAX_WINDOWS = 8
FINE_WINDOW = 1 << 12  # small tables: windows over the whole file, records up to FINE_K
FINE_K = 64
MAX_FINE_WINDOWS = 1024
FINE_SPAN = 1 << 20  # how far a region grows from a small window
MAX_REGIONS = 32
# speed: what is measured on samples (None: on everything)
LOCAL_CONTEXT = 1 << 20  # candidates are judged on themselves and this much around them
FIELD_ROWS = 1024  # fields are chosen on 4 runs of this many records spread over the region
CHAIN_SCREEN = 512  # chains of a span are screened on 8 entries and 8 columns, these kept
FOLLOW_SAMPLE = 4096  # the best of them on this many entries, and FOLLOW_KEEP followed to the end
FOLLOW_KEEP = 8
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


def grow_region(data, k, wo, wlen, rounds=4, span=None):
    """The region of records of size k grown from the window at wo: (start, end, gain in bits).

    The column models are refitted on the region found and the region grown again, as a window
    alone misses values that drift along the file (counters, timestamps). Each byte's score is
    clipped below, so that one value the models have not seen does not end the region. With
    span, the region stays within that many bytes of the window."""
    base = 0
    if span is not None:
        base = max(0, wo - span)
        data = data[base:wo + wlen + span]
        wo -= base
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
    return base + s, base + e, float(gain[e])


def grow_fields(data, k, p, seed, span, rounds=3):
    """The region of records of size k starting at p (mod k) around the rows of seed (start,
    end), with each record coded by fields: (start, end).

    Order-0 columns cannot tell a sorted table from the bytes after it (offsets that rise
    look like any other bytes), so the region found by them can run on. Here the fields of
    the seed's records (split with deltas and xors) code each record against the record
    before, and a record scores the code length of its bytes under the order-0 model of the
    file less that of its fields' planes under column models, refitted on the region."""
    n = len(data)
    lo = p - (p - max(0, seed[0] - span)) // k * k
    nrec = (min(n, seed[1] + span) - lo) // k
    m = data[lo:lo + nrec * k].reshape(nrec, k)
    r0, r1 = (seed[0] - lo + k - 1) // k, (seed[1] - lo) // k
    if r1 - r0 < MIN_RECORDS:
        return seed
    fields, _, _ = split_fields(m[r0:r1], plane_bits, refs=False)
    t = np.concatenate([to_planes(xform(field_values(m, o, w, big), w, how), w) for o, w, big, how in fields], axis=1)
    t = t.astype(np.int64)
    bg = np.bincount(data, minlength=256).astype(np.float64)
    lpbg = np.log2((bg + ALPHA) / (bg.sum() + 256 * ALPHA))
    raw = lpbg[m].sum(axis=1)
    cols = np.arange(t.shape[1])[None, :] * 256
    s, e = r0, r1
    for _ in range(rounds):
        col = np.bincount((cols + t[s:e]).ravel(), minlength=t.shape[1] * 256).reshape(-1, 256)
        pcol = np.log2((col + ALPHA) / (col.sum(axis=1, keepdims=True) + 256 * ALPHA))
        score = np.maximum(pcol.ravel()[cols + t] - lpbg[m], -4.0).sum(axis=1)
        pre = np.concatenate([[0.0], np.cumsum(score)])
        lo_at = np.maximum.accumulate(np.where(pre == np.minimum.accumulate(pre), np.arange(nrec + 1), 0))
        ne = int(np.argmax(pre - pre[lo_at]))
        ns = int(lo_at[ne])
        if (ns, ne) == (s, e):
            break
        s, e = ns, ne
    del raw
    return lo + s * k, lo + e * k


def refine_phase(data, s, e, k, limit=2048):
    """Where the records of a region really start: the region's edges are found byte by byte,
    and a record's first bytes (zeros, say) may fit the model of another column. Records
    start aligned (to the largest power of 2 up to 8 that divides k) near s; of those starts,
    the one whose records split into fields at the least cost."""
    a = math.gcd(k, 8)
    if k > FINE_K:
        return s
    best = (math.inf, s)
    for p in range(max(0, s - k + 1), s + k):
        if p % a:
            continue
        nrec = min((e - p) // k, limit)
        if nrec < MIN_RECORDS:
            continue
        m = data[p:p + nrec * k].reshape(nrec, k)
        _, bits, _ = split_fields(m, plane_bits, refs=False)
        best = min(best, (bits / nrec, p))
    return best[1]


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


def sample_rows(m):
    """FIELD_ROWS records from each of 4 places over m (all of m if that is not fewer)."""
    n = len(m)
    if not FIELD_ROWS or n <= 4 * FIELD_ROWS:
        return m
    starts = np.linspace(0, n - FIELD_ROWS, 4).astype(np.int64)
    return np.concatenate([m[a:a + FIELD_ROWS] for a in starts])


def split_fields(m, cost, refs=True):
    """Fields of the records m (n × k): [(offset, width, big, how)] by DP over the code length,
    measured on sample_rows(m)."""
    m = sample_rows(m)
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
                if refs and k <= REF_K and w > 1:
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
TLV_WINDOWS = 16  # spans over the file; past its start, looked at with the smaller search below
TLV_WINDOW_D = 32
TLV_WINDOW_STARTS = range(0, 129, 4)
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


def find_chains(data, at=0, max_d=64, starts=TLV_STARTS, max_b=64, verbose=False):
    """Hypotheses (p0, d, kind, b, align) whose chains of entries hold together, best first.

    Each hypothesis says that an entry starting at p holds at p + d a length v (a field kind)
    and that the next entry starts at p + align_up(b + v, align). All of them are followed
    from every start at once for TLV_STEPS entries; a chain dies when an entry is shorter than
    its length field or leaves the span, and chains of one length are fixed records, left to
    the column scan. A chain is scored by chain_score().

    The best chains of the span are then followed through the whole file; those that end
    with the same lengths are one (the one whose first entries score best), and the rest are
    ranked by their score times their entries. The chains start within the span at `at`."""
    span = data[at:at + TLV_SPAN]
    n = len(span)
    tables = int_tables(span)
    starts = np.array([p for p in starts if p < n], dtype=np.int64)
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
            v = tables[kind, np.minimum(pos + d, n - 1)]
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
        if CHAIN_SCREEN and P.shape[1] > CHAIN_SCREEN:
            top = np.argsort(-chain_score(padded, P[:9], 8))[:CHAIN_SCREEN]
            P, lane = P[:, top], lane[top]
        agree = chain_score(padded, P)
        for i in np.argsort(-agree)[:16]:
            key = tuple(P[:, i] + d)
            sc = float(agree[i])
            if sc > found.get(key, (-1,))[0]:
                found[key] = (sc, at + int(p0[lane[i]]), d, int(kind[lane[i]]), int(b[lane[i]]), int(al[lane[i]]))
    # through the whole file
    padded = np.concatenate([data, np.zeros(TLV_COLS, dtype=np.uint8)])
    chains = {}
    hyps = sorted(found.values(), reverse=True)[:64]
    if FOLLOW_SAMPLE:
        # screened on their first entries, then the best followed through the file
        def screen(h):
            P = follow(data, *h[1:], limit=FOLLOW_SAMPLE)
            return float(chain_score(padded, P[:, None])[0]) * (len(P) - 1)
        hyps = sorted(hyps, key=screen, reverse=True)[:FOLLOW_KEEP]
    for _, p0, d, kind, b, al in hyps:
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


def chain_score(padded, P, ncols=TLV_COLS):
    """How much more often consecutive entries of chains P (entries × chains) agree in their
    leading bytes, column by column, than with bytes a multiple of 8 into the entry before:
    runs of zeros, text or periodic data (code) agree either way, and score nothing."""
    cols = padded[P[:, :, None] + np.arange(ncols)]  # entries × chains × cols
    L = np.maximum(np.diff(P, axis=0) // 8, 1)
    u = 1 + (np.arange(len(L))[:, None] * 7919) % L
    ctrl = padded[(P[:-1] + 8 * u)[:, :, None] + np.arange(ncols)]
    return (cols[1:] == cols[:-1]).mean(axis=(0, 2)) - (cols[1:] == ctrl).mean(axis=(0, 2))


def follow(data, p0, d, kind, b, al, limit=None):
    """Entry starts from p0 for as long as the entries hold (at most limit of them), ending with
    where the last ends."""
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
        if limit and len(out) > limit:
            break
    return np.array(out, dtype=np.int64)


def extend_back(data, P, d, kind, b, al, reach=4096):
    """The chain extended before its first entry: while some q < P[0] (within reach) holds an
    entry ending at P[0], the nearest such q. Chains are found from wherever a window falls,
    often inside the entries."""
    w = TLV_KINDS[kind][0]
    out = []
    p = int(P[0])
    while p > 0:
        lo = max(0, p - reach)
        tab = int_tables(data[lo:p + d + w])[kind]
        q = np.arange(lo, p)
        L = entry_length(tab[q - lo + d], b, al)
        ok = np.flatnonzero((q + L == p) & (L >= d + w))
        if not len(ok):
            break
        p = int(q[ok[-1]])
        out.append(p)
    return np.concatenate([np.array(out[::-1], dtype=np.int64), P]) if out else P


def rotate_chain(data, P, d, w, b, limit=2048):
    """The chain moved back by x bytes (its length field at d + x) where it codes best.

    A chain fixes the entries only up to rotation: (P, d) and (P − x, d + x) follow the same
    lengths. The right one has the variable part (as long as the length field says) at the
    end of the entry: its tails, one after another, are that part alone (names, payloads),
    which only an LZ coder sees. So each rotation is coded with zstd, on the first entries,
    with the heads (b bytes) transposed."""
    Q = P[:limit + 1]
    L = np.diff(P)
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


def choose_head(data, P, least, b, limit=2048):
    """How many leading bytes of each entry to take as its head: the length field says where
    the variable part ends, not where it starts. Of a few widths up to the shortest entry
    (and b), the one that codes best with zstd on the first entries, heads transposed."""
    Q = P[:limit + 1]
    most = int(np.diff(P).min())  # a head never runs into the next entry
    widths = sorted({h for h in (least, min(b, most), 8, 12, 16, 20, 24, 32, 48, 64) if least <= h <= most})
    best = (math.inf, least)
    for H in widths:
        heads = data[Q[:-1, None] + np.arange(H)]
        tails = b''.join(data[p + H:q].tobytes() for p, q in zip(Q[:-1].tolist(), Q[1:].tolist()))
        best = min(best, (len(_zc.compress(heads.T.tobytes() + tails)), H))
    return best[1]


def tlv_parts(data, P, d, kind, b, al, cost, head=None):
    """The forms of the entries P: their fixed heads (n × H) and their tails, one after another."""
    w = TLV_KINDS[kind][0]
    L = np.diff(P)
    H = int(max(d + w, min(head, L.min()))) if head else choose_head(data, P, d + w, b)
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
    """Regions of fixed-size records: (start, k, records), not overlapping.

    Big windows (a few, k up to max_k) find big tables; small windows over the whole file
    (k up to FINE_K) find small ones, each grown only within FINE_SPAN, and skipped where a
    region of the same k already holds them."""
    n = len(data)
    found = []

    def scan(wo, wlen, max_k, span):
        a = data[wo:wo + wlen]
        k, gain, h0 = scan_window(a, max_k)
        if any(s <= wo and wo + wlen <= e and k == fk for _, s, e, fk, _, _ in found):
            return
        if verbose and span is None:
            print(f'  window {wo:#x}: H0 {h0:.2f}, k {k}, gain {gain:.3f} bit/B', file=sys.stderr)
        if k == 1 or gain < 0.05 or h0 > 7.8:
            return
        s, e, g = grow_region(data, k, wo, wlen, span=span)
        if (e - s) // k >= MIN_RECORDS:
            found.append((g, s, e, k, wo, wlen))

    wlen = min(WINDOW, n)
    for wo in sorted({int(x) for x in np.linspace(0, n - wlen, min(MAX_WINDOWS, max(1, n // wlen)))}):
        scan(wo, wlen, max_k, None)
    if n >= 2 * FINE_WINDOW:
        count = min(MAX_FINE_WINDOWS, n // FINE_WINDOW)
        for wo in sorted({int(x) for x in np.linspace(0, n - FINE_WINDOW, count)}):
            scan(wo, FINE_WINDOW, FINE_K, FINE_SPAN)
    # by gain, without duplicates; overlaps are for zstd to settle
    regions = set()
    seeds = []
    for f in sorted(found, reverse=True):
        g, s, e, k = f[:4]
        # the same table found from several windows
        if not any(k == x[3] and min(e, x[2]) - max(s, x[1]) > max(e - s, x[2] - x[1]) // 2 for x in seeds):
            seeds.append(f)
    for g, s, e, k, wo, wlen in seeds[:MAX_REGIONS]:
        s = refine_phase(data, s, e, k)
        spans = [(s, e)] + ([grow_fields(data, k, s, (wo, wo + wlen), FINE_SPAN)] if k <= FINE_K else [])
        for s, e in spans:
            if (e - s) // k >= MIN_RECORDS:
                regions.add((s, k, (e - s) // k))
    return sorted(regions)


def fixed_cand(data, s, k, nrec, cost=zbits):
    """A candidate region of nrec records of k bytes at s: {s, e, label, shape, forms}."""
    m = data[s:s + nrec * k].reshape(nrec, k)
    fields, _, _ = split_fields(m, cost)
    planes = encode_fields(m, fields)
    assert np.array_equal(decode_fields(planes, fields, nrec, k), m), 'fields do not invert'
    return {'s': s, 'e': s + nrec * k, 'label': f'{k}×{nrec}', 'shape': ('fixed', k),
            'forms': {'raw': m.tobytes(), 'bytes': m.T.tobytes(), 'fields': b''.join(p.tobytes() for p in planes),
                      'desc': describe(fields)}}


def tlv_cand(data, P, d, kind, b, al, cost=zbits, head=None):
    """A candidate region of the entries starting at P[:-1] (P[-1] where the last ends)."""
    return {'s': int(P[0]), 'e': int(P[-1]), 'label': f'entries ×{len(P) - 1}', 'shape': ('tlv', P),
            'forms': tlv_parts(data, P, d, kind, b, al, cost, head)}


def auto_cands(data, max_k=MAX_K, cost=zbits, verbose=False):
    cands = []
    chains = {}
    n = len(data)
    ats = sorted({int(x) for x in np.linspace(0, n, TLV_WINDOWS + 1)[:-1]}) if n > TLV_SPAN // 4 else [0]
    for at in ats:
        kw = {} if at == 0 else {'max_d': TLV_WINDOW_D, 'starts': TLV_WINDOW_STARTS}
        for c in find_chains(data, at, verbose=verbose, **kw)[:3]:
            key = tuple(np.diff(c[1][-9:]).tolist())
            if c[0] > chains.get(key, (-math.inf,))[0]:
                chains[key] = c
    for sc, P, d, kind, b, al in sorted(chains.values(), key=lambda c: -c[0])[:6]:
        P = extend_back(data, P, d, kind, b, al)
        P, d = rotate_chain(data, P, d, TLV_KINDS[kind][0], b)
        P = trim_chain(data, P)
        if len(P) > MIN_RECORDS:
            cands.append(tlv_cand(data, P, d, kind, b, al, cost))
    for s, k, nrec in find_regions(data, max_k, verbose):
        cands.append(fixed_cand(data, s, k, nrec, cost))
    return cands


def choose(data, cands, z0):
    """The candidates to take, and in which form: (size, chosen, report lines).

    Each is measured alone (its best form; under zstd -3, on itself and LOCAL_CONTEXT around
    it, or the whole file). Gains of regions apart roughly add up, so the set apart with the
    largest sum of gains is found first (weighted interval scheduling: one big region of a
    little gain must not shut out the tables it overlaps). Those, from the best, and then the
    rest, from the best, are taken where they still make it smaller among those taken before.
    The size returned is z0 less the gains so measured."""
    def taken(chosen):
        return [(x['s'], x['e'], x['form'], x['forms']) for x in chosen]

    def gains(c, chosen):
        before = size(data, taken(chosen), c['s'], c['e'])
        return {f: before - size(data, taken(chosen) + [(c['s'], c['e'], f, c['forms'])], c['s'], c['e'])
                for f in ('bytes', 'fields')}

    gain = [max(gains(c, []).values()) for c in cands]
    byend = sorted((i for i in range(len(cands)) if gain[i] > 0), key=lambda i: cands[i]['e'])
    ends = [cands[i]['e'] for i in byend]
    best_sum = [0] * (len(byend) + 1)
    for j, i in enumerate(byend):
        prev = bisect.bisect_right(ends, cands[i]['s'], 0, j)
        best_sum[j + 1] = max(best_sum[j], best_sum[prev] + gain[i])
    picked, j = set(), len(byend)
    while j > 0:
        i = byend[j - 1]
        prev = bisect.bisect_right(ends, cands[i]['s'], 0, j - 1)
        if best_sum[j] == best_sum[prev] + gain[i]:
            picked.add(i)
            j = prev
        else:
            j -= 1
    order = sorted(picked, key=lambda i: -gain[i]) + sorted(set(range(len(cands))) - picked, key=lambda i: -gain[i])
    chosen, best, lines = [], z0, []
    for c in (cands[i] for i in order):
        s, e, label, forms = c['s'], c['e'], c['label'], c['forms']
        if any(s < x['e'] and x['s'] < e for x in chosen):
            lines.append(f'  {s:#x}..{e:#x} {label}: overlaps')
            continue
        g = gains(c, chosen)
        f = max(g, key=g.get)
        lines.append(f'  {s:#x}..{e:#x} {label} ({(e - s) / len(data):.1%}):'
                     f' bytes {-g["bytes"]:+}, fields {-g["fields"]:+}{"" if g[f] > 0 else ", left"}')
        lines.append(f'    {forms["desc"]}')
        if g[f] > 0:
            best -= g[f]
            chosen.append(dict(c, form=f))
    return best, chosen, lines


def probe(args):
    path, layout, max_k, cost, verbose = args
    cost = zbits if cost == 'zstd' else plane_bits
    data = np.fromfile(path, dtype=np.uint8)
    z0 = len(_zc.compress(data.tobytes()))
    cands = [fixed_cand(data, *layout, cost)] if layout else auto_cands(data, max_k, cost, verbose)
    best, _, lines = choose(data, cands, z0)
    return '\n'.join([f'{path}: {len(data)} B, zstd -3 {z0} -> {best} ({best / z0:.3f})'] + lines)


def size(data, chosen, s=None, e=None):
    """zstd -3 of the file with the regions chosen (start, end, form, forms) in their forms; of
    [s, e) and LOCAL_CONTEXT around it if given, widened to hold whole the regions it meets."""
    lo, hi = 0, len(data)
    if s is not None and LOCAL_CONTEXT is not None:
        lo, hi = max(0, s - LOCAL_CONTEXT), min(len(data), e + LOCAL_CONTEXT)
        for cs, ce, _, _ in chosen:
            if cs < hi and lo < ce:
                lo, hi = min(lo, cs), max(hi, ce)
    out, at = [], lo
    for cs, ce, f, forms in sorted(chosen, key=lambda c: c[0]):
        if cs < hi and lo < ce:
            out += [data[at:cs].tobytes(), forms[f]]
            at = ce
    out.append(data[at:hi].tobytes())
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
