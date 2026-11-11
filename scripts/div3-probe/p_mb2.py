import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import div3
from div3 import *
import p_mb as M
from collections import Counter
def tail_ok(bs, nblocks):
    """Parse nblocks AC sequences with the B_2 table from bit string bs; True if they consume everything but zero padding."""
    data = bytes(int(bs[i:i + 8].ljust(8, '0'), 2) for i in range(0, len(bs), 8))
    b = Bits(data); st = Esc3()
    try:
        for _ in range(nblocks): read_block_ac(b, C[2], 5, st)
    except Exception: return False
    return b.p <= len(bs) and set(bs[b.p:]) <= {'0'}
res = {}
for V in range(1, 64):
    full = M.body(V); k = bin(V).count('1')
    n = len(full.rstrip('0'))
    cands = []
    for j in range(1, 20):           # j bits of M + MV00 after the skip bit
        rest = full[1 + j:]
        # the end of the data is within the padding: try consumption from rest
        if tail_ok(rest, k): cands.append(j)
    res[V] = (full[1:], cands)
json.dump({str(k): v for k, v in res.items()}, open(os.path.join(M.HERE, 'p_mb2.json'), 'w'))
print(Counter(len(c) for _, c in res.values()))
for V in (1, 2, 4, 8, 12, 63): print(V, res[V][0][:30], res[V][1])
