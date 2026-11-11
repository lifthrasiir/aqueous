"""I-frame CBP (mb_i) table of the first MB (neighbours absent, so the coded flags are not predicted)."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tree import *
from probe import blocks_yuv
def mask(out):
    m = 0
    for i, b in enumerate(blocks_yuv(out)):
        if len(set(b)) > 1: m |= 1 << (5 - i)
    return m
def leaf(obs):
    n = len(obs); fr = [sum((m >> (5 - i)) & 1 for m in obs) / n for i in range(6)]
    if all(f == 0 or f >= 0.85 for f in fr):
        return sum(1 << (5 - i) for i in range(6) if fr[i] >= 0.85), [round(x, 2) for x in fr]
    return None
if __name__ == '__main__':
    mk = lambda bits: tob(hdr_i() + bits + '1' * 60)   # bits: code, ac_pred, ...
    leaves, unres = walk_list(mk, mask, leaf, maxd=14)
    print(len(leaves), 'kraft', kraft(leaves), 'unres', len(unres))
    vs = sorted(v[0] for v in leaves.values()); print('distinct', len(set(vs)), 'missing', [v for v in range(64) if v not in vs])
    json.dump({p: v[0] for p, v in leaves.items()}, open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'cbp_i_raw.json'), 'w'))
    for p, v in sorted(leaves.items(), key=lambda x: (len(x[0]), x[0]))[:10]: print(p, v)
