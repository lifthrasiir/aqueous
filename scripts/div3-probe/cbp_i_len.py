import sys; sys.path.insert(0, "scripts/div3-probe")
from tree import tob, hdr_i, rnd, DEC
import random
import sys, json; sys.path.insert(0, 'scripts/div3-probe')
exec(open('scripts/div3-probe/cbp_i_merge.py', encoding='utf-8').read().split('# resolve')[0].replace("print(", "(lambda *a: None)("))
from cbp_i_probe import mask   # nonflat mask of the 6 blocks
strings = {}
for V in range(64):
    st = bits_of(enc_frames([frame_for(V)], q=5)[0]); strings[V] = st[header(st)['mb']:]
rng = random.Random(3)
codes = {}
for V in range(64):
    for l in range(1, 13):
        p = strings[V][:l]
        pk = [tob(hdr_i(q=5) + p + rnd(40, rng) + '1' * 40) for _ in range(200)]
        obs = [mask(o) for o, b in DEC.frames(pk) if o is not None and not b]
        fr = [sum((m >> (5 - i)) & 1 for m in obs) / max(len(obs), 1) for i in range(6)]
        good = all((fr[i] >= 0.8) if (V >> (5 - i)) & 1 else (fr[i] <= 0.05) for i in range(6))
        if good: codes[V] = p; break
print(len(codes), [V for V in range(64) if V not in codes])
allc = list(codes.values())
print('prefix-free', not any(a != b and b.startswith(a) for a in allc for b in allc), 'kraft', sum(2.0 ** -len(c) for c in allc))
json.dump({str(k): v for k, v in codes.items()}, open('scripts/div3-probe/cbp_i_codes.json', 'w'))

for V in [29, 37, 57]:
    for l in range(10, 20):
        p = strings[V][:l]
        pk = [tob(hdr_i(q=5) + p + rnd(40, rng) + '1' * 40) for _ in range(400)]
        obs = [mask(o) for o, b in DEC.frames(pk) if o is not None and not b]
        fr = [sum((m >> (5 - i)) & 1 for m in obs) / max(len(obs), 1) for i in range(6)]
        if all((fr[i] >= 0.6) if (V >> (5 - i)) & 1 else (fr[i] <= 0.05) for i in range(6)) and not any(o.startswith(p) or p.startswith(o) for o in codes.values()):
            codes[V] = p; print(V, p); break
print(len(codes), sum(2.0 ** -len(c) for c in codes.values()))
json.dump({str(k): v for k, v in codes.items()}, open('scripts/div3-probe/cbp_i_codes.json', 'w'))
