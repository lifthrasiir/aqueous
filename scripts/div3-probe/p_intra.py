"""Intra MB codes inside P frames: exact tails (ap 0, zero DCs, one (0,1) level-1 AC per coded block)."""
import sys, os, json, av; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import p_ac as P
import p_mb3 as M3
from p_ac import *
import cbp_i_probe2 as C
HERE = os.path.dirname(os.path.abspath(__file__))
E = '01110'
AP = ''     # no ac_pred bit in P-frame intra macroblocks (to be confirmed by coverage)
def tail(V):
    t = AP
    for i in range(6):
        t += ('1' if i < 4 else '00') + (E if (V >> (5 - i)) & 1 else '')
    return t
HDR = hdr_p(rl=2, dc=0)
def decode(pkt): return M3.decode_pair(pkt)
def ok(o, V):
    if o is None: return False
    for i, b in enumerate(blocks_yuv(o)):
        if ((V >> (5 - i)) & 1) != (len(set(b)) > 1): return False
    return True
def test(p, V): return ok(decode(tob(HDR + '0' + p + tail(V) + '0' * 16)), V)
if __name__ == '__main__':
    sol = {int(k): v for k, v in json.load(open(os.path.join(HERE, 'p_mb_codes_v3.json'))).items()}
    used = set(sol.values()) | {'01'}
    def covered(p): return any(c.startswith(p) or p.startswith(c) for c in used)
    free = []
    def rec(p):
        if len(p) > 15: return
        if not covered(p): free.append(p); return
        if p in used: return
        rec(p + '0'); rec(p + '1')
    rec('')
    found = {}
    cands = []
    for f in free:
        for extra in range(0, 15 - len(f) + 1):
            for v in range(1 << extra):
                cands.append(f + (format(v, '0%db' % extra) if extra else ''))
    print(len(free), 'free regions', len(cands), 'candidates', flush=True)
    for p in cands:
        for V in range(64):
            if test(p, V): found.setdefault(p, []).append(V)
    json.dump(found, open(os.path.join(HERE, 'p_intra_raw.json'), 'w'))
    print(len(found))
