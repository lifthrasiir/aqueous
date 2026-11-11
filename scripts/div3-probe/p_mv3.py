"""Are the motion-vector codes of table t complete? A code that is only the common prefix of two (vectors reaching beyond the
edge of the small picture looked alike) changes the macroblock when the bit after it changes. The macroblock is in the middle of a
160x160 picture, where every vector shows. Prints the codes that are not complete."""
import sys, os, json, av, random
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from enc_p import *
from tree import tob, rnd
HERE = os.path.dirname(os.path.abspath(__file__))
W = H = 160; MBX = MBY = 5
rng0 = np.random.RandomState(21)
REF_Y = np.clip(128 + rng0.randn(H, W) * 40, 0, 255).astype(np.uint8)
REF = (REF_Y, np.full((H // 2, W // 2), 128, np.uint8), np.full((H // 2, W // 2), 128, np.uint8))
I_PKT = enc_seq([REF], w=W, h=H, q=5)[0]
BEFORE = MBY * (W // 16) + MBX
def decode_many(pkts):
    out = []
    for p in pkts:
        c = av.CodecContext.create('msmpeg4', 'r'); c.width = W; c.height = H; c.pix_fmt = 'yuv420p'; c.thread_count = 1; c.open()
        c.decode(av.Packet(I_PKT))
        with av.logging.Capture() as logs:
            try: fr = c.decode(av.Packet(p))
            except Exception: out.append(None); continue
        bad = any(('conceal' in m[2] or 'rror' in m[2] or 'damaged' in m[2] or 'invalid' in m[2]) for m in logs)
        out.append(None if (not fr or bad) else np.frombuffer(fr[0].to_ndarray().tobytes()[:W * H], np.uint8).reshape(H, W)[MBY * 16:MBY * 16 + 16, MBX * 16:MBX * 16 + 16].tobytes())
        del c
    return out
def mk(mvtab):
    h = '01' + format(5, '05b') + '1' + '11' + '1' + str(mvtab)
    return lambda bits: tob(h + '1' * BEFORE + '0' + '01' + bits + '1' * 80)
def complete(m, p, rng, n=6):
    """True if neither bit after p changes the macroblock (over a few tails each)."""
    res = {}
    for b in '01':
        outs = {o for o in decode_many([m(p + b + rnd(14, rng)) for _ in range(n)]) if o is not None}
        res[b] = outs
    return len(res['0'] | res['1']) <= 1 or not (res['0'] and res['1']) and len(res['0'] | res['1']) <= 1
if __name__ == '__main__':
    t = int(sys.argv[1])
    tab = json.load(open(os.path.join(HERE, 'p_mv_table%d.json' % t)))
    m = mk(t); rng = random.Random(1)
    bad = []
    for i, p in enumerate(sorted(tab['codes'], key=len)):
        # a complete code: the macroblock is the same whatever follows
        outs = {o for o in decode_many([m(p + rnd(16, rng)) for _ in range(8)]) if o is not None}
        if len(outs) > 1: bad.append(p)
    print('table', t, len(tab['codes']), 'codes;', len(bad), 'are not complete', bad[:10], flush=True)
    json.dump(bad, open(os.path.join(HERE, 'p_mv3_bad%d.json' % t), 'w'))
