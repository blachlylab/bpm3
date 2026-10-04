# DuckDB vs SQLite catalog benchmark

**Date:** 2026-10-03
**Harness:** [`benches/engine_comparison`](../../benches/engine_comparison/README.md)
**Raw data:** `engine-comparison-{duckdb,sqlite}-{50,100}M.json`, `engine-comparison-run.log`

## Why

Integrity under UPDATE: DuckDB 1.2.2 and 1.5.6 rewrite an update of an indexed
column on a referenced row as delete plus insert, which fails a foreign key
check. Rename of a Program (indexed `UNIQUE name`), reparent of a node with
children, and a cascade delete in one transaction all fail with real FKs. Foreign
keys from `file_*` tables to `files(id)` work, because `files` is only updated on
non-indexed columns. See `docs/architecture/overview.md` §5.

The question this benchmark answers is whether DuckDB's analytical strengths
outweigh that, at the expected scale (100k nodes, 1M files, 10 to 100M metadata
pairs).

## Method

- Apple M1 Pro, 10 cores, 32 GB, macOS. DuckDB 1.5.6 (Python), SQLite 3.43 (stdlib).
- Same schema on both ("design B"): a single `nodes` table; `entity_metadata`,
  `file_metadata`, `file_digests`, `file_locations`, `file_links`, all with real
  foreign keys. `(key, value)` indexes on both metadata tables. SQLite tables are
  `WITHOUT ROWID`, with an index on `nodes(parent_id)`, `ANALYZE` after load,
  WAL, `synchronous=NORMAL`, 1 GB cache. DuckDB at defaults.
- Data: 100k nodes in the six-level tree, 1M files, 1M links, 2M entity pairs,
  K million file-metadata pairs (50 and 100) across eight key cardinalities from
  5 to 1M distinct values.
- Query times are the best of three runs, except "first", which is the first run
  after reopening the file.

## Results at 100M file-metadata pairs

| | DuckDB | SQLite |
|---|---|---|
| Catalog size | 8.8 GB | 14.1 GB |
| Load plus index build | 297 s | 277 s |
| Rare `key:value` lookup | 14 ms | <0.1 ms |
| `key:value` returning ~10k rows | 63 ms | 2.4 ms |
| `files --under` a Project (50k rows) | 74 ms | 206 ms (first: 2.0 s) |
| `summary` aggregations | 3 to 42 ms | 56 to 771 ms |
| Unlinked files, duplicate detection | 16 to 24 ms | 312 to 437 ms |
| Ingest batch of 1,000 files | 5.3 ms | 4.0 ms |
| Scan batch of 1,000 (updates plus digests) | 16 ms | 51 ms |
| `meta set` on an entity (1 row per txn) | 8.7 ms | 0.13 ms |
| `meta set` on a file (1 row per txn) | 275 ms | 0.78 ms |
| Rename, reparent, link | 0.3 to 0.4 ms | 0.02 to 0.08 ms |
| Cascade delete of a Project subtree | 3.0 s | 1.3 s |

At 50M pairs the picture is the same. DuckDB's single-row file-metadata upsert
was 187 ms there, so it grows with table size. Full per-query tables are in the
JSON files.

## Findings

- Reads are interactive on both engines at 100M pairs. DuckDB's slowest query was
  about 160 ms. SQLite's slowest warm query was about 0.8 s, and its first cold
  `files --under` took about 2 s.
- DuckDB wins analytics, joins, and file size (about 40% smaller). SQLite wins
  selective lookups and small writes by one to three orders of magnitude.
- A one-row file-metadata update on DuckDB costs 190 to 275 ms at this scale.
  Batched writes were not measured.
- DuckDB cannot let a second process read a catalog while a write process holds
  it (`Could not set lock on file`). This contradicts the "many readers, one
  writer" assumption in overview §4. SQLite WAL allows it.
- DuckDB built its indexes faster than SQLite (38 s against 173 s at 100M).

## Limits

One machine, synthetic and evenly distributed data. SQLite is single-threaded and
DuckDB is not. macOS `fsync` is lighter than a Linux server's. Large batch
upserts, concurrent reader and writer throughput on SQLite, and the DuckDB
`ATTACH` of a SQLite file were not measured. The two-selector conjunction query
matched zero rows, so it measures lookup cost only.
