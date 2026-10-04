#!/usr/bin/env python3
"""BPM catalog benchmark: DuckDB vs SQLite on the BPM data shape.

usage: bench.py ENGINE K [--nofk]
  K = file metadata pairs per file (1M files -> K million pairs)
"""
import sys, os, time, json, statistics, shutil, sqlite3
import duckdb

engine, K = sys.argv[1], int(sys.argv[2])
FK = "--nofk" not in sys.argv
NODES, FILES = 100_000, 1_000_000
DIR = os.path.dirname(os.path.abspath(__file__))
PATH = f"{DIR}/bench_{engine}_{K}.db"
for suf in ("", "-wal", ".wal", "-shm"):
    if os.path.exists(PATH + suf): os.remove(PATH + suf)

R = {"engine": engine, "K": K, "fk": FK}
IS_DUCK = engine == "duckdb"

def hexid(e):  # 32-char zero padded hex, time-ordered like UUIDv7
    return f"lpad(to_hex(({e})::BIGINT),32,'0')" if IS_DUCK else f"printf('%032x',{e})"

def fp(e):
    return (f"printf('%016x%016x',({e})*2654435761,({e})*40503+7)")

def connect():
    if IS_DUCK:
        c = duckdb.connect(PATH)
        return c
    c = sqlite3.connect(PATH, isolation_level=None)
    c.execute("PRAGMA journal_mode=WAL"); c.execute("PRAGMA synchronous=NORMAL")
    c.execute("PRAGMA cache_size=-1000000")
    c.execute(f"PRAGMA foreign_keys={'ON' if FK else 'OFF'}")
    return c

def ex(c, sql, p=()):
    return c.execute(sql, p) if p else c.execute(sql)

def q(c, sql, p=()):
    return ex(c, sql, p).fetchall()

con = connect()
TS = "TIMESTAMP" if IS_DUCK else "TEXT"
NOW = "now()" if IS_DUCK else "strftime('%Y-%m-%dT%H:%M:%f','now')"
WOR = "" if IS_DUCK else " WITHOUT ROWID"
ref = lambda t: f" REFERENCES {t}(id)" if FK else ""

def timed(label, fn):
    t = time.perf_counter(); out = fn(); dt = time.perf_counter() - t
    R.setdefault("load", {})[label] = round(dt, 2)
    print(f"  load {label}: {dt:.1f}s", flush=True)
    return out

# ---------------------------------------------------------------- schema
ddl = [
f"CREATE TABLE nodes(id VARCHAR PRIMARY KEY, node_type VARCHAR NOT NULL, parent_id VARCHAR, name VARCHAR, created_at {TS} NOT NULL, updated_at {TS} NOT NULL){WOR}",
f"CREATE TABLE entity_metadata(node_id VARCHAR NOT NULL{ref('nodes')}, key VARCHAR NOT NULL, value VARCHAR NOT NULL, updated_at {TS} NOT NULL, PRIMARY KEY(node_id,key)){WOR}",
f"CREATE TABLE files(id VARCHAR PRIMARY KEY, size_bytes BIGINT, mtime {TS}, fingerprint VARCHAR, fingerprint_scheme VARCHAR, created_at {TS} NOT NULL){WOR}",
f"CREATE TABLE file_metadata(file_id VARCHAR NOT NULL{ref('files')}, key VARCHAR NOT NULL, value VARCHAR NOT NULL, updated_at {TS} NOT NULL, PRIMARY KEY(file_id,key)){WOR}",
f"CREATE TABLE file_digests(file_id VARCHAR NOT NULL{ref('files')}, algorithm VARCHAR NOT NULL, digest VARCHAR NOT NULL, generation INTEGER NOT NULL, current BOOLEAN NOT NULL, observed_at {TS} NOT NULL, PRIMARY KEY(file_id,algorithm,generation)){WOR}",
f"CREATE TABLE file_locations(file_id VARCHAR NOT NULL{ref('files')}, backend VARCHAR NOT NULL, uri VARCHAR NOT NULL, last_size BIGINT, last_mtime {TS}, presence VARCHAR NOT NULL, stat_state VARCHAR NOT NULL, digest_state VARCHAR NOT NULL, PRIMARY KEY(backend,uri)){WOR}",
f"CREATE TABLE file_links(file_id VARCHAR NOT NULL{ref('files')}, node_id VARCHAR NOT NULL{ref('nodes')}, role VARCHAR NOT NULL, PRIMARY KEY(file_id,node_id)){WOR}",
]
for s in ddl: ex(con, s)

