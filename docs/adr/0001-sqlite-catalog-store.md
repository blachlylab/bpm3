# ADR 0001: SQLite as the catalog store

**Status:** Accepted
**Date:** 2026-10-03
**Supersedes:** The DuckDB choice in the architecture overview, Draft 0.1
**Evidence:** [Engine benchmark](../benchmarks/engine-comparison.md), harness in [`benches/engine_comparison`](../../benches/engine_comparison/README.md)

## Decision

A BPM catalog is one SQLite file, opened through the `rusqlite` crate with the engine bundled into the `bpm` binary. The catalog runs in WAL mode with foreign keys enforced on every connection `bpm` opens. The Govern operations database, if it is built, uses SQLite too. Postgres remains the exit described in architecture overview §12, with the same triggers.

DuckDB was the original choice. It was replaced before any catalog shipped.

## Context

BPM's requirements, from the PRD and the architecture overview:

- One static binary, one file per catalog, no database server in Core.
- Expected scale per catalog: about 100k entities, about 1M files, and 10 to 100M metadata pairs, most of them on files.
- Interactive queries, meaning a few seconds at most, on one workstation (PRD §2, scenario 31).
- A six-level entity tree whose integrity matters. Rename, reparent, and cascade delete are ordinary operations.
- `bpm serve` reads the catalog while `bpm ingest` and `bpm scan` write it from another process (overview §4).
- Edits arrive one at a time from the CLI (`bpm meta set`, `bpm link`). Ingest and scan write in batches of 1,000 files.

DuckDB was chosen for its analytical speed over millions of rows. Implementing the schema found two problems.

### 1. DuckDB cannot keep the tree's foreign keys

DuckDB executes an `UPDATE` of an indexed column as a delete plus an insert. On a row that another table references, the delete is checked against the foreign key and fails. It also checks a parent delete before the same transaction's child deletes are visible.

Tested on DuckDB 1.2.2 (the pinned crate) and 1.5.6 (current), with the same result on both:

| Operation with `REFERENCES` declared | DuckDB |
| --- | --- |
| Update a non-indexed column of a referenced row (`files.size_bytes`) | Works |
| Upsert a metadata row whose `file_id` references `files` | Works |
| Rename a Program (`name` is `UNIQUE`, so indexed) | Fails |
| Reparent a Project that has Cases | Fails |
| Reparent a leaf Case | Works |
| Cascade delete, leaves first, in one transaction | Fails |
| The same cascade delete as separate transactions | Works |

The working-tree draft had already removed every `REFERENCES` clause and moved integrity into the library. That leaves `bpm sql --write`, and any library bug, able to write an orphan without the database noticing.

### 2. DuckDB does not allow a reader process beside a writer process

DuckDB lets one process open a file read-write, or many processes open it read-only, but not both at once. A read-only open from a second process while a writer holds the file fails with `IO Error: Could not set lock on file`. Overview §4 assumed "many readers and one writer". Under DuckDB, `bpm serve` could not run during an ingest or scan, which are long by design.

### The workload is mostly not analytical

Most BPM operations are selective: one `key:value` selector, one entity's files, one UUID, one row edited. The analytical exceptions are `summary`, the unlinked-file anti-join, and possible-duplicate detection. DuckDB's columnar engine helps those and does little for the rest.

## Options considered

| Option | Integrity | Reader beside writer | Effort | Outcome |
| --- | --- | --- | --- | --- |
| A. DuckDB as built, integrity in the library, plus an `fsck` command | Library only. `bpm sql --write` can orphan rows | No | None | Rejected |
| B. DuckDB with the schema reshaped around its limits | Real FKs on most tables. Parent rule and sibling-name uniqueness stay in the library. Cascade delete in two transactions | No | Moderate | Rejected |
| C. SQLite | Real FKs, including the parent chain. Single-transaction cascade delete | Yes, in WAL mode | Rewrite of the catalog module and V001 | **Chosen** |
| D. SQLite, with DuckDB attaching the file read-only for analytics | As C | As C | C, plus an optional dependency | Deferred. Possible later, not needed now |
| E. Postgres | Full | Yes | Breaks the single-binary product decision | Rejected. Stays the documented exit |
| F. Wait for DuckDB to fix it | — | — | — | Rejected. Unchanged from 1.2.2 to 1.5.6, and it follows from how DuckDB implements indexes |

