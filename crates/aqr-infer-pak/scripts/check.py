#!/usr/bin/env python3
"""Runs aqr-infer-pak-probe on the test set and checks what it finds against the counts the
headers give (docs/pak-inference-design.md, section 5). The game files are the user's own
installations; the paths are fixed.

Usage (from the root): .venv/Scripts/python -I crates/aqr-infer-pak/scripts/check.py [PROBE]
"""

import os
import re
import subprocess
import sys

CREW = "F:/SteamLibrary/steamapps/common/The Crew/data_win32/"
NMS = "F:/SteamLibrary/steamapps/common/No Man's Sky/GAMEDATA/PCBANKS/"
CORPUS = "F:/Data/aqr-corpus/"

# file: {target: members (as the header has it)}; an empty dict: nothing may be found
EXPECT = {
    CREW + "centralplains.dat": {"centralplains.dat": 4137},
    CREW + "common.dat": {"common.dat": 6442},
    CREW + "citysouthpacific.dat": {"citysouthpacific.dat": 7202},
    NMS + "NMSARC.Precache.pak": {"NMSARC.Precache.pak": 1772, "NMSARC.Precache.pak~decoded": 14909},
    NMS + "NMSARC.AnimMBIN.pak": {"NMSARC.AnimMBIN.pak": 2898, "NMSARC.AnimMBIN.pak~decoded": 3251},
    NMS + "NMSARC.MetadataEtc.pak": {"NMSARC.MetadataEtc.pak": 8972, "NMSARC.MetadataEtc.pak~decoded": 50790},
    NMS + "NMSARC.Shaders.pak": {"NMSARC.Shaders.pak": 9948, "NMSARC.Shaders.pak~decoded": 26131},
    NMS + "NMSARC.MeshPlanetCREATURES.pak": {"NMSARC.MeshPlanetCREATURES.pak": 3396, "NMSARC.MeshPlanetCREATURES.pak~decoded": 662},
    NMS + "NMSARC.TexBiomesALIEN.pak": {"NMSARC.TexBiomesALIEN.pak": 1095, "NMSARC.TexBiomesALIEN.pak~decoded": 60},
    "C:/Windows/explorer.exe": {},
    CORPUS + "fsb/common_sounds-662c3cbe4ece016b.fsb": {},
    CORPUS + "cab/BucketConfidenceData.cab": {},
    CORPUS + "asar/evernote-app.asar": {},
}
# found within this share of the header's count (a table's edge may lose a row)
TOL = 0.01


def main():
    probe = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/release/examples/aqr-infer-pak-probe")
    if os.name == "nt" and not probe.endswith(".exe"):
        probe += ".exe"
    out = subprocess.run([probe, *EXPECT], capture_output=True, text=True).stdout
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
    for f, exp in EXPECT.items():
        got = found.get(f, [])
        name = f.rsplit("/", 1)[1]
        for t, n in exp.items():
            hits = [m for tt, m in got if tt == t and abs(m - n) <= max(1, TOL * n)]
            if hits:
                ok += 1
            else:
                bad += 1
                print(f"MISS  {name}: {t} {n} (found {[(tt, m) for tt, m in got if tt == t]})")
        for t, m in got:
            if t not in exp or abs(m - exp[t]) > max(1, TOL * exp[t]):
                bad += 1
                print(f"EXTRA {name}: {t} {m}")
        print(f"      {name}: {times.get(f, 0):.0f} ms")
    total = sum(times.values())
    print(f"{ok} found, {bad} wrong; {total / 1e3:.1f} s in all")


if __name__ == "__main__":
    main()
