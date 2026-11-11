"""An intra MB in an encoder P frame: find (M code length, ap present, coded set) whose block syntax consumes the frame exactly."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import div3
from div3 import *
from enc_p import *
def frames_with_intra(seed=3, w=16, h=16):
    rng = np.random.RandomState(seed)
    a = np.clip(128 + rng.randn(h, w) * 30, 0, 255).astype(np.uint8)
    b = np.clip(128 + rng.randn(h, w) * 60, 0, 255).astype(np.uint8)
    u = np.clip(128 + rng.randn(h // 2, w // 2) * 25, 0, 255).astype(np.uint8)
    v = np.clip(128 + rng.randn(h // 2, w // 2) * 25, 0, 255).astype(np.uint8)
    return [(a, u, v), (b, v, u)]
def try_parse(bs, j, ap, V, rl=2, dc=1):
    data = bytes(int(bs[i:i + 8].ljust(8, '0'), 2) for i in range(0, len(bs), 8))
    b = Bits(data); b.p = j
    st = Esc3()
    try:
        if ap: b.get(1)
        for i in range(6):
            read_dc(b, ('luma%d' if i < 4 else 'chroma%d') % dc)
            if (V >> (5 - i)) & 1: read_block_ac(b, L[rl] if i < 4 else C[rl], 5, st)
    except Exception: return None
    return b.p
if __name__ == '__main__':
    for seed in range(4):
        e = enc_seq(frames_with_intra(seed), w=16, h=16, q=5, g=30)
        s = bits(e[1])[12:]
        print('body', len(s), s[:40])
        sols = []
        for j in range(1, 20):
            for ap in (0, 1):
                for V in range(64):
                    end = try_parse(s, 1 + j, ap, V)
                    if end is not None and len(s.rstrip('0')) <= end <= len(s.rstrip('0')) + 0 + 7 and end <= len(s): sols.append((j, ap, V))
        print(sols[:10])
