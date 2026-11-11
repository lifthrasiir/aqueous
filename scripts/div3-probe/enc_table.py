"""Exact intra AC tables of the encoder's own (rl=2 luma, rlc=1 chroma): single coefficients give the last=1 codes,
a coefficient followed by (run 0, level 1) the last=0 codes. Labels are exact: (last, run, level)."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from enc_ac import *
from div3 import IDX
from ac_probe import _C
HERE = os.path.dirname(os.path.abspath(__file__))
Q = 2
INV = {i: p for p, i in IDX.items()}
CBP = {3: '00110', 4: '01001'}
def packet(blk, coefs):
    y = np.full((16, 16), 128.0); u = np.full((8, 8), 128.0); v = np.full((8, 8), 128.0)
    for pos, C in coefs:
        b = np.outer(_C[pos[0]], _C[pos[1]])
        if blk < 4: r0, c0 = (blk // 2) * 8, (blk % 2) * 8; y[r0:r0 + 8, c0:c0 + 8] += C * b
        else: u += C * b
    fr = tuple(np.clip(np.round(a), 0, 255).astype(np.uint8) for a in (y, u, v))
    return enc_frames([fr], q=Q)[0]
def ac_part(blk, coefs):
    s = bits_of(packet(blk, coefs)); h = header(s); assert (h['rl'], h['rlc'], h['dc']) == (2, 1, 1)
    z = zero('luma1'); zc = zero('chroma1')
    i = h['mb'] + len(CBP[blk]) + 1 + (4 * len(z) if blk == 4 else (blk + 1) * len(z)) + (len(zc) if blk == 4 else 0)
    if s[h['mb']:h['mb'] + len(CBP[blk])] != CBP[blk]: return None
    tail = (zc * (1 if blk == 4 else 2)) + tail_info(Q)[1]
    for pad in range(8):
        e = len(s) - pad - len(tail)
        if e >= i and s[e:e + len(tail)] == tail and set(s[len(s) - pad:]) <= {'0'}: return s[i:e]
    return None
def C_of(l): return 4 * l + 1
from fast import Dec
_D = Dec()
def decoded(blk, coefs):
    """[(pos, level)] the decoder reads from the encoder's packet for these coefficients."""
    pk = packet(blk, coefs); o, bad = _D.frames([pk])[0]
    F = fdct(blocks_yuv(o)[min(blk, 5) if blk < 4 else blk])
    return sorted(((u, v), round((abs(F[u, v]) - 1) / 4)) for u in range(8) for v in range(8) if abs(F[u, v]) > 3 and (u, v) != (0, 0))
def build(blk):
    last1 = {}
    for idx in range(1, 64):
        pos = INV[idx]; run = idx - 1
        for want in range(3, 420):
            d = decoded(blk, [(pos, want)])
            if not d and want < 20: continue
            if len(d) != 1 or d[0][0] != pos: break
            l = d[0][1]
            if (run, l) in last1: continue
            ac = ac_part(blk, [(pos, want)])
            if ac is None: break
            last1[(run, l)] = ac[:-1]
    return last1
if __name__ == '__main__':
    blk = int(sys.argv[1])
    t = build(blk)
    json.dump({'%d,%d' % k: v for k, v in t.items()}, open(os.path.join(HERE, 'enc_last1_blk%d.json' % blk), 'w'))
    print(len(t))

def exact(blk):
    """The exact table as code -> (run, level, last) plus the escape prefix."""
    t = json.load(open(os.path.join(HERE, 'enc_last1_blk%d.json' % blk)))
    t = {tuple(map(int, k.split(','))): v for k, v in t.items()}
    hi = [v for (r, l), v in t.items() if l >= 30 and r < 40]
    from tree import lcp
    esc1 = lcp(hi)               # ESC + the esc3 marker bits shared
    # the escape prefix: shortest prefix shared by all escaped codes of a run at levels 12..30
    cand = [v for (r, l), v in t.items() if 12 <= l <= 30 and r < 20]
    esc = lcp(cand)
    codes = {}
    for (r, l), v in sorted(t.items()):
        if v.startswith(esc): continue
        codes[v] = (r, l, 1)
    # last=0 : coefficient (idx a, level l) then (idx a+1 = run 0, level 1, last=1)
    seen0 = set()
    for idx in range(1, 63):
        pos = INV[idx]; nxt = INV[idx + 1]; run = idx - 1
        for want in range(3, 420):
            d = decoded(blk, [(pos, want), (nxt, 9)])
            if len(d) == 1 and want < 20: continue
            if len(d) != 2 or d[0][0] != pos or d[1] != (nxt, 2): break
            l = d[0][1]
            if (run, l) in seen0: continue
            ac = ac_part(blk, [(pos, want), (nxt, 9)])
            if ac is None: break
            t2 = codes_inv(codes, (0, d[1][1], 1))
            if not ac.endswith(t2 + ac[-1]): break
            code = ac[:len(ac) - len(t2) - 2]
            if code.startswith(esc) or not code: break
            seen0.add((run, l)); codes[code] = (run, l, 0)
    return codes, esc
def codes_inv(codes, key): return next(c for c, v in codes.items() if v == key)
