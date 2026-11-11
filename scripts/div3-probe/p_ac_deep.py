"""Resume the inter AC walk in the long-code region of each table (unresolved at the first depth cap)."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import p_ac as P
from p_ac import *
HERE = os.path.dirname(os.path.abspath(__file__))
TERMS = {0: ['110', '111', '01010', '01011'], 1: ['0110', '0111', '10010', '10011'], 2: ['01111', '01110']}
ESC = {0: '000001101', 1: '101101001', 2: '0000011'}
def resume(rl, prefix, maxd, q=1):
    P.QMK = q
    path = os.path.join(HERE, 'p_ac_raw_rl%d.json' % rl)
    d = json.load(open(path)); mk = make_mk(rl); terms = TERMS[rl]
    start = [u for u in d['unres'] if u.startswith(prefix) and not u.startswith(ESC[rl])]
    leaves = {}; unres = []; level = start
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
    for p, v in leaves.items(): d['leaves'][p] = [list(v[0]), v[1]]
    d['unres'] = [u for u in d['unres'] if not (u.startswith(prefix) and not u.startswith(ESC[rl]))] + unres
    json.dump(d, open(path, 'w'))
    return len(start), len(leaves), len(unres)
if __name__ == '__main__':
    for rl in (0, 1, 2):
        print(rl, resume(rl, sys.argv[1], int(sys.argv[2])), flush=True)
