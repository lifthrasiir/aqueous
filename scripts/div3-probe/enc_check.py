import sys; sys.path.insert(0,'scripts/div3-probe')
import div3
from div3 import *
from enc import enc_frames
import numpy as np
from collections import Counter
def frames(n=60, seed=7, w=32, h=32):
    rng=np.random.RandomState(seed); out=[]
    for t in range(n):
        q=[2,4,5,8,12,20][t%6]; amp=[3,8,20,50][t//6%4]
        y=np.clip(128+rng.randn(h,w)*amp,0,255).astype(np.uint8)
        u=np.clip(128+rng.randn(h//2,w//2)*amp/2,0,255).astype(np.uint8); v=np.clip(128+rng.randn(h//2,w//2)*amp/2,0,255).astype(np.uint8)
        out.append(enc_frames([(y,u,v)],w=w,h=h,q=q)[0])
    return out
if __name__=='__main__':
    res=Counter(); ex=[]
    for pk in frames():
        try: left=parse_i(pk,w=32,h=32); res['ok' if 17<=left<=24 else 'left']+=1
        except Exception as e: res[str(e)[:22]]+=1
    print(res)
