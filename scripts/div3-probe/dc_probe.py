import sys, json, os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tree import *
def dc_leaf(vals):
    v = sorted(vals)
    if len(v) == 1: return abs(v[0] - 128)
    if len(v) == 2 and (abs(v[0] + v[1] - 256) <= 1 or v[1] >= 254 or v[0] <= 1): return max(abs(x - 128) for x in v)
    return None
def table(dc, pre, blk, maxd=34):
    f = lambda out: out[0] if blk == 0 else out[256]
    mk = lambda bits: tob(hdr_i(dc=dc) + '1' + '0' + pre + bits + '1' * 80)
    leaves, unres = walk(mk, f, dc_leaf, maxd)
    esc = lcp(unres) if unres else None
    if not esc:   # the escape: 8 raw bits follow it, the deepest leaves are under it
        esc = max(leaves, key=len)[:-8]
    if esc: leaves = {p: l for p, l in leaves.items() if not p.startswith(esc)}
    codes = dict(leaves)
    return codes, esc
if __name__ == '__main__':
    out = {}
    for dc in (0, 1):
        codes, esc = table(dc, '', 0)
        z = min((p for p, l in codes.items() if l == 0), key=len)
        out['luma%d' % dc] = {'codes': codes, 'escape': esc, 'zero': z}
        print('luma', dc, len(codes), 'kraft', kraft(codes) + (2.0 ** -len(esc)), 'esc', esc, flush=True)
        codes, esc = table(dc, z * 4, 4)
        out['chroma%d' % dc] = {'codes': codes, 'escape': esc}
        print('chroma', dc, len(codes), 'kraft', kraft(codes) + (2.0 ** -len(esc)), 'esc', esc, flush=True)
    json.dump(out, open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'dc_raw.json'), 'w'))
