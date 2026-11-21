#!/usr/bin/env python3
"""The workspace's packages whose tests a change can affect, for CI.

    scripts/affected-crates.py BASE [HEAD]
    scripts/affected-crates.py --readers PATH...

prints the arguments of `cargo test` (or `cargo nextest run`) that select the packages to test,
one per line: `--package=NAME` for each, or `--workspace` (with `--exclude` of the packages never
tested) where the change reaches everything; nothing where no test is affected. The changes are
those from the merge base of BASE and HEAD to HEAD. With `--readers`, the packages whose sources
name each PATH (repo-relative) instead, a line each: `PATH: NAME...`.

A package is affected when a file under its directory changed, when a package it depends on
(normally, for its build or for its tests) is, but not where only the other's tests, benchmarks,
examples or binaries changed, or when its sources name a changed file out of its directory (the
tests reading another crate's `tests/data`, aqr-archive's reading `spec/index.bs`). Of Cargo.lock,
the packages whose entries changed are affected; of the root's Cargo.toml, the members taking a
dependency of [workspace.dependencies] that changed (everything where anything else changed).

A file outside every package affects everything unless it is of the paths below that no build
reads (documents, the site, the spec's figures) or named by some package's sources.

Only the standard library: CI runs it with the runner's Python.
"""

