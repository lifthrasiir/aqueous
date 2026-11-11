"""The scan order, from pairs of codes: idx(Z) = idx(X) + idx(Y) for a (last=0) code at X followed by a (last=1) code at Y."""
import sys, os, json, itertools; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ac_probe import *
HERE = os.path.dirname(os.path.abspath(__file__))
t = json.load(open(os.path.join(HERE, 'ac_labeled_rl2.json')))
mk = lambda bits: tob(hdr_i(q=5, rl=2) + '00110' + '0' + '1' * 4 + bits + '1' * 60)
def coefs(blk):
    F = fdct(blk); return {(u, v): round(F[u, v]) for u in range(8) for v in range(8) if abs(F[u, v]) >= 5 and (u, v) != (0, 0)}
A = {}; B = {}
for p, v in t.items():
    (A if v['last'] == 0 else B).setdefault(tuple(v['pos']), []).append(p)
rel = []
pk = []; keys = []
for X, ps in A.items():
    a = min(ps, key=len)
    for Y, qs in B.items():
        b = min(qs, key=len)
        pk.append(mk(a + '0' + b + '0')); keys.append((X, Y))
for (X, Y), (o, bad) in zip(keys, DEC.frames(pk)):
    if o is None or bad: continue
    c = coefs(blocks_yuv(o)[3])
    if len(c) == 2 and X in c:
        Z = [k for k in c if k != X][0]; rel.append((X, Y, Z))
print(len(rel), 'relations')
json.dump(rel, open(os.path.join(HERE, 'scan_rel.json'), 'w'))
# successor maps per X
from collections import defaultdict
fx = defaultdict(dict)
for X, Y, Z in rel: fx[X][Y] = Z
for X, m in sorted(fx.items(), key=lambda kv: -len(kv[1]))[:6]:
    print(X, len(m), list(m.items())[:6])