Option B reshapes the schema into one `nodes` table, leaves `parent_id` unindexed and without a foreign key, drops the `UNIQUE` sibling-name constraint, and runs cascade delete as two transactions. It works, and it was verified on 1.2.2. It still leaves the reader-beside-writer problem unsolved, and it bends the schema to an engine limitation.

## Benchmark

The full write-up is in [the benchmark results](../benchmarks/engine-comparison.md). The data is synthetic. It is evenly distributed and generated in SQL, and it is not a real catalog. Treat the numbers as orders of magnitude.

- **Machine:** Apple M1 Pro, 10 cores, 32 GB, macOS.
- **Engines:** DuckDB 1.5.6 at defaults. SQLite 3.43 in WAL mode, with `synchronous=NORMAL`, a 1 GB page cache, `WITHOUT ROWID` tables, and `ANALYZE` after load.
- **Schema:** the same on both. It is option B's single `nodes` table, with real foreign keys from metadata, links, and the file tables, and `(key, value)` indexes on both metadata tables.
- **Data:** 100k nodes, 1M files, 1M links, 2M entity metadata pairs, and 50M or 100M file metadata pairs.
- **Timing:** best of three runs. "Cold" is the first run after reopening the file.

Results at 100M file metadata pairs:

| | DuckDB | SQLite |
| --- | --- | --- |
| Catalog size | 8.8 GB | 14.1 GB |
| Load plus index build | 297 s | 277 s |
| Rare `key:value` lookup | 14 ms | <0.1 ms |
| `key:value` returning about 10k rows | 63 ms | 2.4 ms |
| `files --under` a Project, 50k rows | 74 ms | 206 ms (cold 2.0 s) |
| `summary` aggregations | 3 to 42 ms | 56 to 771 ms |
| Unlinked files, possible duplicates | 16 to 24 ms | 312 to 437 ms |
| Ingest batch of 1,000 files | 5.3 ms | 4.0 ms |
| Scan batch of 1,000 | 16 ms | 51 ms |
| `meta set` on an entity, 1 row per transaction | 8.7 ms | 0.13 ms |
| `meta set` on a file, 1 row per transaction | 275 ms | 0.78 ms |
| Rename, reparent, link | 0.3 to 0.8 ms | 0.02 to 0.08 ms |
| Cascade delete of a Project subtree | 3.0 s | 1.3 s |

What the numbers show:

- Both engines meet the PRD target at 100M pairs. DuckDB's slowest query took about 160 ms. SQLite's slowest warm query took about 0.8 s, and its slowest cold query about 2.5 s.
- DuckDB is faster on whole-table aggregation, from about 3 times (a grouped count on one metadata key) to about 200 times (drift counts), and typically 15 to 25 times. It is also about 40% smaller on disk.
- SQLite is one to three orders of magnitude faster on selective lookups and single-row writes.
- DuckDB's single-row file-metadata upsert grew with table size: 187 ms at 50M pairs, 275 ms at 100M.
- DuckDB built its secondary indexes faster (38 s against 173 s), but loaded tables slower. Total load time was about the same.

Limits of this benchmark:

- One machine.
- SQLite runs single-threaded and DuckDB uses every core.
- macOS `fsync` is cheaper than a Linux server's. The chosen configuration is `synchronous=FULL`, which costs more per commit than the `NORMAL` that was benchmarked.
- Not measured: large batched upserts, sustained reader and writer contention, and DuckDB attaching a SQLite file.
- The benchmark used option B's single node table. The chosen schema keeps six node tables. That difference does not change the lookup or aggregation paths measured.

## Why SQLite

1. **Integrity in the database.** All of these were verified on SQLite 3.43 with the six-table schema:
   - rename, reparent of a node with children, and a leaves-first cascade delete in one transaction, all with `ON DELETE RESTRICT` declared;
   - an illegal parent is rejected;
   - a parent with children cannot be deleted;
   - `STRICT` rejects a value of the wrong type.

   `bpm sql --write` gains the same protection, because `bpm` enables foreign keys on that connection.
