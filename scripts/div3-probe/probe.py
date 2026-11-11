"""Probe an installed ffmpeg (black box) with hand-made DIV3 frames."""
import os, random, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from avi import make_avi

def tob(b):
    b += '0' * (-len(b) % 8)
    return bytes(int(b[i:i + 8], 2) for i in range(0, len(b), 8))

def hdr_i(q=4, rlc=0, rl=0, dc=0):
    d012 = lambda v: '0' if v == 0 else '10' if v == 1 else '11'
    return '00' + format(q, '05b') + '10111' + d012(rlc) + d012(rl) + str(dc)

def run(frames, w=16, h=16):
    """frames: list of bytes (a sequence decoded in one go). Returns (list of frames' yuv420p, bad)."""
    fd, p = tempfile.mkstemp(suffix='.avi'); os.close(fd)
    try:
        open(p, 'wb').write(make_avi(frames, w, h))
        r = subprocess.run(['ffmpeg', '-hide_banner', '-v', 'warning', '-threads', '1', '-i', p, '-vsync', '0',
                            '-f', 'rawvideo', '-pix_fmt', 'yuv420p', '-'], capture_output=True)
    finally:
        os.remove(p)
    err = r.stderr.decode(errors='replace')
    n = w * h * 3 // 2
    outs = [r.stdout[i:i + n] for i in range(0, len(r.stdout), n)]
    bad = 'concealing' in err or 'damaged' in err or 'invalid' in err or 'error' in err.lower()
    return outs, bad

def pmap(f, xs, workers=12):
    with ThreadPoolExecutor(workers) as ex:
        return list(ex.map(f, xs))

def rnd(n, rng):
    return ''.join(rng.choice('01') for _ in range(n))

def blocks_yuv(o, w=16, h=16):
    """The 6 8x8 blocks of the first MB, as lists of 64 values."""
    y = lambda bx, by: [o[(by + r) * w + bx + c] for r in range(8) for c in range(8)]
    u0 = w * h; v0 = u0 + (w // 2) * (h // 2); cw = w // 2
    c = lambda b0: [o[b0 + r * cw + k] for r in range(8) for k in range(8)]
    return [y(0, 0), y(8, 0), y(0, 8), y(8, 8), c(u0), c(v0)]
