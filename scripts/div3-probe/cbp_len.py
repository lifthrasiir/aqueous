"""I-frame CBP codes by an exact-length oracle: a 16x16 frame decodes without any warning only if the MB consumed
exactly the bits that leave room for the 17-bit extension header (leftover in [17, 25))."""
import sys, os, json, av; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tree import tob, hdr_i
from probe import blocks_yuv
def mask(o):
    m = 0
    for i, b in enumerate(blocks_yuv(o)):
        if len(set(b)) > 1: m |= 1 << (5 - i)
    return m
HERE = os.path.dirname(os.path.abspath(__file__))
def clean(b):
    c = av.CodecContext.create('msmpeg4', 'r'); c.width = 16; c.height = 16; c.pix_fmt = 'yuv420p'; c.thread_count = 1; c.open()
    with av.logging.Capture() as logs:
        try: fr = c.decode(av.Packet(b))
        except Exception: return False
    return fr[0].to_ndarray().tobytes() if (fr and not logs) else None
EL, EC = ('01110', '001100'), ('0110', '0110')   # luma rl=2 (0,1) lvl1 / lvl2 last=1 (+sign); chroma rlc=1 (0,1) lvl1, and another
def build(p, F, k, ap='0'):
    t = ap
    for i in range(6):
        t += ('1' if i < 4 else '00')
        if (F >> (5 - i)) & 1: t += (EL if i < 4 else EC)[k]
    return tob(hdr_i(q=5, rl=2, rlc=1, dc=0) + p + t + '0' * 17 + '0' * 7)
if __name__ == '__main__':
    found = {}
    for L in range(1, 13):
        for v in range(1 << L):
            p = format(v, '0%db' % L)
            for F in range(64):
                o = clean(build(p, F, 0))
                if o is not None and mask(o) == F:
                    o1 = clean(build(p, F, 1))
                    if o1 is not None and mask(o1) == F: found.setdefault(p, []).append(F)
        print(L, len(found), flush=True)
    json.dump(found, open(os.path.join(HERE, 'cbp_i_len.json'), 'w'))
