import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ac_probe import *
def resume(rl, prefix, maxd, q=5):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ac_raw_rl%d.json' % rl)
    d = json.load(open(path))
    start = [u for u in d['unres'] if u.startswith(prefix)]
    mk = lambda bits: tob(hdr_i(q=q, rl=rl) + '00110' + '0' + '1' * 4 + bits + '1' * 60)
    leaves = {}; unres = []; level = start
    while level:
        nxt = []
        for p in level:
            r = flip_leaf(mk, 3, p)
            if r is not None: leaves[p] = r
            elif len(p) < maxd:
                pk = [mk(p + rnd(20, random.Random(k))) for k in range(8)]
                if all(b or o is None for o, b in DEC.frames(pk)): continue
                nxt += [p + '0', p + '1']
            else: unres.append(p)
        level = nxt
    for p, v in leaves.items(): d['leaves'][p] = [list(v[0]), v[1]]
    d['unres'] = [u for u in d['unres'] if not u.startswith(prefix)] + unres
    json.dump(d, open(path, 'w'))
    return len(leaves), len(unres)
if __name__ == '__main__':
    print(resume(int(sys.argv[1]), sys.argv[2], int(sys.argv[3])))
