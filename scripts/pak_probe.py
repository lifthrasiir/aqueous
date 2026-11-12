#!/usr/bin/env python3
"""Find the directories of unknown archive formats ("pak" files) and take their members out.

A prototype of docs/pak-inference-design.md. Nothing about any format is known: a directory
is a table of fixed-size records some of whose fields point into a *target* (the file
itself, a sibling file of the same stem, or the concatenation of streams decoded from one of
these), and it is recognised by what the pointers do there:

1. tables: windows (the head and tail of the file, the whole of a small one) are scanned for
   strides k by byte autocorrelation; a table is a run where the bytes repeat at k;
2. fields: every bit field of every record (a 64-bit word at each byte, either endianness,
   any shift, masked to the target's bit length, scaled by 2^a) is a candidate;
3. tiling: an offset field, sorted, gives the gaps d_i between consecutive members; a size
   field fits a row where 0 <= d_i - s_i < A. The fit is measured against a control (the
   sizes paired with the wrong rows). Without an offset field, sizes laid end to end (each
   aligned) must end exactly at the end of the target;
4. members: each is probed for a codec (magic numbers, then headerless decoders that must
   consume exactly the member); the decoded members, laid end to end, are a new target for
   the tables (2-3 again).

The other files of the same stem in FILE's directory are its family: their tables are looked
at, and they are targets, too. zstd frames need the `zstandard` package in the venv.

Usage (from the root): .venv/Scripts/python -I scripts/pak_probe.py [-v] FILE...
"""

import argparse
import math
import os
import sys
import time
import zlib

import numpy as np

try:
    import zstandard
except ImportError:
    zstandard = None

HEAD = 4 << 20  # bytes of the head scanned for tables
TAIL = 1 << 20  # and of the tail
KMIN, KMAX = 4, 128  # shortest and longest record looked for
MINROWS = 8  # fewer records than this are no evidence (see the design: small directories)
SLACK = 4096  # gap allowed between a member's end and the next member (padding)
SAMPLE = 512  # rows of a table on which fields are scored
MAXOFF = 16  # offset fields of a table tried for a tiling
COVER = 0.9  # share of the target from its first member on that the members must fill
BACK = 1 << 16  # bytes before a table its sizes laid end to end may start at
EDGE = 8  # rows the ends of a table may be off by (two tables of alike columns meet)
# Bits of evidence a directory needs: the fields tried for a table are about 2^24 offsets
# times 2^14 sizes, so 38 bits are chance; 48 leave a margin.
EVIDENCE = 48


def bitlen(x):
    return max(int(x).bit_length(), 1)


# ---------------------------------------------------------------- tables


