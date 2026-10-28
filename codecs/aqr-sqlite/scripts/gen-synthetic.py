#!/usr/bin/env python3
"""Deterministically generate synthetic SQLite files exercising format features
into corpus/synthetic/.  Idempotent: existing files are skipped.  Run with `python3 -I`."""
import json, os, random, shutil, sqlite3, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "corpus", "synthetic")
os.makedirs(OUT, exist_ok=True)

WORDS = ("the of and to in is that for it as was with be by on not he this are or his from at which but have an had "
         "they you were their one all we can her has there been if more when will would who so no out time system data "
         "query index table page record value engine storage network server client request response error cache "
         "buffer thread memory file block journal commit rollback schema column row key hash tree node leaf cell "
         "garden river mountain coffee winter market letter window silver orange bridge engine forest yellow").split()
FIRST = "Alice Bob Carol Dave Eve Frank Grace Heidi Ivan Judy Karl Liam Mona Nina Oscar Pia Quinn Rosa Sven Tara Uma Vic Wen Xia Yuki Zed Minjun Sofia Hiro".split()
LAST = "Smith Jones Kim Park Lee Garcia Muller Rossi Tanaka Ivanov Nguyen Silva Khan Cohen Larsen Dubois Novak Ahmed Chen Brown".split()
TLD = ["com", "org", "net", "io", "dev", "co.uk"]
KINDS = ["click", "view", "purchase", "login", "logout", "error", "scroll", "search"]
T0 = 1_600_000_000

def sentence(r, n):
    return " ".join(r.choice(WORDS) for _ in range(n)).capitalize() + "."

def text(r, nbytes):
    parts, total = [], 0
    while total < nbytes:
        s = sentence(r, r.randint(5, 14)); parts.append(s); total += len(s) + 1
    return " ".join(parts)[:nbytes]

