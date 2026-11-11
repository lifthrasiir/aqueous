"""P-frame tokenizer (mirrors div3.parse_p) producing modelled events: (name, ctx, symbol, nsym, original_bits)."""
import sys, math; sys.path.insert(0, 'scripts/div3-probe')
from div3 import *
import div3

def vlc_sym(b, codes, maxlen):
    s = b.peek_str(maxlen)
    for l in range(1, maxlen + 1):
        if s[:l] in codes: b.p += l; return s[:l], l
    raise ValueError('bad vlc ' + s)

def tokens_p(data, w=640, h=352):
    ev = []
    CBPm = dict(MBI); CBPa = dict(MBA)
    b = Bits(data)
    if b.get(2) != 1: raise ValueError('not P')
    q = b.get(5); use_skip = b.get(1); rl = d012(b); dc = b.get(1); mvt = b.get(1)
    tab = C[rl]; mvtab = MVT[mvt]
    mbw, mbh = w // 16, h // 16
    kind = [[0] * mbw for _ in range(mbh)]           # 0 skip, 1 inter, 2 intra
    mvz = [[(0, 0)] * mbw for _ in range(mbh)]
    st = Esc3()
    for my in range(mbh):
        for mx in range(mbw):
            lk = kind[my][mx - 1] if mx else 0; uk = kind[my - 1][mx] if my else 0
            ctx = (lk, uk)
            if use_skip:
                sk = b.get(1); ev.append(('skip', ctx, sk, 2, 1))
                if sk: continue
            s = b.peek_str(24); intra = False
            for l in range(1, 24):
                if s[:l] in CBPm: V = CBPm[s[:l]]; code = s[:l]; b.p += l; break
                if s[:l] in CBPa: V = CBPa[s[:l]]; code = s[:l]; b.p += l; intra = True; break
            else: raise ValueError('bad mb code')
            kind[my][mx] = 2 if intra else 1
            ev.append(('mbt', ctx, (intra, V), 128, len(code)))
            cb = [(V >> (5 - i)) & 1 for i in range(6)]
            if intra:
                b.get(1); ev.append(('ap', (), 0, 2, 1))
                for i in range(6):
                    tdc = ('luma%d' if i < 4 else 'chroma%d') % dc
                    p0 = b.p; read_dc(b, tdc); ev.append(('dcx', (i < 4,), b.p - p0, 20, b.p - p0))
                    if cb[i]: read_ac_tok(b, L[rl] if i < 4 else C[rl], q, st, ev, (0 if i < 4 else 1) + 2, 63)
            else:
                p0 = b.p
                lmv = mvz[my][mx - 1] if mx else (0, 0); umv = mvz[my - 1][mx] if my else (0, 0)
                mctx = (lmv == (0, 0), umv == (0, 0))
                s = b.peek_str(24)
                if s.startswith(mvtab['esc']):
                    b.p += len(mvtab['esc']); mxv = b.get(6); myv = b.get(6); sym = 'esc'; vec = (mxv, myv)
                else:
                    for l in range(1, 22):
                        if s[:l] in mvtab['codes']: sym = s[:l]; vec = tuple(mvtab['codes'][s[:l]]); b.p += l; break
                    else: raise ValueError('bad mv')
                mvz[my][mx] = vec
                ev.append(('mv', mctx, sym, 1100, b.p - p0))
                for i in range(6):
                    if cb[i]: read_ac_tok(b, tab, q, st, ev, (0 if i < 4 else 1), 64)
    return ev, b.n - b.p

def read_ac_tok(b, tab, q, st, ev, btype, maxi):
    i = 0; prevl = 0
    while True:
        p0 = b.p
        s = b.peek_str(tab.maxlen); et = 0
        if s.startswith(tab.esc):
            b.p += len(tab.esc)
            if b.get(1):
                run, lvl, last = vlc(b, tab.codes, tab.maxlen); sym = ('e1', run, lvl, last); b.get(1)
            elif b.get(1):
                run, lvl, last = vlc(b, tab.codes, tab.maxlen); sym = ('e2', run, lvl, last); b.get(1)
            else:
                last = b.get(1); run = b.get(6); lv = b.get(8); sym = ('e3', run, lv, last)
                run = run; 
            run = {'e1': 0}.get(sym[0], run) if False else run
        else:
            code, l = vlc_sym(b, tab.codes, tab.maxlen); run, lvl, last = tab.codes[code]; sym = code; b.get(1)
        nb = b.p - p0
        ev.append(('ac', (btype, min(i, 15) if True else 0, min(prevl, 2)), sym, 200, nb - 1 if sym[0] != 'e' else nb))
        ev.append(('sign', (), 0, 2, 1))
        i += (run if isinstance(run, int) else 0) + 1
        prevl = abs(lvl) if isinstance(lvl, int) else 2
        last_f = last
        if last_f or i > maxi: return
