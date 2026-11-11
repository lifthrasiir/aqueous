"""AC (run/level/last) tables of an intra block via sign-flip differences."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from tree import *
from probe import blocks_yuv
N = 8
_C = np.array([[np.sqrt((1 if u == 0 else 2) / N) * np.cos((2 * x + 1) * u * np.pi / 16) for x in range(N)] for u in range(N)])
def fdct(b): a = np.array(b, float).reshape(8, 8); return _C @ a @ _C.T
def single(D, minamp=6):
    """If the 8x8 difference D is one DCT basis function (up to pixel rounding), return ((u, v), coefficient) else None."""
    D = np.array(D, float).reshape(8, 8); F = fdct(D); i = np.unravel_index(np.abs(F).argmax(), F.shape); a = F[i]
    if abs(a) < minamp: return None
    fit = a * np.outer(_C[i[0]], _C[i[1]])
    if np.abs(D - fit).max() > 1.05: return None
    return (int(i[0]), int(i[1])), float(a)

THRESH = 0.85
LASTRATE = [0.0]
TERMS = ['01110', '0011110', '0011100', '00100000']   # rl=2 last=1 codes + sign 0
def flip_leaf(mk, blk, p, K=40, rng=None):
    return flip_leaf_t(mk, blk, p)
def flip_leaf_t(mk, blk, p, per=4):
    """As flip_leaf, but the bits after the flipped one are a known terminating symbol, so the block ends cleanly."""
    rng = random.Random(hash(p) & 0xffffffff)
    rests = [t + rnd(8, rng) for t in TERMS for _ in range(per)]
    K = len(rests)
    res = DEC.frames([mk(p + s + r) for r in rests for s in '01'])
    got = []
    for k in range(K):
        (o0, b0), (o1, b1) = res[2 * k], res[2 * k + 1]
        if b0 or b1 or o0 is None or o1 is None: continue
        D = np.array(blocks_yuv(o0)[blk], float) - np.array(blocks_yuv(o1)[blk], float)
        s = single(D)
        if s is not None:
            F0 = fdct(blocks_yuv(o0)[blk]); F1 = fdct(blocks_yuv(o1)[blk]); u, v = s[0]
            if abs(F0[u, v] + F1[u, v]) > 2.0 + 0.1 * abs(s[1]): continue
            got.append((s[0], round(abs(s[1]) / 2)))
    LASTRATE[0] = 0.0
    if not got: return None
    from collections import Counter
    pos = Counter(g[0] for g in got).most_common(1)[0][0]
    amps = sorted(g[1] for g in got if g[0] == pos); med = amps[len(amps) // 2]
    good = [x for x in amps if abs(x - med) <= max(3, 0.12 * med)]
    LASTRATE[0] = len(good) / K
    if len(good) < THRESH * K: return None
    return pos, float(np.mean(good))
def flip_leaf_old(mk, blk, p, K=40, rng=None):
    """p is a complete code iff, for most random rests, the outputs for p+'0'+rest and p+'1'+rest differ by one basis pattern
    (the same one every time)."""
    rng = rng or random.Random(hash(p) & 0xffffffff)
    rests = [rnd(24, rng) for _ in range(K)]
    res = DEC.frames([mk(p + s + r) for r in rests for s in '01'])
    got = []
    for k in range(K):
        (o0, b0), (o1, b1) = res[2 * k], res[2 * k + 1]
        if b0 or b1 or o0 is None or o1 is None: continue
        D = np.array(blocks_yuv(o0)[blk], float) - np.array(blocks_yuv(o1)[blk], float)
        s = single(D)
        if s is not None:
            # a real sign flip: each output holds the coefficient there, with opposite signs (not two levels of one run)
            F0 = fdct(blocks_yuv(o0)[blk]); F1 = fdct(blocks_yuv(o1)[blk]); u, v = s[0]
            if abs(F0[u, v] + F1[u, v]) > 2.0 + 0.1 * abs(s[1]): continue
            got.append((s[0], round(abs(s[1]) / 2)))
    LASTRATE[0] = 0.0
    if len(got) < THRESH * K: return None
    from collections import Counter
    (pos, amp), n = Counter((g[0], g[1] // 3) for g in got).most_common(1)[0]
    LASTRATE[0] = n / K
    if n < THRESH * K: return None
    amps = [g[1] for g in got if g[0] == pos and g[1] // 3 == amp]
    return pos, float(np.mean(amps))

def walk_ac(mk, blk, maxd=17):
    leaves = {}; unres = []; level = ['']
    while level:
        nxt = []
        for p in level:
            r = flip_leaf(mk, blk, p)
            if r is not None: leaves[p] = r
            elif len(p) < maxd:
                # prune: a prefix no stream of which decodes cleanly is dead
                pk = [mk(p + rnd(20, random.Random(k))) for k in range(8)]
                if all(b or o is None for o, b in DEC.frames(pk)): continue
                nxt += [p + '0', p + '1']
            else: unres.append(p)
        level = nxt
    return leaves, unres

if __name__ == '__main__':
    rl = int(sys.argv[1]); q = 5
    import json as _j
    _old = _j.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ac_labeled_rl%d.json' % rl)))
    TERMS[:] = [c + '0' for _, c in sorted((len(c), c) for c, v in _old.items() if v['last'] == 1)[:4]]
    # block 3 only coded (cbp code 00110, found by the CBP probe), dc table 0 (zero code '1')
    mk = lambda bits: tob(hdr_i(q=q, rl=rl) + '00110' + '0' + '1' * 4 + bits + '1' * 60)
    leaves, unres = walk_ac(mk, 3)
    print('rl', rl, len(leaves), 'kraft', kraft(leaves), 'unres', len(unres))
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ac_raw_rl%d.json' % rl)
    json.dump({'leaves': {p: [list(v[0]), v[1]] for p, v in leaves.items()}, 'unres': unres}, open(out, 'w'))
    for p, v in sorted(leaves.items(), key=lambda x: (len(x[0]), x[0]))[:12]: print(p, v)
