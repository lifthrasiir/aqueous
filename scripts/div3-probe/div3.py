"""Prototype DIV3 I-frame parser over the tables probed so far (to validate them on real frames)."""
import json, os
HERE = os.path.dirname(os.path.abspath(__file__))
def J(n): return json.load(open(os.path.join(HERE, n)))
DC = J('dc_raw.json')
CBP_FLAGS = {v: int(k) for k, v in J('cbp_i_codes2.json').items()}
def predf(a, bb, c, var): return (a if bb == c else c) if var == 0 else (c if a == bb else a)
def to_symbol(F, var):
    # the first MB has no neighbours (flags 0): the symbol bits are the flags xor their prediction
    f = [(F >> (5 - i)) & 1 for i in range(6)]
    s0 = f[0]; s1 = f[1] ^ predf(f[0], 0, 0, var); s2 = f[2] ^ predf(0, 0, f[0], var); s3 = f[3] ^ predf(f[2], f[0], f[1], var)
    return (s0 << 5) | (s1 << 4) | (s2 << 3) | (s3 << 2) | (f[4] << 1) | f[5]
CBP = {}
def _zigzag():
    order = []
    for d in range(1, 15):
        pts = [(u, d - u) for u in range(max(0, d - 7), min(7, d) + 1)]
        order += pts if d % 2 else pts[::-1]
    return {p: i + 1 for i, p in enumerate(order)}
IDX = _zigzag()
_known = {tuple(map(int, k[1:-1].split(','))): v for k, v in J('scan_idx_block3.json').items()}
assert all(IDX[k] == v for k, v in _known.items())
ESC = {'L0': '0010110', 'L1': '001001010', 'L2': '0000011', 'C0': '000001101', 'C1': '101101001', 'C2': '00000110'}

class Tab:
    def __init__(self, labeled, esc):
        self.codes = {}; self.esc = esc
        self.maxl = {}; self.maxr = {}
        for p, v in labeled.items():
            pos = tuple(v['pos']); run = IDX.get(pos, IDX.get((pos[1], pos[0])))
            if run is None: run = 64
            run -= 1
            self.codes[p] = (run, v['level'], v['last'])
            self.maxl[(v['last'], run)] = max(self.maxl.get((v['last'], run), 0), v['level'])
            self.maxr[(v['last'], v['level'])] = max(self.maxr.get((v['last'], v['level']), -1), run)
        self.maxlen = max(len(p) for p in self.codes)
L = [Tab(J('ac_labeled_rl%d.json' % i), ESC['L%d' % i]) for i in range(3)]
C = [Tab(J('ac_labeled_c%d.json' % i), ESC['C%d' % i]) for i in range(3)]

class Bits:
    def __init__(self, data): self.d = data; self.p = 0; self.n = len(data) * 8
    def get(self, k):
        v = 0
        for _ in range(k):
            if self.p >= self.n: raise EOFError
            v = (v << 1) | ((self.d[self.p >> 3] >> (7 - (self.p & 7))) & 1); self.p += 1
        return v
    def peek_str(self, k):
        return ''.join(str((self.d[(self.p + i) >> 3] >> (7 - ((self.p + i) & 7))) & 1) if self.p + i < self.n else '0' for i in range(k))

def vlc(b, codes, maxlen):
    s = b.peek_str(maxlen)
    for l in range(1, maxlen + 1):
        if s[:l] in codes: b.p += l; return codes[s[:l]]
    raise ValueError('bad vlc ' + s)
DCT = {}
def dctab(name):
    if name not in DCT:
        d = DC[name]; DCT[name] = ({p: l for p, l in d['codes'].items()}, d['escape'], max(len(p) for p in d['codes']), max(len(d['escape']), 1))
    return DCT[name]
def read_dc(b, name):
    codes, esc, ml, _ = dctab(name)
    s = b.peek_str(len(esc) + 1)
    if s.startswith(esc): b.p += len(esc); lvl = b.get(8)
    else:
        for l in range(1, ml + 1):
            if s_all(b, l) in codes: lvl = codes[s_all(b, l)]; b.p += l; break
        else: raise ValueError('bad dc')
    if lvl and b.get(1): lvl = -lvl
    return lvl
def s_all(b, l): return b.peek_str(l)

TRACE = None
ESC2_ADD = 0
class Esc3:
    ll = 0
def read_block_ac(b, tab, q, st):
    i = 0
    while True:
        s = b.peek_str(tab.maxlen); et = 0
        if s.startswith(tab.esc):
            b.p += len(tab.esc)
            if b.get(1):
                et = 1          # esc 1: level + max_level
                run, lvl, last = vlc(b, tab.codes, tab.maxlen); lvl += tab.maxl[(last, run)]
                sign = b.get(1)
            elif b.get(1):        # esc 2: run + max_run + 1
                et = 2
                run, lvl, last = vlc(b, tab.codes, tab.maxlen); run += tab.maxr[(last, lvl)] + ESC2_ADD
                sign = b.get(1)
            else:
                et = 3
                last = b.get(1); run = b.get(6)
                lvl = b.get(8)
                if lvl >= 128: lvl -= 256
                sign = 1 if lvl < 0 else 0; lvl = abs(lvl)
        else:
            run, lvl, last = vlc(b, tab.codes, tab.maxlen); sign = b.get(1)
        if TRACE is not None: TRACE.append((run, lvl, last, sign, et))
        i += run + 1
        if i > 63: raise ValueError('run past block')
        if last: return