import json
import os
import re
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Outside the packages, read by no build or test (but where a package's sources name them).
INERT = re.compile(
    r"""^(
        docs/ | site/ | spec/ | third_party/ | \.github/(?!workflows/test\.yml$) |
        scripts/(?!affected-crates\.py$) |
        [^/]+\.md$ | LICENSE | \.gitignore$ | \.gitattributes$ | rustfmt\.toml$ | flake\.(nix|lock)$
    )""",
    re.VERBOSE,
)

# The packages whose tests are never built: aqr-dll's Rust dylibs (std twice in a test harness).
UNTESTED = re.compile(r"^aqr-dll(-|$)")
WORKSPACE = ["--workspace", "--exclude=aqr-dll*"]

# Inside a package, read by no build or test of it.
INERT_IN_PACKAGE = re.compile(r"(^|/)(README|FORMAT|CHANGELOG)\.md$")

# Inside a package, read by its own targets alone (no dependent package builds them): its tests,
# benchmarks, examples and binaries. Where another package reads one (a `tests/data` file, a
# `tests/common` module), its sources name it.
LOCAL = re.compile(r"^(tests|benches|examples|src/bin)/")


def git(*args):
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def show(rev, path):
    r = subprocess.run(["git", "show", f"{rev}:{path}"], check=False, cwd=ROOT, capture_output=True)
    return tomllib.loads(r.stdout.decode()) if r.returncode == 0 else None


def lock_changes(base, head, by_id):
    """The ids of the packages whose entries in Cargo.lock differ (new or changed) at `head`."""
    old, new = show(git("merge-base", base, head).strip(), "Cargo.lock"), show(head, "Cargo.lock")
    if old is None or new is None:
        return set(by_id)

    def entries(lock):
        return {(p["name"], p["version"]): p for p in lock.get("package", [])}

    old = entries(old)
    changed = {k for k, p in entries(new).items() if old.get(k) != p}
    return {i for i, p in by_id.items() if (p["name"], p["version"]) in changed}


def workspace_dependency_changes(base, head):
    """The keys of [workspace.dependencies] changed in the root's Cargo.toml, or None where
    anything else of it changed."""
    old, new = show(git("merge-base", base, head).strip(), "Cargo.toml"), show(head, "Cargo.toml")
    if old is None or new is None:
        return None
    od = old.get("workspace", {}).pop("dependencies", {})
    nd = new.get("workspace", {}).pop("dependencies", {})
    if old != new:
        return None
    return {k for k in od.keys() | nd.keys() if od.get(k) != nd.get(k)}


def main():
    readers_of = sys.argv[2:] if sys.argv[1] == "--readers" else None
    if readers_of is None:
        base = sys.argv[1]
        head = sys.argv[2] if len(sys.argv) > 2 else "HEAD"
        changed = [p for p in git("diff", "--name-only", "--no-renames", f"{base}...{head}").splitlines() if p]

    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    members = set(meta["workspace_members"])
    by_id = {p["id"]: p for p in meta["packages"]}
    dirs = {}  # repo-relative directory -> package id
    for i in members:
        d = os.path.relpath(os.path.dirname(by_id[i]["manifest_path"]), ROOT).replace(os.sep, "/")
        dirs[d] = i
    # Reverse dependencies of every package (of every kind: normal, build, dev).
    rdeps = {i: set() for i in by_id}
    for node in meta["resolve"]["nodes"]:
        for dep in node["deps"]:
            rdeps[dep["pkg"]].add(node["id"])

    # Paths out of its directory named by each package's sources
    # (`../../codecs/aqr-jpeg/tests/data/gradient.jpg`, `"/../aqr-modules/wasm/bcj-delta.wasm"`
    # after `CARGO_MANIFEST_DIR`): relative to the source file (`include_bytes!`) or to the package's
    # directory (the tests' working directory, `CARGO_MANIFEST_DIR`), whichever is there; and
    # string literals of paths from the repository's root (`fixture("codecs/aqr-7z/tests/data/copy.7z")`,
    # a helper putting `../../` before it), up to a `{` of a format string (the directory then).
    named = {}  # repo-relative path -> (package id, whether its build takes it)
    pat = re.compile(r"(?:\.\./)+[A-Za-z0-9_][A-Za-z0-9_./-]*")
    rooted = re.compile(r'"((?:codecs|crates|spec)/[A-Za-z0-9_][A-Za-z0-9_./-]*)')
    for d, i in dirs.items():
        top = os.path.join(ROOT, d)
        for sub, subdirs, files in os.walk(top):
            # (not into the build's output, local data, nor the packages inside it)
            subdirs[:] = [
                s
                for s in subdirs
                if s not in ("target", "corpus", "cache", ".git")
                and os.path.relpath(os.path.join(sub, s), ROOT).replace(os.sep, "/") not in dirs
            ]
            for f in files:
                if not f.endswith(".rs"):
                    continue
                with open(os.path.join(sub, f), encoding="utf-8", errors="replace") as fh:
                    text = fh.read()
                local_file = LOCAL.match(os.path.relpath(os.path.join(sub, f), top).replace(os.sep, "/")) is not None
                for m in pat.finditer(text):
                    # (read as the tests run, or by the targets of LOCAL, but for `include_bytes!` and
                    # the like in what others build)
                    line = text[text.rfind("\n", 0, m.start()) + 1 : m.start()]
                    built = re.search(r"\binclude(_bytes|_str)?!", line) is not None and not local_file
                    for start in (sub, top):
                        p = os.path.relpath(os.path.normpath(os.path.join(start, m.group(0))), ROOT)
                        p = p.replace(os.sep, "/").rstrip("/")
                        if p.startswith(("../", d + "/")):
                            continue
                        if os.path.exists(os.path.join(ROOT, p)):
                            named.setdefault(p, set()).add((i, built))
                for m in rooted.finditer(text):
                    line = text[text.rfind("\n", 0, m.start()) + 1 : m.start()]
                    built = re.search(r"\binclude(_bytes|_str)?!", line) is not None and not local_file
                    p = os.path.normpath(m.group(1)).replace(os.sep, "/").rstrip("/")
                    if p.startswith(d + "/") or p == d:
                        continue
                    if os.path.exists(os.path.join(ROOT, p)):
                        named.setdefault(p, set()).add((i, built))

    if readers_of is not None:
        for path in readers_of:
            rs = {r for prefix, rs in named.items() for r, _ in rs if path == prefix or path.startswith(prefix + "/")}
            print(f"{path}: " + " ".join(sorted(by_id[i]["name"] for i in rs)))
        return

    def owner(path):
        best = None
        for d in dirs:
            if path.startswith(d + "/") and (best is None or len(d) > len(best)):
                best = d
        return dirs[best] if best else None

    by_id_dir = {i: d for d, i in dirs.items()}
    seeds = set()
    local = set()  # affecting the package's own tests alone
    for path in changed:
        if path == "Cargo.lock":
            # (a member's entry lists its dependencies alone: what its Cargo.toml says, which
            # affects others where it changed)
            ids = lock_changes(base, head, by_id)
            seeds |= ids - members
            local |= ids & members
            continue
        if path == "Cargo.toml":
            deps = workspace_dependency_changes(base, head)
            if deps is not None:
                seeds |= {i for i in members for d in by_id[i]["dependencies"] if (d["rename"] or d["name"]) in deps}
                continue
        readers = {r for prefix, rs in named.items() for r in rs if path == prefix or path.startswith(prefix + "/")}
        if not INERT_IN_PACKAGE.search(path):
            seeds |= {i for i, built in readers if built}
            local |= {i for i, built in readers if not built}
        pkg = owner(path)
        if pkg is not None:
            rel = path[len(by_id_dir[pkg]) + 1 :]
            if INERT_IN_PACKAGE.search(rel):
                pass
            elif LOCAL.match(rel):
                local.add(pkg)
            else:
                seeds.add(pkg)
        elif not INERT.match(path) and not readers:
            print(f"{path}: outside the packages, affecting all", file=sys.stderr)
            print("\n".join(WORKSPACE))
            return

    affected = set()
    stack = list(seeds)
    while stack:
        n = stack.pop()
        if n not in affected:
            affected.add(n)
            stack.extend(rdeps[n])
    affected |= local
    names = {by_id[i]["name"] for i in affected & members if not UNTESTED.match(by_id[i]["name"])}
    if names >= {by_id[i]["name"] for i in members if not UNTESTED.match(by_id[i]["name"])}:
        print("\n".join(WORKSPACE))
        return
    for n in sorted(names):
        print(f"--package={n}")


if __name__ == "__main__":
    main()
