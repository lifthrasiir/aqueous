"""Checks the cases against `aqr -Z interpret`, which prints the archive value of an archive.

    python check.py [CASE.edn...] [--aqr "cargo aqr"] [--work DIR]

Each case is assembled and interpreted: it must fail as `interpreted` says (by the exit code), or
give the archive value it gives, compared in full (extras, forks and the order of every map; link
groups up to a renaming). An overlay archive given a base is skipped: `aqr` takes no base (a
standalone archive's base changes nothing).
"""

import argparse
import difflib
import glob
import os
import shlex
import shutil
import subprocess
import sys
import tempfile

import assemble
import edn
from edn import Map, Tag, freeze

# aqr's exit codes (crates/aqr/src/exit.rs)
EXIT = {0: "value", 3: "invalid", 4: "decryption", 5: "resource", 6: "unsupported"}


def lines(v):
    """An archive value as lines of text, one for each node by its path, in their order: link
    groups renamed in the order they first appear, defaults filled in."""
    out = []
    groups = {}

    def show(x):
        return repr(x) if len(repr(x)) <= 200 else repr(x)[:200] + f"... ({len(x)} in all)"

    def walk(node, path):
        d = []
        if "children" in node:
            names = [n for n, _ in node["children"].items]
            d.append(f"directory {names!r}")
        elif "content" in node:
            d.append(f"file {show(node['content'])}")
            g = node.get("link_group")
            if g is not None:
                d.append(f"link group #{groups.setdefault(freeze(g), len(groups))}")
        elif "target" in node:
            d.append(f"link to {node['target']!r}")
        elif isinstance(node.get("kind"), str):
            d.append(f"{node['kind']} {node.get('device')!r}")
        else:
            d.append(f"unknown node of kind {node.get('kind')!r}, data {show(node.get('data'))}")
        d.append(f"extra {node.get('extra')!r}")
        forks = node.get("forks", Map())
        if forks:
            d.append(f"forks {[(k, show(x)) for k, x in forks.items]!r}")
        out.append(f"{path or '(root)'}: " + "; ".join(d))
        for n, child in node.get("children", Map()).items:
            walk(child, f"{path}/{n!r}")

    walk(v["root"], "")
    out.append(f"archive extra {v.get('extra', Map())!r}")
    return out


def external(v, here):
    """`v` with each `#6.80003(path)` replaced by the contents of the file at `path`."""
    if isinstance(v, Tag) and v.tag == assemble.EXT and isinstance(v.value, str):
        with open(os.path.join(here, v.value), "rb") as f:
            return f.read()
    if isinstance(v, Tag):
        return Tag(v.tag, external(v.value, here))
    if isinstance(v, list):
        return [external(x, here) for x in v]
    if isinstance(v, Map):
        return Map((external(k, here), external(x, here)) for k, x in v.items)
    return v


def check(path, aqr, work):
    case, data = assemble.assemble(path)
    name = os.path.splitext(os.path.basename(path))[0]
    a = case["archive"]
    if "base" in case and isinstance(a, Map) and a.get("header_magic") == "OVERLAY_HEADER_MAGIC":
        return "skip", "an overlay archive given a base"
    f = os.path.join(work, name + ".aqr")
    with open(f, "wb") as o:
        o.write(data)
    r = subprocess.run(aqr + ["-Z", "interpret", f], capture_output=True)
    got = EXIT.get(r.returncode, f"exit {r.returncode}")
    err = r.stderr.decode(errors="replace").strip()
    want = case["interpreted"]
    if isinstance(want, str):
        if got != want:
            return "fail", f"{got}, not {want}: {err}"
        return "ok", got
    if got != "value":
        return "fail", f"{got}: {err}"
    want = external(want, os.path.dirname(os.path.abspath(path)))
    have, expect = lines(edn.loads(r.stdout.decode("utf-8", "surrogateescape"))), lines(want)
    if have != expect:
        diff = difflib.unified_diff(expect, have, "expected", "interpreted", lineterm="", n=0)
        sep = "\n      "
        return "fail", sep + sep.join(list(diff)[2:12])
    return "ok", "value"


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("cases", nargs="*")
    p.add_argument("--aqr", default="cargo aqr")
    p.add_argument("--work")
    a = p.parse_args(argv)
    sys.stdout.reconfigure(errors="backslashreplace")
    here = os.path.dirname(os.path.abspath(__file__))
    cases = a.cases or sorted(glob.glob(os.path.join(here, "..", "cases", "*.edn")))
    work = a.work or tempfile.mkdtemp(prefix="aqr-conformance-")
    os.makedirs(work, exist_ok=True)
    aqr = shlex.split(a.aqr)
    failed = 0
    for c in cases:
        try:
            status, msg = check(c, aqr, work)
        except (assemble.CaseError, edn.EdnError) as e:
            status, msg = "error", str(e)
        failed += status in ("fail", "error")
        print(f"{status:5} {os.path.basename(c)}: {msg}")
    if not a.work:
        shutil.rmtree(work, ignore_errors=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
