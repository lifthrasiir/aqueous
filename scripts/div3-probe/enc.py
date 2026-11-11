"""ffmpeg's MS-MPEG4 v3 encoder (black box) for labelled bitstreams."""
import av, numpy as np, fractions
def enc_frames(planes_list, w=16, h=16, q=5, br=None):
    """planes_list: list of (Y (h,w) uint8, U, V) -> list of packet bytes. All I frames unless gop allows P."""
    c = av.CodecContext.create('msmpeg4', 'w'); c.width = w; c.height = h; c.pix_fmt = 'yuv420p'
    c.time_base = fractions.Fraction(1, 25)
    c.options = {'qmin': str(q), 'qmax': str(q), 'g': '1', 'bf': '0', 'mbd': 'simple', 'me_method': 'zero', 'trellis': '0', 'flags': '+qscale', 'global_quality': str(q * 118)}
    if br: c.bit_rate = br
    c.open(); out = []
    for i, (y, u, v) in enumerate(planes_list):
        arr = np.concatenate([y.reshape(-1, w), u.reshape(-1, w // 2), v.reshape(-1, w // 2)], 0) if False else None
        f = av.VideoFrame(w, h, 'yuv420p')
        for pl, d in zip(f.planes, (y, u, v)):
            a = np.frombuffer(memoryview(pl), dtype=np.uint8) if False else None
            rows = d.shape[0]; ls = pl.line_size
            buf = bytearray(ls * rows)
            for r in range(rows): buf[r * ls:r * ls + d.shape[1]] = d[r].tobytes()
            pl.update(bytes(buf))
        f.pts = i
        for p in c.encode(f): out.append(bytes(p))
    for p in c.encode(None): out.append(bytes(p))
    return out
