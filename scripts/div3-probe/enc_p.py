"""ffmpeg's encoder as a source of labelled P frames."""
import av, numpy as np, fractions
def enc_seq(frames, w=32, h=32, q=5, g=30, me='zero', extra=None):
    c = av.CodecContext.create('msmpeg4', 'w'); c.width = w; c.height = h; c.pix_fmt = 'yuv420p'; c.time_base = fractions.Fraction(1, 25)
    o = {'qmin': str(q), 'qmax': str(q), 'g': str(g), 'bf': '0', 'mbd': 'simple', 'me_method': me, 'trellis': '0', 'flags': '+qscale', 'global_quality': str(q * 118)}
    if extra: o.update(extra)
    c.options = o; c.open(); out = []
    for i, (y, u, v) in enumerate(frames):
        f = av.VideoFrame(w, h, 'yuv420p')
        for pl, d in zip(f.planes, (y, u, v)):
            ls = pl.line_size; buf = bytearray(ls * d.shape[0])
            for r in range(d.shape[0]): buf[r * ls:r * ls + d.shape[1]] = d[r].tobytes()
            pl.update(bytes(buf))
        f.pts = i
        for p in c.encode(f): out.append(bytes(p))
    for p in c.encode(None): out.append(bytes(p))
    return out
def bits(pk): return ''.join(format(x, '08b') for x in pk)
def flat(w, h, v=128): return (np.full((h, w), v, np.uint8), np.full((h // 2, w // 2), 128, np.uint8), np.full((h // 2, w // 2), 128, np.uint8))
