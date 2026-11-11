import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import p_intra as PI
import p_mb3 as M3
from p_ac import *
HERE = os.path.dirname(os.path.abspath(__file__))
ap = sys.argv[1]
PI.AP = ap
found = {}
for L in range(1, 15):
    for v in range(1 << L):
        p = format(v, '0%db' % L)
        for V in range(64):
            if PI.test(p, V): found.setdefault(p, []).append(V)
    print(L, len(found), flush=True)
    json.dump(found, open(os.path.join(HERE, 'p_intra3_ap%s.json' % (ap or 'none')), 'w'))
