#!/usr/bin/env python3
"""Self-time profile from a samply recording, symbolicated with atos (inlined frames shown).

Usage: profile.py PROFILE.json.gz BINARY [TOP]
Records with: samply record --save-only -o PROFILE.json.gz -- BINARY ARGS...
"""
import collections, gzip, json, os, subprocess, sys

prof = json.load(gzip.open(sys.argv[1]))
binary = sys.argv[2]
top = int(sys.argv[3]) if len(sys.argv) > 3 else 40
name = os.path.basename(binary)
lib_idx = [i for i, l in enumerate(prof['libs']) if l['name'] == name]
counts = collections.Counter()
total = 0
for t in prof['threads']:
    ft, st, fun, res = t['frameTable'], t['stackTable'], t['funcTable'], t['resourceTable']
    for s in t['samples']['stack']:
        if s is None:
            continue
        total += 1
        f = st['frame'][s]
        func = ft['func'][f]
        r = fun['resource'][func]
        lib = res['lib'][r] if r is not None and r >= 0 else None
        if lib in lib_idx:
            counts[ft['address'][f]] += 1
        else:
            counts[('lib', prof['libs'][lib]['name'] if lib is not None else '?')] += 1
addrs = [a for a in counts if not isinstance(a, tuple)]
sym = {}
if addrs:
    out = subprocess.run(['atos', '-o', binary, '-l', '0x100000000', '-i'] + [hex(0x100000000 + a) for a in addrs],
                         capture_output=True, text=True).stdout
    # atos -i prints one block per address, blocks separated by blank lines.
    blocks = out.strip('\n').split('\n\n')
    for a, b in zip(addrs, blocks):
        lines = [l for l in b.split('\n') if l]
        sym[a] = lines
by_inner = collections.Counter()
incl = collections.Counter()
by_line = collections.Counter()
for a, n in counts.items():
    if isinstance(a, tuple):
        by_inner[a[1]] += n
        continue
    lines = sym.get(a, ['?'])
    for fname in set(l.split(' (in ')[0][:110] for l in lines):
        incl[fname] += n
    inner = lines[0]
    fn = inner.split(' (in ')[0]
    loc = inner[inner.rfind('(') + 1:-1] if inner.endswith(')') else ''
    by_inner[fn[:110]] += n
    by_line[(fn[:60], loc)] += n
print(f'{total} samples')
for k, n in by_inner.most_common(top):
    print(f'{100.0 * n / total:6.2f}%  {k}')
print('--- inclusive (own binary, through inlining)')
for k, n in incl.most_common(top):
    print(f'{100.0 * n / total:6.2f}%  {k}')
print('--- by source line')
for (fn, loc), n in by_line.most_common(top):
    print(f'{100.0 * n / total:6.2f}%  {loc:28} {fn}')
