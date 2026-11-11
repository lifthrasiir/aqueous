"""Extract the numeric tables of ATSC A/52 (AC-3) from the text of the standard (pdftotext -layout) into ac3_tables.json.
The standard is public (https://www.atsc.org/atsc-documents/a522012-digital-audio-compression-standard/); the tables are
numbers it prescribes, taken from its text and not from any implementation. Usage: extract_tables.py A52.txt"""
import re, sys, json, os
t = open(sys.argv[1], encoding='utf-8', errors='replace').read().split('\n')
def section(title, nth=None):
    idx = [i for i, l in enumerate(t) if title in l]
    return idx
def hexes(lines): return [int(x, 16) for l in lines for x in re.findall(r'0x[0-9a-fA-F]+', l)]
def body(start_title, end_title, which=-1):
    s = section(start_title)[which]; e = next(i for i in range(s + 1, len(t)) if end_title in t[i])
    return t[s + 1:e]
T = {}
# frame sizes: frmsizecod 0..37, by fscod 0 (48 kHz), 1 (44.1 kHz), 2 (32 kHz)
rows = {}
s = section('Table 5.18 Frame Size Code Table (1 word = 16 bits)')[-1]
for l in t[s:s + 120]:
    m = re.match(r"\s*`([01]{6})'\s*\(\d+\)\s+(\d+) kbps\s+(\d+)\s+(\d+)\s+(\d+)", l)
    if m: rows[int(m.group(1), 2)] = [int(m.group(5)), int(m.group(4)), int(m.group(3))]
T['frame_words'] = [rows[i] for i in range(38)]
T['bitrate'] = {}
for l in t[s:s + 120]:
    m = re.match(r"\s*`([01]{6})'\s*\(\d+\)\s+(\d+) kbps", l)
    if m: T['bitrate'][int(m.group(1), 2)] = int(m.group(2))
T['fastgain'] = hexes(body('Table 7.11 Fast Gain Table', 'Table 7.12'))
bnd = []
for l in body('Table 7.12 Banding Structure Tables', 'Table 7.13'):
    for m in re.finditer(r'(\d+)\s+(\d+)\s+(\d+)', l): bnd.append(tuple(map(int, m.groups())))
bnd = sorted(set(bnd))
T['bndtab'] = [b[1] for b in bnd]; T['bndsz'] = [b[2] for b in bnd]
assert len(bnd) == 50 and [b[0] for b in bnd] == list(range(50)), len(bnd)
T['latab'] = hexes(body('Table 7.14 Log-Addition Table', 'Table 7.15'))
assert len(T['latab']) == 256, len(T['latab'])
hth = {}
for l in body('Table 7.15 Hearing Threshold Table', 'Table 7.16'):
    for m in re.finditer(r'\b(\d+)\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)', l): hth[int(m.group(1))] = [int(m.group(i), 16) for i in (2, 3, 4)]
assert sorted(hth) == list(range(50))
T['hth'] = [hth[i] for i in range(50)]
bap = {}
for l in body('Table 7.16 Bit Allocation Pointer Table', 'Table 7.17'):
    for m in re.finditer(r'\b(\d+)\s+(\d+)\b', l): bap[int(m.group(1))] = int(m.group(2))
bap = {a: b for a, b in bap.items() if 0 <= a < 64}
T['baptab'] = [bap[i] for i in range(64)]
for name, title, nxt in (('slowdec', 'Table 7.6 Slow Decay', 'Table 7.7'), ('fastdec', 'Table 7.7 Fast Decay', 'Table 7.8'), ('slowgain', 'Table 7.8 Slow Gain', 'Table 7.9'), ('dbpbtab', 'Table 7.9 dB/Bit', 'Table 7.10')):
    T[name] = hexes(body(title, nxt))
T['floortab'] = [v - 0x10000 if v >= 0x8000 else v for v in hexes(body('Table 7.10 Floor Table', 'Table 7.11'))]   # (0xf800 is -2048: a signed 16-bit value)
for k, n in (('fastgain', 8), ('slowdec', 4), ('fastdec', 4), ('slowgain', 4), ('dbpbtab', 4), ('floortab', 8)): assert len(T[k]) == n, (k, T[k])
json.dump(T, open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ac3_tables.json'), 'w'))
print({k: (len(v) if hasattr(v, '__len__') else v) for k, v in T.items()})
