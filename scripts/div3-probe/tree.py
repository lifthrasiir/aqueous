"""Prefix-tree walking against the in-process decoder."""
import sys, os, random, itertools
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fast import Dec, tob, hdr_i, rnd

DEC = Dec()

def obs_set(mk, p, nenum=6, extra=2, rng=None, f=lambda out: out):
    """Observations f(decoded) over tails: all 2**nenum combinations of the next bits, `extra` random rests.
    mk(bits) -> packet bytes. Returns the set of observations (hashable) and the count of failures."""
    rng = rng or random.Random(hash(p) & 0xffffffff)
    pk = []
    for v in range(1 << nenum):
        for _ in range(extra):
            pk.append(mk(p + format(v, '0%db' % nenum) + rnd(10, rng)))
    res = DEC.frames(pk)
    vals = set(); nbad = 0
    for out, bad in res:
        if bad or out is None: nbad += 1
        else: vals.add(f(out))
    return vals, nbad

def walk(mk, f, is_leaf, maxd=30, start=('',), nenum=6, extra=2):
    """Breadth-first walk. is_leaf(vals) -> payload or None. Returns (leaves {prefix: payload}, unresolved [prefix])."""
    leaves = {}; unres = []; level = list(start)
    while level:
        nxt = []
        for p in level:
            vals, nbad = obs_set(mk, p, nenum, extra, f=f)
            if not vals: continue
            r = is_leaf(vals)
            if r is not None: leaves[p] = r
            elif len(p) < maxd: nxt += [p + '0', p + '1']
            else: unres.append(p)
        level = nxt
    return leaves, unres

def lcp(ps):
    a = min(ps); b = max(ps); i = 0
    while i < len(a) and a[i] == b[i]: i += 1
    return a[:i]

def kraft(ps): return sum(2.0 ** -len(p) for p in ps)

def obs_list(mk, p, nenum=6, extra=2, f=lambda out: out, rng=None):
    """Like obs_set but returns the list of observations of the good decodes (and the count of failures)."""
    rng = rng or random.Random(hash(p) & 0xffffffff)
    pk = [mk(p + format(v, '0%db' % nenum) + rnd(12, rng)) for v in range(1 << nenum) for _ in range(extra)]
    res = DEC.frames(pk)
    return [f(o) for o, bad in res if not bad and o is not None], sum(1 for o, bad in res if bad or o is None)

def walk_list(mk, f, is_leaf, maxd=20, start=('',), nenum=6, extra=2):
    leaves = {}; unres = []; level = list(start)
    while level:
        nxt = []
        for p in level:
            obs, nbad = obs_list(mk, p, nenum, extra, f)
            if not obs: continue
            r = is_leaf(obs)
            if r is not None: leaves[p] = r
            elif len(p) < maxd: nxt += [p + '0', p + '1']
            else: unres.append(p)
        level = nxt
    return leaves, unres
