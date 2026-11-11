"""Inter AC tables: walk the code tree of block 3 of an inter MB in a P frame after a flat I frame."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from tree import *
from fast import Dec
from probe import blocks_yuv
from ac_probe import fdct, single, _C
import ac_probe as A
from enc_p import enc_seq, flat, bits as bitstr
HERE = os.path.dirname(os.path.abspath(__file__))
_rng = np.random.RandomState(5)
REF_Y = np.clip(128 + _rng.randn(16, 16) * 30, 0, 255).astype(np.uint8)
REF = (REF_Y, np.full((8, 8), 128, np.uint8), np.full((8, 8), 128, np.uint8))
def p_target(C=-15):
    y = REF_Y.astype(float); y[8:, 8:] += C * np.outer(_C[0], _C[1])
    return (np.clip(np.round(y), 0, 255).astype(np.uint8),) + REF[1:]
_enc = enc_seq([REF, p_target()], w=16, h=16, q=5)
I_PKT = _enc[0]
_b = bitstr(_enc[1])
PREFIX = '0' + '11100'      # skip 0, M(inter, only block 3 coded), mv(0,0): found by bit-flip sensitivity of the encoder's frame
def terms_for(rl_levels=(-15, -25, -35, -45)):
    out = []
    for C in rl_levels:
        e = enc_seq([REF, p_target(C)], w=16, h=16, q=5); b = bitstr(e[1])[12:]
        assert b.startswith(PREFIX), b
        out.append(b[len(PREFIX):].rstrip('0'))
    return out
_o, _b_ = Dec().frames([I_PKT])[0]
PRED = np.array(blocks_yuv(_o)[3], float)
def d012(v): return '0' if v == 0 else '10' if v == 1 else '11'
def hdr_p(q=5, rl=2, dc=1, mv=1): return '01' + format(q, '05b') + '1' + d012(rl) + str(dc) + str(mv)
def decP(pkts):
    seq = []
    for p in pkts: seq += [I_PKT, p]
    res = DEC.frames(seq)
    return [res[2 * i + 1] for i in range(len(pkts))]
def make_mk(rl): return lambda bits_: tob(hdr_p(rl=rl) + PREFIX + bits_ + '0' * 40)
THRESH_P = 0.85
def flip_leaf_p(mk, p, terms, per=8, thresh=None):
    thresh = THRESH_P if thresh is None else thresh
    rng = random.Random(hash(p) & 0xffffffff)
    rests = [t + rnd(8, rng) for t in terms for _ in range(per)]
    K = len(rests)
    res = decP([mk(p + s + r) for r in rests for s in '01'])
    got = []
    for k in range(K):
        (o0, b0), (o1, b1) = res[2 * k], res[2 * k + 1]
        if b0 or b1 or o0 is None or o1 is None: continue
        bo0 = blocks_yuv(o0); bo1 = blocks_yuv(o1)
        if any(bo0[k] != bo1[k] for k in (0, 1, 2, 4, 5)): continue
        D = np.array(bo0[3], float) - np.array(bo1[3], float)
        s = single(D)
        if s is not None:
            F0 = fdct(np.array(bo0[3]) - PRED); F1 = fdct(np.array(bo1[3]) - PRED); u, v = s[0]
            if abs(F0[u, v] + F1[u, v]) > 2.0 + 0.1 * abs(s[1]): continue
            got.append((s[0], round(abs(s[1]) / 2)))
    if not got: return None
    from collections import Counter
    pos = Counter(g[0] for g in got).most_common(1)[0][0]
    amps = sorted(g[1] for g in got if g[0] == pos); med = amps[len(amps) // 2]
    good = [x for x in amps if abs(x - med) <= max(3, 0.12 * med)]
    if len(good) < thresh * K: return None
    return pos, float(np.mean(good))
def walk_p(mk, terms, maxd=17):
    leaves = {}; unres = []; level = ['']
    while level:
        nxt = []
        for p in level:
            r = flip_leaf_p(mk, p, terms)
            if r is not None: leaves[p] = r
            elif len(p) < maxd:
                pk = [mk(p + rnd(20, random.Random(k))) for k in range(6)]
                if all(b or o is None for o, b in decP(pk)): continue
                nxt += [p + '0', p + '1']
            else: unres.append(p)
        level = nxt
    return leaves, unres
if __name__ == '__main__':
    rl = int(sys.argv[1])
    terms = ['01111', '01110']
    print('terms', terms)
    leaves, unres = walk_p(make_mk(rl), terms)
    print('inter rl', rl, len(leaves), 'kraft', kraft(leaves), 'unres', len(unres), flush=True)
    json.dump({'leaves': {p: [list(v[0]), v[1]] for p, v in leaves.items()}, 'unres': unres}, open(os.path.join(HERE, 'p_ac_raw_rl%d.json' % rl), 'w'))
    for p, v in sorted(leaves.items(), key=lambda x: (len(x[0]), x[0]))[:8]: print(p, v)
