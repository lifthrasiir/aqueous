import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import p_chain_intra as PCI
from p_chain_intra import *
inter = {int(k): v for k, v in json.load(open(os.path.join(HERE, 'p_mb_codes_v5.json'))).items()}
intra = {int(k): v for k, v in json.load(open(os.path.join(HERE, 'p_intra_codes.json'))).items()}
used = set(inter.values()) | set(intra.values()) | {'01'}
def covered(p): return any(c.startswith(p) or p.startswith(c) for c in used)
free = []
def rec(p):
    if len(p) > 20: return
    if not covered(p): free.append(p); return
    if p in used: return
    rec(p + '0'); rec(p + '1')
rec('')
print(len(free), 'free regions', sum(2.0 ** -len(f) for f in free), sorted(free, key=len)[:10], flush=True)
missing = [V for V in range(64) if V not in intra]
found = {}
for f in free:
    for extra in range(0, 21 - len(f)):
        if extra > 6: break
        for v in range(1 << extra):
            p = f + (format(v, '0%db' % extra) if extra else '')
            for V in missing:
                if PCI.test(p, V, '0'): found.setdefault(p, []).append(V)
json.dump(found, open(os.path.join(HERE, 'p_chain_intra2.json'), 'w'))
print(len(found), sorted(set(sum(found.values(), []))))
