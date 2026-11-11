"""MB codes with a sentinel: a two-macroblock P frame (no skip bits) whose second MB is `01` + mv(0,0)='00'; the first MB's
hypothesis must leave the decoder exactly at the sentinel, so a code that absorbs or loses a bit is rejected."""
import sys, os, json, av; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from enc_p import *
from tree import tob
from probe import blocks_yuv
HERE = os.path.dirname(os.path.abspath(__file__))
W, H = 32, 16
_rng = np.random.RandomState(9)
REF_Y = np.clip(128 + _rng.randn(H, W) * 30, 0, 255).astype(np.uint8)
REF = (REF_Y, np.full((8, 16), 128, np.uint8), np.full((8, 16), 128, np.uint8))
I_PKT = enc_seq([REF], w=W, h=H, q=5)[0]
def new_ctx():
    c = av.CodecContext.create('msmpeg4', 'r'); c.width = W; c.height = H; c.pix_fmt = 'yuv420p'; c.thread_count = 1; c.open()
    return c
def run(pkt):
    c = new_ctx(); c.decode(av.Packet(I_PKT))
    with av.logging.Capture() as logs:
        try: fr = c.decode(av.Packet(pkt))
        except Exception: return None
    return fr[0].to_ndarray().tobytes() if fr and not logs else None
SENT = '01' + '1010110100'          # inter cbp 0 + the motion code of (4, 2) in table 1
REFO = run(tob('01' + format(5, '05b') + '0' + '11' + '1' + '1' + '01' + '00' + '01' + '00' + '0' * 16))
_refy = np.frombuffer(REFO[:W * H], np.uint8).reshape(H, W).astype(int)
def predict_mb1(dx, dy, rr):
    pad = np.pad(_refy, 40, mode='edge'); ix, fx = dx >> 1, dx & 1; iy, fy = dy >> 1, dy & 1
    def at(y, x): return pad[40 + y + iy: 40 + y + iy + 16, 40 + 16 + x + ix: 40 + 16 + x + ix + 16]
    a, b, c, d = at(0, 0), at(0, 1), at(1, 0), at(1, 1)
    if not fx and not fy: return a
    if fx and not fy: return (a + b + 1 - rr) >> 1
    if fy and not fx: return (a + c + 1 - rr) >> 1
    return (a + b + c + d + 2 - rr) >> 2
_P1 = [predict_mb1(4, 2, 0), predict_mb1(4, 2, 1)]
def region(o, mbx):
    """The luma of macroblock mbx, and its chroma 8x8 blocks."""
    y = np.frombuffer(o[:W * H], np.uint8).reshape(H, W)[:, mbx * 16:(mbx + 1) * 16]
    u = np.frombuffer(o[W * H:W * H + 8 * 16], np.uint8).reshape(8, 16)[:, mbx * 8:(mbx + 1) * 8]
    v = np.frombuffer(o[W * H + 8 * 16:], np.uint8).reshape(8, 16)[:, mbx * 8:(mbx + 1) * 8]
    return y, u, v
def blocks_of(o, mbx):
    y, u, v = region(o, mbx)
    return [y[:8, :8], y[:8, 8:], y[8:, :8], y[8:, 8:], u, v]
def hdr(q): return '01' + format(q, '05b') + '0' + '11' + '1' + '1'      # use_skip 0, rl 2, dc 1, mv 1
AC = '0011111'
def mask_resid(o):
    m = 0
    for i, (a, b) in enumerate(zip(blocks_of(o, 0), blocks_of(REFO, 0))):
        if (a.astype(int) != b.astype(int)).any(): m |= 1 << (5 - i)
    return m
def sentinel_ok(o):
    if o is None: return False
    y = region(o, 1)[0].astype(int)
    return any(np.abs(y - pr).max() <= 1 for pr in _P1)
def test_inter(q, p, V):
    t = ''.join(AC for i in range(6) if (V >> (5 - i)) & 1)
    o = run(tob(hdr(q) + p + '00' + t + SENT + '0' * 16))
    return sentinel_ok(o) and mask_resid(o) == V
if __name__ == '__main__':
    q = int(sys.argv[1]); lo, hi = int(sys.argv[2]), int(sys.argv[3])
    found = {}
    for L in range(lo, hi + 1):
        for v in range(1 << L):
            p = format(v, '0%db' % L)
            for V in range(1, 64):
                if test_inter(q, p, V): found.setdefault(p, []).append(V)
        print(L, len(found), flush=True)
        json.dump(found, open(os.path.join(HERE, 'p_chain_q%d.json' % q), 'w'))
