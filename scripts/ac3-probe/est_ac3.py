"""A rough estimate of the gain of a context model on AC-3 frames: the parser's reads are logged by kind, and each kind is
coded with adaptive frequency counts under a small context; the cost is -log2 p."""
import sys, os, math, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from stream import audio_chunks, syncframes
from collections import defaultdict

# a copy of ac3.py whose mantissa and exponent reads are marked
_src = open(os.path.join(HERE, 'ac3.py'), encoding='utf-8').read()
_src = _src.replace("        def mant(bap):\n", "        def mant(bap):\n            KIND[0] = 'mant'\n            CTX[0] = bap\n")
_src = _src.replace("        g = b.get(7)\n", "        KIND[0] = 'expgrp'\n        g = b.get(7)\n        KIND[0] = 'side'\n")
LOG = []
KIND = ['side']
CTX = [0]
ns = {'__file__': os.path.join(HERE, 'ac3.py'), '__name__': 'ac3est', 'KIND': KIND, 'CTX': CTX}
exec(compile(_src, 'ac3est', 'exec'), ns)
Bits = ns['Bits']
_get = Bits.get


def get(self, k):
    v = _get(self, k)
    LOG.append((KIND[0], CTX[0], k, v))
    if KIND[0] == 'mant':
        KIND[0] = 'side'
    return v


Bits.get = get


class Model:
    def __init__(self):
        self.t = defaultdict(lambda: defaultdict(float))
        self.bits = defaultdict(float)
        self.orig = defaultdict(float)
        self.n = defaultdict(int)

    def code(self, kind, ctx, sym, nbits, alpha=0.5):
        d = self.t[(kind, ctx)]
        tot = sum(d.values())
        if sym in d and tot > 0:
            p = d[sym] / (tot + alpha)
        else:
            p = (alpha / (tot + alpha)) / max(1, (1 << nbits) - len(d))
        c = -math.log2(max(p, 1e-12))
        self.bits[kind] += c
        self.orig[kind] += nbits
        self.n[kind] += 1
        d[sym] = d.get(sym, 0.0) + 1.0
        if tot > 6000:
            for s in list(d): d[s] *= 0.5


def run(buf, frames, maxframes):
    M = Model()
    orig_total = 0
    for off, size, fscod, frm in frames[:maxframes]:
        d = buf[off:off + size]
        LOG.clear()
        KIND[0] = 'side'
        try:
            ns['parse_frame'](d)
        except Exception:
            continue
        orig_total += size * 8
        last = {}
        prev_g = 0
        prev_m = {}
        for kind, ctx, k, v in LOG:
            if kind == 'expgrp':
                M.code('expgrp', (prev_g // 25,), v, 7)
                prev_g = v
            elif kind == 'mant':
                # the value of a mantissa of ctx = bap: its context is the bap and the previous mantissa of that bap
                M.code('mant%02d' % ctx, (prev_m.get(ctx, -1) if ctx <= 5 else prev_m.get(ctx, -1) >> max(0, k - 3),), v, k)
                prev_m[ctx] = v
            else:
                M.code('side', (k, last.get(k, -1)), v, k)
                last[k] = v
    return M, orig_total


if __name__ == '__main__':
    ch = audio_chunks('F:/Data/aqr-corpus/aqr-div3/mari.avi', limit=3000)
    buf = b''.join(ch)
    fr = syncframes(buf)
    t = time.time()
    M, orig = run(buf, fr, 500)
    print('frames 500, original bits', orig, round(time.time() - t), 's')
    to = tm = 0
    for k in sorted(M.bits):
        print(' ', k, 'orig', round(M.orig[k]), 'model', round(M.bits[k]), 'n', M.n[k])
        to += M.orig[k]
        tm += M.bits[k]
    print('total counted', round(to), '->', round(tm), '(%.1f%%) of the frames %d bits' % (100 * tm / to, orig))
