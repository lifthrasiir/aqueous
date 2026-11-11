"""Label and clean the walked AC tables: level from the amplitude, last from rest-independence, duplicates and over-long junk dropped."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ac_probe import *
HERE = os.path.dirname(os.path.abspath(__file__))
ESC = {'rl0': '0010110', 'rl1': '001001010', 'rl2': '0000011', 'c0': '000001101', 'c1': '101101001', 'c2': '00000110'}
def build(name):
    d = json.load(open(os.path.join(HERE, 'ac_raw_%s.json' % name)))
    esc = ESC[name]
    if name.startswith('rl'):
        rl = int(name[2]); blk = 3
        mk = lambda bits: tob(hdr_i(q=5, rl=rl) + '00110' + '0' + '1' * 4 + bits + '1' * 60)
    else:
        rlc = int(name[1]); blk = 4
        mk = lambda bits: tob(hdr_i(q=5, rl=0, rlc=rlc) + '01001' + '0' + '1' * 4 + '00' + bits + '1' * 60)
    out = {}
    for p, (pos, amp) in sorted(d['leaves'].items(), key=lambda x: (len(x[0]), x[0])):
        if p.startswith(esc): continue
        rng = random.Random(1)
        res = DEC.frames([mk(p + '0' + rnd(24, rng)) for _ in range(12)])
        blocks = set(tuple(blocks_yuv(o)[blk]) for o, b in res if o is not None and not b)
        out[p] = dict(pos=pos, level=round((amp - 5) / 10), amp=amp, last=1 if len(blocks) == 1 else 0)
    seen = {}; clean = {}; k = 2.0 ** -len(esc)
    for p, v in out.items():
        key = (tuple(v['pos']), v['level'], v['last'])
        if key in seen: continue
        seen[key] = p; clean[p] = v; k += 2.0 ** -len(p)
        if k >= 0.9985: break
    json.dump(clean, open(os.path.join(HERE, 'ac_labeled_%s.json' % name), 'w'))
    print(name, len(out), '->', len(clean), 'kraft', round(k, 5), 'maxlen', max(len(p) for p in clean))
if __name__ == '__main__':
    for n in sys.argv[1:]: build(n)
