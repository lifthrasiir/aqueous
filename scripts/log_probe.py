"""The prototype of aqr-log (docs/log-design.md): lines of text as templates and their fields.

Each line is cut into tokens at spaces; a token whose digit-masked form is rare in the file
(fewer than T = max(64, lines / 500) times) is a string variable (0x02 in the template), any
other keeps its letters in the template and its digit runs as numbers (0x01). Numbers are
columns by (template, slot), each by the best of three predictors (as it is, against the
column's previous number, against slot k of the line before), chosen by zlib; their padding
(leading zeros) is a byte each. Streams are measured by zstd -9 (the archive's range).

    python3 scripts/log_probe.py raw+skel+line FILE...
"""
import sys, re, lzma, zlib, collections
def zz(v): return (v<<1)^(v>>63)
def vb(out, v):
    while v>=0x80: out.append(v&0x7f|0x80); v>>=7
    out.append(v)
from compression import zstd as Z
def x(b): return len(Z.compress(bytes(b), level=9))
DIG=re.compile(rb'\d+')
def run(path, preds):
    d=open(path,'rb').read()
    lines=d.split(b'\n'); T=max(64,len(lines)//500)
    cnt=collections.Counter()
    for l in lines:
        for t in l.split(b' '): cnt[DIG.sub(b'\x01',t)]+=1
    skel_id={}; skel_text=bytearray(); ids=bytearray(); pads=bytearray()
    cols=collections.defaultdict(list); strs=collections.defaultdict(bytearray)
    prevline=[]
    for l in lines:
        nums=[]; svars=[]; sk=[]
        for t in l.split(b' '):
            m=DIG.sub(b'\x01',t)
            if cnt[m]<T: svars.append(t); sk.append(b'\x02')
            else: nums+=DIG.findall(t); sk.append(m)
        sk=b' '.join(sk)
        if sk not in skel_id: skel_id[sk]=len(skel_id); skel_text+=sk+b'\n'
        sid=skel_id[sk]; vb(ids,sid)
        vals=[int(n) for n in nums]
        for k,n in enumerate(nums):
            v=vals[k]; w=len(n); pads.append(w if w>len(str(v)) else 0)
            cols[(sid,k)].append((v, prevline[k] if k<len(prevline) else 0))
        prevline=vals
        for k,s in enumerate(svars): strs[(sid,k)]+=s+b'\n'
    allc=bytearray(); choice=collections.Counter()
    for key in sorted(cols):
        c=cols[key]; best=None
        for p in preds:
            b=bytearray(); prev=0
            for v,pl in c:
                ref={'raw':0,'skel':prev,'line':pl}[p]; vb(b,zz(v-ref)); prev=v
            sz=len(zlib.compress(bytes(b),6))
            if best is None or sz<best[0]: best=(sz,p,b)
        choice[best[1]]+=len(c); allc+=best[2]
    alls=bytearray(); [alls.extend(strs[k]) for k in sorted(strs)]
    st=dict(skel=x(skel_text), ids=x(ids), pads=x(pads), nums=x(allc), strs=x(alls))
    tot=sum(st.values())
    print(f"{path.split('/')[-1]:24} {'+'.join(preds):14} T={T} {len(skel_id):6} skel total {tot/1e6:6.3f} MB {tot/len(d)*100:5.2f}% "+" ".join(f"{k}={v/1e6:.3f}" for k,v in st.items()), dict(choice), flush=True)
for p in sys.argv[2:]:
    for preds in sys.argv[1].split(','): run(p, preds.split('+'))
