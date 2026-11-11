"""Inter AC tables for rl 0,1,2: walk, find the escape, label (run = scan index, DC included), compare to the encoder's rl=2."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import p_ac as P
from p_ac import *
from div3 import IDX
from tree import lcp
HERE = os.path.dirname(os.path.abspath(__file__))
IDXI = dict(IDX); IDXI[(0, 0)] = 0
def label(rl):
    d = json.load(open(os.path.join(HERE, 'p_ac_raw_rl%d.json' % rl)))
    unres = d['unres']
    # the escape: the common prefix of the largest unresolved group
    from collections import Counter
    g = Counter(u[:7] for u in unres).most_common(3)
    print('rl', rl, 'unresolved groups', g)
    return d
def labeled(rl, esc):
    d = json.load(open(os.path.join(HERE, 'p_ac_raw_rl%d.json' % rl)))
    mk = make_mk(rl)
    out = {}
    for p, (pos, amp) in sorted(d['leaves'].items(), key=lambda x: (len(x[0]), x[0])):
        if p.startswith(esc): continue
        rng = random.Random(1)
        res = decP([mk(p + '0' + rnd(24, rng)) for _ in range(12)])
        blocks = set(tuple(blocks_yuv(o)[3]) for o, b in res if o is not None and not b)
        out[p] = dict(pos=pos, level=round((amp - 5) / 10), amp=amp, last=1 if len(blocks) == 1 else 0)
    seen = set(); clean = {}
    for p, v in out.items():
        key = (tuple(v['pos']), v['level'], v['last'])
        if key in seen: continue
        seen.add(key); clean[p] = v
    json.dump(clean, open(os.path.join(HERE, 'p_ac_labeled_rl%d.json' % rl), 'w'))
    return clean
if __name__ == '__main__':
    mode = sys.argv[1]
    if mode == 'walk':
        for rl in (0, 1):
            mk = make_mk(rl)
            # bootstrap run-0 last=1 terminators with random rests (last=1 codes ignore their rest)
            P.THRESH_P = 0.6
            boot, _ = walk_p(mk, [''], maxd=9)
            P.THRESH_P = 0.85
            terms = []
            for p, (pos, amp) in sorted(boot.items(), key=lambda x: (len(x[0]), x[0])):
                rng = random.Random(1)
                res = decP([mk(p + '0' + rnd(24, rng)) for _ in range(8)])
                blocks = set(tuple(blocks_yuv(o)[3]) for o, b in res if o is not None and not b)
                if len(blocks) == 1 and tuple(pos) in ((0, 0), (0, 1)):
                    terms += [p + '0', p + '1']
                if len(terms) >= 4: break
            print('rl', rl, 'boot', len(boot), 'terms', terms, flush=True)
            leaves, unres = walk_p(mk, terms)
            print('inter rl', rl, len(leaves), 'kraft', kraft(leaves), 'unres', len(unres), flush=True)
            json.dump({'leaves': {p: [list(v[0]), v[1]] for p, v in leaves.items()}, 'unres': unres}, open(os.path.join(HERE, 'p_ac_raw_rl%d.json' % rl), 'w'))
    else:
        for rl in (0, 1, 2): label(rl)