if not IS_DUCK:
    ex(con, "CREATE TEMP TABLE nums(i INTEGER PRIMARY KEY)")
    ex(con, f"WITH RECURSIVE r(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM r WHERE i<{FILES}) INSERT INTO nums SELECT i FROM r")
def rng(n, alias="i"):
    return f"range({n}) t({alias})" if IS_DUCK else f"(SELECT i AS {alias} FROM nums WHERE i<{n}) t"
def krng(n, alias="j"):
    return f"range({n}) k({alias})" if IS_DUCK else f"(SELECT i AS {alias} FROM nums WHERE i<{n}) k"
def b(c):
    if not IS_DUCK: ex(c, "BEGIN")
def cm(c):
    if not IS_DUCK: ex(c, "COMMIT")

CARD = [5, 20, 100, 1000, 10000, 100000, 1000000, 1000000]  # per key j%8

def load():
    b(con)
    # --- nodes: 1 program, 20 projects, 20k cases, 40k samples, 30k raw, 10k analyses (ids = index)
    ntype = ("CASE WHEN i=0 THEN 'program' WHEN i<=20 THEN 'project' WHEN i<=20020 THEN 'case' "
             "WHEN i<=60020 THEN 'sample' WHEN i<=90020 THEN 'raw_data' ELSE 'analysis' END")
    npar = ("CASE WHEN i=0 THEN NULL WHEN i<=20 THEN 0 WHEN i<=20020 THEN 1+(i%20) "
            "WHEN i<=60020 THEN 21+(i%20000) WHEN i<=90020 THEN 20021+(i%40000) ELSE 60021+(i%30000) END")
    ex(con, f"INSERT INTO nodes SELECT {hexid('i')}, {ntype}, "
            f"CASE WHEN ({npar}) IS NULL THEN NULL ELSE {hexid(npar)} END, "
            f"CASE WHEN i<=20 THEN 'N'||i END, {NOW}, {NOW} FROM {rng(NODES)}")
    # --- entity metadata: 20 generic keys per node + sample_kind + assay
    ex(con, f"INSERT INTO entity_metadata SELECT {hexid('i')}, printf('ek%02d',j), 'ev'||((i*37+j*101)%{{card}}), {NOW} "
            f"FROM {rng(NODES)}, {krng(20)}".replace("{card}", "1000"))
    ex(con, f"INSERT INTO entity_metadata SELECT {hexid('i')}, 'sample_kind', 'sk'||(i%7), {NOW} FROM {rng(NODES)} WHERE i>20020 AND i<=60020")
    ex(con, f"INSERT INTO entity_metadata SELECT {hexid('i')}, 'assay', 'as'||(i%8), {NOW} FROM {rng(NODES)} WHERE i>60020 AND i<=90020")
    cm(con)

    b(con)
    FB = 10_000_000  # file id offset
    ex(con, f"INSERT INTO files SELECT {hexid(f'i+{FB}')}, (i*7919)%5000000000, {NOW}, {fp('i')}, 'xxh3-128-full', {NOW} FROM {rng(FILES)}")
    ex(con, f"INSERT INTO file_digests SELECT {hexid(f'i+{FB}')}, 'blake3', {fp('i*3')}||{fp('i*5')}, 1, true, {NOW} FROM {rng(FILES)}")
    ex(con, f"INSERT INTO file_locations SELECT {hexid(f'i+{FB}')}, 'posix', '/data/run'||(i%1000)||'/f'||i||'.fq.gz', (i*7919)%5000000000, {NOW}, "
            f"'present', CASE WHEN i%50=0 THEN 'changed' ELSE 'unchanged' END, CASE WHEN i%97=0 THEN 'mismatch' WHEN i%3=0 THEN 'unverified' ELSE 'match' END FROM {rng(FILES)}")
    # links to raw_data / analysis nodes (ids 60021..99999)
    ex(con, f"INSERT INTO file_links SELECT {hexid(f'i+{FB}')}, {hexid('60021+(i%39979)')}, 'data' FROM {rng(FILES)}")
    cm(con)

    # file_metadata in chunks of 100k files to keep txns bounded
    cards = "CASE j%8 " + " ".join(f"WHEN {k} THEN {c}" for k, c in enumerate(CARD)) + " END"
    for lo in range(0, FILES, 200_000):
        b(con)
        sub = (f"(SELECT i FROM range({lo},{lo+200000}) x(i))" if IS_DUCK else
               f"(SELECT i FROM nums WHERE i>={lo} AND i<{lo+200000})")
        ex(con, f"INSERT INTO file_metadata SELECT {hexid(f'i+{FB}')}, printf('fk%02d',j), 'fv'||((i*37+j*101)%({cards})), {NOW} "
                f"FROM {sub} t, {krng(K)}")
        cm(con)
