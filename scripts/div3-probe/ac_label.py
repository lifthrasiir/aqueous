"""Label the walker's AC leaves: level from the amplitude, last by whether the block depends on what follows."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ac_probe import *
HERE = os.path.dirname(os.path.abspath(__file__))
def label(rl, esc, q=5):
    d = json.load(open(os.path.join(HERE, 'ac_raw_rl%d.json' % rl)))
    mk = lambda bits: tob(hdr_i(q=q, rl=rl) + '00110' + '0' + '1' * 4 + bits + '1' * 60)
    out = {}
    for p, (pos, amp) in d['leaves'].items():
        if p.startswith(esc): continue
        rng = random.Random(1)
        res = DEC.frames([mk(p + '0' + rnd(24, rng)) for _ in range(12)])
        blocks = set(tuple(blocks_yuv(o)[3]) for o, b in res if o is not None and not b)
        lvl = round((amp - 5) / 10)
        out[p] = dict(pos=pos, level=lvl, amp=amp, last=1 if len(blocks) == 1 else 0)
    return out
if __name__ == '__main__':
    rl = int(sys.argv[1]); esc = sys.argv[2]
    t = label(rl, esc)
    json.dump(t, open(os.path.join(HERE, 'ac_labeled_rl%d.json' % rl), 'w'))
    from collections import Counter
    print(len(t), Counter(v['last'] for v in t.values()), 'odd amps', [ (p, v['amp']) for p, v in t.items() if abs(v['amp'] - (10 * v['level'] + 5)) > 1.5][:5])
