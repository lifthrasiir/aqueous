import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import p_chain as C
from p_chain import *
HERE = os.path.dirname(os.path.abspath(__file__))
sol = {int(k): v for k, v in json.load(open(os.path.join(HERE, 'p_mb_codes_v5.json'))).items()}
used = set(sol.values()) | {'01'}
def covered(p): return any(c.startswith(p) or p.startswith(c) for c in used)
free = []
def rec(p):
    if len(p) > 16: return
    if not covered(p): free.append(p); return
    if p in used: return
    rec(p + '0'); rec(p + '1')
rec('')
print(len(free), 'free regions, mass', sum(2.0 ** -len(f) for f in free), flush=True)
E = '01110'
def tail(V, ap):
    t = ap
    for i in range(6):
        t += ('1' if i < 4 else '00') + (E if (V >> (5 - i)) & 1 else '')
    return t
HD = '01' + format(5, '05b') + '0' + '11' + '0' + '1'      # use_skip 0, rl 2, dc 0, mv 1
def test(p, V, ap):
    o = run(tob(HD + p + tail(V, ap) + SENT + '0' * 16))
    if not sentinel_ok(o): return False
    bl = blocks_of(o, 0)
    return all(((V >> (5 - i)) & 1) == (len(set(b.flatten())) > 1) for i, b in enumerate(bl))
if __name__ == '__main__':
    found = {}
    for f in free:
        for extra in range(0, 17 - len(f)):
            for v in range(1 << extra):
                p = f + (format(v, '0%db' % extra) if extra else '')
                for ap in ('0', '1', ''):
                    for V in range(64):
                        if test(p, V, ap): found.setdefault((ap, p), []).append(V)
    json.dump({a + '|' + p: Vs for (a, p), Vs in found.items()}, open(os.path.join(HERE, 'p_chain_intra.json'), 'w'))
    print(len(found))
