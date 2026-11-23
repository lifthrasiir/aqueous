"""Checks the cases against `aqr`, as far as its command line tells.

    python check.py [CASE.edn...] [--aqr "cargo aqr"] [--work DIR]

Each case is assembled; `aqr -K` must succeed or fail as `interpreted` says (by its exit code),
and for an archive value, `aqr -t --json` must list the same nodes (names, kinds, sizes, link
targets, devices) and `aqr -d` write the same contents. What the command line doesn't tell is not
checked: extras, forks, link groups, the order of children, and the contents of files whose names
a file system can't hold or aqr doesn't extract (names it may take as another's). A case given a base is skipped: `aqr` takes no base.
"""

import argparse
import glob
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile

import assemble
from edn import Map, Tag

# aqr's exit codes (crates/aqr/src/exit.rs)
EXIT = {0: "value", 3: "invalid", 4: "decryption", 5: "resource", 6: "unsupported"}


def name_str(n):
    """A name as aqr lists it, and whether a file system holds it as that."""
    if isinstance(n, str):
        return n, True
    if isinstance(n, Tag) and n.tag == 273:
        return n.value.decode("utf-8", "surrogatepass"), False
    if isinstance(n, bytes):
        return n.decode("utf-8", "replace"), False
    raise ValueError(f"not a name: {n!r}")


def flatten(node, path, plain, out):
    """(path, plain, description) of every node under `node`."""
    if "children" in node:
        if path:
            out.append((path, plain, {"type": "dir"}))
        for n, child in node["children"].items:
            s, p = name_str(n)
            flatten(child, f"{path}/{s}" if path else s, plain and p, out)
    elif "content" in node:
        out.append((path, plain, {"type": "file", "size": len(node["content"]), "content": node["content"]}))
    elif "target" in node:
        out.append((path, plain, {"type": "link", "target": name_str(node["target"])[0]}))
    else:
        kind = node["kind"]
        d = {"fifo": {"type": "fifo"}, "char-device": {"type": "chr"}, "block-device": {"type": "blk"}}.get(kind)
        if d is None:
            d = {"type": "special", "kind": kind}
        else:
            dev = node.get("device")
            if dev is not None:
                d = dict(d, major=dev[0], minor=dev[1])
        out.append((path, plain, d))


def listed_path(entry):
    # aqr lists a path not of UTF-8 with its bytes in hex as well.
    if "path_hex" in entry:
        return bytes.fromhex(entry["path_hex"]).decode("utf-8", "surrogatepass")
    return entry["path"]


def listed(entry):
    d = {"type": entry["type"]}
    for k in ("size", "target", "major", "minor", "kind"):
        if k in entry:
            d[k] = entry[k]
    return d


def check(path, aqr, work):
    case, data = assemble.assemble(path)
    name = os.path.splitext(os.path.basename(path))[0]
    if "base" in case:
        return "skip", "given a base"
    f = os.path.join(work, name + ".aqr")
    with open(f, "wb") as o:
        o.write(data)
    r = subprocess.run(aqr + ["-K", f], capture_output=True)
    got = EXIT.get(r.returncode, f"exit {r.returncode}")
    want = case["interpreted"]
    if isinstance(want, str):
        if got != want:
            return "fail", f"{got}, not {want}: {r.stderr.decode(errors='replace').strip()}"
        return "ok", got
    if got != "value":
        return "fail", f"{got}: {r.stderr.decode(errors='replace').strip()}"

    r = subprocess.run(aqr + ["-t", "--json", f], capture_output=True)
    if r.returncode:
        return "fail", f"-t --json: {r.stderr.decode(errors='replace').strip()}"
    entries = json.loads(r.stdout.splitlines()[0])["entries"]
    root = want["root"]
    notes = []

    if "content" in root:
        if len(entries) != 1 or entries[0]["path"] is not None:
            return "fail", f"not a single file: {entries}"
        expect = [(None, True, {"type": "file", "size": len(root["content"]), "content": root["content"]})]
    else:
        expect = []
        flatten(root, "", True, expect)
    have = sorted((listed_path(e), json.dumps(listed(e), sort_keys=True)) for e in entries)
    exp = sorted(
        (p, json.dumps({k: v for k, v in d.items() if k != "content"}, sort_keys=True)) for p, _, d in expect
    )
    if have != exp:
        return "fail", f"listed {have}, expected {exp}"

    out = os.path.join(work, name + ".out")
    if os.path.isdir(out):
        shutil.rmtree(out)
    elif os.path.exists(out):
        os.remove(out)
    r = subprocess.run(aqr + ["-d", f, "-o", out], capture_output=True)
    if r.returncode:
        notes.append("-d failed: " + r.stderr.decode(errors="replace").strip().splitlines()[-1])
    else:
        for p, plain, d in expect:
            if d["type"] != "file":
                continue
            if not plain:
                notes.append(f"{p!r} not read back")
                continue
            fp = out if p is None else os.path.join(out, *p.split("/"))
            try:
                with open(fp, "rb") as i:
                    c = i.read()
            except FileNotFoundError:
                # aqr leaves out names the file system may take as another's (extraction's rules)
                notes.append(f"{p!r} not extracted")
                continue
            if c != d["content"]:
                return "fail", f"{p}: content {c!r}, expected {d['content']!r}"
    return "ok", "; ".join(notes) or "value"


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
        except assemble.CaseError as e:
            status, msg = "error", str(e)
        failed += status in ("fail", "error")
        print(f"{status:5} {os.path.basename(c)}: {msg}")
    if not a.work:
        shutil.rmtree(work, ignore_errors=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
