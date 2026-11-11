import sys, json; sys.path.insert(0, 'scripts/div3-probe')
exec(open('scripts/div3-probe/cbp_i_merge.py', encoding='utf-8').read().split('# resolve')[0].replace("print(", "(lambda *a: None)("))
cand = {V: sorted(res[V], key=len) for V in range(64)}
cand[0] = ['1']

miss=[V for V in range(64) if not cand[V]]
print('empty', miss)
strings={}
for V in range(64):
    st = bits_of(enc_frames([frame_for(V)], q=5)[0]); strings[V] = st[header(st)['mb']:]
order = sorted([V for V in range(64) if cand[V]], key=lambda V: len(cand[V]))
sol = {}
def ok(c): return not any(o.startswith(c) or c.startswith(o) for o in sol.values())
def kr(): return sum(2.0 ** -len(c) for c in sol.values())
count=[0]; best=[None]
def rec(i):
    count[0]+=1
    if count[0]>3000000: return True
    if i==len(order):
        # place the missing: each takes the shortest prefix of its string free of conflicts; check total Kraft
        t=dict(sol); 
        for V in miss:
            for l in range(2,13):
                c=strings[V][:l]
                if not any(o.startswith(c) or c.startswith(o) for o in t.values()): t[V]=c; break
        k=sum(2.0**-len(c) for c in t.values())
        if len(t)==64 and abs(k-1)<1e-9: best[0]=t; return True
        return False
    V=order[i]
    for c in cand[V]:
        if ok(c):
            sol[V]=c
            if kr()<=1+1e-9 and rec(i+1): return True
            del sol[V]
    return False
rec(0)
print(count[0], best[0] is not None)
if best[0]: json.dump({str(k): v for k, v in best[0].items()}, open('scripts/div3-probe/cbp_i_codes.json','w')); print(sorted(best[0].items())[:8])
