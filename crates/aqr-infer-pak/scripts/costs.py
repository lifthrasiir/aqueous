#!/usr/bin/env python3
"""Where a probe's time goes: AQR_PAK_VERBOSE output on standard input, the table and target
pairs by cost. Usage: AQR_PAK_VERBOSE=1 PROBE FILE 2>&1 | python costs.py"""
import re
import sys

rows, last = [], None
for line in sys.stdin:
    m = re.search(r"(\S+) by (\S+) (0x[0-9a-f]+)\.\.(0x[0-9a-f]+)/(\d+) \((\d+) rows\): (\d+) sizes \(([\d.]+) ms\), "
                  r"(\d+) explicit \(([\d.]+) ms\), (\d+) implicit \(([\d.]+) ms\)", line)
    if m:
        last = [float(m.group(10)), float(m.group(12)), 0.0, f"{m.group(1)} by {m.group(2)} {m.group(3)}/{m.group(5)}",
                int(m.group(6)), int(m.group(7))]
        rows.append(last)
    elif line.startswith("  (") or line.startswith("  find"):
        print(line.rstrip()[:200])
print(f"{len(rows)} pairs; explicit {sum(r[0] for r in rows):.0f} ms, implicit {sum(r[1] for r in rows):.0f} ms")
rows.sort(key=lambda r: -(r[0] + r[1]))
for r in rows[:10]:
    print(f"  {r[0]:7.1f} {r[1]:7.1f}  {r[3]} ({r[4]} rows, {r[5]} sizes)")