def name(r): return r.choice(FIRST) + " " + r.choice(LAST)
def email(r, i): return f"{r.choice(FIRST).lower()}.{r.choice(LAST).lower()}{i}@{r.choice(WORDS)}.{r.choice(TLD)}"
def url(r): return f"https://www.{r.choice(WORDS)}.{r.choice(TLD)}/{r.choice(WORDS)}/{r.choice(WORDS)}?id={r.randint(1,10**6)}&q={r.choice(WORDS)}"
def blob(r, n):  # semi-structured binary: repeated small records with some entropy
    rec = bytes(r.getrandbits(8) for _ in range(8))
    return b"".join(rec + bytes([r.getrandbits(8) & 0x0F, r.getrandbits(8)]) for _ in range(n // 10 + 1))[:n]
def js(r):
    return json.dumps({"session": r.randint(1, 5000), "ua": r.choice(["Firefox/98", "Chrome/101", "Safari/15"]),
                       "tags": [r.choice(WORDS) for _ in range(r.randint(0, 4))], "ok": r.random() < .9,
                       "lat": round(r.uniform(-90, 90), 5), "lon": round(r.uniform(-180, 180), 5)})

SCHEMA = """
CREATE TABLE users(
  id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, email TEXT UNIQUE, created INTEGER,
  score REAL, bio TEXT, avatar BLOB, deleted_at INTEGER, nick TEXT);
CREATE TABLE events(
  id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users(id), ts INTEGER NOT NULL, kind TEXT,
  url TEXT, payload TEXT, amount REAL, flag INTEGER, note TEXT);
CREATE INDEX events_user_ts ON events(user_id, ts);
CREATE INDEX events_kind_ts ON events(kind, ts DESC);
CREATE INDEX events_url ON events(url);
CREATE UNIQUE INDEX users_name_created ON users(name, created, id);
"""

def populate(con, seed, nusers, nevents, bio_max=200, with_blob=True):
    r = random.Random(seed)
    con.executescript(SCHEMA)
    ts = T0
    users = []
    for i in range(nusers):
        ts += r.randint(0, 300)
        users.append((name(r), email(r, i), ts, r.choice([None, round(r.gauss(50, 20), 3)]),
                      text(r, r.randint(0, bio_max)) if r.random() < .8 else None,
                      blob(r, r.randint(16, 400)) if with_blob and r.random() < .3 else None,
                      None if r.random() < .95 else ts + 5, r.choice([None, r.choice(FIRST).lower()])))
    con.executemany("INSERT INTO users(name,email,created,score,bio,avatar,deleted_at,nick) VALUES(?,?,?,?,?,?,?,?)", users)
    rows = []
    for i in range(nevents):
        ts += r.randint(0, 20)
        rows.append((r.randint(1, nusers), ts, r.choice(KINDS), url(r) if r.random() < .8 else None,
                     js(r) if r.random() < .7 else None, round(r.expovariate(.05), 2) if r.random() < .5 else None,
                     r.randint(0, 1), sentence(r, r.randint(3, 20)) if r.random() < .4 else None))
    con.executemany("INSERT INTO events(user_id,ts,kind,url,payload,amount,flag,note) VALUES(?,?,?,?,?,?,?,?)", rows)
    con.commit()

def churn(con, seed, rounds, nusers, delete_frac=0.0, grow=True):
    """Random UPDATE/DELETE/INSERT churn to create freeblocks, fragmentation and freelist pages."""
    r = random.Random(seed)
    for _ in range(rounds):
        k = r.random()
        uid = r.randint(1, nusers)
        if k < .5:
            con.execute("UPDATE events SET note=?, payload=? WHERE id=?",
                        (text(r, r.randint(5, 600 if grow else 40)), js(r) if r.random() < .5 else None, r.randint(1, 10**9) % max(1, con.execute("SELECT max(id) FROM events").fetchone()[0]) + 1))
        elif k < .7:
            con.execute("UPDATE users SET bio=?, score=? WHERE id=?", (text(r, r.randint(0, 500)), r.random() * 100, uid))
        elif k < .9:
            con.execute("DELETE FROM events WHERE id % 97 = ? AND ts % 7 = ?", (r.randint(0, 96), r.randint(0, 6)))
        else:
            con.execute("INSERT INTO events(user_id,ts,kind,url,payload,amount,flag,note) VALUES(?,?,?,?,?,?,?,?)",
                        (uid, T0 + r.randint(0, 10**7), r.choice(KINDS), url(r), js(r), r.random(), 0, sentence(r, 8)))
    if delete_frac:
        con.execute("DELETE FROM events WHERE (id * 2654435761) % 1000 < ?", (int(delete_frac * 1000),))
    con.commit()

def open_new(fname, page_size=4096, encoding=None, auto_vacuum=None, secure_delete=None, reserve=None, journal=None):
    path = os.path.join(OUT, fname)
    tmp = path + ".tmp"
    for p in (tmp, tmp + "-wal", tmp + "-shm", tmp + "-journal"):
        if os.path.exists(p): os.remove(p)
    if reserve:  # only the sqlite3 CLI exposes the reserve_bytes file control
        subprocess.run(["sqlite3", tmp], check=True, text=True,
                       input=f"PRAGMA page_size={page_size};\n.filectrl reserve_bytes {reserve}\nCREATE TABLE _x(a);\nDROP TABLE _x;\nVACUUM;\n")
    con = sqlite3.connect(tmp, isolation_level=None)
    con.execute("PRAGMA page_size=%d" % page_size)
    if encoding: con.execute("PRAGMA encoding='%s'" % encoding)
    if auto_vacuum is not None: con.execute("PRAGMA auto_vacuum=%d" % auto_vacuum)
    if secure_delete is not None: con.execute("PRAGMA secure_delete=%d" % secure_delete)
    if journal: con.execute("PRAGMA journal_mode=%s" % journal)
    con.isolation_level = ""
    return con, path, tmp

def finish(con, path, tmp, vacuum=False):
    con.commit()
    if vacuum: con.execute("VACUUM")
    con.close()
    for s in ("-wal", "-shm", "-journal"):
        assert not os.path.exists(tmp + s), tmp + s
    os.replace(tmp, path)
    print("wrote", os.path.basename(path), os.path.getsize(path))

def want(fname):
    if os.path.exists(os.path.join(OUT, fname)):
        print("skip", fname); return False
    return True

def patch_u32(path, off, val):
    with open(path, "r+b") as f:
        f.seek(off); f.write(val.to_bytes(4, "big"))

def main():
    # --- page sizes ---
    for ps, nu, ne in ((512, 1500, 8000), (1024, 2500, 15000), (4096, 2000, 12000), (8192, 2000, 12000), (65536, 2000, 12000)):
        f = f"pagesize_{ps}.db"
        if want(f):
            con, p, t = open_new(f, ps); populate(con, 100 + ps, nu, ne); finish(con, p, t)

    # --- overflow chains: long text and blobs ---
    for ps in (1024, 4096):
        f = f"overflow_{ps}.db"
        if want(f):
            con, p, t = open_new(f, ps); r = random.Random(7 + ps)
            con.executescript("CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT, body TEXT, attach BLOB, meta TEXT);"
                              "CREATE INDEX docs_title ON docs(title);")
            for i in range(150 if ps == 1024 else 200):
                n = r.choice([100, 900, 3000, 8000, 30000, 120000])
                con.execute("INSERT INTO docs(title,body,attach,meta) VALUES(?,?,?,?)",
                            (" ".join(r.choice(WORDS) for _ in range(r.randint(2, 40)) ) * r.choice([1, 1, 20]),
                             text(r, n), blob(r, r.choice([0, 500, 5000, 70000])) if r.random() < .7 else None, js(r)))
            finish(con, p, t)

    # --- WITHOUT ROWID ---
    if want("without_rowid.db"):
        con, p, t = open_new("without_rowid.db", 4096); r = random.Random(11)
        con.executescript("""
CREATE TABLE kv(k TEXT PRIMARY KEY, v BLOB, updated INTEGER) WITHOUT ROWID;
CREATE TABLE edges(src INTEGER, dst INTEGER, weight REAL, label TEXT, PRIMARY KEY(src, dst)) WITHOUT ROWID;
CREATE INDEX edges_dst ON edges(dst, weight);
CREATE TABLE tags(doc INTEGER, tag TEXT, PRIMARY KEY(tag, doc)) WITHOUT ROWID;
CREATE TABLE big(id TEXT PRIMARY KEY, body TEXT) WITHOUT ROWID;""")
        con.executemany("INSERT OR IGNORE INTO kv VALUES(?,?,?)",
                        ((url(r), blob(r, r.randint(0, 300)) if r.random() < .8 else None, T0 + i) for i in range(6000)))
        con.executemany("INSERT OR IGNORE INTO edges VALUES(?,?,?,?)",
                        ((r.randint(1, 3000), r.randint(1, 3000), r.random(), r.choice(WORDS) if r.random() < .5 else None) for _ in range(15000)))
        con.executemany("INSERT OR IGNORE INTO tags VALUES(?,?)", ((r.randint(1, 20000), r.choice(WORDS)) for _ in range(30000)))
        con.executemany("INSERT OR IGNORE INTO big VALUES(?,?)", ((f"{r.choice(WORDS)}-{i}", text(r, r.randint(200, 20000))) for i in range(500)))
        con.execute("DELETE FROM kv WHERE rowid IS NULL OR substr(k,13,1) = 'a'") if False else con.execute("DELETE FROM kv WHERE substr(k,13,1) IN ('a','e')")
        finish(con, p, t)

    # --- churn: fragmentation, freeblocks, freelist; secure_delete on/off; vacuumed variant ---
    for fname, sd, vac, ps in (("churn_plain.db", 0, False, 4096), ("churn_secure.db", 1, False, 4096),
                               ("churn_vacuumed.db", 0, True, 4096), ("churn_1024.db", 0, False, 1024)):
        if want(fname):
            con, p, t = open_new(fname, ps, secure_delete=sd)
            populate(con, 555, 3000, 16000 if ps == 4096 else 12000)
            churn(con, 556, 4000, 3000, delete_frac=0.35)
            finish(con, p, t, vacuum=vac)

    # --- auto_vacuum (pointer-map pages) ---
    for fname, av in (("autovacuum_full.db", 1), ("autovacuum_incremental.db", 2)):
        if want(fname):
            con, p, t = open_new(fname, 1024 if av == 2 else 4096, auto_vacuum=av)
            populate(con, 600 + av, 2500, 20000)
            churn(con, 700 + av, 1500, 2500, delete_frac=0.25)
            if av == 1:
                finish(con, p, t)
            else:  # leave some freelist pages unreclaimed, reclaim only part
                con.execute("PRAGMA incremental_vacuum(50)"); finish(con, p, t)

    # --- UTF-16 ---
    for fname, enc in (("utf16le.db", "UTF-16le"), ("utf16be.db", "UTF-16be")):
        if want(fname):
            con, p, t = open_new(fname, 4096, encoding=enc); populate(con, 800, 2000, 9000); finish(con, p, t)

    # --- reserved bytes per page ---
    for fname, ps, rb in (("reserve_8.db", 4096, 8), ("reserve_32.db", 1024, 32), ("reserve_16_ps512.db", 512, 16)):
        if want(fname):
            try:
                con, p, t = open_new(fname, ps, reserve=rb)
            except Exception as e:
                print("skip", fname, e); continue
            populate(con, 900 + rb, 1500, 8000)
            churn(con, 901, 300, 1500, delete_frac=0.1)
            finish(con, p, t)

    # --- legacy schema format numbers (header patched; schema has no DESC indexes / features needing format 4) ---
    for fmt in (1, 2, 3):
        fname = f"schema_format_{fmt}.db"
        if want(fname):
            con, p, t = open_new(fname, 4096); r = random.Random(1000 + fmt)
            con.executescript("CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT, v REAL, note TEXT);"
                              "CREATE INDEX a_name ON a(name, v);")
            con.executemany("INSERT INTO a(name,v,note) VALUES(?,?,?)",
                            ((name(r), r.random(), sentence(r, 9)) for _ in range(12000)))
            if fmt >= 2:  # ALTER TABLE ADD COLUMN with default needs format >= 2
                con.execute("ALTER TABLE a ADD COLUMN extra TEXT DEFAULT 'x'")
            con.commit(); con.close()
            patch_u32(t, 44, fmt)
            c = sqlite3.connect(t); assert c.execute("PRAGMA integrity_check").fetchone()[0] == "ok"; c.close()
            os.replace(t, p); print("wrote", fname, os.path.getsize(p))

    # --- FTS5 ---
    if want("fts5.db"):
        con, p, t = open_new("fts5.db", 4096); r = random.Random(1200)
        con.execute("CREATE VIRTUAL TABLE docs USING fts5(title, body, tokenize='porter unicode61')")
        con.execute("CREATE VIRTUAL TABLE docs_tri USING fts5(title, content='', tokenize='trigram', detail=none)")
        for i in range(2500):
            title = sentence(r, r.randint(3, 8))
            con.execute("INSERT INTO docs(title, body) VALUES(?,?)", (title, text(r, r.randint(200, 2500))))
            if i % 3 == 0: con.execute("INSERT INTO docs_tri(rowid, title) VALUES(?,?)", (i + 1, title))
        con.execute("DELETE FROM docs WHERE rowid % 11 = 0")
        con.execute("INSERT INTO docs(docs) VALUES('optimize')") if False else None
        finish(con, p, t)

    # --- R*Tree ---
    if want("rtree.db"):
        con, p, t = open_new("rtree.db", 4096); r = random.Random(1300)
        con.executescript("CREATE VIRTUAL TABLE spots USING rtree(id, minx, maxx, miny, maxy);"
                          "CREATE VIRTUAL TABLE spots_aux USING rtree(id, minx, maxx, miny, maxy, +name TEXT, +kind TEXT);"
                          "CREATE TABLE places(id INTEGER PRIMARY KEY, name TEXT, lat REAL, lon REAL);")
        for i in range(1, 30001):
            x, y = r.uniform(-180, 180), r.uniform(-90, 90); w, h = r.random() * 2, r.random() * 2
            con.execute("INSERT INTO spots VALUES(?,?,?,?,?)", (i, x, x + w, y, y + h))
            if i % 3 == 0:
                con.execute("INSERT INTO spots_aux VALUES(?,?,?,?,?,?,?)", (i, x, x + w, y, y + h, name(r), r.choice(KINDS)))
            con.execute("INSERT INTO places VALUES(?,?,?,?)", (i, name(r), y, x))
        con.execute("DELETE FROM spots WHERE id % 13 = 0")
        finish(con, p, t)

    # --- WAL mode after checkpoint ---
    if want("wal_checkpointed.db"):
        con, p, t = open_new("wal_checkpointed.db", 4096)
        con.execute("PRAGMA journal_mode=WAL")
        populate(con, 1400, 3000, 20000)
        churn(con, 1401, 800, 3000)
        con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        finish(con, p, t)
        c = sqlite3.connect(p); c.execute("PRAGMA journal_mode=WAL"); c.close()  # no-op sanity check
        for s in ("-wal", "-shm"):
            if os.path.exists(p + s): os.remove(p + s)

if __name__ == "__main__":
    main()
