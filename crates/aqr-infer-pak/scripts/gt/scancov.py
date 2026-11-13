"""How much of a sample's ground truth the scan finds by itself (AQR_PAKS=0, AQR_SCAN_DUMP).

    python scancov.py AQR GT_LAYOUT DIR
"""
import collections, os, subprocess, sys

aqr, gt, root = sys.argv[1:4]
files = {f.lower(): os.path.getsize(os.path.join(root, f)) for f in os.listdir(root)}
os.makedirs('work', exist_ok=True)
out = os.path.join('work', 'scancov.aqr')
env = dict(os.environ, AQR_PAKS='0', AQR_SCAN_DUMP='1')
r = subprocess.run([aqr, '-c', '-q', '-o', out, root], env=env, capture_output=True, text=True, errors='replace')
os.remove(out)
by_len = collections.defaultdict(list)  # data length -> [(depth, p, n, format)]
for l in r.stderr.splitlines():
    if l.startswith('SCAN\t'):
        _, ln, depth, p, n, fmt = l.split('\t')
        by_len[int(ln)].append((int(depth), int(p), int(n), fmt))
gtm = collections.defaultdict(list)
for l in open(gt, encoding='utf-8'):
    f, o, s = l.rstrip('\n').split('\t')
    gtm[f.lower()].append((int(o), int(s)))
tot = dict(n=0, bytes=0, exact=0, exact_bytes=0, start=0, covered_bytes=0)
kinds = collections.Counter()
for f, ms in gtm.items():
    ln = files[f.lower()]
    found = by_len.get(ln, [])
    top = min((x[0] for x in found), default=None)
    found = [x for x in found if x[0] == top]
    ex = {(p, n): fmt for _, p, n, fmt in found}
    starts = {p for _, p, _, _ in found}
    iv = sorted((p, p + n) for _, p, n, _ in found)
    for o, s in ms:
        tot['n'] += 1
        tot['bytes'] += s
        if (o, s) in ex:
            tot['exact'] += 1
            tot['exact_bytes'] += s
            kinds[ex[(o, s)]] += 1
        if o in starts:
            tot['start'] += 1
        tot['covered_bytes'] += sum(max(0, min(o + s, b) - max(o, a)) for a, b in iv if a < o + s and b > o)
print(tot)
print('exact by format', kinds.most_common(8))
print('members %.1f%% exact, bytes %.1f%% exact, %.1f%% covered' % (
    100 * tot['exact'] / max(tot['n'], 1), 100 * tot['exact_bytes'] / max(tot['bytes'], 1),
    100 * tot['covered_bytes'] / max(tot['bytes'], 1)))