timed("tables", load)

def idx():
    ex(con, "CREATE INDEX entity_metadata_kv ON entity_metadata(key,value)")
    ex(con, "CREATE INDEX file_metadata_kv ON file_metadata(key,value)")
    ex(con, "CREATE INDEX file_links_node ON file_links(node_id)")
    ex(con, "CREATE INDEX file_locations_file ON file_locations(file_id)")
    if not IS_DUCK: ex(con, "CREATE INDEX nodes_parent ON nodes(parent_id)")
timed("secondary_indexes", idx)
if IS_DUCK: ex(con, "CHECKPOINT")
else:
    ex(con, "ANALYZE"); ex(con, "PRAGMA wal_checkpoint(TRUNCATE)")
con.close()

def size():
    return sum(os.path.getsize(PATH + s) for s in ("", "-wal", ".wal") if os.path.exists(PATH + s))
R["size_gb"] = round(size() / 1e9, 2)
print(f"  size {R['size_gb']} GB", flush=True)

# ---------------------------------------------------------------- queries
t0 = time.perf_counter(); con = connect(); q(con, "SELECT count(*) FROM nodes WHERE id=" + hexid(1).replace("printf", "printf"))
R["open_plus_first_query_s"] = round(time.perf_counter() - t0, 3)

P = hexid(1)
if not IS_DUCK:
    ex(con, "CREATE TEMP TABLE nums(i INTEGER PRIMARY KEY)")
    ex(con, "WITH RECURSIVE r(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM r WHERE i<1000) INSERT INTO nums SELECT i FROM r")
def under(depth_cols="id"):
    lv = [f"SELECT {P} AS id",
          f"SELECT c.id FROM nodes c WHERE c.parent_id={P}",
          f"SELECT s.id FROM nodes c JOIN nodes s ON s.parent_id=c.id WHERE c.parent_id={P}",
          f"SELECT r.id FROM nodes c JOIN nodes s ON s.parent_id=c.id JOIN nodes r ON r.parent_id=s.id WHERE c.parent_id={P}",
          f"SELECT a.id FROM nodes c JOIN nodes s ON s.parent_id=c.id JOIN nodes r ON r.parent_id=s.id JOIN nodes a ON a.parent_id=r.id WHERE c.parent_id={P}"]
    return "WITH d AS (" + " UNION ALL ".join(lv) + ")"

FILES_UNDER = (f"{under()} SELECT f.id,f.size_bytes,f.fingerprint,l.role,loc.uri,loc.presence,loc.stat_state "
               "FROM d JOIN file_links l ON l.node_id=d.id JOIN files f ON f.id=l.file_id JOIN file_locations loc ON loc.file_id=f.id")

def fsel(*pairs, extra=""):
    s = "SELECT f.id FROM files f WHERE " + " AND ".join(
        f"EXISTS(SELECT 1 FROM file_metadata m WHERE m.file_id=f.id AND m.key='{k}' AND m.value='{v}')" for k, v in pairs)
    return s

