"""P-frame inter MB: split `skip 0 | M(V) | MV(0,0) | AC...` using the known inter AC table (the chroma tables c_k)."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import p_ac as P
from enc_p import *
from ac_probe import _C
HERE = os.path.dirname(os.path.abspath(__file__))
AC1 = '000000001001'        # (0,1) at level 2, last 1, sign 1: the inter code of the B_2 table
def target(V, C=-25):
    y = P.REF_Y.astype(float); u = np.full((8, 8), 128.0); v = np.full((8, 8), 128.0)
    for i in range(6):
        if (V >> (5 - i)) & 1:
            b = C * np.outer(_C[0], _C[1])
            if i < 4: r0, c0 = (i // 2) * 8, (i % 2) * 8; y[r0:r0 + 8, c0:c0 + 8] += b
            elif i == 4: u += b
            else: v += b
    return tuple(np.clip(np.round(a), 0, 255).astype(np.uint8) for a in (y, u, v))
def body(V):
    e = enc_seq([P.REF, target(V)], w=16, h=16, q=5)
    return bits(e[1])[12:]
if __name__ == '__main__':
    out = {}
    for V in range(1, 64):
        b = body(V).rstrip('0'); k = bin(V).count('1')
        ok = b.endswith(AC1 * k)
        out[V] = (b[:len(b) - len(AC1) * k] if ok else None)
    bad = [V for V, x in out.items() if x is None]
    print('not ending in', len(bad), bad[:10])
    good = {V: x for V, x in out.items() if x is not None}
    # the longest common suffix of M(V)+MV00 is MV00
    ref = list(good.values())
    suf = ''
    while all(len(x) > len(suf) for x in ref) and len({x[-len(suf) - 1] for x in ref}) == 1: suf = ref[0][-len(suf) - 1] + suf
    print('MV00 candidate', suf, [x for x in list(good.values())[:5]])
    json.dump({str(k): v for k, v in out.items()}, open(os.path.join(HERE, 'p_mb_raw.json'), 'w'))
