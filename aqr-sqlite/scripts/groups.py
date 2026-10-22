"""Joins SQLR_DUMP output with dbstat names. usage: python3 -I scripts/groups.py DB [N]"""
import sqlite3, subprocess, sys, os, re
db = sys.argv[1]
top = int(sys.argv[2]) if len(sys.argv) > 2 else 20
con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
try:
    names = dict(con.execute("select pageno, name from dbstat"))
except sqlite3.Error:
    names = {}
env = dict(os.environ, SQLR_DUMP="1")
exe = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "release", "sqlr")
out = subprocess.run([exe, "bench", db], env=env, capture_output=True, text=True).stderr
rows = []
for line in out.splitlines():
    if not line.startswith("G "):
        continue
    head, *cols = line.split(" | ")
    first = int(re.search(r"first=(\d+)", head).group(1))
    tot = 0
    cs = []
    for c in cols:
        d = dict(kv.split("=") for kv in c.split()[6:])
        b = sum(int(v) for v in d.values())
        tot += b
        cs.append((b, c))
    rows.append((tot, names.get(first, "?"), head, cs))
rows.sort(reverse=True)
for tot, name, head, cs in rows[:top]:
    print(f"{tot:>11} {name}: {head}")
    for b, c in sorted(cs, reverse=True)[:6]:
        print(f"      {c}")
