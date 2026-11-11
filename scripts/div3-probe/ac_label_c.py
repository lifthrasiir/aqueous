import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ac_probe import *
HERE = os.path.dirname(os.path.abspath(__file__))
rlc = int(sys.argv[1]); esc = sys.argv[2]
d = json.load(open(os.path.join(HERE, 'ac_raw_c%d.json' % rlc)))
mk = lambda bits: tob(hdr_i(q=5, rl=0, rlc=rlc) + '01001' + '0' + '1' * 4 + '00' + bits + '1' * 60)
out = {}
for p, (pos, amp) in d['leaves'].items():
    if p.startswith(esc): continue
    rng = random.Random(1)
    res = DEC.frames([mk(p + '0' + rnd(24, rng)) for _ in range(12)])
    blocks = set(tuple(blocks_yuv(o)[4]) for o, b in res if o is not None and not b)
    out[p] = dict(pos=pos, level=round((amp - 5) / 10), amp=amp, last=1 if len(blocks) == 1 else 0)
json.dump(out, open(os.path.join(HERE, 'ac_labeled_c%d.json' % rlc), 'w'))
from collections import Counter
c = Counter((tuple(v['pos']), v['level'], v['last']) for v in out.values())
print(rlc, len(out), Counter(v['last'] for v in out.values()), 'dups', sum(1 for n in c.values() if n > 1))
