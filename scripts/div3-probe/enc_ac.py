"""Single-coefficient I frames from ffmpeg's encoder: the code of (position, level) as the encoder writes it."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from enc import enc_frames
from fast import Dec
from ac_probe import _C, fdct
from probe import blocks_yuv

def bits_of(pk): return ''.join(format(x, '08b') for x in pk)
def d012(s, i):
    if s[i] == '0': return 0, i + 1
    return (1 if s[i + 1] == '0' else 2), i + 2

def header(s):
    t = s[:2]; q = int(s[2:7], 2); sl = s[7:12]
    i = 12; rlc, i = d012(s, i); rl, i = d012(s, i); dc = int(s[i]); i += 1
    return dict(type=t, q=q, rlc=rlc, rl=rl, dc=dc, mb=i)

def make_frame(blk, pos_levels, q, amp_of):
    y = np.full((16, 16), 128.0)
    r0, c0 = (blk // 2) * 8, (blk % 2) * 8
    for (u, v), C in pos_levels:
        b = np.outer(_C[u], _C[v])        # orthonormal 2D basis: pixel(x,y)=sum C_uv * _C[u][x] * _C[v][y]
        y[r0:r0 + 8, c0:c0 + 8] += C * b
    return np.clip(np.round(y), 0, 255).astype(np.uint8), np.full((8, 8), 128, np.uint8), np.full((8, 8), 128, np.uint8)

DC = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'dc_raw.json')))
def zero(table): return min((p for p, l in DC[table]['codes'].items() if l == 0), key=len)

def ext_bits(q=5):
    fl = (np.full((16, 16), 128, np.uint8), np.full((8, 8), 128, np.uint8), np.full((8, 8), 128, np.uint8))
    s = bits_of(enc_frames([fl], q=q)[0]); h = header(s)
    z = zero('luma%d' % h['dc']); zc = zero('chroma%d' % h['dc'])
    rest = s[h['mb'] + 1 + 1 + 4 * len(z) + 2 * len(zc):]
    return h, rest[:17], rest

def single_code(blk, pos, C, q=5):
    """The encoder's AC bits (code + sign) for one coefficient (orthonormal amplitude C) in a luma block 0..3."""
    fr = make_frame(blk, [(pos, C)], q, None)
    pk = enc_frames([fr], q=q)[0]; s = bits_of(pk); h = header(s)
    z = zero('luma%d' % h['dc']); zc = zero('chroma%d' % h['dc'])
    i = h['mb']
    return h, s, i

_EXT = {}
def tail_info(q):
    if q not in _EXT: _EXT[q] = ext_bits(q)
    return _EXT[q]

def ac_bits(blk_pos_levels, q=5, blk=3, cbp='00110'):
    """Encode, parse around the AC part; returns (header, ac_bits) or None."""
    fr = make_frame(blk, blk_pos_levels, q, None)
    s = bits_of(enc_frames([fr], q=q)[0]); h = header(s)
    z = zero('luma%d' % h['dc']); zc = zero('chroma%d' % h['dc'])
    i = h['mb']
    if s[i:i + len(cbp)] != cbp: return h, None
    i += len(cbp) + 1 + (blk + 1) * len(z)
    tail = zc * 2 + tail_info(q)[1]
    for pad in range(8):
        e = len(s) - pad - len(tail)
        if e >= i and s[e:e + len(tail)] == tail and set(s[len(s) - pad:]) <= {'0'}:
            return h, s[i:e]
    return h, None
