"""Inter MB-type codes by exact tails: hdr | skip 0 | p | mv(0,0)='00' | one level-1 AC per coded block | zero padding must reproduce
the reference plus the residual in exactly the coded blocks, without a warning."""
import sys, os, json, av; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import p_ac as P
from p_ac import *
HERE = os.path.dirname(os.path.abspath(__file__))
AC = '0011111'
REF_O, _ = Dec().frames([P.I_PKT])[0]
def blocks_of(o):
    return blocks_yuv(o)
REFB = blocks_yuv(REF_O)
def decode_pair(pkt):
    c = av.CodecContext.create('msmpeg4', 'r'); c.width = 16; c.height = 16; c.pix_fmt = 'yuv420p'; c.thread_count = 1; c.open()
    c.decode(av.Packet(P.I_PKT))
    with av.logging.Capture() as logs:
        try: fr = c.decode(av.Packet(pkt))
        except Exception: return None
    return fr[0].to_ndarray().tobytes() if fr and not logs else None
_o4 = decode_pair(tob(hdr_p(rl=2) + '0' + '111' + '00' + AC + '0' * 16))
DPAT = np.array(blocks_yuv(_o4)[3]) - np.array(REFB[3])
def mask_vs_ref(o):
    bl = blocks_yuv(o); m = 0
    for i in range(6):
        d = np.array(bl[i]) - np.array(REFB[i])
        if d.any():
            if np.abs(d - DPAT).max() > 2: return -1
            m |= 1 << (5 - i)
    return m
HDR = hdr_p(rl=2)
def test(p, V):
    t = ''.join(AC for i in range(6) if (V >> (5 - i)) & 1)
    o = decode_pair(tob(HDR + '0' + p + '00' + t + '0' * 16))
    return o is not None and mask_vs_ref(o) == V
if __name__ == '__main__':
    lo, hi = int(sys.argv[1]), int(sys.argv[2])
    found = {}
    for L in range(lo, hi + 1):
        for v in range(1 << L):
            p = format(v, '0%db' % L)
            for V in range(1, 64):
                if test(p, V): found.setdefault(p, []).append(V)
        print(L, len(found), flush=True)
        json.dump(found, open(os.path.join(HERE, 'p_mb3_%d_%d.json' % (lo, hi)), 'w'))
