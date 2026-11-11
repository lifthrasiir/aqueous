import sys, os, json, av; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
import p_ac as P
from p_ac import *
HERE = os.path.dirname(os.path.abspath(__file__))
codes = {int(k): v for k, v in json.load(open(os.path.join(HERE, 'p_mb_codes_m2.json'))).items()}
used = set(codes.values())
ref_o, _ = Dec().frames([P.I_PKT])[0]
def clean_decode(pkt):
    c = av.CodecContext.create('msmpeg4', 'r'); c.width = 16; c.height = 16; c.pix_fmt = 'yuv420p'; c.thread_count = 1; c.open()
    c.decode(av.Packet(P.I_PKT))
    with av.logging.Capture() as logs:
        try: fr = c.decode(av.Packet(pkt))
        except Exception: return None
    return fr[0].to_ndarray().tobytes() if fr and not logs else None
hdr = hdr_p(rl=2)
hits = []
for L in range(1, 13):
    for v in range(1 << L):
        p = format(v, '0%db' % L)
        if any(p.startswith(u) or u.startswith(p) for u in used): continue
        pkt = tob(hdr + '0' + p + '00' + '0' * 16)
        o = clean_decode(pkt)
        if o is not None and o == ref_o: hits.append(p)
print(len(hits), hits[:20])
