"""Motion-vector VLC tables: walk the code tree after `skip 0 | M(inter, cbp 0)='01'`; a code is complete when the macroblock does
not change with what follows. Its vector is found by matching the output with the shifted reference."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import p_ac as P
from p_ac import *
HERE = os.path.dirname(os.path.abspath(__file__))
REFO, _ = Dec().frames([P.I_PKT])[0]
REFY = np.frombuffer(REFO[:256], np.uint8).reshape(16, 16).astype(int)
def predict(dx, dy, rnd):
    """Half-pel (dx, dy) bilinear prediction of the 16x16 MB at (0, 0), edges replicated."""
    pad = np.pad(REFY, 40, mode='edge')
    ix, fx = dx >> 1, dx & 1; iy, fy = dy >> 1, dy & 1
    def at(y, x): return pad[40 + y + iy: 40 + y + iy + 16, 40 + x + ix: 40 + x + ix + 16]
    a, b, c, d = at(0, 0), at(0, 1), at(1, 0), at(1, 1)
    if not fx and not fy: return a
    if fx and not fy: return (a + b + 1 - rnd) >> 1
    if fy and not fx: return (a + c + 1 - rnd) >> 1
    return (a + b + c + d + 2 - rnd) >> 2
CANDS = {}
for dx in range(-40, 40):
    for dy in range(-40, 40):
        for rr in (0, 1): CANDS[(dx, dy, rr)] = predict(dx, dy, rr)
def vector_of(o):
    y = np.frombuffer(o[:256], np.uint8).reshape(16, 16).astype(int)
    best = None
    for k, pr in CANDS.items():
        e = np.abs(pr - y).max()
        if e <= 1 and (best is None or e < best[0]): best = (e, k)
    return best[1][:2] if best else None
def mk_mv(mvtab, rl=2, dc=1):
    return lambda bits_: tob(hdr_p(rl=rl, dc=dc, mv=mvtab) + '0' + '01' + bits_ + '0' * 40)
def node(mk, p, rng):
    pk = [mk(p + format(v, '04b') + rnd(12, rng)) for v in range(16)]
    res = decP(pk); outs = set(); ok = 0
    for o, b in res:
        if o is None or b: continue
        ok += 1; outs.add(o[:384])
    return ok, outs
def walk_mv(mvtab, maxd=22):
    mk = mk_mv(mvtab); leaves = {}; unres = []; level = ['']
    rng = random.Random(1)
    while level:
        nxt = []
        for p in level:
            ok, outs = node(mk, p, rng)
            if ok == 0: continue
            if len(outs) == 1 and ok >= 12:
                v = vector_of(next(iter(outs))); leaves[p] = v
            elif len(p) < maxd: nxt += [p + '0', p + '1']
            else: unres.append(p)
        level = nxt
    return leaves, unres
if __name__ == '__main__':
    t = int(sys.argv[1])
    leaves, unres = walk_mv(t)
    print('mv table', t, len(leaves), 'kraft', kraft(leaves), 'unres', len(unres), 'unlabelled', sum(1 for v in leaves.values() if v is None), flush=True)
    json.dump({'leaves': {p: v for p, v in leaves.items()}, 'unres': unres}, open(os.path.join(HERE, 'p_mv_raw%d.json' % t), 'w'))