2. **The process model works as designed.** WAL mode gives concurrent readers beside one writer across processes, which was verified. Readers see the last committed state. SQLite's `BEGIN IMMEDIATE` and `busy_timeout` replace the separate `<catalog>.lock` advisory lock.
3. **The workload fits.** BPM's common operations are selective reads and small writes. The analytical exceptions stay inside the PRD target.
4. **The product stays one binary and one file.** `rusqlite` bundles the engine, and nothing runs outside the binary.
5. **The file format is stable.** SQLite's format has been backward compatible since 2004. That removes the pinned-crate storage-format failure the DuckDB plan had to manage (migrations doc).
6. **Tooling.** Every platform has a `sqlite3` shell and drivers in every language, which suits an open catalog format for labs.

## Consequences

Gains:

- Declared foreign keys on the parent chain and on every `file_id`, with `ON DELETE RESTRICT`.
- `bpm serve` runs during ingest and scan.
- Single-row CLI edits are sub-millisecond.
- The `<catalog>.lock` file and its code go away. The per-run liveness flock stays.
- Migrations use SQLite's transactional DDL. The table rebuild procedure is documented in the migrations doc.

Costs:

- `summary`-style queries are typically 15 to 25 times slower than on DuckDB. They take about 0.5 to 0.8 s at 100M pairs, within target but not instant. Option D is the remedy if that changes.
- Catalog files are about 60% larger than DuckDB's at this scale.
- The catalog file must be on a local filesystem. WAL shared memory does not work over NFS or SMB. Data files may still be on network storage. For a core facility, the catalog lives on the server's local disk.
- Every connection must set `foreign_keys=ON`, which SQLite leaves off by default. A third-party `sqlite3` shell that does not set it can write rows that break a declared key. A future `bpm fsck` can run `PRAGMA foreign_key_check`.
- `STRICT` tables need SQLite 3.37 or later, so an older system `sqlite3` shell cannot open a catalog.
- A file copy taken while a writer is active may miss commits still in the `-wal` file. Copy while no writer is running, or add a `bpm backup` command (overview §14).
- SQLite runs one query on one core. A query that is slow cannot be made faster with more cores.

Implementation, completed with this ADR:

- `Cargo.toml` depends on `rusqlite` (`bundled`, pinned) in place of `duckdb`. `fs4` is dropped with the advisory lock, which was its only use. The per-run liveness flock in stage 2 will bring a file-lock dependency back.
- `migrations/V001__init.sql` is rewritten for SQLite: `STRICT` tables, `WITHOUT ROWID` where the key is text or composite, and `REFERENCES ... ON DELETE RESTRICT` restored. No catalog had shipped, so V001 was rewritten rather than followed by a V002.
- `src/catalog.rs` and `src/migrate.rs` use `BEGIN IMMEDIATE` for every write, `busy_timeout` for the busy error, a `SQLITE_OPEN_READ_ONLY` connection for reads, and ISO 8601 `Z` timestamps.
- `bpm init` sets `PRAGMA application_id`. A SQLite file without BPM's id is refused and never migrated.
- `src/lock.rs` became `src/perms.rs`, which keeps only the file-mode helpers.
- The default catalog is `~/.bpm/default.db`.
- New integration tests cover:
  - a second writer reported as busy;
  - a reader in another process seeing committed rows while a writer holds the lock;
  - `bpm sql --write` refused when it would orphan a parent or a file link;
  - a foreign SQLite file refused unchanged.

## Revisit when

- A real catalog's `summary` or search exceeds the PRD target. First try option D, a read-only DuckDB attachment, before changing the store.
- Several hosts must write one catalog, or the database itself must enforce point-in-time recovery or row-level security. Those are the Postgres triggers in overview §12.

## Follow-ups

- **One node table.** Merging the six node tables would make `(node_type, node_id)` on links and metadata a real foreign key. Overview §14, open question 4.
- **Durability setting.** `synchronous=FULL` is the starting choice. `NORMAL` in WAL mode never corrupts the file, but it can lose the last commits on power loss. Measure the cost on Linux before relaxing it.
- **`ANALYZE` cadence.** Run `PRAGMA optimize` after large ingests and scans, and confirm the planner keeps choosing the `(key, value)` indexes as metadata grows.
- **Backup.** `bpm backup` through the SQLite backup API or `VACUUM INTO`. Overview §14, open question 5.
