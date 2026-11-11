"""Exact inter AC table of the encoder (rl=2): block 3 of an inter MB in a P frame over a textured reference."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import p_ac as P
from p_ac import *
from div3 import IDX
from ac_probe import _C
HERE = os.path.dirname(os.path.abspath(__file__))
INV = {i: p for p, i in IDX.items()}; INV[0] = (0, 0)   # inter blocks code the DC position too
PRE = '0' + '11100'
def target(coefs):
    y = P.REF_Y.astype(float)
    for (u, v), C in coefs: y[8:, 8:] += C * np.outer(_C[u], _C[v])
    return (np.clip(np.round(y), 0, 255).astype(np.uint8),) + P.REF[1:]
_D = Dec()
def one(coefs):
    """(AC bits with the sign trick, decoded [(pos, level)]) for the encoder's P frame, or None."""
    e = enc_seq([P.REF, target(coefs)], w=16, h=16, q=5)
    b = bitstr(e[1])[12:]
    if not b.startswith(PRE): return None
    pk = e[1]; _D.new(); _D.frames([e[0]]); o, bad = _D.frames([pk])[0]
    F = fdct(np.array(blocks_yuv(o)[3]) - P.PRED)
    d = sorted(((u, v), round((abs(F[u, v]) - 5) / 10)) for u in range(8) for v in range(8) if abs(F[u, v]) > 8)
    return b[len(PRE):].rstrip('0'), d
def build():
    last1 = {}
    for idx in range(0, 64):
        pos = INV[idx]; run = idx
        for C in range(-10, -400, -2):
            r = one([(pos, C)])
            if r is None:
                continue
            ac, d = r
            if len(d) != 1 or d[0][0] != pos: break
            l = d[0][1]
            if (run, l) not in last1: last1[(run, l)] = ac
    return last1
if __name__ == '__main__':
    t = build()
    json.dump({'%d,%d' % k: v for k, v in t.items()}, open(os.path.join(HERE, 'enc_p_last1.json'), 'w'))
    print(len(t))
    for k in [(0, 1), (0, 2), (0, 3), (1, 1), (2, 1), (5, 1), (5, 2), (20, 1), (40, 1)]: print(k, t.get(k))

def exact(esc='0000011'):
    t = {tuple(map(int, k.split(','))): v for k, v in json.load(open(os.path.join(HERE, 'enc_p_last1.json'))).items()}
    codes = {}
    for (r, l), v in t.items():
        if not v.startswith(esc): codes[v[:-1]] = (r, l, 1)
    seen = set()
    for idx in range(0, 63):
        pos = INV[idx]; nxt = INV[idx + 1]; run = idx
        for C in range(-10, -400, -2):
            r = one([(pos, C), (nxt, -30)])
            if r is None: continue
            ac, d = r
            if len(d) == 1 and C > -30: continue
            if len(d) != 2 or d[0][0] != pos or d[1][0] != nxt: break
            l = d[0][1]; l2 = d[1][1]
            term = t.get((0, l2))
            if term is None or term.startswith(esc): continue
            if (run, l) in seen or not ac.endswith(term): continue
            code = ac[:len(ac) - len(term) - 1]
            if not code or code.startswith(esc): continue
            seen.add((run, l)); codes[code] = (run, l, 0)
    return codes
