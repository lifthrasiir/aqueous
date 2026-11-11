"""Inter MB-type codes for every q (the table depends on q): bootstrap V=4, then search all V with the exact residual pattern."""
import sys, os, json, av; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import p_ac as P
from p_ac import *
HERE = os.path.dirname(os.path.abspath(__file__))
AC = '0011111'
REF_O, _ = Dec().frames([P.I_PKT])[0]
REFB = blocks_yuv(REF_O)
def decode_pair(pkt):
    c = av.CodecContext.create('msmpeg4', 'r'); c.width = 16; c.height = 16; c.pix_fmt = 'yuv420p'; c.thread_count = 1; c.open()
    c.decode(av.Packet(P.I_PKT))
    with av.logging.Capture() as logs:
        try: fr = c.decode(av.Packet(pkt))
        except Exception: return None
    return fr[0].to_ndarray().tobytes() if fr and not logs else None
def hdr(q): return '01' + format(q, '05b') + '1' + '11' + '1' + '1'
def residual_mask(o, dpat=None):
    bl = blocks_yuv(o); m = 0
    for i in range(6):
        d = np.array(bl[i]) - np.array(REFB[i])
        if d.any():
            if dpat is not None and np.abs(d - dpat).max() > 2: return -1
            m |= 1 << (5 - i)
    return m
def tailV(V): return ''.join(AC for i in range(6) if (V >> (5 - i)) & 1)
def search(q):
    h = hdr(q)
    # bootstrap: any code giving exactly block 3 with a clean decode
    boot = None; dpat = None
    for L in range(1, 11):
        for v in range(1 << L):
            p = format(v, '0%db' % L)
            o = decode_pair(tob(h + '0' + p + '00' + tailV(4) + '0' * 16))
            if o is not None and residual_mask(o) == 4:
                d = np.array(blocks_yuv(o)[3]) - np.array(REFB[3])
                if np.abs(d).max() >= 2: boot = p; dpat = d; break
        if boot: break
    found = {}
    for L in range(1, 13):
        for v in range(1 << L):
            p = format(v, '0%db' % L)
            for V in range(1, 64):
                o = decode_pair(tob(h + '0' + p + '00' + tailV(V) + '0' * 16))
                if o is not None and residual_mask(o, dpat) == V: found.setdefault(p, []).append(V)
    byV = {}
    for p, Vs in found.items():
        for V in Vs: byV.setdefault(V, []).append(p)
    sol = {V: sorted(byV[V], key=lambda x: (len(x), x))[0] for V in byV}
    return boot, sol
if __name__ == '__main__':
    q = int(sys.argv[1])
    boot, sol = search(q)
    codes = list(sol.values())
    print('q', q, 'boot', boot, len(sol), 'prefix-free', not any(a != b and b.startswith(a) for a in codes for b in codes), 'kraft', sum(2.0 ** -len(c) for c in codes), 'missing', [V for V in range(1, 64) if V not in sol])
    json.dump({str(k): v for k, v in sol.items()}, open(os.path.join(HERE, 'p_mb_codes_q%d.json' % q), 'w'))
