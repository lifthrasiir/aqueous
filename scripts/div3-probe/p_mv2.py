"""Motion-vector VLC tables again, with the macroblock in the middle of a 160x160 picture: a vector reaching beyond the
picture's edge shows nothing of what it is (every such vector gives the same pixels), and the first walk took a node
whose children were vectors of that kind for a code of its own, so that table 0 came out with codes too short."""
import sys, os, json, av, random; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from enc_p import *
from tree import tob, rnd, kraft
HERE = os.path.dirname(os.path.abspath(__file__))
W = H = 160
MBX = MBY = 5
rng0 = np.random.RandomState(21)
REF_Y = np.clip(128 + rng0.randn(H, W) * 40, 0, 255).astype(np.uint8)
REF = (REF_Y, np.full((H // 2, W // 2), 128, np.uint8), np.full((H // 2, W // 2), 128, np.uint8))
I_PKT = enc_seq([REF], w=W, h=H, q=5)[0]
NMB = (W // 16) * (H // 16)
BEFORE = MBY * (W // 16) + MBX
def decode_many(pkts):
    out = []
    for p in pkts:
        c = av.CodecContext.create('msmpeg4', 'r'); c.width = W; c.height = H; c.pix_fmt = 'yuv420p'; c.thread_count = 1; c.open()
        c.decode(av.Packet(I_PKT))
        with av.logging.Capture() as logs:
            try: fr = c.decode(av.Packet(p))
            except Exception: out.append(None); continue
        out.append(None if (not fr or any(('conceal' in m[2] or 'rror' in m[2] or 'damaged' in m[2] or 'invalid' in m[2]) for m in logs)) else fr[0].to_ndarray().tobytes())
    return out
def mk(mvtab):
    h = '01' + format(5, '05b') + '1' + '11' + '1' + str(mvtab)       # use_skip 1, rl 2, dc 1, mv table
    return lambda bits: tob(h + '1' * BEFORE + '0' + '01' + bits + '1' * 80)
def mb_pixels(o):
    y = np.frombuffer(o[:W * H], np.uint8).reshape(H, W)
    return y[MBY * 16:MBY * 16 + 16, MBX * 16:MBX * 16 + 16]
def predict(dx, dy, rr):
    pad = np.pad(REF_Y.astype(int), 60, mode='edge'); ix, fx = dx >> 1, dx & 1; iy, fy = dy >> 1, dy & 1
    def at(y, x): return pad[60 + MBY * 16 + y + iy: 60 + MBY * 16 + y + iy + 16, 60 + MBX * 16 + x + ix: 60 + MBX * 16 + x + ix + 16]
    a, b, c, d = at(0, 0), at(0, 1), at(1, 0), at(1, 1)
    if not fx and not fy: return a
    if fx and not fy: return (a + b + 1 - rr) >> 1
    if fy and not fx: return (a + c + 1 - rr) >> 1
    return (a + b + c + d + 2 - rr) >> 2
CANDS = {(dx, dy, rr): predict(dx, dy, rr) for dx in range(-48, 48) for dy in range(-48, 48) for rr in (0, 1)}
def vector_of(y):
    best = None
    for k, pr in CANDS.items():
        e = np.abs(pr - y.astype(int)).max()
        if e <= 1 and (best is None or e < best[0]): best = (e, k)
    return best[1][:2] if best else None
def node(m, p, r):
    res = decode_many([m(p + format(v, '04b') + rnd(12, r)) for v in range(16)])
    outs = set(); ok = 0
    for o in res:
        if o is None: continue
        ok += 1; outs.add(mb_pixels(o).tobytes())
    return ok, outs
def walk(mvtab, maxd=22):
    m = mk(mvtab); leaves = {}; unres = []; level = ['']; r = random.Random(1)
    while level:
        nxt = []
        for p in level:
            ok, outs = node(m, p, r)
            if ok == 0: continue
            if len(outs) == 1 and ok >= 12:
                leaves[p] = vector_of(np.frombuffer(next(iter(outs)), np.uint8).reshape(16, 16))
            elif len(p) < maxd: nxt += [p + '0', p + '1']
            else: unres.append(p)
        level = nxt
    return leaves, unres
if __name__ == '__main__':
    t = int(sys.argv[1])
    leaves, unres = walk(t)
    print('mv table', t, len(leaves), 'kraft', kraft(leaves), 'unres', len(unres), 'unlabelled', sum(1 for v in leaves.values() if v is None), flush=True)
    json.dump({'leaves': leaves, 'unres': unres}, open(os.path.join(HERE, 'p_mv2_raw%d.json' % t), 'w'))