def d012(b):
    if not b.get(1): return 0
    return 1 if not b.get(1) else 2

def parse_i(data, w=640, h=352, predvar=0, trace=False):
    CBP.clear(); CBP.update({c: to_symbol(F, predvar) for c, F in CBP_FLAGS.items()})
    b = Bits(data)
    if b.get(2) != 0: raise ValueError('not I')
    q = b.get(5); b.get(5)
    rlc = d012(b); rl = d012(b); dc = b.get(1)
    mbw, mbh = w // 16, h // 16
    flags = [[0] * (mbw * 2 + 2) for _ in range(mbh * 2 + 2)]    # coded flags per 8x8 block, 1-border
    st = Esc3()
    for my in range(mbh):
        for mx in range(mbw):
            V = vlc(b, CBP, 14) if False else None
            s = b.peek_str(20)
            for l in range(1, 20):
                if s[:l] in CBP: V = CBP[s[:l]]; b.p += l; break
            else: raise ValueError('bad cbp at %d,%d' % (mx, my))
            cb = []
            for i in range(6):
                bit = (V >> (5 - i)) & 1
                if i < 4:
                    x = mx * 2 + (i & 1) + 1; y = my * 2 + (i >> 1) + 1
                    a = flags[y][x - 1]; bb = flags[y - 1][x - 1]; c = flags[y - 1][x]
                    pred = (a if bb == c else c) if predvar == 0 else (c if a == bb else a)
                    bit ^= pred; flags[y][x] = bit
                cb.append(bit)
            b.get(1)              # ac_pred
            for i in range(6):
                read_dc(b, ('luma%d' if i < 4 else 'chroma%d') % dc)
                if cb[i]: read_block_ac(b, L[rl] if i < 4 else C[rlc], q, st)
    return b.n - b.p
if __name__ == '__main__':
    import sys
    sys.path.insert(0, HERE)
    from avi_read import video_frames
    P = 'F:/Data/aqr-corpus/aqr-div3/mari.avi'
    n = 0
    for k, d in video_frames(P, 0, 400):
        if d[0] >> 6: continue
        for pv in (0, 1):
            try: print(k, len(d), 'predvar', pv, 'left bits', parse_i(d, predvar=pv))
            except Exception as e: print(k, len(d), 'predvar', pv, 'ERR', repr(e)[:80])
        n += 1
        if n >= 4: break

# ---- P frames (work in progress) ----
MVT = [J('p_mv_table0.json'), J('p_mv_table1.json')]
MBI = {v: int(k) for k, v in J('p_mb_codes_v5.json').items()}     # inter code -> flags of the first MB
MBI['01'] = 0

def read_mv(b, tab):
    s = b.peek_str(24)
    if s.startswith(tab['esc']):
        b.p += len(tab['esc']); mx = b.get(6); my = b.get(6); return (mx, my, 'esc')
    for l in range(1, 22):
        if s[:l] in tab['codes']: b.p += l; return tab['codes'][s[:l]]
    raise ValueError('bad mv')

MBA = {v: int(k) for k, v in J('p_intra_codes.json').items()}

def parse_p(data, w=640, h=352, predvar=0, trace=None, no_skip_flag=False, extra=0):
    CBP.clear(); CBP.update(MBI)
    CBPA = dict(MBA)
    b = Bits(data)
    if b.get(2) != 1: raise ValueError('not P')
    q = b.get(5); use_skip = b.get(1); rl = d012(b); dc = b.get(1); mvt = b.get(1)
    mbw, mbh = w // 16, h // 16
    flags = [[0] * (mbw * 2 + 2) for _ in range(mbh * 2 + 2)]
    st = Esc3()
    for my in range(mbh):
        for mx in range(mbw):
            if use_skip and b.get(1): continue
            s = b.peek_str(24); intra = False
            for l in range(1, 24):
                if s[:l] in CBP: V = CBP[s[:l]]; b.p += l; break
                if s[:l] in CBPA: V = CBPA[s[:l]]; b.p += l; intra = True; break
            else: raise ValueError('bad mb code %s at %d,%d' % (s[:14], mx, my))
            cb = []
            for i in range(6):
                bit = (V >> (5 - i)) & 1
                cb.append(bit)
            if intra:
                b.get(1)                                    # ac_pred
                for i in range(6):
                    read_dc(b, ('luma%d' if i < 4 else 'chroma%d') % dc)
                    if cb[i]: read_block_ac(b, L[rl] if i < 4 else C[rl], q, st)
            else:
                read_mv(b, MVT[mvt])
                for i in range(6):
                    if cb[i]: read_block_ac(b, C[rl], q, st)
    return b.n - b.p