Q = {
 # entity metadata selectors (2M rows)
 "ent key:value (rare ~1k)":       "SELECT node_id FROM entity_metadata WHERE key='ek03' AND value='ev123'",
 "ent key: (present, 100k)":       "SELECT node_id FROM entity_metadata WHERE key='ek03'",
 "ent :value (any key)":           "SELECT DISTINCT node_id FROM entity_metadata WHERE value='ev123'",
 # file metadata selectors (K M rows)
 "file key:value rare (~1)":       "SELECT file_id FROM file_metadata WHERE key='fk07' AND value='fv123457'",
 "file key:value (~100)":          "SELECT file_id FROM file_metadata WHERE key='fk04' AND value='fv123'",
 "file key:value (~10k)":          "SELECT file_id FROM file_metadata WHERE key='fk02' AND value='fv5'",
 "file key:value (~200k)":         "SELECT file_id FROM file_metadata WHERE key='fk00' AND value='fv1'",
 "file 2-selector conj":           "SELECT a.file_id FROM file_metadata a JOIN file_metadata b ON a.file_id=b.file_id WHERE a.key='fk04' AND a.value='fv123' AND b.key='fk02' AND b.value='fv23'" if K > 4 else "SELECT 1",
 "file key: present (count)":      "SELECT count(*) FROM file_metadata WHERE key='fk03'",
 "file :value any key (count)":    "SELECT count(*) FROM file_metadata WHERE value='fv777'",
 # hierarchy
 "files --under project (~50k rows)": FILES_UNDER,
 "files --under project + selector": FILES_UNDER.replace("FROM d JOIN", "FROM d JOIN") + " WHERE EXISTS(SELECT 1 FROM file_metadata m WHERE m.file_id=f.id AND m.key='fk02' AND m.value='fv5')",
 "entities --under project":       under() + " SELECT d.id FROM d",
 "uuid lookup":                    f"SELECT * FROM nodes WHERE id={hexid(55555)}",
 "file detail (1 file, 5 tables)": f"SELECT f.*, (SELECT count(*) FROM file_metadata WHERE file_id=f.id), (SELECT count(*) FROM file_digests WHERE file_id=f.id), (SELECT count(*) FROM file_locations WHERE file_id=f.id), (SELECT count(*) FROM file_links WHERE file_id=f.id) FROM files f WHERE f.id={hexid(10_000_000+424242)}",
 # summary
 "summary: files by node_type":    "SELECT n.node_type,count(*),sum(f.size_bytes) FROM file_links l JOIN nodes n ON n.id=l.node_id JOIN files f ON f.id=l.file_id GROUP BY 1",
 "summary: files by assay":        "SELECT m.value,count(*),sum(f.size_bytes) FROM file_links l JOIN entity_metadata m ON m.node_id=l.node_id AND m.key='assay' JOIN files f ON f.id=l.file_id GROUP BY 1",
 "summary: drift counts":          "SELECT presence,stat_state,digest_state,count(*) FROM file_locations GROUP BY 1,2,3",
 "summary: files by format-key":   "SELECT value,count(*) FROM file_metadata WHERE key='fk01' GROUP BY 1",
 "unlinked files (anti-join)":     "SELECT count(*) FROM files f WHERE NOT EXISTS(SELECT 1 FROM file_links l WHERE l.file_id=f.id)",
 "possible dups (size+fp)":        "SELECT count(*) FROM (SELECT size_bytes,fingerprint_scheme,fingerprint FROM files GROUP BY 1,2,3 HAVING count(*)>1)",
}
R["queries"] = {}
for name, sql in Q.items():
    ts = []
    n = None
    for _ in range(3):
        t = time.perf_counter(); rows = q(con, sql); ts.append(time.perf_counter() - t); n = len(rows)
    R["queries"][name] = {"rows": n, "first_ms": round(ts[0]*1000, 1), "best_ms": round(min(ts)*1000, 1)}
    print(f"  {name:40s} rows={n:<7} first={ts[0]*1000:9.1f}ms best={min(ts)*1000:9.1f}ms", flush=True)

# ---------------------------------------------------------------- writes
W = {}
def wr(name, n, fn):
    t = time.perf_counter()
    for i in range(n): fn(i)
    dt = time.perf_counter() - t
    W[name] = {"n": n, "ms_each": round(dt / n * 1000, 2)}
    print(f"  write {name:36s} {dt/n*1000:9.2f} ms each (n={n})", flush=True)

def txn(c, stmts):
    b(c)
    for s, p in stmts: ex(c, s, p) if p else ex(c, s)
    if IS_DUCK: pass
    cm(c)

def ingest_batch(i):
    base = 2_000_000_000 + i * 1000
    if IS_DUCK: ex(con, "BEGIN")
    else: ex(con, "BEGIN")
    ex(con, f"INSERT INTO files SELECT {hexid(f'i+{base}')}, i*10, {NOW}, {fp(f'i+{base}')}, 'xxh3-128-full', {NOW} FROM {rng(1000)}")
    ex(con, f"INSERT INTO file_locations SELECT {hexid(f'i+{base}')}, 'posix', '/new/b{i}/f'||i, i*10, {NOW}, 'present','unchanged','unverified' FROM {rng(1000)}")
    ex(con, "COMMIT")
