"""Chroma intra AC tables (rl_chroma_table_index 0..2): block 4 (U) coded alone, cbp 01001."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ac_probe import *
HERE = os.path.dirname(os.path.abspath(__file__))
if __name__ == '__main__':
    rlc = int(sys.argv[1])
    import ac_probe as _A
    _old = json.load(open(os.path.join(HERE, 'ac_labeled_c%d.json' % rlc)))
    _A.TERMS[:] = [c + '0' for _, c in sorted((len(c), c) for c, v in _old.items() if v['last'] == 1)[:4]]
    mk = lambda bits: tob(hdr_i(q=5, rl=0, rlc=rlc) + '01001' + '0' + '1' * 4 + '00' + bits + '1' * 60)
    leaves, unres = walk_ac(mk, 4, maxd=17)
    print('rlc', rlc, len(leaves), 'kraft', kraft(leaves), 'unres', len(unres), lcp(unres) if unres else None, flush=True)
    json.dump({'leaves': {p: [list(v[0]), v[1]] for p, v in leaves.items()}, 'unres': unres},
              open(os.path.join(HERE, 'ac_raw_c%d.json' % rlc), 'w'))
