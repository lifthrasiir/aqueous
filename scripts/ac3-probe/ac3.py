"""AC-3 (ATSC A/52) syncframe parser down to the quantized mantissas, to check that the syntax consumes exactly the frame.
Written from the text of the standard (sections 5, 7.1, 7.2, 7.3)."""
import json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
T = json.load(open(os.path.join(HERE, 'ac3_tables.json')))
BNDTAB, BNDSZ, LATAB, HTH, BAPTAB = T['bndtab'], T['bndsz'], T['latab'], T['hth'], T['baptab']
MASKTAB = []
for _b in range(50): MASKTAB += [_b] * BNDSZ[_b]
MASKTAB += [49] * (256 - len(MASKTAB))
MANT_BITS = {0: 0, 1: 5, 2: 7, 3: 3, 4: 7, 5: 4, 6: 5, 7: 6, 8: 7, 9: 8, 10: 9, 11: 10, 12: 11, 13: 12, 14: 14, 15: 16}
NFCHANS = [2, 1, 2, 3, 3, 4, 4, 5]


class Bits:
    def __init__(self, d):
        self.d = d
        self.p = 0
        self.n = len(d) * 8

    def get(self, k):
        if k == 0: return 0
        if self.p + k > self.n: raise EOFError
        lo = self.p >> 3
        hi = (self.p + k + 7) >> 3
        v = int.from_bytes(self.d[lo:hi], 'big')
        v >>= (hi - lo) * 8 - (self.p & 7) - k
        self.p += k
        return v & ((1 << k) - 1)


def logadd(a, b):
    c = a - b
    addr = min(abs(c) >> 1, 255)
    return a + LATAB[addr] if c >= 0 else b + LATAB[addr]


def calc_lowcomp(a, b0, b1, bin_):
    if bin_ < 7:
        if b0 + 256 == b1: a = 384
        elif b0 > b1: a = max(0, a - 64)
    elif bin_ < 20:
        if b0 + 256 == b1: a = 320
        elif b0 > b1: a = max(0, a - 64)
    else:
        a = max(0, a - 128)
    return a


def bit_alloc(exp, start, end, fscod, fgain, snroffset, sdecay, fdecay, sgain, dbknee, floor, fastleak0, slowleak0, delta):
    """bap[] for bins start..end-1 from the exponents `exp` (a dict by bin)."""
    psd = {b: 3072 - (exp[b] << 7) for b in range(start, end)}
    bndpsd = {}
    j = start
    k = MASKTAB[start]
    while True:
        lastbin = min(BNDTAB[k] + BNDSZ[k], end)
        bndpsd[k] = psd[j]
        j += 1
        for _ in range(j, lastbin):
            bndpsd[k] = logadd(bndpsd[k], psd[j])
            j += 1
        k += 1
        if not end > lastbin: break
    bndstrt = MASKTAB[start]
    bndend = MASKTAB[end - 1] + 1
    excite = {}
    lowcomp = 0
    if bndstrt == 0:
        lowcomp = calc_lowcomp(lowcomp, bndpsd[0], bndpsd[1], 0)
        excite[0] = bndpsd[0] - fgain - lowcomp
        lowcomp = calc_lowcomp(lowcomp, bndpsd[1], bndpsd[2], 1)
        excite[1] = bndpsd[1] - fgain - lowcomp
        begin = 7
        for bin_ in range(2, 7):
            if bndend != 7 or bin_ != 6:
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin_], bndpsd[bin_ + 1], bin_)
            fastleak = bndpsd[bin_] - fgain
            slowleak = bndpsd[bin_] - sgain
            excite[bin_] = fastleak - lowcomp
            if bndend != 7 or bin_ != 6:
                if bndpsd[bin_] <= bndpsd[bin_ + 1]:
                    begin = bin_ + 1
                    break
        for bin_ in range(begin, min(bndend, 22)):
            if bndend != 7 or bin_ != 6:
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin_], bndpsd[bin_ + 1], bin_)
            fastleak -= fdecay
            fastleak = max(fastleak, bndpsd[bin_] - fgain)
            slowleak -= sdecay
            slowleak = max(slowleak, bndpsd[bin_] - sgain)
            excite[bin_] = max(fastleak - lowcomp, slowleak)
        begin = 22
    else:
        begin = bndstrt
        fastleak = fastleak0
        slowleak = slowleak0
    for bin_ in range(begin, bndend):
        fastleak -= fdecay
        fastleak = max(fastleak, bndpsd[bin_] - fgain)
        slowleak -= sdecay
        slowleak = max(slowleak, bndpsd[bin_] - sgain)
        excite[bin_] = max(fastleak, slowleak)
    mask = {}
    for bin_ in range(bndstrt, bndend):
        if bndpsd[bin_] < dbknee:
            excite[bin_] += (dbknee - bndpsd[bin_]) >> 2
        mask[bin_] = max(excite[bin_], HTH[bin_][fscod])
    if delta:
        band = 0
        for off, ln, ba in delta:
            band += off
            d = (ba - 3) << 7 if ba >= 4 else (ba - 4) << 7
            for _ in range(ln):
                if band in mask: mask[band] += d
                band += 1
    bap = {}
    i = start
    j = MASKTAB[start]
    while True:
        lastbin = min(BNDTAB[j] + BNDSZ[j], end)
        mask[j] -= snroffset
        mask[j] -= floor
        if mask[j] < 0: mask[j] = 0
        mask[j] &= 0x1fe0
        mask[j] += floor
        for _ in range(i, lastbin):
            address = (psd[i] - mask[j]) >> 5
            address = min(63, max(0, address))
            bap[i] = BAPTAB[address]
            i += 1
        j += 1
        if not end > lastbin: break
    return bap


