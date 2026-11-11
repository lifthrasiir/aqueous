"""In-process probe decoder (PyAV in the project's .venv): one packet, one result, no processes."""
import av, random, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import tob, hdr_i, rnd

class Dec:
    def __init__(self, w=16, h=16):
        self.w, self.h = w, h
        self.new()
    def new(self):
        c = av.CodecContext.create('msmpeg4', 'r'); c.width = self.w; c.height = self.h; c.pix_fmt = 'yuv420p'
        c.thread_count = 1
        c.open(); self.c = c
    def frames(self, pkts):
        """pkts: list of bytes. Returns list of (yuv bytes or None, bad: bool) for each packet."""
        out = []
        for b in pkts:
            with av.logging.Capture() as logs:
                try:
                    fr = self.c.decode(av.Packet(b))
                except Exception:
                    out.append((None, True)); self.new(); continue
            bad = any(('conceal' in m[2] or 'damaged' in m[2] or 'invalid' in m[2] or 'rror' in m[2]) for m in logs)
            out.append((fr[0].to_ndarray().tobytes() if fr else None, bad))
        return out
