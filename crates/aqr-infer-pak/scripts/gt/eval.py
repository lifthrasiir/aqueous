"""Measures a sample three ways: no paks, inferred, given its ground truth.

    python eval.py AQR NAME GT_LAYOUT PATH...

GT_LAYOUT: lines `relative/path<TAB>offset<TAB>size` (the paths as they end in the archive).
PATH a directory: aqr plans paks only for archives of several files. Works in `work/` of the
current directory and appends a line of JSON to its results.jsonl. Needs a build of aqr not of
the dist profile (AQR_PAK_LAYOUT, AQR_PAK_DUMP).
"""
import json, os, subprocess, sys, time

aqr, name, gt = sys.argv[1], sys.argv[2], sys.argv[3]
paths = sys.argv[4:]
here = os.getcwd()
work = os.path.join(here, 'work')
os.makedirs(work, exist_ok=True)


def run(mode, env_extra):
    out = os.path.join(work, f'{name}.{mode}.aqr')
    if os.path.exists(out):
        os.remove(out)
    env = dict(os.environ, **env_extra)
    t = time.time()
    r = subprocess.run([aqr, '-c', '-q', '-o', out, *paths], env=env, capture_output=True, text=True, errors='replace')
    dt = time.time() - t
    if r.returncode:
        raise SystemExit(f'{mode}: {r.stderr[-2000:]}')
    size = os.path.getsize(out)
    ok = None
    if mode != 'none':
        ok = subprocess.run([aqr, '-t', '-q', out], capture_output=True).returncode == 0
    os.remove(out)
    return {'s': round(dt, 1), 'bytes': size, 'test': ok}


def norm(p):
    return p.replace('\\', '/').lower()


def load(p):
    m = {}
    if not os.path.exists(p):
        return m
    for l in open(p, encoding='utf-8'):
        if not l.strip():
            continue
        f, o, s = l.rstrip('\n').split('\t')
        m.setdefault(norm(f), set()).add((int(o), int(s)))
    return m


dump = os.path.join(work, f'{name}.dump.txt')
res = {'name': name, 'input': sum(os.path.getsize(os.path.join(dp, f)) for p in paths for dp, _, fs in (os.walk(p) if os.path.isdir(p) else [(os.path.dirname(p), None, [os.path.basename(p)])]) for f in fs)}
res['none'] = run('none', {'AQR_PAKS': '0'})
res['infer'] = run('infer', {'AQR_PAK_DUMP': dump})
res['gt'] = run('gt', {'AQR_PAK_LAYOUT': gt})
g, i = load(gt), load(dump)
gm = sum(len(v) for v in g.values())
hit = 0
im = 0
for f, ms in i.items():
    key = next((k for k in g if f.endswith(k)), None)
    im += len(ms)
    if key:
        hit += len(ms & g[key])
res['members'] = {'gt': gm, 'inferred': im, 'right': hit, 'recall': round(hit / max(gm, 1), 3), 'precision': round(hit / max(im, 1), 3)}
print(json.dumps(res))
with open(os.path.join(here, 'results.jsonl'), 'a', encoding='utf-8') as f:
    f.write(json.dumps(res) + '\n')