def find_tables(b, lo, hi, verbose=False):
    """Runs of records of a fixed stride in b[lo:hi], as (start, end, k)."""
    w = b[lo:hi]
    n = len(w)
    if n < 64:
        return []
    # strides whose repetition is not explained by a divisor's, by tiles of 64 KiB (a small
    # table drowns in the window's), and their divisors and multiples (a table of a divisor's
    # stride may sit next to a bigger one: it repeats at both)
    cand = set()
    T = 1 << 16
    for t0 in range(0, n, T):
        z = w[t0:t0 + T]
        if len(z) < 16 * KMIN:
            continue
        ks = np.arange(KMIN, min(KMAX, len(z) // 4) + 1)
        eq = np.array([np.count_nonzero(z[:-k] == z[k:]) / (len(z) - k) for k in ks])
        base = max(np.median(eq), 1 / 256)
        got = []
        for i in np.argsort(-eq):
            k = int(ks[i])
            if eq[i] < base * 1.5 or eq[i] < 0.02 or len(got) >= 3:
                break
            if any(k % c == 0 and eq[c - KMIN] >= 0.85 * eq[i] for c in got):
                continue
            got.append(k)
        cand |= set(got)
    cand = sorted(cand | {d for k in cand for d in (k // 2, k // 4) if d >= KMIN and k % d == 0} |
                  {m for k in cand for m in (2 * k, 4 * k) if m <= KMAX})
    if not cand:
        return []
    # the density of bytes equal to the byte k before, by blocks of B bytes, smoothed over
    # a few records; each block labelled with the least stride as dense as the densest
    B = 16
    nb = n // B
    dens = np.zeros((len(cand), nb), np.float32)
    for i, k in enumerate(cand):
        m = np.zeros(nb * B, np.int8)
        e = (w[:-k] == w[k:])[: nb * B]
        m[: len(e)] = e
        f = m.reshape(nb, B).sum(axis=1).astype(np.float32) / B
        r = max(1, (4 * k) // B)
        c = np.concatenate(([0], np.cumsum(f)))
        lo_ = np.clip(np.arange(nb) - r // 2, 0, nb)
        hi_ = np.clip(np.arange(nb) + r - r // 2, 0, nb)
        dens[i] = (c[hi_] - c[lo_]) / np.maximum(hi_ - lo_, 1)
    # regions: blocks where some stride repeats (gaps of a few blocks bridged)
    good = dens.max(axis=0) >= 0.12
    d = np.diff(np.concatenate(([0], good.view(np.int8), [0])))
    runs = list(zip(np.flatnonzero(d == 1), np.flatnonzero(d == -1)))
    merged = []
    for s, e in runs:
        if merged and s - merged[-1][1] <= 4:
            merged[-1][1] = e
        else:
            merged.append([s, e])
    bg = np.bincount(w, minlength=256) + 0.5
    bg = np.log2(bg / bg.sum())
    tables = []
    for s, e in merged:
        rs, re = s * B, min(e * B + KMAX, n)
        if re - rs < MINROWS * KMIN:
            continue
        # each window of the region labelled with the stride of least column code length
        Wn = 2048
        wins = list(range(rs, re, Wn))
        lab = []
        for a in wins:
            z = w[a:min(a + Wn, re)]
            costs = [colcost(z, k) if len(z) >= MINROWS * k else np.inf for k in cand]
            lab.append(cand[int(np.argmin(costs))] if np.isfinite(min(costs)) else None)
        i = 0
        while i < len(wins):
            k = lab[i]
            j = i
            while j < len(lab) and lab[j] == k:
                j += 1
            if k is not None:
                a, z = wins[i], min(wins[j - 1] + Wn, re)
                # the run at its stride, and at the strides related to it: a table smaller
                # than a window takes its neighbour's stride (the tiling tells them apart)
                # and the models fitted on its first and last windows alone: a run may be two
                # tables met (a directory and its chunks' sizes)
                for kk in [k] + [x for x in cand if x != k and (x % k == 0 or k % x == 0) and max(x, k) // min(x, k) <= 4]:
                    seeds = [(a, z)] + ([(a, a + Wn), (z - Wn, z)] if z - a > 2 * Wn else [])
                    for ta, tz in seeds:
                        t = refine(w, a, z, kk, bg, ta, tz)
                        if t and (t[1] - t[0]) // kk >= MINROWS:
                            tables.append((lo + t[0], lo + t[1], kk, t[2], kk == k))
            i = j
    # fragments of one table (same stride and phase, meeting): one table
    tables.sort(key=lambda t: (t[2], t[0] % t[2], t[0]))
    out = []
    for t in tables:
        p = out[-1] if out else None
        if p and p[2] == t[2] and (t[0] - p[0]) % t[2] == 0 and t[0] <= p[1] + t[2]:
            out[-1] = (p[0], max(p[1], t[1]), p[2], p[3] + t[3], p[4] or t[4])
        else:
            out.append(t)
    tables = out
    if verbose:
        print(f"    strides {cand}, {len(merged)} regions -> {len(tables)} tables", file=sys.stderr)
    return tables


LGH = None


def kt_bits(cnt, n):
    """Code length of a column of n bytes with these counts under an adaptive KT model."""
    global LGH
    if LGH is None or len(LGH) <= n + 256:
        x = np.arange(max(1 << 14, 2 * (n + 256)))
        LGH = np.array([math.lgamma(v + 0.5) for v in x]) / math.log(2)  # log2 G(v + 1/2), G the gamma function
    # log2 G(n + 128) / G(128) - sum of log2 G(c + 1/2) / G(1/2)
    tot = (math.lgamma(n + 128) - math.lgamma(128)) / math.log(2)
    return tot - (LGH[cnt] - LGH[0]).sum()


NULL = {}


def null_cost(n, k):
    """colcost of n random bytes at stride k."""
    if (n, k) not in NULL:
        z = np.random.default_rng(n * 131 + k).integers(0, 256, n).astype(np.uint8)
        NULL[n, k] = colcost(z, k)
    return NULL[n, k]


def colcost(z, k):
    """Bits a byte of z under k adaptive (KT) order-0 column models."""
    n = len(z) // k
    m = z[: n * k].reshape(n, k)
    bits = 0.0
    for c in range(k):
        bits += kt_bits(np.bincount(m[:, c], minlength=256), n)
    return bits / (n * k)


def refine(w, a, z, k, bg, ta=None, tz=None):
    """The table of stride k found in w[a:z], its ends moved to a maximum-sum run of the
    bytes' log-ratio under the column models (fitted on w[ta:tz], by default the run) against
    order 0, searched a window around."""
    n = len(w)
    lo_, hi_ = max(0, a - 2048), min(n, z + 2048)
    ta, tz = (a, z) if ta is None else (ta, tz)
    rows = (tz - ta) // k
    if rows < 2:
        return None
    m = w[ta: ta + rows * k].reshape(rows, k)
    cols = np.stack([np.bincount(m[:, c], minlength=256) for c in range(k)]) + 0.5
    cols = np.log2(cols / cols.sum(axis=1, keepdims=True))
    p = np.arange(lo_, hi_)
    s = cols[(p - ta) % k, w[p]] - bg[w[p]]
    # maximum-sum run
    c = np.concatenate(([0.0], np.cumsum(s)))
    j = int(np.argmax(c - np.minimum.accumulate(c)))
    i = int(np.argmin(c[: j + 1]))
    if j <= i:
        return None
    st, en = lo_ + i, lo_ + j
    # a whole number of rows from the start; and the bits the column models save there
    return st, st + (en - st + k // 2) // k * k, float(c[j] - c[i])


def words(b, pos):
    """The 64-bit words at positions pos, little- and big-endian."""
    g = b[np.clip(pos, 0, len(b) - 8)[:, None] + np.arange(8)]
    return g.copy().view("<u8")[:, 0], g.copy().view(">u8")[:, 0]


SHIFTS = np.arange(64, dtype=np.uint64)


def fields(b, table, rows, cs=None):
    """For each (byte, endianness): rows x 64 shifts of the word there."""
    st, en, k = table
    for c in (range(k) if cs is None else cs):
        pos = st + c + k * rows
        for e, v in zip("<>", words(b, pos)):
            yield c, e, v[:, None] >> SHIFTS[None, :]


def mask(bits):
    return np.uint64((1 << bits) - 1) if bits < 64 else np.uint64(0xFFFF_FFFF_FFFF_FFFF)


# ---------------------------------------------------------------- tiling


class Field:
    def __init__(self, c, e, r, bits, a=0):
        self.c, self.e, self.r, self.bits, self.a = c, e, int(r), int(bits), a

    def get(self, b, table, rows):
        st, _, k = table
        v = words(b, st + self.c + k * rows)[self.e == ">"]
        return ((v >> np.uint64(self.r)) & mask(self.bits)).astype(np.int64) << self.a

    def __repr__(self):
        a = f"<<{self.a}" if self.a else ""
        return f"@{self.c}{self.e}[{self.r}:+{self.bits}]{a}"


def size_fit(b, table, rows, need, slack, near):
    """Size fields s with 0 <= need - s < slack on most rows: [(fit, field)] best first.
    They are looked for within a record of byte `near` (the offset's): the table's rows
    need not start where its records do."""
    out = []
    k = table[2]
    for c, e, f in fields(b, table, rows, range(max(near - k + 1, -table[0]), near + k)):
        for bits in sorted({bitlen(need.max()), 8, 16, 24, 32}):
            g = (f & mask(bits)).astype(np.int64)
            d = need[:, None] - g
            fit = ((d >= 0) & (d < slack)).mean(axis=0)
            for r in np.flatnonzero(fit >= 0.3):
                if bits < 64 - r or bits == bitlen(need.max()):
                    out.append((float(fit[r]), Field(c, e, r, bits)))
    out.sort(key=lambda t: -t[0])
    # one field per distinct value set
    seen, res = set(), []
    for fit, fl in out:
        key = fl.get(b, table, rows[:64]).tobytes()
        if key not in seen:
            seen.add(key)
            res.append((fit, fl))
    return res[:4]


def explicit(b, table, L, slack=SLACK):
    """Offset fields of the table into a target of length L whose sorted values tile it."""
    st, en, k = table
    n = (en - st) // k
    rows = np.arange(n)
    samp = rows if n <= SAMPLE else np.linspace(0, n - 1, SAMPLE).astype(np.int64)
    found = []
    for c, e, f in fields(b, table, samp):
        for a in range(0, 12):
            La = (L + (1 << a) - 1) >> a
            bl = bitlen(La)
            g = f & mask(bl)
            # the two bits above the mask must be clear: else the field is wider and this is
            # its lower part (aliased offsets tile too, scaled by the same 2^a as their sizes)
            clear = (((f >> np.uint64(bl)) & np.uint64(3)) == 0).mean(axis=0) >= 0.9 if bl < 62 else True
            inr = (g < np.uint64(La)).mean(axis=0) * clear
            srt = np.sort(g, axis=0)
            dist = (np.diff(srt, axis=0) != 0).mean(axis=0) if len(g) > 1 else np.zeros(64)
            spread = srt[-1] >= np.uint64(La // 8)
            # members may be shared (rows of one offset): half the rows distinct will do
            for r in np.flatnonzero((inr >= 0.9) & (dist >= 0.5) & spread):
                found.append(Field(c, e, r, bitlen(La), a))
    # the same values under different (shift, scale) are one field; the ones reaching
    # nearest the end of the target first (a directory's last member ends there)
    uniq, seen = [], set()
    for fl in found:
        o = fl.get(b, table, samp)
        key = o.tobytes()
        if key in seen:
            continue
        seen.add(key)
        uniq.append((abs(L - int(o.max())), fl, o))
    uniq.sort(key=lambda t: t[0])
    res = []
    for _, fl, _ in uniq[:MAXOFF]:
        # the room of a member is up to the next offset: all rows are needed for it
        o = fl.get(b, table, rows)
        ok = (o >= 0) & (o < L)
        rs = rows[ok]
        u = np.unique(o[ok])
        if len(u) < 3:
            continue
        # the room of each member up to the next distinct offset; the last member's unknown
        nxt = np.searchsorted(u, o[ok], "right")
        keep = nxt < len(u)
        rs, need = rs[keep], u[nxt[keep]] - o[ok][keep]
        if len(rs) > SAMPLE:  # the sizes are looked for on a sample of the members
            pick = np.linspace(0, len(rs) - 1, SAMPLE).astype(np.int64)
            rs, need = rs[pick], need[pick]
        # a quick look on a few members first
        q = np.linspace(0, len(rs) - 1, min(len(rs), 48)).astype(np.int64)
        quick = size_fit(b, table, rs[q], need[q], slack, fl.c)
        if not quick or quick[0][0] < 0.8:
            continue
        sizes = size_fit(b, table, rs, need, slack, fl.c)
        if not sizes:
            continue
        fit, sf = sizes[0]
        sf = widen(b, table, rs, need, sf, slack)
        s = sf.get(b, table, rs)
        ctrl = np.roll(s, 1)  # sizes paired with the wrong members
        cfit = (((need - ctrl) >= 0) & ((need - ctrl) < slack)).mean()
        # rows the best field misses, taken by a second
        miss = ~(((need - s) >= 0) & ((need - s) < slack))
        alt = None
        if miss.any() and len(sizes) > 1:
            s2 = sizes[1][1].get(b, table, rs)
            ok2 = ((need - s2) >= 0) & ((need - s2) < slack)
            if ok2[miss].mean() > 0.8:
                alt = sizes[1][1]
                fit = float((~miss | ok2).mean())
        s_used = s if alt is None else np.where(miss, alt.get(b, table, rs), s)
        ev = evidence(need, s_used)
        # against the control: the fits of the members as a binomial, the control's rate the
        # null (sizes concentrated on a few values fit wrong members too)
        nn, p1, p0 = len(rs), min(fit, 1 - 1e-9), min(max(cfit, 1 / len(rs)), 1 - 1e-9)
        lr = nn * (p1 * np.log2(p1 / p0) + (1 - p1) * np.log2((1 - p1) / (1 - p0))) if p1 > p0 else 0.0
        ev = min(ev, float(lr))
        if fit >= 0.8 and ev >= EVIDENCE:
            res.append(dict(kind="explicit", off=fl, size=sf, alt=alt, fit=fit, ctrl=float(cfit), ev=ev))
    res.sort(key=lambda d: -d["ev"])
    return res


def widen(b, table, rows, need, fl, slack):
    """The field's width taken as far as its fit holds (the sample may not reach its top bits)."""
    def fit(f):
        d = need - f.get(b, table, rows)
        return ((d >= 0) & (d < slack)).mean()

    best = fit(fl)
    for bits in range(fl.bits + 1, 65 - fl.r):
        g = Field(fl.c, fl.e, fl.r, bits, fl.a)
        if fit(g) < best:
            break
        fl = g
    return fl


def evidence(room, size):
    """Bits against chance: each member fitting its room to within the slack G seen."""
    gap = room - size
    ok = (gap >= 0) & (gap < SLACK)
    if not ok.any():
        return 0.0
    G = 1 << bitlen(int(gap[ok].max()))  # the slack, a power of two
    # a size in [0, room] lands within G of its room with chance G / (room + 1)
    bits = np.log2(np.maximum(room[ok] + 1, G) / G).sum()
    return float(bits - (~ok).sum() * 8)


def implicit(b, table0, L, same):
    """Size fields that, laid end to end from a base, end exactly at the end of the target.

    The sum needs every row, and the table's ends may be off (two tables of alike columns
    meet, or a table comes in fragments). Its end is tried from 4 EDGE rows before to 4 KiB
    after. Its start is
    where the sizes start: after the last row before the end whose value is no size (zero, or
    over L), looking back BACK bytes, and EDGE rows after that. The members start right
    after the table: within its last record, or aligned up from there (16 to 4096); in
    another target, at its start."""
    st0, en0, k = table0
    back = min(st0, BACK) // k
    fwd = min(4096 // k, max(0, (len(b) - 8 - en0) // k))
    table = (st0 - back * k, en0 + fwd * k, k)
    st, en, _ = table
    n = (en - st) // k
    if n < MINROWS:
        return []
    rows = np.arange(n)
    i1s = np.arange(max(MINROWS, n - fwd - 4 * EDGE), n + 1)  # the end: 4 EDGE rows before, 4 KiB after
    idx = np.arange(n)[:, None]
    res = []
    for c, e, f in fields(b, table, rows):
        for bits in sorted({bitlen(L), 16, 32}):
            g = (f & mask(bits)).astype(np.int64)
            bad = (g == 0) | (g > L) | (g < 0)
            last = np.maximum.accumulate(np.where(bad, idx, -1), axis=0)  # last bad row <= i
            lastbad = np.concatenate((np.full((1, 64), -1), last))  # ... before row i1
            fe = (st + (i1s - 1) * k + c)[:, None] + (np.arange(64)[None, :] + bits + 7) // 8  # the field's end
            for al in (1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096):
                ga = (g + (al - 1)) // al * al
                cs = np.concatenate((np.zeros((1, 64), np.int64), np.cumsum(ga, axis=0)))
                for d0 in range(EDGE + 1):
                    i0 = np.minimum(lastbad[i1s] + 1 + d0, n)
                    if (i0 > n - MINROWS).all():
                        break
                    tot = cs[i1s] - np.take_along_axis(cs, i0, axis=0)
                    base = L - tot
                    if same:
                        rel = base - fe
                        okb = (rel >= 0) & (rel < k)
                        for a in (16, 512, 2048, 4096):
                            okb |= base == (fe + a - 1) // a * a
                    else:
                        okb = (base >= 0) & (base < 16)
                    ok = okb & (i1s[:, None] - i0 >= MINROWS) & (lastbad[i1s] < i0)
                    for a, r in zip(*np.nonzero(ok)):
                        i1, i0_ = int(i1s[a]), int(i0[a, r])
                        # the sum lands on one of about k + 4 bases by chance
                        ev = float(np.log2(max(L, 1) / (k + 4)))
                        t = (st + i0_ * k, st + i1 * k, k)
                        res.append(dict(kind="implicit", size=Field(c, e, r, bits), align=al, base=int(base[a, r]),
                                        pad=0, ev=ev, table=t))
    # one per value set, span and alignment: a sum is weak evidence, the codecs decide
    out, seen = [], set()
    for d in res:
        t = d["table"]
        key = (d["size"].get(b, t, np.arange((t[1] - t[0]) // k)).tobytes(), t, d["align"])
        if key not in seen:
            seen.add(key)
            out.append(d)
    out.sort(key=lambda d: -(d["table"][1] - d["table"][0]) // k)  # a chance sum is mostly short
    return out


def members_of(b, table, d, L):
    table = d.get("table", table)
    st, en, k = table
    rows = np.arange((en - st) // k)
    if d["kind"] == "explicit":
        o = d["off"].get(b, table, rows)
        s = d["size"].get(b, table, rows)
        u = np.unique(o[(o >= 0) & (o < L)])
        nxt = np.searchsorted(u, o, "right")
        room = np.where(nxt < len(u), u[np.minimum(nxt, len(u) - 1)], L) - o
        ok = (room - s >= 0) & (room - s < SLACK)
        if d["alt"] is not None:
            s2 = d["alt"].get(b, table, rows)
            ok2 = (room - s2 >= 0) & (room - s2 < SLACK)
            s = np.where(ok, s, s2)
            ok |= ok2
        # rows that do not fit are not members (the table's ends may hold rows of its
        # neighbours); the last member's room runs to the end of the target
        keep = ok & (o >= 0) & (o + s <= L)
        return list(zip(o[keep].tolist(), s[keep].tolist(), rows[keep].tolist()))
    s = d["size"].get(b, table, rows)
    al = d["align"]
    pos = d["base"] + np.concatenate(([0], np.cumsum((s + al - 1) // al * al)[:-1]))
    return list(zip(pos.tolist(), s.tolist(), rows.tolist()))


# ---------------------------------------------------------------- codecs


def lzo1x(src, limit):
    """LZO1X decoding (from the kernel's Documentation/staging/lzo.rst): (out, consumed)."""
    out = bytearray()
    i, n = 0, len(src)

    def ext(base):
        nonlocal i
        z = 0
        while src[i] == 0:
            z += 1
            i += 1
        v = src[i]
        i += 1
        return base + 255 * z + v

    def lits(m):
        nonlocal i
        if i + m > n:
            raise ValueError
        out.extend(src[i:i + m])
        i += m

    def copy(dist, m):
        p = len(out) - dist
        if p < 0:
            raise ValueError
        for _ in range(m):
            out.append(out[p])
            p += 1

    state = 0
    t = src[0]
    if t > 17:
        i = 1
        lits(t - 17)
        state = 4 if t - 17 >= 4 else t - 17
    while True:
        if len(out) > limit:
            raise ValueError
        t = src[i]
        i += 1
        if t < 16:
            if state == 0:
                m = ext(18) if t == 0 else 3 + t
                lits(m)
                state = 4
                continue
            h = src[i]
            i += 1
            if state < 4:
                copy((h << 2) + (t >> 2 & 3) + 1, 2)
            else:
                copy((h << 2) + (t >> 2 & 3) + 2049, 3)
            s = t & 3
        elif t < 32:
            m = ext(7) + 2 if t & 7 == 0 else (t & 7) + 2
            ds = src[i] | src[i + 1] << 8
            i += 2
            dist = 16384 + ((t & 8) << 11) + (ds >> 2)
            if dist == 16384:
                return bytes(out), i
            copy(dist, m)
            s = ds & 3
        elif t < 64:
            m = ext(31) + 2 if t & 31 == 0 else (t & 31) + 2
            ds = src[i] | src[i + 1] << 8
            i += 2
            copy((ds >> 2) + 1, m)
            s = ds & 3
        else:
            h = src[i]
            i += 1
            if t < 128:
                copy((h << 3) + (t >> 2 & 7) + 1, 3 + (t >> 5 & 1))
            else:
                copy((h << 3) + (t >> 2 & 7) + 1, 5 + (t >> 5 & 3))
            s = t & 3
        lits(s)
        state = s


def lz4_block(src, limit):
    out = bytearray()
    i, n = 0, len(src)
    while True:
        t = src[i]
        i += 1
        m = t >> 4
        if m == 15:
            while True:
                v = src[i]
                i += 1
                m += v
                if v != 255:
                    break
        if i + m > n:
            raise ValueError
        out.extend(src[i:i + m])
        i += m
        if i == n:
            return bytes(out), i
        dist = src[i] | src[i + 1] << 8
        i += 2
        m = (t & 15) + 4
        if t & 15 == 15:
            while True:
                v = src[i]
                i += 1
                m += v
                if v != 255:
                    break
        p = len(out) - dist
        if dist == 0 or p < 0 or len(out) + m > limit:
            raise ValueError
        for _ in range(m):
            out.append(out[p])
            p += 1


def zlibish(src, wbits):
    d = zlib.decompressobj(wbits)
    out = d.decompress(src)
    if not d.eof:
        raise ValueError
    return out, len(src) - len(d.unused_data)


def zstd_frames(src):
    if zstandard is None:
        raise ValueError
    out, i = bytearray(), 0
    while i + 4 <= len(src) and src[i:i + 4] == b"\x28\xb5\x2f\xfd":
        d = zstandard.ZstdDecompressor().decompressobj()
        out += d.decompress(src[i:])
        i = len(src) - len(d.unused_data)
    if i == 0:
        raise ValueError
    return bytes(out), i


CODECS = [
    ("zstd", lambda s, lim: zstd_frames(s)),
    ("zlib", lambda s, lim: zlibish(s, 15)),
    ("gzip", lambda s, lim: zlibish(s, 31)),
    ("deflate", lambda s, lim: zlibish(s, -15)),
    ("lzo1x", lzo1x),
    ("lz4b", lz4_block),
]


HEADERLESS = ("deflate", "lzo1x", "lz4b")


def probe(src, hints):
    """The codec that decodes src whole (up to 16 bytes of padding), or None."""
    for name, f in CODECS:
        if name in HEADERLESS and len(src) < 32:
            continue
        try:
            out, used = f(src, 1 << 28)
        except (ValueError, IndexError, zlib.error, Exception):
            continue
        if len(src) - 16 <= used <= len(src) and src[used:].strip(b"\0") == b"":
            # LZO1X and DEFLATE end by a marker of their own, and ending exactly at the member's
            # end is evidence; an LZ4 block ends where its input does, so its decoded size
            # must be a field of the row
            if name == "lz4b" and len(out) not in hints:
                continue
            return name, out
    return None


def row_values(b, table, row):
    """The values of the row's 16-, 32- and 64-bit fields (a decoded size must be among them)."""
    st, _, k = table
    pos = st + k * row + np.arange(k)
    vals = set()
    for e, v in zip("<>", words(b, pos)):
        for bits in (16, 32, 64):  # whole fields only: any shift and mask would match anything
            vals.update((v & mask(bits)).tolist())
    return vals


# ---------------------------------------------------------------- driver


def kinds(blobs):
    c = {}
    for x in blobs:
        h = x[:4]
        key = h.decode("latin1") if all(32 <= ch < 127 for ch in h) else h.hex()
        c[key] = c.get(key, 0) + 1
    return sorted(c.items(), key=lambda t: -t[1])[:6]


def siblings(path):
    d, f = os.path.split(path)
    stem = os.path.splitext(f)[0].lower()
    out = []
    for g in sorted(os.listdir(d or ".")):
        if g != f and os.path.splitext(g)[0].lower() == stem:
            out.append(os.path.join(d, g))
    return out


def load(path):
    with open(path, "rb") as fh:
        return np.frombuffer(fh.read() + b"\0" * 8, np.uint8)


def describe(d):
    if d["kind"] == "explicit":
        alt = f" or {d['alt']}" if d["alt"] is not None else ""
        return f"offset {d['off']} size {d['size']}{alt} (fit {d['fit']:.2f}, control {d['ctrl']:.2f})"
    return f"sizes {d['size']} end to end from {d['base']:#x}, aligned {d['align']} (pad {d['pad']})"


class Target:
    """What the pointers of a directory point into: a file, or members decoded end to end."""

    def __init__(self, name, buf=None, parts=None):
        self.name, self.buf, self.parts = name, buf, parts
        if buf is not None:
            self.L = len(buf) - 8
        else:
            self.starts = np.cumsum([0] + [n for n, _ in parts])
            self.L = int(self.starts[-1])
        self.cache = {}

    def read(self, o, s):
        if self.buf is not None:
            return self.buf[o:o + s].tobytes()
        out = bytearray()
        i = int(np.searchsorted(self.starts, o, "right")) - 1
        while s > 0 and i < len(self.parts):
            if i not in self.cache:
                if len(self.cache) > 64:
                    self.cache.clear()
                self.cache[i] = self.parts[i][1]()
            c = self.cache[i]
            a = o - int(self.starts[i])
            out += c[a:a + s]
            s -= len(c) - a
            o = int(self.starts[i + 1])
            i += 1
        return bytes(out)


ZSTD = bytes.fromhex("28b52ffd")


def decoded_size(blob):
    """A member's decoded size without decoding it, where its codec's header tells."""
    if zstandard is not None and blob[:4] == ZSTD:
        n = zstandard.frame_content_size(blob)
        if n >= 0:
            return n
    return None


def strongest(tables, N, keep=14):
    """The tables saving most bits under their column models, those near the file's head or
    tail first (where directories are; data has tables of its own: blocks of textures), at
    most `keep`, at most four for a region; as (start, end, k)."""
    # near: within 64 KiB of the head or tail, or meeting a table that is (a directory's
    # tables follow one another: NMS's chunk sizes start 1.6 MB in, right after its entries)
    edge = [t for t in tables if t[0] < 1 << 16 or t[1] > N - (1 << 16)]
    near = set(edge) | {t for t in tables if any(t[0] <= u[1] + 4096 and t[1] >= u[0] - 4096 for u in edge)}
    # the stride a run was labelled with before the strides related to it
    tables = sorted(tables, key=lambda t: (t not in near, not t[4], -t[3]))
    out, per = [], {}
    for t in tables:
        key = (t[0] >> 12, t[1] >> 12)
        if per.get(key, 0) >= 4 or t[:3] in out:
            continue
        per[key] = per.get(key, 0) + 1
        out.append(t[:3])
    return out[:keep]


def run(path, verbose):
    t0 = time.perf_counter()
    family = [path] + siblings(path)
    bufs = {os.path.basename(f): load(f) for f in family}
    targets = [Target(n, buf=bb) for n, bb in bufs.items()]
    tables = []  # (owner's name, owner's buffer, table)
    for n, bb in bufs.items():
        N = len(bb) - 8
        ts = find_tables(bb, 0, N, verbose) if N <= HEAD + TAIL else             find_tables(bb, 0, HEAD, verbose) + find_tables(bb, N - TAIL, N, verbose)
        tables += [(n, bb, t) for t in strongest(ts, N)]
    sizes = ", ".join(f"{n} {len(bb) - 8}" for n, bb in bufs.items())
    print(f"{path}: {sizes}; {len(tables)} tables ({time.perf_counter() - t0:.2f} s)")
    if verbose:
        for n, _, t in tables:
            print(f"  table {n} {t[0]:#x}..{t[1]:#x} stride {t[2]} ({(t[1] - t[0]) // t[2]} rows)")
    found, done, queued = [], set(), {t.name for t in targets}
    pending = list(targets)
    while pending:
        tg = pending.pop(0)
        L = tg.L
        for on, b, t in tables:
            st, en, k = t
            t1 = time.perf_counter()
            ds = explicit(b, t, L) + implicit(b, t, L, tg.buf is b)
            best = None
            for d in ds[:16]:
                ms = members_of(b, t, d, L)
                if not ms:
                    continue
                # members must fill the target from the first one on, and half of it: fields
                # shifted down (offsets and sizes both divided by 2^j) tile too, but only a part
                filled = sum(dict((o, s) for o, s, _ in ms).values())  # shared members once
                span = filled / max(L - min(o for o, _, _ in ms), 1)
                if span < COVER or filled < L / 2:
                    if verbose:
                        print(f"    (rejected: {describe(d)} fills {span:.1%} of {tg.name})")
                    continue
                # codecs of the members (a sample): decoding a member exactly is evidence too
                pick = ms if len(ms) <= 24 else [ms[i] for i in np.linspace(0, len(ms) - 1, 24).astype(int)]
                got, raw = {}, []
                for o, s, row in pick:
                    blob = tg.read(o, s)
                    if not blob:
                        continue
                    p = probe(blob, row_values(b, d.get("table", t), row))
                    got[p[0] if p else "stored"] = got.get(p[0] if p else "stored", 0) + 1
                    raw.append(p[1] if p else blob)
                coded = sum(v for c, v in got.items() if c != "stored")
                # a decoder may succeed on a member by chance (LZ4 blocks of a few bytes do):
                # the members' codecs count only where most of the sample decodes
                ev = d["ev"] + (16 * coded * len(ms) / max(len(pick), 1) if coded >= 0.5 * len(pick) else 0)
                if best is None or ev > best[0]:
                    best = (ev, d, ms, got, raw, coded, pick)
            if verbose:
                print(f"    {tg.name} by {on} {st:#x}/{k}: {len(ds)} candidates ({time.perf_counter() - t1:.2f} s)")
            if best is None or best[0] < EVIDENCE:
                continue
            ev, d, ms, got, raw, coded, pick = best
            cov = sum(dict((o, s) for o, s, _ in ms).values()) / max(L, 1)  # shared members once
            key = (tg.name, frozenset((o, s) for o, s, _ in ms))
            if key in done:  # the same directory by another view of its table
                continue
            done.add(key)
            print(f"  -> {tg.name} ({L} B) by {on} {st:#x}..{en:#x}/{k}: {len(ms)} members, {describe(d)}; "
                  f"covers {cov:.1%}, {ev:.0f} bits")
            found.append((tg.name, t, d, ms))
            # the bytes before the directory found, on their own: a smaller table met it there
            # (TexBiomesALIEN's 60 entries before its chunk sizes took the chunks' stride)
            a0 = d.get("table", t)[0]
            if a0 > 64:
                more = strongest(find_tables(b, max(0, a0 - BACK), a0), len(b) - 8, keep=4)
                more = [(on, b, u) for u in more if all(u != x[2] for x in tables)]
                if more and verbose:
                    print(f"    before it: {[(hex(u[0]), hex(u[1]), u[2]) for _, _, u in more]}")
                tables += more
            print(f"     codecs {sorted(got.items(), key=lambda t: -t[1])}; contents {kinds(raw)}")
            if coded >= 0.9 * len(pick) and cov > 0.9 and tg.buf is not None and f"{tg.name}~decoded" not in queued:
                # the target decoded in place (chunks): a target in turn. The bytes before the
                # first member and between members are kept, unless they are zeros (padding)
                parts, at = [], 0
                for o, s, row in sorted(ms):
                    if o > at:
                        gap = tg.read(at, o - at)
                        if gap.strip(b"\0"):
                            parts.append((len(gap), (lambda gap=gap: gap)))
                    blob = tg.read(o, s)
                    n = decoded_size(blob)
                    if n is None:
                        p = probe(blob, row_values(b, d.get("table", t), row))
                        n = len(p[1]) if p else len(blob)
                    parts.append((n, (lambda blob=blob: (probe(blob, ()) or (None, blob))[1])))
                    at = max(at, o + s)
                v = Target(f"{tg.name}~decoded", parts=parts)
                queued.add(v.name)
                print(f"     members decoded end to end: {v.L} bytes")
                pending.append(v)
    print(f"  ({time.perf_counter() - t0:.2f} s)")
    return found


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-v", action="store_true")
    ap.add_argument("files", nargs="+")
    a = ap.parse_args()
    for f in a.files:
        run(f, a.v)


if __name__ == "__main__":
    main()
