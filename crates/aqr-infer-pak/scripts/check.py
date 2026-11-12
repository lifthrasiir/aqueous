#!/usr/bin/env python3
"""Runs aqr-infer-pak-probe on samples and checks what it finds against what is known of them.

What is known is in an expectations file, one sample a line (`#` starts a comment):

    PATH  EXPECT...

PATH is the sample (relative to the expectations file's directory, or absolute). Each EXPECT is
a directory that must be found, as `TARGET=MEMBERS`, where TARGET is `file` (the sample itself),
`decoded` (the sample decoded in place) or `.EXT` (the sibling of the sample's stem with that
extension, `decoded.EXT` decoded); or `none`: nothing may be found. A directory found that is
not expected is wrong. MEMBERS is the count the format's header gives; a count within TOL of it
is found (a table's edge may lose a row).

Usage (from the root):
    .venv/Scripts/python -I crates/aqr-infer-pak/scripts/check.py EXPECT_FILE [--probe PROBE]
"""

import argparse
import os
import re
import subprocess

TOL = 0.01


def parse(path):
    base = os.path.dirname(os.path.abspath(path))
    samples = []
    with open(path, encoding="utf-8") as f:
        for no, line in enumerate(f, 1):
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            # the path may hold spaces: the expectations are the words after it that look so
            words = line.split()
            i = len(words)
            while i > 1 and (words[i - 1] == "none" or "=" in words[i - 1]):
                i -= 1
            sample = " ".join(words[:i])
            exps = words[i:]
            if not exps:
                raise SystemExit(f"{path}:{no}: no expectation")
            sample = sample if os.path.isabs(sample) else os.path.join(base, sample)
            want = {}
            for e in exps:
                if e == "none":
                    continue
                t, n = e.split("=")
                want[t] = int(n)
            samples.append((sample, want))
    return samples


def target_of(sample, role):
    """The target name the probe prints for a role of a sample."""
    name = os.path.basename(sample)
    decoded = role.startswith("decoded")
    rest = role[len("decoded"):] if decoded else role
    if rest in ("", "file"):
        t = name
    elif rest.startswith("."):
        t = os.path.splitext(name)[0] + rest
    else:
        raise SystemExit(f"unknown target {role!r}")
    return t + "~decoded" if decoded else t


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("expect", help="the expectations file")
    ap.add_argument("--probe", default="target/release/examples/aqr-infer-pak-probe")
    a = ap.parse_args()
    probe = os.path.abspath(a.probe)
    if os.name == "nt" and not probe.endswith(".exe"):
        probe += ".exe"
    samples = parse(a.expect)
    missing = [s for s, _ in samples if not os.path.exists(s)]
    if missing:
        raise SystemExit(f"missing samples: {missing}")
    out = subprocess.run([probe, *(s for s, _ in samples)], capture_output=True, text=True).stdout
    found, times, cur = {}, {}, None
    for line in out.splitlines():
        if not line.startswith(" "):
            cur = line.split(": ")[0]
            found[cur] = []
            continue
        m = re.match(r"\s+-> (\S+) \(\d+ B\) by .*?: (\d+) members", line)
        if m:
            found[cur].append((m.group(1), int(m.group(2))))
        m = re.match(r"\s+\(([\d.]+) ms", line)
        if m:
            times[cur] = float(m.group(1))
    ok = bad = 0
    for sample, want in samples:
        got = found.get(sample, [])
        exp = {target_of(sample, r): n for r, n in want.items()}
        name = os.path.basename(sample)
        for t, n in exp.items():
            if any(tt == t and abs(m - n) <= max(1, TOL * n) for tt, m in got):
                ok += 1
            else:
                bad += 1
                print(f"MISS  {name}: {t} {n} (found {[(tt, m) for tt, m in got if tt == t]})")
        for t, m in got:
            if t not in exp or abs(m - exp[t]) > max(1, TOL * exp[t]):
                bad += 1
                print(f"EXTRA {name}: {t} {m}")
        print(f"      {name}: {times.get(sample, 0):.0f} ms")
    print(f"{ok} found, {bad} wrong; {sum(times.values()) / 1e3:.1f} s in all")


if __name__ == "__main__":
    main()
