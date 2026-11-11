import sys; sys.path.insert(0,'scripts/div3-probe')
import div3
from div3 import *
from enc import enc_frames
from fast import Dec
from ac_probe import fdct
from probe import blocks_yuv
import numpy as np
from collections import Counter
inv={i:p for p,i in IDX.items()}
def run(n=200, seed=11, amps=(6,12,25), qs=(4,8,12)):
    rng=np.random.RandomState(seed); D_=Dec(); stats=Counter(); ex=[]
    for t in range(n):
        q=qs[t%len(qs)]; amp=amps[t//len(qs)%len(amps)]
        y=np.clip(128+rng.randn(16,16)*amp,0,255).astype(np.uint8)
        u=np.clip(128+rng.randn(8,8)*amp/2,0,255).astype(np.uint8); v=np.clip(128+rng.randn(8,8)*amp/2,0,255).astype(np.uint8)
        pk=enc_frames([(y,u,v)],w=16,h=16,q=q)[0]
        o,bad=D_.frames([pk])[0]
        F=fdct(blocks_yuv(o)[0])
        dec={(a,b) for a in range(8) for b in range(8) if abs(F[a,b])>=q and (a,b)!=(0,0)}
        div3.TRACE=[]
        try: left=parse_i(pk,w=16,h=16)
        except Exception as e: left=str(e)[:20]
        pos=0; seq=[]
        for run_,l,last,sg,et in div3.TRACE:
            pos+=run_+1; seq.append((pos,l,et,last))
            if last: break
        P={inv.get(i) for i,_,_,_ in seq if i<=63}; PT={(b,a) for a,b in P if a is not None}
        cons=all(p in dec for p in P) or all(p in dec for p in PT)
        if cons: stats['block0 ok' if isinstance(left,int) and 17<=left<=24 else 'block0 ok, frame %s'%(left if not isinstance(left,int) else 'left')]+=1
        else:
            bi=next(i for i,(pp,l,et,last) in enumerate(seq) if inv.get(pp) not in dec and (inv.get(pp) is None or (inv[pp][1],inv[pp][0]) not in dec))
            stats['block0 mismatch esc%d'%seq[bi][2]]+=1; ex.append((q,amp,bi,seq[max(0,bi-1):bi+1]))
    return stats,ex
if __name__=='__main__':
    s,ex=run(); print(s); print(ex[:4])
