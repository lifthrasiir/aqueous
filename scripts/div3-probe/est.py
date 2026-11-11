"""Estimate the gain of a context model on the parsed P frames: adaptive frequency counts per context, cost = -log2 p."""
import sys, math; sys.path.insert(0, 'scripts/div3-probe')
import div3
from div3 import *
from avi_read import video_frames
from collections import defaultdict
class Model:
    def __init__(self): self.t = defaultdict(lambda: defaultdict(float)); self.bits = defaultdict(float); self.n = defaultdict(int)
    def code(self, name, ctx, sym, alpha=0.4, nsym=None):
        d = self.t[(name, ctx)]
        tot = sum(d.values()); k = len(d)
        # KT-like estimator with an escape for unseen symbols (cost: uniform over nsym)
        if sym in d: p = d[sym] / (tot + alpha * (k + 1) + 0.0) if False else (d[sym]) / (tot + alpha)
        else: p = (alpha / (tot + alpha)) / max(1, (nsym or 64) - k)
        c = -math.log2(max(p, 1e-12)); self.bits[name] += c; self.n[name] += 1
        d[sym] = d.get(sym, 0.0) + 1.0
        if tot > 4000: 
            for s in list(d): d[s] *= 0.5
        return c
M = Model(); ORIG = defaultdict(float)
def run(maxframes=600, start=0):
    import re
    nfr = 0; orig = 0
    for k, d in video_frames('F:/Data/aqr-corpus/aqr-div3/mari.avi', start, maxframes):
        if d[0] >> 6 != 1: continue
        div3.EV = []
        try: left = parse_p(d)
        except Exception: continue
        if left >= 8: continue
        nfr += 1; orig += len(d) * 8
        prev = None
        for ev in div3.EV:
            M.code(*ev[:3], nsym=ev[3] if len(ev) > 3 else None)
            ORIG[ev[0]] += ev[-1] if isinstance(ev[-1], (int, float)) and ev[0] != 'x' else 0
    return nfr, orig
if __name__ == '__main__':
    pass