def decode_exps(b, absexp, ngrps, grpsize):
    d = []
    for _ in range(ngrps):
        g = b.get(7)
        if g >= 125: raise ValueError('bad exponent group')
        d += [g // 25 - 2, g % 25 // 5 - 2, g % 5 - 2]
    out = [absexp]
    prev = absexp
    for x in d:
        prev += x
        if not 0 <= prev <= 24: raise ValueError('exponent out of range')
        out += [prev] * grpsize
    return out


def parse_frame(data, group_across=True):
    """Returns (bits left unconsumed before the final 18 bits, the last block's state)."""
    b = Bits(data)
    if b.get(16) != 0x0B77: raise ValueError('syncword')
    b.get(16)
    fscod = b.get(2)
    frm = b.get(6)
    total = T['frame_words'][frm][fscod] * 16
    if b.get(5) != 8: raise ValueError('bsid')
    b.get(3)
    acmod = b.get(3)
    if acmod & 1 and acmod != 1: b.get(2)
    if acmod & 4: b.get(2)
    if acmod == 2: b.get(2)
    lfeon = b.get(1)
    b.get(5)
    if b.get(1): b.get(8)
    if b.get(1): b.get(8)
    if b.get(1):
        b.get(5)
        b.get(2)
    if acmod == 0:
        b.get(5)
        if b.get(1): b.get(8)
        if b.get(1): b.get(8)
        if b.get(1):
            b.get(5)
            b.get(2)
    b.get(2)
    if b.get(1): b.get(14)
    if b.get(1): b.get(14)
    if b.get(1):
        n = b.get(6)
        b.get((n + 1) * 8)
    nf = NFCHANS[acmod]
    st = dict(cplinu=0, chincpl=[0] * 5, cplbegf=0, cplendf=0, ncplsubnd=0, ncplbnd=0, cplexpstr=0, chexpstr=[0] * 5, lfeexpstr=0,
              exps=[None] * 5, cplexp=None, lfeexp=None, chbwcod=[0] * 5, cplabs=0, sdcycod=0, fdcycod=0, sgaincod=0, dbpbcod=0, floorcod=0,
              csnroffst=0, cplfsnroffst=0, cplfgaincod=0, fsnroffst=[0] * 5, fgaincod=[0] * 5, lfefsnroffst=0, lfefgaincod=0, cplfleak=0, cplsleak=0,
              deltbae=[0] * 5, delta=[None] * 5, cpldeltbae=0, cpldelta=None, cplbndstrc=[], phsflginu=0)
    for blk in range(6):
        for ch in range(nf): b.get(1)       # blksw
        for ch in range(nf): b.get(1)       # dithflag
        if b.get(1): b.get(8)
        if acmod == 0 and b.get(1): b.get(8)
        if b.get(1):                          # cplstre
            st['cplinu'] = b.get(1)
            if st['cplinu']:
                st['chincpl'] = [b.get(1) for _ in range(nf)] + [0] * (5 - nf)
                st['phsflginu'] = b.get(1) if acmod == 2 else 0
                st['cplbegf'] = b.get(4)
                st['cplendf'] = b.get(4)
                st['ncplsubnd'] = 3 + st['cplendf'] - st['cplbegf']
                if st['ncplsubnd'] <= 0: raise ValueError('cpl bands')
                st['cplbndstrc'] = [0] + [b.get(1) for _ in range(1, st['ncplsubnd'])]
                st['ncplbnd'] = st['ncplsubnd'] - sum(st['cplbndstrc'][1:])
            else:
                st['chincpl'] = [0] * 5
        if st['cplinu']:
            cplcoe = [0] * 5
            for ch in range(nf):
                if st['chincpl'][ch]:
                    cplcoe[ch] = b.get(1)
                    if cplcoe[ch]:
                        b.get(2)
                        for _ in range(st['ncplbnd']):
                            b.get(4)
                            b.get(4)
            if acmod == 2 and st['phsflginu'] and (cplcoe[0] or cplcoe[1]):
                for _ in range(st['ncplbnd']): b.get(1)
        if acmod == 2:
            if b.get(1):
                cb = st['cplbegf']
                n = 4 if (cb > 2 or not st['cplinu']) else (3 if cb > 0 else 2)
                for _ in range(n): b.get(1)
        if st['cplinu']: st['cplexpstr'] = b.get(2)
        for ch in range(nf): st['chexpstr'][ch] = b.get(2)
        if lfeon: st['lfeexpstr'] = b.get(1)
        cplstrtmant = 37 + 12 * st['cplbegf']
        cplendmant = 37 + 12 * (st['cplendf'] + 3)
        for ch in range(nf):
            if st['chexpstr'][ch] and not st['chincpl'][ch]:
                st['chbwcod'][ch] = b.get(6)
        endmant = [0] * 5
        for ch in range(nf):
            endmant[ch] = cplstrtmant if (st['cplinu'] and st['chincpl'][ch]) else 37 + 3 * (st['chbwcod'][ch] + 12)
        if st['cplinu'] and st['cplexpstr']:
            g = [0, 1, 2, 4][st['cplexpstr']]
            ncplgrps = (cplendmant - cplstrtmant) // (3 * g)
            st['cplabs'] = b.get(4)
            e = decode_exps(b, st['cplabs'] << 1, ncplgrps, g)
            st['cplexp'] = {cplstrtmant + n: e[n + 1] for n in range(cplendmant - cplstrtmant)}
        for ch in range(nf):
            if st['chexpstr'][ch]:
                g = [0, 1, 2, 4][st['chexpstr'][ch]]
                nch = {1: (endmant[ch] - 1) // 3, 2: (endmant[ch] - 1 + 3) // 6, 3: (endmant[ch] - 1 + 9) // 12}[st['chexpstr'][ch]]
                a = b.get(4)
                st['exps'][ch] = decode_exps(b, a, nch, g)
                b.get(2)
        if lfeon and st['lfeexpstr']:
            a = b.get(4)
            st['lfeexp'] = decode_exps(b, a, 2, 1)
        if b.get(1):
            st['sdcycod'] = b.get(2)
            st['fdcycod'] = b.get(2)
            st['sgaincod'] = b.get(2)
            st['dbpbcod'] = b.get(2)
            st['floorcod'] = b.get(3)
        if b.get(1):
            st['csnroffst'] = b.get(6)
            if st['cplinu']:
                st['cplfsnroffst'] = b.get(4)
                st['cplfgaincod'] = b.get(3)
            for ch in range(nf):
                st['fsnroffst'][ch] = b.get(4)
                st['fgaincod'][ch] = b.get(3)
            if lfeon:
                st['lfefsnroffst'] = b.get(4)
                st['lfefgaincod'] = b.get(3)
        if st['cplinu']:
            if b.get(1):
                st['cplfleak'] = b.get(3)
                st['cplsleak'] = b.get(3)
        if b.get(1):
            if st['cplinu']: st['cpldeltbae'] = b.get(2)
            for ch in range(nf): st['deltbae'][ch] = b.get(2)
            if st['cplinu'] and st['cpldeltbae'] == 1:
                n = b.get(3)
                st['cpldelta'] = [(b.get(5), b.get(4), b.get(3)) for _ in range(n + 1)]
            for ch in range(nf):
                if st['deltbae'][ch] == 1:
                    n = b.get(3)
                    st['delta'][ch] = [(b.get(5), b.get(4), b.get(3)) for _ in range(n + 1)]
        if b.get(1):
            n = b.get(9)
            b.get(n * 8)
        # bit allocation
        sdecay = T['slowdec'][st['sdcycod']]
        fdecay = T['fastdec'][st['fdcycod']]
        sgain = T['slowgain'][st['sgaincod']]
        dbknee = T['dbpbtab'][st['dbpbcod']]
        floor = T['floortab'][st['floorcod']]
        allzero = st['csnroffst'] == 0 and st['cplfsnroffst'] == 0 and st['lfefsnroffst'] == 0 and all(x == 0 for x in st['fsnroffst'][:nf])
        baps = {}

        def alloc(exp, s, e, fg, snr, f0=0, s0=0, delta=None):
            if allzero: return {i: 0 for i in range(s, e)}
            return bit_alloc(exp, s, e, fscod, T['fastgain'][fg], snr, sdecay, fdecay, sgain, dbknee, floor, f0, s0, delta)

        for ch in range(nf):
            snr = (((st['csnroffst'] - 15) << 4) + st['fsnroffst'][ch]) << 2
            ex = {i: v for i, v in enumerate(st['exps'][ch])}
            baps[ch] = alloc(ex, 0, endmant[ch], st['fgaincod'][ch], snr, delta=st['delta'][ch] if st['deltbae'][ch] in (0, 1) else None)
        if st['cplinu']:
            snr = (((st['csnroffst'] - 15) << 4) + st['cplfsnroffst']) << 2
            baps['cpl'] = alloc(st['cplexp'], cplstrtmant, cplendmant, st['cplfgaincod'], snr, (st['cplfleak'] << 8) + 768, (st['cplsleak'] << 8) + 768,
                                st['cpldelta'] if st['cpldeltbae'] in (0, 1) else None)
        if lfeon:
            snr = (((st['csnroffst'] - 15) << 4) + st['lfefsnroffst']) << 2
            ex = {i: v for i, v in enumerate(st['lfeexp'])}
            baps['lfe'] = alloc(ex, 0, 7, st['lfefgaincod'], snr)
        # mantissas
        pend = {1: 0, 2: 0, 4: 0}

        def mant(bap):
            if bap in (1, 2, 4):
                if pend[bap] == 0:
                    b.get(MANT_BITS[bap])
                    pend[bap] = {1: 2, 2: 2, 4: 1}[bap]
                else:
                    pend[bap] -= 1
            elif bap:
                b.get(MANT_BITS[bap])

        got = 0
        for ch in range(nf):
            for bin_ in range(endmant[ch]): mant(baps[ch][bin_])
            if st['cplinu'] and st['chincpl'][ch] and not got:
                for bin_ in range(cplstrtmant, cplendmant): mant(baps['cpl'][bin_])
                got = 1
            if not group_across: pend = {1: 0, 2: 0, 4: 0}
        if lfeon:
            for bin_ in range(7): mant(baps['lfe'][bin_])
    return total - b.p - 18, st


if __name__ == '__main__':
    from stream import audio_chunks, syncframes
    from collections import Counter
    ch = audio_chunks('F:/Data/aqr-corpus/aqr-div3/mari.avi', limit=1500)
    buf = b''.join(ch)
    fr = syncframes(buf)
    c = Counter()
    for off, size, fscod, frm in fr[:300]:
        try:
            left, st = parse_frame(buf[off:off + size])
            c[left] += 1
        except Exception as e:
            c['err ' + str(e)[:30]] += 1
    print(c.most_common(8))
