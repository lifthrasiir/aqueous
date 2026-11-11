"""I-frame CBP table: test each code string against each coded set with an exactly built tail."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tree import *
from probe import blocks_yuv
HERE = os.path.dirname(os.path.abspath(__file__))
E = '01110'            # rl=2: (0,1) level 1, last=1, sign +   (the encoder's own bits)
EC = '0110'     # chroma, rlc=1: (0,1) level 1 last 1
Z = '1'                # dc_table 0: level 0 (luma)
ZC = '00'              # and chroma
def tail(V, ap='0'):
    t = ap                        # ac_pred
    for i in range(6):
        t += (Z if i < 4 else ZC) + ((E if i < 4 else EC) if (V >> (5 - i)) & 1 else '')
    return t
def ref_blocks():
    """The exact blocks a coded one (E) and a flat one give, from a stream known to be right: cbp 63 is not known yet,
    so use the encoder's single-block frame (block 3 coded; the chroma block 4 by its own frame)."""
    from enc_ac import make_frame
    from enc import enc_frames
    import numpy as np
    from ac_probe import _C
    y = np.full((16, 16), 128.0); y[8:, 8:] += 15 * np.outer(_C[0], _C[1])
    fr = (np.clip(np.round(y), 0, 255).astype(np.uint8), np.full((8, 8), 128, np.uint8), np.full((8, 8), 128, np.uint8))
    o = DEC.frames([enc_frames([fr], q=5)[0]])[0][0]
    return tuple(blocks_yuv(o)[3])
REF = None
def ac_part(b):
    m = sum(b) / len(b)
    return tuple(round(x - m) for x in b)
def match(out, V):
    global REF
    if REF is None: REF = ac_part(ref_blocks())
    for i, b in enumerate(blocks_yuv(out)):
        coded = (V >> (5 - i)) & 1
        if coded:
            T = tuple(REF[(k % 8) * 8 + k // 8] for k in range(64))
            if min(max(abs(x - y) for x, y in zip(ac_part(b), R)) for R in (REF, T)) > 2: return False
        elif len(set(b)) != 1: return False
    return True
if __name__ == '__main__':
    found = {}
    for L in range(1, 11):
        pk = []; keys = []; pk1 = []
        for v in range(1 << L):
            p = format(v, '0%db' % L)
            for V in range(1, 64):
                pk.append(tob(hdr_i(q=5, rl=2, rlc=1, dc=0) + p + tail(V) + '1' * 40)); pk1.append(tob(hdr_i(q=5, rl=2, rlc=1, dc=0) + p + tail(V, '1') + '1' * 40)); keys.append((p, V))
        res = DEC.frames(pk); res1 = DEC.frames(pk1)
        for (p, V), (o, bad), (o1, bad1) in zip(keys, res, res1):
            if o is not None and not bad and match(o, V):
                found.setdefault(p, []).append(V)
        print(L, len(found), flush=True)
    json.dump(found, open(os.path.join(HERE, 'cbp_i_raw2.json'), 'w'))
    found['1'] = [0]
    print(len(found), sum(2.0 ** -len(p) for p in found), sorted(set(sum(found.values(), []))) == list(range(64)))
    json.dump(found, open(os.path.join(HERE, 'cbp_i_raw2.json'), 'w'))
