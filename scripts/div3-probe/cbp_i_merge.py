import sys,json; sys.path.insert(0,'scripts/div3-probe')
from enc_ac import *
from ac_probe import _C
found=json.load(open('scripts/div3-probe/cbp_i_raw2.json'))
def frame_for(V):
    y=np.full((16,16),128.0); u=np.full((8,8),128.0); v=np.full((8,8),128.0)
    for i in range(6):
        if (V>>(5-i))&1:
            b=15*np.outer(_C[0],_C[1])
            if i<4:
                r0,c0=(i//2)*8,(i%2)*8; y[r0:r0+8,c0:c0+8]+=b
            elif i==4: u+=b
            else: v+=b
    return tuple(np.clip(np.round(a),0,255).astype(np.uint8) for a in (y,u,v))
res={}
for V in range(64):
    s=bits_of(enc_frames([frame_for(V)],q=5)[0]); h=header(s); st=s[h['mb']:]
    res[V]=[p for p,vs in found.items() if V in vs and st.startswith(p)]
print(sum(1 for c in res.values() if c),'of 64 have candidates; unique:',sum(1 for c in res.values() if len(c)==1))
print({V:c for V,c in res.items() if len(c)!=1})

# resolve: longest candidate per V among probe matches; the rest from free code space
known = {V: max(c, key=len) for V, c in res.items() if c}
bad = [V for V, c in known.items() if any(o != c and (o.startswith(c) or c.startswith(o)) for W, o in known.items() if W != V)]
print('conflicts', bad)
strings = {}
for V in range(64):
    s = bits_of(enc_frames([frame_for(V)], q=5)[0]); h = header(s); strings[V] = s[h['mb']:]
for V in bad: known.pop(V)
free = {}
for V in range(64):
    if V in known: continue
    st = strings[V]
    for l in range(1, 13):
        c = st[:l]
        if not any(o.startswith(c) or c.startswith(o) for o in known.values()):
            free[V] = c; break
print(len(known), len(free), 'kraft known', sum(2.0 ** -len(c) for c in known.values()), 'free', sum(2.0 ** -len(c) for c in free.values()))
allc = {**known, **free}
print('prefix-free', not any(a != b and (allc[a].startswith(allc[b])) for a in allc for b in allc), 'kraft', sum(2.0 ** -len(c) for c in allc.values()))
json.dump(allc, open('scripts/div3-probe/cbp_i_codes.json', 'w'))