wr("ingest batch 1000 files (files+locations)", 50, ingest_batch)

def scan_batch(i):
    lo = 10_000_000 + i * 20_000
    ex(con, "BEGIN")
    ex(con, f"UPDATE files SET size_bytes=size_bytes+1, mtime={NOW}, fingerprint={fp('1')} WHERE id >= {hexid(lo)} AND id < {hexid(lo+1000)}")
    ex(con, f"UPDATE file_locations SET stat_state='changed', last_size=last_size+1 WHERE file_id >= {hexid(lo)} AND file_id < {hexid(lo+1000)}")
    ex(con, f"INSERT INTO file_digests SELECT {hexid(f'i+{lo}')}, 'blake3', {fp('i')}, 2, true, {NOW} FROM {rng(1000)}")
    ex(con, "COMMIT")
wr("scan batch 1000 (update files+locs, insert digests)", 50, scan_batch)

def meta_set(i):
    ex(con, "BEGIN")
    ex(con, f"INSERT INTO entity_metadata VALUES({hexid(20021 + (i*17)%40000)}, 'ek'||printf('%02d', {i}%25), 'newval{i}', {NOW}) "
            f"ON CONFLICT (node_id,key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at")
    ex(con, "COMMIT")
wr("meta set (1 row/txn upsert)", 300, meta_set)

def fmeta_set(i):
    ex(con, "BEGIN")
    ex(con, f"INSERT INTO file_metadata VALUES({hexid(10_000_000 + (i*131)%FILES)}, 'fk'||printf('%02d', {i}%{K}), 'newval{i}', {NOW}) "
            f"ON CONFLICT (file_id,key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at")
    ex(con, "COMMIT")
wr("file meta set (1 row/txn upsert)", 300, fmeta_set)

def link_one(i):
    ex(con, "BEGIN")
    ex(con, f"INSERT INTO file_links VALUES({hexid(10_000_000 + 500_000 + i)}, {hexid(2 + i%18)}, 'doc')")
    ex(con, "COMMIT")
wr("link (1 row/txn)", 300, link_one)

def rename(i):
    ex(con, "BEGIN")
    ex(con, f"UPDATE nodes SET name='R{i}', updated_at={NOW} WHERE id={hexid(3)}")
    ex(con, "COMMIT")
wr("rename node", 100, rename)

def reparent(i):
    ex(con, "BEGIN")
    ex(con, f"UPDATE nodes SET parent_id={hexid(1 + (i%20))}, updated_at={NOW} WHERE id={hexid(25)}")
    ex(con, "COMMIT")
try:
    wr("reparent case", 100, reparent)
except Exception as e:
    W["reparent case"] = {"error": str(e)[:100]}; print("  reparent FAILED", e)
R["writes"] = W

# cascade delete of project-sized subtree (~5k nodes), 2 txns for duckdb-FK safety
try:
    t = time.perf_counter()
    ex(con, "BEGIN")
    ex(con, "CREATE TEMP TABLE doomed AS " + under().replace(P, hexid(20)) + " SELECT id FROM d" if False else
         "CREATE TEMP TABLE doomed AS " + under().replace(f"{P}", hexid(20)) + " SELECT id FROM d")
    ex(con, "DELETE FROM file_links WHERE node_id IN (SELECT id FROM doomed)")
    ex(con, "DELETE FROM entity_metadata WHERE node_id IN (SELECT id FROM doomed)")
    ex(con, "COMMIT")
    ex(con, "BEGIN"); ex(con, "DELETE FROM nodes WHERE id IN (SELECT id FROM doomed)"); ex(con, "COMMIT")
    R["cascade_delete_project_s"] = round(time.perf_counter() - t, 3)
    print(f"  cascade delete project {R['cascade_delete_project_s']}s", flush=True)
except Exception as e:
    R["cascade_delete_project_s"] = "ERR " + str(e)[:100]; print("  cascade FAILED", e)

con.close()
R["size_gb_after_writes"] = round(size() / 1e9, 2)
json.dump(R, open(f"{DIR}/result_{engine}_{K}.json", "w"), indent=1)
print("done", R["size_gb_after_writes"], "GB")
