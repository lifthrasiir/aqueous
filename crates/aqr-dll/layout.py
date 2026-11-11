"""The shared libraries of the modules built as native libraries (build.sh runs it).

Each module's library would hold a copy of everything it uses, the standard library among them
(about 190 KB each). Instead, the crates that more than one module uses go into Rust dylibs
that the modules' libraries link against: the crates that exactly the same modules use, in one
dylib (a group), so that a program carrying some of the modules carries nothing of the groups
that none of them uses. The group that every module uses is the runtime, `aqr-dll-rt`, which
holds the standard library; every other group links against it, and against the groups of its
crates' dependencies.

It writes, from `cargo tree -p aqr-dll` and the modules' sources (crates/aqr-modules/src/bin,
the crates each names):

- shared/NAME/ (Cargo.toml, src/lib.rs): each group's dylib, `aqr-dll-NAME`;
- shared.rs: an `extern crate` of each, which build.rs puts in every module (a crate of a
  group is then taken from it, whether or not the module was seen to use it);
- the dependencies of aqr-dll on them, in Cargo.toml between its markers.

aqr-modules is in none (its runtime part, the module blocks, is not for the libraries), nor
aqr-native (its table names every module's code; each module takes its one entry).
"""

import collections
import glob
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, '..', '..'))
BINS = os.path.join(ROOT, 'crates', 'aqr-modules', 'src', 'bin')
SHARED = os.path.join(HERE, 'shared')
BEGIN, END = '# (begin: layout.py)', '# (end: layout.py)'
LEFT_OUT = {'aqr-modules', 'aqr-dll', 'aqr-native'}


def tree():
    """The packages under aqr-dll (name: (version, path or None)) and their dependencies,
    the groups' crates left out."""
    def run():
        return subprocess.run(
            ['cargo', 'tree', '-p', 'aqr-dll', '-e', 'normal', '--no-dedupe', '--prefix', 'depth', '--format', '{p}'],
            cwd=ROOT, capture_output=True, text=True, encoding='utf-8',
        )

    r = run()
    if r.returncode != 0:
        # (a group's crate missing, say: aqr-dll without them, as they are made again anyway)
        write(TOML, re.sub(re.escape(BEGIN) + r'.*?' + re.escape(END) + '\n', '', read(TOML), flags=re.S))
        r = run()
        if r.returncode != 0:
            sys.exit(f'layout.py: cargo tree failed:\n{r.stderr}')
    out = r.stdout
    packages, deps, stack = {}, collections.defaultdict(set), []
    for line in out.splitlines():
        m = re.match(r'(\d+)(\S+) v(\S+)(?: \((.*?)\))?', line)
        if not m:
            continue
        depth, name, version, path = int(m.group(1)), m.group(2), m.group(3), m.group(4)
        del stack[depth:]
        # (a group's crate, and what is under it, as if not there: they are what is laid out)
        if name.startswith('aqr-dll-') or any(s is None for s in stack):
            stack.append(None)
            continue
        if path is not None and not os.path.isdir(path):
            path = None  # (`(*)`, `(proc-macro)` and the like)
        packages.setdefault(name, (version, path))
        if path is not None:
            packages[name] = (version, path)
        if stack:
            deps[stack[-1]].add(name)
        stack.append(name)
    return packages, deps


TOML = os.path.join(HERE, 'Cargo.toml')


def read(path):
    return open(path, encoding='utf-8', newline='').read()


def write(path, text):
    """Writes `text` to `path` unless it is there already (something may hold the files: Dropbox,
    say)."""
    try:
        if read(path) == text:
            return
    except FileNotFoundError:
        pass
    open(path, 'w', encoding='utf-8', newline='').write(text)


def ident(name):
    return name.replace('-', '_')


def closure(p, deps, seen):
    if p not in seen:
        seen.add(p)
        for d in deps[p]:
            closure(d, deps, seen)


