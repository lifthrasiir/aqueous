"""P-frame inter MB: the encoder's streams for every set of coded blocks (zero motion, flat reference)."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from enc_p import *
from ac_probe import _C
HERE = os.path.dirname(os.path.abspath(__file__))
def frame_for(V, C=-15, w=16, h=16):
    y = np.full((h, w), 128.0); u = np.full((h // 2, w // 2), 128.0); v = np.full((h // 2, w // 2), 128.0)
    for i in range(6):
        if (V >> (5 - i)) & 1:
            b = C * np.outer(_C[0], _C[1])
            if i < 4: r0, c0 = (i // 2) * 8, (i % 2) * 8; y[r0:r0 + 8, c0:c0 + 8] += b
            elif i == 4: u += b
            else: v += b
    return tuple(np.clip(np.round(a), 0, 255).astype(np.uint8) for a in (y, u, v))
def streams(q=5):
    out = {}
    for V in range(1, 64):
        pk = enc_seq([flat(16, 16), frame_for(V)], w=16, h=16, q=q)
        s = bits(pk[1]); out[V] = s
    return out
if __name__ == '__main__':
    S = streams()
    json.dump(S, open(os.path.join(HERE, 'p_streams.json'), 'w'))
    for V in (1, 2, 4, 8, 16, 32, 12, 3, 48, 63): print(V, format(V, '06b'), S[V][:12], S[V][12:])