def main():
    packages, deps = tree()
    by_crate = {p.replace('-', '_'): p for p in packages}
    modules = sorted(os.path.basename(f)[:-3] for f in glob.glob(os.path.join(BINS, '*.rs')))
    users = collections.defaultdict(set)
    for m in modules:
        src = open(os.path.join(BINS, m + '.rs'), encoding='utf-8').read()
        named = {by_crate[c] for c in re.findall(r'\b([a-z][a-z0-9_]*)::', src) if c in by_crate}
        seen = set()
        for p in named:
            closure(p, deps, seen)
        for p in seen:
            users[p].add(m)
    groups = collections.defaultdict(list)
    for p, ms in users.items():
        if p not in LEFT_OUT and len(ms) > 1:
            groups[frozenset(ms)].append(p)
    everyone = frozenset(modules)
    if everyone not in groups:
        sys.exit('layout.py: no crate that every module uses (the runtime)')

    # each group's name: `rt` for the runtime; else its first aqr crate's, or that of the crate
    # of it with the most of the others under it
    def name_of(ps):
        aqr = sorted(p for p in ps if p.startswith('aqr-'))
        if aqr:
            return aqr[0][len('aqr-'):]

        def under(p):
            seen = set()
            closure(p, deps, seen)
            return len(seen & set(ps))

        return sorted(ps, key=lambda p: (-under(p), p))[0]

    named = {}
    for ms, ps in groups.items():
        named[ms] = 'rt' if ms == everyone else name_of(ps)
    group_of = {p: named[ms] for ms, ps in groups.items() for p in ps}

    # (the groups gone removed; the others' files written where they change)
    for old in glob.glob(os.path.join(SHARED, '*')):
        if os.path.basename(old) not in named.values():
            shutil.rmtree(old)
    order = sorted(groups.items(), key=lambda kv: (-len(kv[0]), named[kv[0]]))
    for ms, ps in order:
        g = named[ms]
        # the groups of what its crates depend on, the runtime always
        below = {'rt'} if g != 'rt' else set()
        for p in ps:
            seen = set()
            closure(p, deps, seen)
            below |= {group_of[d] for d in seen if d in group_of and group_of[d] != g}
        d = os.path.join(SHARED, g)
        os.makedirs(os.path.join(d, 'src'), exist_ok=True)
        lines = [
            '[package]',
            f'name = "aqr-dll-{g}"',
            'version = "0.1.0"',
            'edition = "2024"',
            f'description = "Made by layout.py: the crates that the same {len(ms)} modules use, '
            'as a dylib that their native libraries link against"',
            'publish = false',
            '',
            '[lib]',
            'crate-type = ["dylib"]',
            '',
            '[dependencies]',
        ]
        for b in sorted(below):
            lines.append(f'aqr-dll-{b} = {{ path = "../{b}" }}')
        for p in sorted(ps):
            version, path = packages[p]
            if path is not None:
                rel = os.path.relpath(path, d).replace(os.sep, '/')
                lines.append(f'{p} = {{ path = "{rel}", default-features = false }}')
            else:
                lines.append(f'{p} = {{ version = "={version}", default-features = false }}')
        write(os.path.join(d, 'Cargo.toml'), '\n'.join(lines) + '\n')
        src = ['//! Made by layout.py: ' + ', '.join(sorted(ps)) + '.', '']
        src += [f'extern crate aqr_dll_{ident(b)};' for b in sorted(below)]
        src += [f'pub extern crate {p.replace("-", "_")};' for p in sorted(ps)]
        write(os.path.join(d, 'src', 'lib.rs'), '\n'.join(src) + '\n')

    names = sorted(named.values())
    rs = ['// Made by layout.py: the shared libraries, linked against by every module.']
    rs += [f'extern crate aqr_dll_{ident(g)};' for g in names]
    write(os.path.join(HERE, 'shared.rs'), '\n'.join(rs) + '\n')

    toml = read(TOML)
    block = '\n'.join([BEGIN] + [f'aqr-dll-{g} = {{ path = "shared/{g}" }}' for g in names] + [END])
    if BEGIN in toml:
        toml = re.sub(re.escape(BEGIN) + r'.*?' + re.escape(END), lambda _: block, toml, flags=re.S)
    else:
        toml = toml.replace('[dependencies]\n', '[dependencies]\n' + block + '\n', 1)
    write(TOML, toml)
    for ms, ps in order:
        print(f'aqr-dll-{named[ms]}: {len(ms)} modules, {" ".join(sorted(ps))}')


if __name__ == '__main__':
    main()
